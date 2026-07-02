//! Streamable-HTTP serving glue. The host (kasaterm) is a synchronous
//! winit/wgpu app, so we own a small multi-thread tokio runtime on a
//! dedicated background thread and run axum there. The `Backend` is
//! channel-based and `Send + Sync`, so calling it from async handlers on
//! another thread is safe.

use std::sync::Arc;

use kasa_socket::backend::{Backend, PanelKind};
use axum::{
    body::Bytes,
    extract::{Path as AxPath, Query},
    http::{header, HeaderMap, Method},
    response::IntoResponse,
    routing::{any, get, post},
    Json,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpService,
};

use crate::{git, KasaspaceTools};

/// `GET /git-status` — JSON snapshot of the host's current working dir for
/// the webview panel to poll. The wildcard CORS header lets the webview
/// (a different origin) fetch it; the server only binds to 127.0.0.1 so the
/// open origin stays local-only.
/// Directory git commands run in: follow the active pane's shell cwd so the
/// panel tracks the user's terminal directory; fall back to the host cwd.
fn resolve_cwd(backend: &Arc<dyn Backend>) -> std::path::PathBuf {
    backend
        .active_cwd()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}

/// Body for `POST /git-commit`: which files to stage and the message.
#[derive(serde::Deserialize)]
struct CommitReq {
    files: Vec<String>,
    message: String,
}

async fn git_status_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = git::git_status(&resolve_cwd(&backend));
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /git-diff?path=<file>` — diff of one file for inline expansion.
async fn git_diff_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params.get("path").cloned().unwrap_or_default();
    let body = git::git_diff(&resolve_cwd(&backend), &path);
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /git-commit` — stage exactly the checked files and commit.
///
/// Body is a raw JSON *string* (Content-Type text/plain), not an
/// `application/json` body. The webview panel loads from `with_html` (null
/// origin); a json content-type would trip a CORS preflight (OPTIONS) that
/// axum's `post()` route answers with 405, silently killing the request.
/// text/plain is a CORS "simple" content-type, so no preflight — and unlike a
/// query string it carries the file list + multi-line message cleanly.
async fn git_commit_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    let resp = match serde_json::from_str::<CommitReq>(&body) {
        Ok(req) => git::git_commit(&resolve_cwd(&backend), &req.files, &req.message),
        Err(e) => serde_json::json!({ "ok": false, "error": format!("bad request body: {e}") }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(resp))
}

/// `POST /git-push` — push the current branch.
async fn git_push_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = git::git_push(&resolve_cwd(&backend));
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

// ── 스케줄러(반복 지시 루프 · 예약 크론 · 타이머/리마인더) ───────────────────
// 모두 "정해진 시각에 surface 로 text 를 send" 로 통일. loop=interval 마다 반복,
// cron=at_ts 1회, timer=now+interval 1회. 백그라운드 task 가 10s 마다 발사한다.

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ScheduleItem {
    id: String,
    kind: String, // "loop" | "cron" | "timer"
    surface: String,
    text: String,
    #[serde(default)]
    interval_sec: u64,
    #[serde(default)]
    at_ts: f64,
    #[serde(default)]
    next_ts: f64,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    label: String,
}
fn default_true() -> bool {
    true
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn schedule_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/schedule.json"))
}

fn read_schedule() -> Vec<ScheduleItem> {
    let Some(p) = schedule_path() else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&p) else { return Vec::new() };
    serde_json::from_str(&s).unwrap_or_default()
}

fn write_schedule(items: &[ScheduleItem]) {
    let Some(p) = schedule_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(items) {
        let _ = std::fs::write(&p, s);
    }
}

/// 10초마다 due 항목 발사. loop 는 next_ts 갱신, cron/timer 는 발사 후 disable.
async fn schedule_loop(backend: Arc<dyn Backend>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let mut items = read_schedule();
        if items.is_empty() {
            continue;
        }
        let now = now_unix();
        let mut changed = false;
        for it in items.iter_mut() {
            if !it.enabled || it.next_ts <= 0.0 || now < it.next_ts {
                continue;
            }
            // 발사 — 학생 TUI 제출(submit_payload).
            let _ = backend.send_text(Some(&it.surface), &submit_payload(&it.text));
            // 모모톡에도 노란 버블로 — send_text 는 PTY 주입만 하고 messages.jsonl 에 안 남겨
            // 예약/타이머 발신이 대화창에 안 떴다(거노). read=false 로 inbox(미확인) 기록.
            persist_sensei_msg(
                &resolve_cwd(&backend),
                &it.surface,
                &it.text,
                false,
                backend.active_room().as_deref(),
            );
            changed = true;
            match it.kind.as_str() {
                "loop" if it.interval_sec > 0 => {
                    it.next_ts = now + it.interval_sec as f64;
                }
                _ => {
                    it.enabled = false; // cron·timer 1회성
                }
            }
        }
        if changed {
            write_schedule(&items);
        }
    }
}

/// `GET /schedule` — 스케줄 목록.
async fn schedule_list_handler() -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": true, "items": read_schedule() })),
    )
}

/// `POST /schedule` — 항목 추가. body{kind,surface,text,interval_sec?,at_ts?,label?}.
/// next_ts 는 kind 로 계산(loop/timer=now+interval, cron=at_ts).
async fn schedule_add_handler(body: String) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })));
        }
    };
    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let surface = v.get("surface").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if !matches!(kind.as_str(), "loop" | "cron" | "timer") || surface.is_empty() || text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "kind/surface/text required" })));
    }
    let interval_sec = v.get("interval_sec").and_then(|x| x.as_u64()).unwrap_or(0);
    let at_ts = v.get("at_ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let now = now_unix();
    let next_ts = match kind.as_str() {
        "cron" => at_ts,
        _ => now + interval_sec.max(1) as f64, // loop·timer
    };
    let id = format!("{:08x}", (now * 1000.0) as u64 & 0xffff_ffff);
    let item = ScheduleItem {
        id: id.clone(),
        label: v.get("label").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        kind,
        surface,
        text,
        interval_sec,
        at_ts,
        next_ts,
        enabled: true,
    };
    let mut items = read_schedule();
    items.push(item);
    write_schedule(&items);
    (cors, Json(serde_json::json!({ "ok": true, "id": id })))
}

/// `POST /schedule-delete?id=<id>` — 항목 삭제(없으면 toggle 용 enabled 도 받음).
async fn schedule_delete_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let id = params.get("id").cloned().unwrap_or_default();
    let mut items = read_schedule();
    let before = items.len();
    if let Some(toggle) = params.get("toggle") {
        // toggle=1 → enabled 뒤집기(삭제 대신).
        if toggle == "1" {
            for it in items.iter_mut() {
                if it.id == id {
                    it.enabled = !it.enabled;
                }
            }
            write_schedule(&items);
            return (cors, Json(serde_json::json!({ "ok": true })));
        }
    }
    items.retain(|it| it.id != id);
    write_schedule(&items);
    (cors, Json(serde_json::json!({ "ok": true, "removed": before - items.len() })))
}

/// `POST /open-file?path=<abs>` — OS 기본 뷰어로 파일 열기(대화창 이미지 클릭 →
/// macOS Preview 등). `~` 확장. macOS=open, Linux=xdg-open, Windows=cmd start.
async fn open_file_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let raw = params.get("path").cloned().unwrap_or_default();
    let path = match raw.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or(raw),
        None => raw,
    };
    if path.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "path required" })));
    }
    let spawned = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&path).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", &path]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&path).spawn()
    };
    (cors, Json(serde_json::json!({ "ok": spawned.is_ok() })))
}

/// `GET /image-file?path=<abs>` — 로컬 이미지 파일을 바이트로 서빙(BA GUI 대화창
/// 인라인 표시용). 이미지 확장자만 허용(임의 파일 노출 방지), 127.0.0.1 한정.
async fn image_file_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let raw = params.get("path").cloned().unwrap_or_default();
    // 화면 파싱 경로는 `~/...` 일 수 있다(터미널이 ~ 로 표시) — HOME 으로 확장.
    let path = match raw.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|h| format!("{h}/{rest}"))
            .unwrap_or(raw.clone()),
        None => raw.clone(),
    };
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ctype = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        "ico" => "image/x-icon",
        _ => return (StatusCode::BAD_REQUEST, cors, Vec::new()).into_response(),
    };
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CONTENT_TYPE, ctype),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, cors, Vec::new()).into_response(),
    }
}

/// `GET /sent-images?surface=<id>&n=N` — 그 방의 sent-images.jsonl 에서 이 pane 이
/// SendUserFile 로 보낸 이미지 경로 최근 N 개(auto-imgopen 훅이 기록). BA GUI 대화창
/// 인라인 이미지 소스. transcript 엔 경로가 안 남아(input:{}) 훅 기록이 유일.
async fn sent_images_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let surface = params.get("surface").cloned().unwrap_or_default();
    let n = params.get("n").and_then(|s| s.parse::<usize>().ok()).unwrap_or(12);
    let cwd = resolve_cwd(&backend);
    // sent-images.jsonl 은 messages.jsonl 과 독립이라 find_collab_dir 의 messages.jsonl
    // 존재 게이트를 거치면 안 된다 — 터미널서 이미지만 보내고 모모톡 발신이 0이면
    // messages.jsonl 이 없어 게이트가 실패해 영영 빈 배열이었다. collab_messages 와
    // 똑같이 방-인지 dir 을 직접 계산(방 모드면 `{slug}__room_{r}`, 훅 기록 경로와 일치).
    let dir = match backend.active_room().as_deref() {
        Some(r) if !r.is_empty() => {
            std::path::PathBuf::from(format!("/tmp/kasaterm-collab/{}__room_{}", mode_slug(&cwd), r))
        }
        _ => std::path::Path::new("/tmp/kasaterm-collab").join(mode_slug(&cwd)),
    };
    // 세션 경계: since(현재 세션 첫 이벤트 ts, unix sec) 이전 이미지는 이전 대화 잔류물 —
    // 제외(거노: 이전 pane 이미지가 새 대화에 남던 것). sent-images.jsonl 은 방단위 append-only
    // 라 /clear·세션전환 후에도 옛 경로가 누적된다. since 없으면(transcript 빈 경우) 전체.
    let since = params.get("since").and_then(|s| s.parse::<f64>().ok());
    let mut imgs: Vec<String> = Vec::new();
    if let Ok(content) = std::fs::read_to_string(dir.join("sent-images.jsonl")) {
        for line in content.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let pane = v.get("pane").and_then(|p| p.as_str()).unwrap_or("");
            if !surface.is_empty() && pane != surface {
                continue;
            }
            if let Some(s) = since {
                let ts = v.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
                if ts < s {
                    continue;
                }
            }
            if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                imgs.push(p.to_string());
            }
        }
    }
    if imgs.len() > n {
        imgs.drain(0..imgs.len() - n);
    }
    (cors, Json(serde_json::json!({ "ok": true, "images": imgs })))
}

/// task 디렉토리에서 `[(id, subject, status)]` 파싱. id(숫자) 오름차순. 비-json 제외.
fn read_tasks_in_dir(dir: &std::path::Path) -> Vec<(String, String, String)> {
    let mut tasks: Vec<(u64, String, String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else { continue };
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let subject = v.get("subject").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("pending").to_string();
            if subject.is_empty() {
                continue;
            }
            let ord = id.parse::<u64>().unwrap_or(u64::MAX);
            tasks.push((ord, id, subject, status));
        }
    }
    tasks.sort_by_key(|t| t.0);
    tasks.into_iter().map(|(_, id, s, st)| (id, s, st)).collect()
}

/// session_id → task. 신형 `session-<8hex>` 우선·구형 full-uuid 폴백(solo claude 용).
fn read_claude_tasks(session_id: &str) -> Vec<(String, String, String)> {
    if session_id.is_empty() {
        return Vec::new();
    }
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Vec::new();
    };
    let base = home.join(".claude/tasks");
    let prefix: String = session_id.chars().take(8).collect();
    // shim 이 CLAUDE_TASK_LIST_ID=<full session> 를 주입하면 store dir 이 full-uuid 또는
    // session-<full> 형태일 수 있다. 신형 session-<8hex> → full-uuid → session-<full> 순.
    let candidates = [
        base.join(format!("session-{prefix}")),
        base.join(session_id),
        base.join(format!("session-{session_id}")),
    ];
    match candidates.iter().find(|p| p.is_dir()) {
        Some(dir) => read_tasks_in_dir(dir),
        None => Vec::new(),
    }
}

/// pane cwd → 그 cwd 의 **팀(TeamCreate) 세션** task 디렉토리. claude 가 팀 컨텍스트에서
/// TaskCreate 하면 task store 가 개별 대화 세션이 아니라 **팀 세션 id**(`~/.claude/teams/
/// session-<id>` = `~/.claude/tasks/session-<id>`)로 keying 된다(실측: 아로나 task=팀
/// session-4c79638c, 그 팀 cwd=/Users/kasa/Desktop). statusline session 으로 못 잡힐 때
/// (팀 lead≠메인 대화·statusline 미보고) cwd 로 팀을 찾는 폴백. 같은 cwd 팀 여럿이면 mtime 최신.
fn team_task_dir_for_cwd(cwd: &str) -> Option<std::path::PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let teams = home.join(".claude/teams");
    let tasks_base = home.join(".claude/tasks");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(&teams).ok()?.flatten() {
        let Ok(content) = std::fs::read_to_string(entry.path().join("config.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else { continue };
        let matches = v
            .get("members")
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter().any(|mem| mem.get("cwd").and_then(|c| c.as_str()) == Some(cwd)))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let task_dir = tasks_base.join(entry.file_name());
        if !task_dir.is_dir() {
            continue;
        }
        // 같은 cwd 에 팀 여럿(매 세션 새 팀) — 빈 팀(task 0개)은 건너뛰고, 실제 task 가
        // 있는 팀 중 가장 최근 task 가 쓰인 것을 고른다(빈 새 팀이 옛 task 팀을 가리지 않게).
        let mut latest_task: Option<std::time::SystemTime> = None;
        if let Ok(rd) = std::fs::read_dir(&task_dir) {
            for f in rd.flatten() {
                if f.path().extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(mt) = f.metadata().and_then(|m| m.modified()) {
                    if latest_task.map(|b| mt > b).unwrap_or(true) {
                        latest_task = Some(mt);
                    }
                }
            }
        }
        if let Some(mt) = latest_task {
            if best.as_ref().map(|(b, _)| mt > *b).unwrap_or(true) {
                best = Some((mt, task_dir));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// `GET /pane-tasks?surface=<id>` — claude TaskCreate 태스크를 pane 별로(arona 업무 탭).
/// `pane_session_ids`(bound transcript stem) → 없으면 board cwd 로 팀 task 디렉토리 폴백.
async fn pane_tasks_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let surface = params.get("surface").cloned().unwrap_or_default();
    let board = backend.collab_board().unwrap_or_default();
    let reported: std::collections::HashMap<String, String> =
        backend.pane_session_ids().unwrap_or_default().into_iter().collect();
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut debug: Vec<serde_json::Value> = Vec::new();
    // cwd 팀 폴백은 한 pane 에만 매긴다 — 같은 cwd 의 여러 pane 이 같은 옛 팀(TeamCreate)
    // 디렉토리를 공유해, 매핑 못 잡은 pane 마다 똑같은 태스크가 중복으로 떴다(거노: 두
    // 미도리가 같은 태스크). 처음 그 팀을 가져간 pane 만 표시(surface 명시 단일 요청은
    // board 가 1행이라 영향 없음).
    let mut claimed_team: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for row in &board {
        if !surface.is_empty() && row.surface_id != surface {
            continue;
        }
        // 1) bound transcript session(solo claude — session==task), 2) 팀(cwd) 폴백.
        let reported_sid = reported.get(&row.surface_id).cloned().unwrap_or_default();
        let mut tasks = read_claude_tasks(&reported_sid);
        let team = team_task_dir_for_cwd(&row.cwd);
        if tasks.is_empty() {
            if let Some(dir) = &team {
                if claimed_team.insert(dir.clone()) {
                    tasks = read_tasks_in_dir(dir);
                }
            }
        }
        debug.push(serde_json::json!({
            "pane": row.surface_id, "cwd": row.cwd, "reported_session": reported_sid,
            "team_dir": team.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "n": tasks.len(),
        }));
        for (id, subject, status) in tasks {
            out.push(serde_json::json!({
                "pane": row.surface_id, "id": id, "subject": subject, "status": status,
            }));
        }
    }
    (cors, Json(serde_json::json!({ "ok": true, "tasks": out, "debug": debug })))
}

/// `POST /paste-image?surface=%N` (body=이미지 raw 바이트) — 아로나 프롬프트 입력창에
/// 이미지 드롭. 그 pane claude 에 시스템 클립보드 비트맵+Ctrl+V 로 첨부(GUI 위임).
async fn paste_image_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let surface = params.get("surface").cloned().unwrap_or_default();
    if surface.is_empty() || body.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false })));
    }
    // 클립보드+Ctrl+V 로 claude 입력에 [Image] 첨부만. 아로나 대화창엔 send 후 프록시가
    // 캡처한 user 메시지(텍스트+이미지)로 말풍선에 뜬다 — sent-images 큰 박스 write 안 함(거노).
    let ok = backend.paste_image(&surface, body.to_vec()).is_ok();
    (cors, Json(serde_json::json!({ "ok": ok })))
}

/// `POST /git-panel` — 아로나 타이틀바 버튼 → 터미널 GUI git 소스컨트롤 패널 토글(거노).
async fn git_panel_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let ok = backend.toggle_git_panel().is_ok();
    (cors, Json(serde_json::json!({ "ok": ok })))
}

/// `GET /list-dir?path=<path>` — 그 경로의 하위 디렉터리 목록(방 경로 변경 모달).
/// path 없으면 active 방 cwd. 숨김(.) 제외·디렉터리만·이름 정렬. parent 로 상위 이동.
async fn list_dir_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params
        .get("path")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| resolve_cwd(&backend));
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&path) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.starts_with('.') {
                    dirs.push(name);
                }
            }
        }
    }
    dirs.sort();
    let parent = path.parent().map(|p| p.to_string_lossy().into_owned());
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "ok": true,
            "path": path.to_string_lossy(),
            "parent": parent,
            "dirs": dirs,
        })),
    )
}

/// `POST /room-cd?path=<path>` — 방(active pane)을 그 경로로 이동.
/// **셸 pane 일 때만** `cd '<path>'` + CR 을 주입한다. claude 등 다른 포그라운드가
/// 떠 있으면 raw `cd` 가 그 프로그램 입력칸에 박히므로(거노: "프롬프트에 cd~~가 입력돼")
/// 아무것도 보내지 않고 현재 cwd 를 유지한다 — BA GUI 는 돌아가는 세션을 건드리지 않는다.
async fn room_cd_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let path = match params.get("path").filter(|s| !s.is_empty()) {
        Some(p) => p.clone(),
        None => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": "path required" })));
        }
    };
    let proc = backend.active_process_name().unwrap_or_default();
    let base = proc.strip_prefix('-').unwrap_or(&proc);
    let is_shell = matches!(base, "zsh" | "bash" | "fish" | "sh" | "dash" | "tcsh" | "ksh");
    if !is_shell {
        // 셸이 아님(claude/vim/build…) → cd 미주입, 세션 무접촉.
        return (cors, Json(serde_json::json!({ "ok": true, "path": path, "skipped": proc })));
    }
    let quoted = path.replace('\'', "'\\''");
    let ok = backend.send_text(None, &format!("cd '{quoted}'\r")).is_ok();
    (cors, Json(serde_json::json!({ "ok": ok, "path": path })))
}

/// Body for `POST /git-ai-commit`: the files the user checked in the panel.
/// Empty → let the AI decide what to include.
#[derive(serde::Deserialize)]
struct AiCommitReq {
    #[serde(default)]
    files: Vec<String>,
}

/// `POST /git-ai-commit` — delegate the commit to the AI. If the active pane
/// runs claude, inject a commit instruction (with the checked files) so the
/// working agent does the commit; otherwise ask the user to focus a claude
/// pane (agent spawn is phase 2).
async fn git_ai_commit_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    // Raw JSON string body (text/plain) to avoid the CORS preflight — see
    // git_commit_handler. Empty/garbage body falls back to "no files".
    let req: AiCommitReq = serde_json::from_str(&body).unwrap_or(AiCommitReq { files: Vec::new() });
    let proc = backend.active_process_name().unwrap_or_default();
    let body = if proc.contains("claude") {
        let msg = if req.files.is_empty() {
            "git 패널에서 AI 커밋을 눌렀어. 지금 작업 디렉토리의 변경사항을 검토하고 적절한 한국어 커밋 메시지로 git add + commit 해줘.\n".to_string()
        } else {
            format!(
                "git 패널에서 AI 커밋을 눌렀어. 체크된 파일은 다음과 같아: {}. 이 파일들만 stage해서 적절한 한국어 커밋 메시지로 commit 해줘.\n",
                req.files.join(", ")
            )
        };
        let _ = backend.send_text(None, &msg);
        serde_json::json!({ "ok": true, "output": "작업 중인 claude에게 커밋을 요청했어요" })
    } else {
        let who = if proc.is_empty() { "셸".to_string() } else { proc };
        serde_json::json!({ "ok": false, "output": format!("claude가 켜진 pane에서 눌러주세요 (활성: {who})") })
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /open-image?path=<file>` — open a separate image-viewer window.
/// `GET /open-markdown?path=<file>` — open a separate markdown editor.
///
/// GET with a query param (not a JSON body) on purpose: the `imgopen` /
/// `mdopen` shims behind these are tiny `curl` one-liners, and a bodyless
/// GET is the simplest no-preflight call. `path` is resolved to an absolute
/// path by the shim before it gets here.
async fn open_image_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params.get("path").cloned().unwrap_or_default();
    // `pane` (the caller's $KASATERM_PANE_ID) lets the host split the preview
    // beside the requesting pane instead of the last-focused sidebar window.
    let pane = params.get("pane").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let body = match backend.open_preview("image", &path, pane) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

async fn open_markdown_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params.get("path").cloned().unwrap_or_default();
    let pane = params.get("pane").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let body = match backend.open_preview("markdown", &path, pane) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// Body for `POST /save-markdown`: the file to overwrite and its new text.
#[derive(serde::Deserialize)]
struct SaveMarkdownReq {
    path: String,
    content: String,
}

/// `POST /save-markdown` — overwrite a markdown file from the editor window.
/// Raw JSON *string* body (text/plain) to dodge the CORS preflight, same as
/// `/git-commit`. The file IO is local and quick, so it runs straight on the
/// tokio thread — no main-thread hop needed (unlike window creation).
async fn save_markdown_handler(body: String) -> impl IntoResponse {
    let resp = match serde_json::from_str::<SaveMarkdownReq>(&body) {
        Ok(req) => match std::fs::write(&req.path, req.content) {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        Err(e) => serde_json::json!({ "ok": false, "error": format!("bad request body: {e}") }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(resp))
}

/// `GET /sessions` — JSON snapshot of the tmux-style session tabs for the
/// session panel to poll: `{ count, active }`.
async fn sessions_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let s = backend.sessions();
    let body = serde_json::json!({ "count": s.count, "active": s.active, "saved": s.saved, "labels": s.labels });
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /board` — JSON snapshot of every pane's activity (`collab.board`) for
/// the board panel to poll: `{ board: [{surface_id, intent, status, files}] }`.
async fn board_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let board = backend.collab_board().unwrap_or_default();
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "board": board })),
    )
}


/// characters.json 후보 경로 — kasaterm-assign-character.py 와 같은 우선순위:
/// ~/.config/kasaterm/characters.json → 번들 collab-hooks (env 오버라이드 →
/// .app Resources → 레포 소스). 파싱 실패 파일은 건너뛰고 다음 후보로 (py 동일).
fn characters_candidate_paths() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        v.push(std::path::PathBuf::from(home).join(".config/kasaterm/characters.json"));
    }
    if let Ok(p) = std::env::var("KASATERM_COLLAB_HOOKS_DIR") {
        v.push(std::path::PathBuf::from(p).join("characters.json"));
    }
    if let Ok(exe) = std::env::current_exe() {
        // <bundle>/Contents/MacOS/kasaterm → <bundle>/Contents/Resources/collab-hooks
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/collab-hooks/characters.json"))
        {
            v.push(res);
        }
    }
    // cargo run (dev): 이 crate 기준 레포 안 정본
    v.push(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/kasaterm/collab-hooks/characters.json"),
    );
    v
}

/// 후보들 중 첫 번째로 읽히고 JSON 으로 파싱되는 파일의 내용.
fn first_valid_json(paths: &[std::path::PathBuf]) -> Option<serde_json::Value> {
    for p in paths {
        let Ok(s) = std::fs::read_to_string(p) else { continue };
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            return Some(v);
        }
    }
    None
}

/// `GET /characters` — 캐릭터 테마 정의를 그대로 JSON 으로 반환. 없으면 404
/// (테마 미설치 = 기능 전체 skip 이 규약이라, 프런트가 404 로 분기한다).
async fn characters_handler() -> impl IntoResponse {
    let (status, body) = match first_valid_json(&characters_candidate_paths()) {
        Some(v) => (axum::http::StatusCode::OK, v),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "characters.json not found" }),
        ),
    };
    (status, [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// SKILL.md / commands/*.md frontmatter("--- … ---" 사이)의 description 추출.
fn frontmatter_desc(path: &std::path::Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let mut in_fm = false;
    for line in s.lines() {
        let t = line.trim();
        if t == "---" {
            if in_fm {
                break;
            }
            in_fm = true;
            continue;
        }
        if in_fm {
            if let Some(rest) = t.strip_prefix("description:") {
                return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
            }
        }
    }
    None
}

fn add_cmd(
    cmds: &mut Vec<serde_json::Value>,
    seen: &mut std::collections::HashSet<String>,
    cmd: String,
    desc: String,
) {
    if seen.insert(cmd.clone()) {
        cmds.push(serde_json::json!({ "cmd": cmd, "desc": desc }));
    }
}

/// `GET /slash-commands` — claude 가 `/` 자동완성에 보여주는 동적 명령(스킬·커스텀·플러그인)을
/// 디스크 스캔(거노: 스킬 이런 거 다). ~/.claude/skills·commands·plugins + 프로젝트 .claude/skills.
/// MCP 프롬프트는 서버 런타임이라 파일 스캔 불가 — 프런트 정적 목록이 내장 명령을 커버한다.
async fn slash_commands_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let mut cmds: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::Path::new(&home);
        // ~/.claude/skills/<name>/SKILL.md → /<name>
        if let Ok(rd) = std::fs::read_dir(home.join(".claude/skills")) {
            for e in rd.flatten() {
                let md = e.path().join("SKILL.md");
                if md.exists() {
                    let name = e.file_name().to_string_lossy().to_string();
                    add_cmd(&mut cmds, &mut seen, format!("/{name}"), frontmatter_desc(&md).unwrap_or_default());
                }
            }
        }
        // ~/.claude/commands/<name>.md → /<name>
        if let Ok(rd) = std::fs::read_dir(home.join(".claude/commands")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    add_cmd(&mut cmds, &mut seen, format!("/{name}"), frontmatter_desc(&p).unwrap_or_default());
                }
            }
        }
        // 플러그인 ~/.claude/plugins/cache/<mk>/<plugin>/<ver>/skills/<name>/SKILL.md → /<plugin>:<name>
        if let Ok(mks) = std::fs::read_dir(home.join(".claude/plugins/cache")) {
            for mk in mks.flatten() {
                let Ok(plugins) = std::fs::read_dir(mk.path()) else { continue };
                for plugin in plugins.flatten() {
                    let pname = plugin.file_name().to_string_lossy().to_string();
                    let Ok(vers) = std::fs::read_dir(plugin.path()) else { continue };
                    for v in vers.flatten() {
                        if let Ok(rd) = std::fs::read_dir(v.path().join("skills")) {
                            for e in rd.flatten() {
                                let md = e.path().join("SKILL.md");
                                if md.exists() {
                                    let name = e.file_name().to_string_lossy().to_string();
                                    add_cmd(&mut cmds, &mut seen, format!("/{pname}:{name}"), frontmatter_desc(&md).unwrap_or_default());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // 프로젝트 .claude/skills/<name>/SKILL.md (활성 방 cwd)
    if let Ok(rd) = std::fs::read_dir(resolve_cwd(&backend).join(".claude/skills")) {
        for e in rd.flatten() {
            let md = e.path().join("SKILL.md");
            if md.exists() {
                let name = e.file_name().to_string_lossy().to_string();
                add_cmd(&mut cmds, &mut seen, format!("/{name}"), frontmatter_desc(&md).unwrap_or_default());
            }
        }
    }
    (cors, Json(serde_json::json!({ "commands": cmds }))).into_response()
}

/// cwd → 모드 마커 파일명 slug. kasacollab.py `mode_path` 와 동일 규칙
/// ('/' 와 '.' 을 '-' 로): 두 구현이 같은 마커를 읽고 써야 한다.
fn mode_slug(cwd: &std::path::Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// 이 cwd 방의 영속 모드 마커 경로(~/.config/kasaterm/collab-mode/<slug>).
/// pub: 호스트(kasaterm)의 첫 실행 온보딩이 같은 마커로 '미설정 방'을 판정한다.
pub fn mode_marker_path(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config/kasaterm/collab-mode")
            .join(mode_slug(cwd)),
    )
}

/// 앱 전역 1회 온보딩 완료 플래그(~/.config/kasaterm/onboarded). per-cwd
/// collab-mode 마커와 분리한다 — 온보딩(첫 실행 환영 ModePicker)은 방이 아니라
/// 앱 단위 1회 사건이다. 부팅 시 active pane cwd 가 임의적이라(데스크탑에서
/// 열면 데스크탑 온보딩, 실측 사고) 방 기준 판정을 폐기하고 이 플래그로 대체.
pub fn onboarded_marker_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/onboarded"))
}

/// 온보딩 완료를 영속 기록(원자 쓰기). 어떤 모드를 고르든 사용자가 첫 실행
/// 선택을 끝낸 것이므로 모드 set 경로에서 호출한다.
pub fn mark_onboarded() {
    if let Some(p) = onboarded_marker_path() {
        let _ = write_mode_file(&p, "1");
    }
}

/// 이전 버전에서 방 하나라도 모드를 정한 적이 있나 — 글로벌 플래그 도입 전
/// 사용자를 '첫 실행'으로 오인해 재온보딩하지 않으려는 마이그레이션 판정.
pub fn any_collab_mode_marker() -> bool {
    let Some(home) = std::env::var("HOME").ok() else {
        return false;
    };
    let dir = std::path::PathBuf::from(home).join(".config/kasaterm/collab-mode");
    std::fs::read_dir(&dir)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// 마커 내용 → 모드. 없거나 알 수 없는 값이면 solo (kasacollab.py 동일).
fn read_mode_file(path: &std::path::Path) -> &'static str {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim() == "god" => "god",
        _ => "solo",
    }
}

/// 마커 원자 쓰기(tmp + rename) — 읽는 쪽은 완전한 모드명만 본다.
fn write_mode_file(path: &std::path::Path, mode: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, mode)?;
    std::fs::rename(&tmp, path)
}

/// `GET /mode` — 활성 pane cwd 방의 협업 모드 `{ mode, cwd, configured }`.
/// `configured=false` = 마커 자체가 없는 미설정 방(첫 실행) — mode 는 solo 로
/// 뭉개지므로 이 필드 없이는 ModePicker 온보딩 대상을 웹이 구분할 수 없다.
async fn mode_get_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let cwd = resolve_cwd(&backend);
    let marker = mode_marker_path(&cwd);
    let configured = marker.as_deref().is_some_and(|p| p.exists());
    let mode = marker.as_deref().map(read_mode_file).unwrap_or("solo");
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "mode": mode,
            "cwd": cwd.to_string_lossy(),
            "configured": configured,
        })),
    )
}

/// `POST /mode?set=solo|god` — 활성 pane cwd 방의 모드 전환. 값이 쿼리로 오는
/// 이유는 session-switch 와 같다(null-origin webview 의 CORS preflight 회피).
async fn mode_set_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let set = params.get("set").map(String::as_str).unwrap_or("");
    let body = if set != "solo" && set != "god" {
        serde_json::json!({ "ok": false, "error": "set=solo|god required" })
    } else {
        let cwd = resolve_cwd(&backend);
        match mode_marker_path(&cwd) {
            Some(p) => match write_mode_file(&p, set) {
                Ok(()) => {
                    // 모드를 골랐다 = 첫 실행 온보딩을 끝냈다. 전역 플래그를
                    // 세워 다음 부팅에 ModePicker 가 다시 뜨지 않게 한다.
                    mark_onboarded();
                    serde_json::json!({ "ok": true, "mode": set })
                }
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            },
            None => serde_json::json!({ "ok": false, "error": "HOME unset" }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// arona-ui 정적 번들 루트: env 오버라이드 → .app Resources → 레포 dev 빌드.
/// (characters_candidate_paths 와 같은 3단 resolve 철학.)
fn arona_ui_root() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_ARONA_UI_DIR") {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // <bundle>/Contents/MacOS/kasaterm → <bundle>/Contents/Resources/arona-ui
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/arona-ui"))
        {
            if res.is_dir() {
                return Some(res);
            }
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/arona-ui/dist");
    if dev.is_dir() {
        return Some(dev);
    }
    None
}

/// 확장자 → Content-Type. vite dist 가 내는 파일 종류만 커버하면 충분.
fn static_content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// `GET /arona-ui/` + `GET /arona-ui/{*path}` — arona-ui dist 정적 서빙.
/// webview 가 http 로 로드하면 MCP 와 same-origin 이 돼 fetch 가 CORS/포트
/// 문제 없이 붙는다(file:// 로드 대비 이게 선택 이유). canonicalize 비교로
/// 루트 밖 탈출(../)을 차단한다.
async fn arona_ui_serve(rel: String) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let not_found = || {
        (
            axum::http::StatusCode::NOT_FOUND,
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            "not found",
        )
            .into_response()
    };
    let Some(root) = arona_ui_root() else { return not_found() };
    let rel = if rel.is_empty() { "index.html".to_string() } else { rel };
    let (Ok(canon_root), Ok(canon)) = (root.canonicalize(), root.join(&rel).canonicalize())
    else {
        return not_found();
    };
    if !canon.starts_with(&canon_root) {
        return not_found();
    }
    match std::fs::read(&canon) {
        // no-store: webview(WKWebView)가 옛 index.html+JS 를 통째 캐시해 relaunch 후에도
        // stale UI 를 띄우던 문제 차단(거노: 모달 z-fix 가 안 보이던 근본). 로컬·소번들
        // 이라 매 로드 재요청 비용 무시 가능 — 항상 최신.
        Ok(bytes) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, static_content_type(&canon)),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

/// `POST /focus?surface=<id>` — pane 포커스(arona-ui 카드 클릭 → 해당 pane).
/// 쿼리 파라미터인 이유는 session-switch 와 같다(null-origin webview 의 CORS
/// preflight 회피). surface id 의 '%' 는 %25 인코딩(encodeURIComponent) 권장
/// — 다만 실측상 미인코딩 '%1' 도 디코더가 literal 로 통과시킨다(curl 검증).
async fn focus_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match params.get("surface").map(String::as_str) {
        Some(id) if !id.is_empty() => match backend.focus_surface(id) {
            Ok(()) => serde_json::json!({ "ok": true, "surface_id": id }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        _ => serde_json::json!({ "ok": false, "error": "surface query param required" }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /close-pane?surface=<id>` — 학생(워커) pane 종료. PtyBackend 가
/// SocketClose 로 GUI 에 위임 → layout.rs close_pane 이 leaf 제거 + 포커스 이동.
async fn close_pane_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match params.get("surface").map(String::as_str) {
        Some(id) if !id.is_empty() => match backend.close_surface(id) {
            Ok(()) => serde_json::json!({ "ok": true, "surface_id": id }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        _ => serde_json::json!({ "ok": false, "error": "surface query param required" }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}


/// Read a required `usize` query param, defaulting to 0 when absent/garbage.
fn query_idx(params: &std::collections::HashMap<String, String>) -> usize {
    params.get("idx").and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// `POST /session-switch?idx=<n>` — switch the visible session to `idx`.
///
/// The index rides in the query string (not a JSON body) on purpose: the
/// webview panel loads from `with_html` (a null origin), so a JSON body would
/// add a `Content-Type: application/json` header and trip a CORS *preflight*
/// (OPTIONS) that axum's `post()` route answers with 405 — silently killing
/// the request. A bodyless POST is a CORS "simple request": no preflight.
async fn session_switch_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match backend.switch_session(query_idx(&params)) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-new?god=<name>` — 새 방(윈도우) + 선택 god(아로나/프라나) 스폰
/// (거노: 방 추가 시 god 선택). god 미지정이면 아로나 기본.
async fn session_new_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let god = params.get("god").filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or("아로나");
    let body = match backend.new_room(god) {
        Ok(()) => serde_json::json!({ "ok": true, "god": god }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /spawn-student?character=<name>` — 현재 방에 캐릭터 지정 학생 추가(아로나/
/// 프라나 포함). 자동 빈슬롯 배정 대신 사용자가 고른 캐릭터로 split.
async fn spawn_student_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let character = params.get("character").map(|s| s.as_str()).unwrap_or("");
    let body = if character.is_empty() {
        serde_json::json!({ "ok": false, "error": "character required" })
    } else {
        match backend.spawn_student(character) {
            Ok(()) => serde_json::json!({ "ok": true, "character": character }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /swap-character?surface=<id>&character=<name>` — pane 캐릭터 교체(PTY respawn,
/// 대화 리셋). persona 가 셸 spawn 시 고정이라 그 pane 을 새 persona 로 다시 띄운다.
async fn swap_character_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(|s| s.as_str()).unwrap_or("");
    let character = params.get("character").map(|s| s.as_str()).unwrap_or("");
    let body = if surface.is_empty() || character.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface and character required" })
    } else {
        match backend.swap_character(surface, character) {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-close?idx=<n>` — close the session at `idx`. Query param for
/// the same no-preflight reason as session-switch.
async fn session_close_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match backend.close_session(query_idx(&params)) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-restore?idx=<n>` — restore a saved (on-disk) session at
/// `idx` and switch to it. Query param for the same no-preflight reason as
/// session-switch.
async fn session_restore_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match backend.restore_session(query_idx(&params)) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-rename?idx=<n>&name=<name>` — set the session's custom
/// display name (URL-encoded `name`; blank clears it). Query params for the
/// same no-preflight reason as session-switch.
async fn session_rename_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = params.get("name").cloned().unwrap_or_default();
    let body = match backend.rename_session(query_idx(&params), &name) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /recent-sessions?cwd=<abs>` — recent Claude sessions under `cwd` (or
/// the active pane's cwd when omitted) for the arona-ui resume picker. Newest
/// first: `{ ok, sessions: [{id, label, mtime, cwd}] }`.
async fn recent_sessions_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cwd = params.get("cwd").filter(|s| !s.is_empty()).map(|s| s.as_str());
    let body = match backend.recent_sessions(cwd) {
        Ok(sessions) => serde_json::json!({ "ok": true, "sessions": sessions }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-resume?id=<uuid>&cwd=<abs>&newroom=<bool>` — open a pane and
/// inject `claude --resume <id>` once its shell prompt is up. `newroom=true`
/// opens a fresh window; otherwise it splits the active one. Query params for
/// the same no-preflight reason as session-switch.
async fn session_resume_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = params.get("id").cloned().unwrap_or_default();
    let cwd = params.get("cwd").filter(|s| !s.is_empty()).cloned();
    let newroom = params
        .get("newroom")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);
    let attach = params
        .get("attach")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);
    let body = if id.is_empty() {
        serde_json::json!({ "ok": false, "error": "missing id" })
    } else {
        match backend.resume_session(&id, cwd.as_deref(), newroom, attach) {
            Ok(()) => serde_json::json!({ "ok": true, "id": id }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-save?surface=%N` — foreground claude 를 background daemon 으로
/// detach(←← agents-view 주입). surface 없으면 active pane. "대화 저장하기" — 터미널이
/// 꺼져도 daemon 이 세션을 들고 살아남아 웹뷰에서 계속 보인다(거노 핵심).
async fn session_save_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").filter(|s| !s.is_empty()).map(|s| s.as_str());
    let body = match backend.save_session(surface) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// Locate the `claude` binary. A GUI app's PATH is minimal (launchd, not the
/// login shell), so PATH lookup alone misses npm-global/local installs — probe
/// the common locations, honoring `CLAUDE_BIN` for an explicit override, and
/// fall back to bare `claude` (PATH) as a last resort.
fn claude_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_BIN") {
        if !p.is_empty() {
            return p.into();
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.claude/local/claude"),
        format!("{home}/.npm-global/bin/claude"),
        format!("{home}/.local/bin/claude"),
        "/opt/homebrew/bin/claude".to_string(),
        "/usr/local/bin/claude".to_string(),
    ];
    for c in candidates {
        if std::path::Path::new(&c).exists() {
            return c.into();
        }
    }
    "claude".into()
}

/// pid 프로세스 argv 의 `--resume <경로>` basename(부모 세션 uuid). ←← detach 는 부모
/// 대화를 fork 해 새 sessionId 로 잇는데, jsonl 엔 부모 정보가 전혀 없어 이 argv 가
/// A(원본 foreground)→B(background) 를 잇는 유일한 끈이다. macOS/Linux(ps); 그 외 None.
fn parent_session_from_pid(pid: u64) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let cmd = String::from_utf8_lossy(&out.stdout);
    let mut it = cmd.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "--resume" {
            let path = it.next()?;
            return std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string);
        }
    }
    None
}

/// `GET /background-agents?cwd=<abs>` — the `claude agents --json --all` view:
/// the background/interactive sessions Claude's own supervisor hosts, as
/// `{ ok, agents: [{pid,id,cwd,kind,startedAt,sessionId,name,status,state}] }`.
/// The arona classroom polls this to render off-pane "students" (background
/// agents) alongside the local-pane ones; a card click resumes its `sessionId`
/// via `/session-resume`, promoting it back to a foreground pane. `cwd` filters
/// to sessions started under that path (`--cwd`); omitted shows all rooms.
/// Runs the binary directly so the shell `claude` alias/shim is bypassed; the
/// `agents` view is read-only, so no permission flags are involved.
async fn background_agents_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let mut cmd = std::process::Command::new(claude_bin());
    cmd.args(["agents", "--json", "--all"]);
    if let Some(cwd) = params.get("cwd").filter(|s| !s.is_empty()) {
        cmd.args(["--cwd", cwd]);
    }
    let body = match cmd.output() {
        Ok(out) if out.status.success() => {
            match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                Ok(mut agents) => {
                    // background 세션마다 부모(넘어오기 전) surface/sessionId 를 얹는다 —
                    // 웹뷰가 "지금 보던 pane 이 background 로 넘어갔다"를 판정하는 유일한 근거.
                    let pane_sids = backend.pane_session_ids().unwrap_or_default();
                    if let Some(arr) = agents.as_array_mut() {
                        for a in arr.iter_mut() {
                            if a.get("kind").and_then(|k| k.as_str()) != Some("background") {
                                continue;
                            }
                            let Some(pid) = a.get("pid").and_then(|p| p.as_u64()) else {
                                continue;
                            };
                            if let Some(parent_sid) = parent_session_from_pid(pid) {
                                if let Some(obj) = a.as_object_mut() {
                                    if let Some((pane, _)) =
                                        pane_sids.iter().find(|(_, sid)| *sid == parent_sid)
                                    {
                                        obj.insert("parentSurface".into(), serde_json::json!(pane));
                                    }
                                    obj.insert(
                                        "parentSessionId".into(),
                                        serde_json::json!(parent_sid),
                                    );
                                }
                            }
                        }
                    }
                    serde_json::json!({ "ok": true, "agents": agents })
                }
                Err(e) => serde_json::json!({ "ok": false, "error": format!("parse: {e}") }),
            }
        }
        Ok(out) => serde_json::json!({
            "ok": false,
            "error": String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /background-kill?pid=<pid>` — claude agents background 세션을 종료(SIGTERM).
/// claude agents 에 공식 kill 명령이 없어 pid 로 직접 보낸다. pid 는 `/background-agents`
/// 가 준 것(claude 워커 프로세스). 거노: 백그라운드 패널에서 세션을 쉽게 정리.
async fn background_kill_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match params.get("pid").and_then(|s| s.parse::<u32>().ok()) {
        Some(pid) => match std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .output()
        {
            Ok(o) if o.status.success() => serde_json::json!({ "ok": true, "pid": pid }),
            Ok(o) => serde_json::json!({ "ok": false, "error": String::from_utf8_lossy(&o.stderr).trim().to_string() }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        None => serde_json::json!({ "ok": false, "error": "missing/invalid pid" }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-reset` — tear down every session/pane and leave one fresh
/// empty session.
async fn session_reset_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = match backend.reset_sessions() {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// Resolve the `which` query param into a `PanelKind`.
fn query_which(
    params: &std::collections::HashMap<String, String>,
) -> Result<PanelKind, String> {
    match params.get("which").map(|s| s.as_str()) {
        Some("git") => Ok(PanelKind::Git),
        Some("session") | Some("sessions") => Ok(PanelKind::Session),
        Some("board") => Ok(PanelKind::Board),
        other => Err(format!("bad or missing which={other:?} (expected git|session|board)")),
    }
}

/// `POST /terminal-reveal?show=0|1[&pane=%N]` — show/hide the main terminal
/// window. The arona classroom calls this when it opens (hide — the
/// classroom takes the screen over) and from its red-pill button (show —
/// back to the terminal). `pane` optionally focuses that pane on reveal so
/// the classroom can jump the user to a character's seat.
async fn terminal_reveal_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match params.get("show").map(String::as_str) {
        Some("0") | Some("1") => {
            let show = params.get("show").map(String::as_str) == Some("1");
            let pane = params.get("pane").map(String::as_str).filter(|s| !s.is_empty());
            match backend.reveal_terminal(show, pane) {
                Ok(()) => serde_json::json!({ "ok": true, "show": show }),
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            }
        }
        other => serde_json::json!({
            "ok": false,
            "error": format!("bad or missing show={other:?} (expected 0|1)"),
        }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /peek?surface=%N&lines=40[&ansi=1]` — a pane's visible screen text.
/// `ansi=1` returns SGR-encoded color/attribute sequences so a viewer can
/// render terminal colors. Without `ansi`, returns plain text (default).
/// Polling-friendly by design: one lock + visible-text copy, no transcript IO.
async fn peek_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        let lines = params
            .get("lines")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(40);
        let ansi = params.get("ansi").map_or(false, |v| v == "1" || v == "true");
        let result = if ansi {
            backend.peek_ansi(surface, lines)
        } else {
            backend.peek(surface, lines)
        };
        match result {
            Ok(text) => serde_json::json!({
                "ok": true,
                "surface_id": surface,
                "text": text,
                "ansi": ansi,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /blocks?surface=%N&limit=50` — a plain terminal pane's Warp-style
/// command blocks (OSC 133 C/D delimited: command, output, exit code,
/// duration). Newest last. Backs the BA GUI's command-block stack.
async fn blocks_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        let limit = params
            .get("limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(50);
        match backend.pane_blocks(surface, limit) {
            Ok(blocks) => serde_json::json!({
                "ok": true,
                "surface_id": surface,
                "blocks": blocks,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /transcript?surface=%N&turns=20` — a pane's structured dialogue
/// (user prompts + assistant replies, including off-screen turns). Unlike
/// `/peek` (raw rendered screen), this is the clean conversation for the
/// classroom "click a student → see the chat" view; tool_use/tool_result
/// noise is already stripped by `parse_turn`.
async fn transcript_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        let turns = params
            .get("turns")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(20);
        match backend.transcript_tail(surface, turns) {
            Ok(ts) => serde_json::json!({ "ok": true, "surface_id": surface, "turns": ts }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /transcript-raw?surface=%N&offset=<n>` — a pane's bound transcript jsonl,
/// raw and *incremental*. `offset=0` (or omitted) returns the tail window with
/// `reset:true`; `offset>0` returns only whole lines appended since that byte
/// (`reset:false`, empty when unchanged). The BA GUI accumulates `offset` and
/// appends, instead of re-parsing the whole (multi-MB) file every poll. Response
/// `{ ok, surface_id, raw, offset, reset }`.
async fn transcript_raw_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let offset = params.get("offset").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        match backend.transcript_raw(surface, offset) {
            Ok(c) => serde_json::json!({
                "ok": true, "surface_id": surface,
                "raw": c.raw, "offset": c.offset, "reset": c.reset,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /session-transcript-raw?id=<uuid>&cwd=<abs>` — a *past* (offline)
/// session's transcript jsonl, raw and unparsed, addressed by its session uuid
/// + the cwd it ran in (no live pane needed). The BA GUI's resume picker reads
/// this to preview a recent session read-only before the user decides to resume.
async fn session_transcript_raw_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = params.get("id").map(String::as_str).unwrap_or("");
    let cwd = params.get("cwd").filter(|s| !s.is_empty()).map(String::as_str);
    let body = if id.is_empty() {
        serde_json::json!({ "ok": false, "error": "id=<uuid> required" })
    } else {
        match backend.session_transcript_raw(id, cwd) {
            Ok(raw) => serde_json::json!({ "ok": true, "id": id, "raw": raw }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /subagents?surface=%N` — the subagents (Task/Agent) a pane's claude has
/// spawned, newest first, from its `subagents/agent-*.meta.json` sidecars. The
/// BA GUI lists these so the user can drill into a subagent's full dialogue.
async fn subagents_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        match backend.subagents(surface) {
            Ok(list) => serde_json::json!({ "ok": true, "surface_id": surface, "subagents": list }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /subagent-transcript-raw?surface=%N&agentId=<id>` — one subagent's
/// transcript jsonl, raw and unparsed. Same `{ raw }` shape as `/transcript-raw`;
/// the BA GUI renders it with the same per-tool path.
async fn subagent_transcript_raw_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let agent_id = params.get("agentId").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() || agent_id.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N and agentId=<id> required" })
    } else {
        match backend.subagent_transcript_raw(surface, agent_id) {
            Ok(raw) => serde_json::json!({ "ok": true, "surface_id": surface, "agent_id": agent_id, "raw": raw }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /paste-active` body:`{text, submit}` — inject `text` into the active
/// pane's PTY (no `surface` — uses whatever pane is focused). `submit=false`
/// (default) types the text without a trailing newline so the user reviews and
/// presses Enter themselves; `submit=true` appends a newline to run it. The BA
/// GUI's offline-session "resume in current terminal" button uses this.
async fn paste_active_handler(
    backend: Arc<dyn Backend>,
    body: String,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let (text, submit) = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => (
                v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                v.get("submit").and_then(|s| s.as_bool()).unwrap_or(false),
            ),
            Err(e) => {
                return (cors, Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })));
            }
        }
    } else {
        (body.trim().to_string(), false)
    };
    if text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "text is empty" })));
    }
    let payload = if submit { format!("{text}\n") } else { text };
    let body = match backend.send_text(None, &payload) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    (cors, Json(body))
}

/// `GET /layout` — 현재 윈도우의 pane split 배치(% rect 배열, window_layout 재활용).
/// BA GUI 가 이걸로 터미널 분할을 그대로 미러한 그리드를 그린다(각 pane = 세션 뷰어 칸).
/// rect 가 이미 % 좌표라 프론트는 position:absolute 로 배치만 하면 된다.
async fn layout_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = match backend.window_layout() {
        Ok(panes) => serde_json::json!({ "ok": true, "panes": panes }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// /tmp/kasaterm-collab/ 아래 모든 방을 순회해 lead 파일이 있는 첫 god pane id
/// 를 반환한다. active_cwd 가 쉘 디렉토리를 따르므로 슬러그 계산 대신 스캔.
fn find_god_pane() -> Option<String> {
    let base = std::path::Path::new("/tmp/kasaterm-collab");
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        let lead = entry.path().join("lead");
        if let Ok(g) = std::fs::read_to_string(&lead) {
            let g = g.trim().to_string();
            if !g.is_empty() {
                return Some(g);
            }
        }
    }
    None
}

/// 선생님(인간) 발신을 messages.jsonl 에 영속한다 — god/모모톡 단톡방 가시용.
/// `read=true`: `/send`·`/tell-god` 로 이미 PTY 전달됐으니 학생 inbox drain 은 막고
/// 기록·표시만 남긴다. god 가시성은 cc 사본이 아니라 board-context 가 이 파일 전체를
/// god 에게 보여주는 것으로 얻는다(단일 messages.jsonl 이라 사본 불필요).
/// claude TUI(Ink)에 텍스트를 *제출까지* 보내는 페이로드. 단순 `\n`(LF)은 Ink 가
/// 입력 내 개행으로 먹어 Enter 제출이 씹힌다(거노 실측: 텍스트만 입력칸에 남음).
/// cli `tell` 과 동일하게 Ctrl-U(줄 비움) + bracketed paste + `\r`(CR=Enter):
/// handler 의 `split_trailing_submit` 가 끝 `\r` 을 떼어 140ms 후 보내(Ink 가
/// paste 처리를 끝낸 뒤) 제출이 확실히 먹는다.
fn submit_payload(text: &str) -> String {
    format!("\x15\x1b[200~{}\x1b[201~\r", text)
}

/// 선생님 발신을 messages.jsonl 에 append. `read=true`: 이미 PTY 로 전달돼 표시·
/// god 가시용만(학생 inbox drain 막음). `read=false`: 모모톡 inbox 발신 — 받는
/// 에이전트의 drain_unread(to==me·read==false)가 집어 올려 컨텍스트로 받는다.
/// to/to_pane 은 surface(%N) — drain_unread 가 pane id 도 내 주소로 매칭한다.
fn persist_sensei_msg(room_cwd: &std::path::Path, surface: &str, text: &str, read: bool, room: Option<&str>) {
    // 활성 방 디렉터리에 직접 기록(없으면 생성) — 읽기와 달리 존재 여부로 안 거른다.
    // 방별 분리(거노): room 있으면 slug 에 `__room_<id>` — 모모톡 inbox 도 방별 격리.
    let slug = match room {
        Some(r) => format!("{}__room_{}", mode_slug(room_cwd), r),
        None => mode_slug(room_cwd),
    };
    let dir = std::path::Path::new("/tmp/kasaterm-collab").join(slug);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let id = format!("{:08x}", (now * 1000.0) as u64 & 0xffff_ffff);
    let line = serde_json::json!({
        "id": id, "from": "sensei", "from_pane": "sensei",
        "to": surface, "to_pane": surface,
        "text": text, "ts": now, "read": read
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("messages.jsonl"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// `POST /send?surface=%N` — 학생 pane에 텍스트 주입.
/// body `{"text":"...","submit":true|false}` or raw text.
/// `submit` 기본값=true → 끝에 개행 추가(제출). false → 개행 없음(타이핑만).
/// 없는 surface·빈 text는 ok:false 거부.
async fn send_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: String,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let surface = match params.get("surface").filter(|s| !s.is_empty()) {
        Some(s) => s.clone(),
        None => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": "surface=%N required" })))
                .into_response();
        }
    };
    let (text, submit) = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => (
                v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                v.get("submit").and_then(|s| s.as_bool()).unwrap_or(true),
            ),
            Err(e) => {
                return (
                    cors,
                    Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })),
                )
                    .into_response();
            }
        }
    } else {
        (body.trim().to_string(), true)
    };
    if text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "text is empty" })))
            .into_response();
    }
    // peek(lines=0) 로 surface 존재 확인 — 없으면 에러 반환
    if let Err(e) = backend.peek(&surface, 0) {
        return (
            cors,
            Json(serde_json::json!({ "ok": false, "error": format!("surface not found: {e}") })),
        )
            .into_response();
    }
    // 모모톡 inbox 발신(`inbox=1`): PTY 에 *주입하지 않고* messages.jsonl 에 read=false
    // 로만 적는다(거노: 모모톡은 프롬프트가 아니라 에이전트 inbox). 받는 에이전트는
    // drain_unread 로 컨텍스트에 받고, idle 이면 god-loop nudge 가 4s 내 깨운다.
    let inbox = params.get("inbox").map(|v| v == "1" || v == "true").unwrap_or(false);
    if inbox {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        let clean = clean.trim();
        if clean.is_empty() {
            return (cors, Json(serde_json::json!({ "ok": false, "error": "text is empty" })))
                .into_response();
        }
        persist_sensei_msg(&resolve_cwd(&backend), &surface, clean, false, backend.active_room().as_deref());
        return (cors, Json(serde_json::json!({ "ok": true, "surface": surface, "inbox": true })))
            .into_response();
    }
    let payload = if submit { submit_payload(&text) } else { text.clone() };
    let resp = match backend.send_text(Some(&surface), &payload) {
        Ok(()) => {
            // 선생님 발신을 messages.jsonl 에 영속(모모톡 가시) — 단, 실제 제출(submit)
            // 일 때만. 실시간 미러는 키 한 자마다 `\x15+부분입력`(submit=false)을 쏘는데,
            // 그걸 다 기록하면 모모톡에 "안녕 너"→"안녕 너 누"→… 한 자씩 쌓이고 `\x15`가
            // ⊠ 글리프로 보였다(거노 리포트). 메뉴 선택·Ctrl 키도 submit=false → 제외.
            // 제어문자는 한 번 더 걸러 영속 텍스트를 깨끗이 유지한다.
            // `nopersist=1`: 학생별 대화 패널의 개인 지시는 그 학생 대화(캡처 프록시)에만
            // 떠야 하는데 persist 하면 모모톡 단톡방에까지 노란버블로 샜다(거노). 모모톡
            // 발신(/tell-god·모모톡 학생지목)만 persist, 학생별 대화는 nopersist 로 끈다.
            let nopersist = params.get("nopersist").map(|v| v == "1" || v == "true").unwrap_or(false);
            if submit && !nopersist {
                let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                let clean = clean.trim();
                if !clean.is_empty() {
                    persist_sensei_msg(&resolve_cwd(&backend), &surface, clean, true, backend.active_room().as_deref());
                }
            }
            serde_json::json!({ "ok": true, "surface": surface, "submit": submit })
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    (cors, Json(resp)).into_response()
}

// ── 협업 콘솔 헬퍼 ─────────────────────────────────────────────────────────

/// 협업방 디렉터리. `room_cwd`(활성 pane cwd — `/mode`·`/git-status` 와 같은
/// 소스)가 주어지면 **그 방만** 본다: 다른 방의 stale 데이터로 폴백하지 않고,
/// 없으면 None(빈 결과). 예전엔 MCP 프로세스 cwd(보통 `/`)라 slug 불일치 →
/// readdir 첫 dir(엉뚱한 방)을 집어 모모톡/기록에 stale 가 떴다. room_cwd 가
/// 없을 때(헤드리스 등)만 레거시 추정으로 폴백한다.
fn find_collab_dir(room_cwd: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    let base = std::path::Path::new("/tmp/kasaterm-collab");
    if let Some(cwd) = room_cwd {
        let dir = base.join(mode_slug(cwd));
        return dir.join("messages.jsonl").exists().then_some(dir);
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = base.join(mode_slug(&cwd));
        if candidate.join("messages.jsonl").exists() {
            return Some(candidate);
        }
    }
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        if entry.path().join("messages.jsonl").exists() {
            return Some(entry.path());
        }
    }
    None
}

/// `%N` → character 마커에서 이름 읽기. 마커 없으면 pane id 그대로.
fn char_from_pane(pane: &str, collab_dir: &std::path::Path) -> String {
    let n = pane.trim_start_matches('%');
    if let Ok(name) = std::fs::read_to_string(collab_dir.join(format!("character-{n}"))) {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    pane.to_string()
}

#[derive(serde::Serialize)]
struct Event {
    ts: f64,
    kind: String,
    actor: String,
    summary: String,
}

#[derive(serde::Serialize)]
struct MessageEntry {
    id: String,
    ts: f64,
    from_pane: String,
    from_name: String,
    to_pane: String,
    to_name: String,
    text: String,
    read: bool,
}

/// messages.jsonl 의 done 보고 + git log 를 ts 내림차순으로 합쳐 최근 N 반환.
/// `room_cwd` = 활성 pane cwd(방 해석·git log 기준).
fn collab_events(room_cwd: &std::path::Path, n: usize) -> Vec<Event> {
    let mut events: Vec<Event> = Vec::new();

    // done 보고
    if let Some(dir) = find_collab_dir(Some(room_cwd)) {
        if let Ok(content) = std::fs::read_to_string(dir.join("messages.jsonl")) {
            for line in content.lines() {
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if !text.starts_with("done:") {
                    continue;
                }
                let ts = msg.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
                let from_pane = msg.get("from_pane").and_then(|t| t.as_str()).unwrap_or("");
                let actor = char_from_pane(from_pane, &dir);
                let summary = text
                    .trim_start_matches("done:")
                    .split('|')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                events.push(Event { ts, kind: "done".into(), actor, summary });
            }
        }
    }

    // git 커밋 — ts 는 unix epoch(정수). 활성 방 cwd 기준 로그.
    {
        if let Ok(output) = std::process::Command::new("git")
            .args(["log", &format!("--format=%at\t%s"), &format!("-{}", n)])
            .current_dir(room_cwd)
            .output()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let mut parts = line.splitn(2, '\t');
                let ts = parts.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let summary = parts.next().unwrap_or("").to_string();
                if summary.is_empty() { continue; }
                events.push(Event { ts, kind: "commit".into(), actor: String::new(), summary });
            }
        }
    }

    events.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    events.truncate(n);
    events
}

/// messages.jsonl 을 캐릭터명 해석 포함해 최근 N 개 반환(ts 내림차순).
/// `room_cwd` = 활성 pane cwd(방 해석). `room` 있으면 방별 slug(거노: 방끼리 inbox 격리).
fn collab_messages(room_cwd: &std::path::Path, n: usize, room: Option<&str>) -> Vec<MessageEntry> {
    let dir = match room {
        Some(r) => std::path::PathBuf::from(format!(
            "/tmp/kasaterm-collab/{}__room_{}",
            mode_slug(room_cwd),
            r
        )),
        None => match find_collab_dir(Some(room_cwd)) {
            Some(d) => d,
            None => return Vec::new(),
        },
    };
    let Ok(content) = std::fs::read_to_string(dir.join("messages.jsonl")) else {
        return Vec::new();
    };

    let mut entries: Vec<MessageEntry> = content
        .lines()
        .filter_map(|line| {
            let msg = serde_json::from_str::<serde_json::Value>(line).ok()?;
            let id = msg.get("id").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let ts = msg.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
            let from_pane =
                msg.get("from_pane").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let to_pane = msg
                .get("to_pane")
                .or_else(|| msg.get("to"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let read = msg.get("read").and_then(|t| t.as_bool()).unwrap_or(false);
            let from_name = char_from_pane(&from_pane, &dir);
            let to_name = char_from_pane(&to_pane, &dir);
            Some(MessageEntry { id, ts, from_pane, from_name, to_pane, to_name, text, read })
        })
        .collect();

    entries.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    entries.truncate(n);
    entries
}

/// `GET /events?n=20` — done 보고 + git 커밋을 합친 행정 로그(ts 내림차순 최근 N).
async fn events_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let n = params.get("n").and_then(|s| s.parse::<usize>().ok()).unwrap_or(20);
    let events = collab_events(&resolve_cwd(&backend), n);
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": true, "events": events })),
    )
}

/// claude oauth usage API 토큰 — `~/.claude/.credentials.json` 우선, 없으면 macOS
/// Keychain(`Claude Code-credentials` 서비스). claude Code 가 토큰을 둘 중 하나에 둔다.
fn read_claude_token() -> Option<String> {
    let pick = |v: &serde_json::Value| {
        v.pointer("/claudeAiOauth/accessToken")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    };
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(s) = std::fs::read_to_string(format!("{home}/.claude/.credentials.json")) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(t) = pick(&v) {
                    return Some(t);
                }
            }
        }
    }
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    serde_json::from_str::<serde_json::Value>(s.trim())
        .ok()
        .and_then(|v| pick(&v))
}

/// `GET /claude-usage` — claude oauth usage API(5시간/주간 한도·사용률·리셋)를 그대로
/// 프록시한다. rate limit 은 claude CLI 가 안 내보내지만 `/api/oauth/usage` 가 직접 준다
/// (거노: ba모드 사용량 패널). 토큰 만료/실패는 그 상태를 ok:false 로 전달.
async fn claude_usage_handler() -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let Some(token) = read_claude_token() else {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "no claude token" })));
    };
    let resp = reqwest::Client::new()
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) => (cors, Json(serde_json::json!({ "ok": true, "usage": v }))),
                Err(e) => (cors, Json(serde_json::json!({ "ok": false, "error": e.to_string() }))),
            },
            Err(e) => (cors, Json(serde_json::json!({ "ok": false, "error": e.to_string() }))),
        },
        Ok(r) => (
            cors,
            Json(serde_json::json!({ "ok": false, "error": format!("usage api {}", r.status()) })),
        ),
        Err(e) => (cors, Json(serde_json::json!({ "ok": false, "error": e.to_string() }))),
    }
}

/// `GET /messages?n=50` — messages.jsonl 을 캐릭터명 해석 포함 최근 N 개(ts 내림차순).
async fn messages_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let n = params.get("n").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    // 방별 분리(거노): 활성 방의 messages.jsonl 만 본다. 다른 방 inbox 는 mcp 로만.
    let messages = collab_messages(&resolve_cwd(&backend), n, backend.active_room().as_deref());
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": true, "messages": messages })),
    )
}

/// `POST /tell-god` — 교실 '새 의뢰 작성': body `{"text":"..."}` or raw text
/// → lead 마커의 god pane 에 text+\n 제출(send_text). god 부재(lead 없음) 시
/// `{"ok":false}`. text/plain 본문을 권장(JSON 도 허용).
async fn tell_god_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let text = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                return (
                    cors,
                    Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })),
                )
                    .into_response();
            }
        }
    } else {
        body.trim().to_string()
    };
    if text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "text is empty" })))
            .into_response();
    }
    let god_pane = match find_god_pane() {
        Some(g) => g,
        None => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": "god not found" })))
                .into_response();
        }
    };
    let payload = submit_payload(&text);
    let resp = match backend.send_text(Some(&god_pane), &payload) {
        Ok(()) => {
            // 선생님 → god 발신 영속(휘발 X) — 모모톡 단톡방·god 가시.
            persist_sensei_msg(&resolve_cwd(&backend), &god_pane, &text, true, backend.active_room().as_deref());
            serde_json::json!({ "ok": true, "to": god_pane })
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    (cors, Json(resp)).into_response()
}

/// ~/.config/kasaterm/schale-state.json 경로.
fn schale_state_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/schale-state.json"))
}

/// schale-state.json 읽기. 파일 없으면 초기값 반환.
fn read_schale_state() -> serde_json::Value {
    let default = serde_json::json!({ "credits": 0, "gold": 0, "affinity_lv": 1, "exp": 0 });
    let Some(path) = schale_state_path() else { return default };
    let Ok(s) = std::fs::read_to_string(&path) else { return default };
    serde_json::from_str::<serde_json::Value>(&s).unwrap_or(default)
}

/// `GET /schale-state` — SCHALE OS 재화/Exp 영속 스냅샷.
async fn schale_state_handler() -> impl IntoResponse {
    let s = read_schale_state();
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(s))
}

/// `POST /arona-close` — close the arona classroom window and bring the main
/// terminal back. The ModePicker's "터미널로" choice calls this; the page
/// can't close its own host window. No-op (still ok) when it isn't open.
async fn arona_close_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = match backend.close_arona() {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /panel-open?which=git|session` — open a panel window.
async fn panel_open_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match query_which(&params) {
        Ok(w) => match backend.set_panel(w, true) {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /panel-close?which=git|session` — close a panel window.
async fn panel_close_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match query_which(&params) {
        Ok(w) => match backend.set_panel(w, false) {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /panel-resize?which=git&w=900&h=700` — resize a panel window and
/// re-bound its webview.
async fn panel_resize_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let w = params.get("w").and_then(|s| s.parse::<u32>().ok());
    let h = params.get("h").and_then(|s| s.parse::<u32>().ok());
    let body = match (query_which(&params), w, h) {
        (Ok(which), Some(w), Some(h)) => match backend.resize_panel(which, w, h) {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        (Err(e), _, _) => serde_json::json!({ "ok": false, "error": e }),
        _ => serde_json::json!({ "ok": false, "error": "need w and h" }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /panel-info?which=git|session` — window + webview geometry. When the
/// panel is responsive, `view_w/view_h` equal `win_w/win_h`.
async fn panel_info_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = match query_which(&params) {
        Ok(which) => match backend.panel_info(which) {
            Ok(g) => serde_json::json!({
                "ok": true, "open": g.open,
                "win_w": g.win_w, "win_h": g.win_h,
                "view_w": g.view_w, "view_h": g.view_h,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// Bind an MCP-over-HTTP server at `127.0.0.1:<port>/mcp` and run it on a
/// background thread. Tries `preferred_port` first, then falls back to an
/// OS-assigned port. Returns the actual port bound so the host can write
/// it into `.mcp.json` / an env var.
pub fn spawn_http_server(
    backend: Arc<dyn Backend>,
    preferred_port: u16,
) -> std::io::Result<u16> {
    spawn_http_server_opts(backend, preferred_port, true)
}

/// Like [`spawn_http_server`] but lets the caller disable the schedule loop.
/// The standalone webview server (`kasa-serve-web`) passes `run_scheduler=false`:
/// a headless backend can't deliver a due reminder (`send_text` bails), yet the
/// loop would still persist the item as consumed in the SHARED
/// `~/.config/kasaterm/schedule.json` (silent reminder loss), append phantom
/// "sensei" bubbles, and race kasaterm's own loop on the same file. Firing and
/// consuming schedules is kasaterm's job alone.
pub fn spawn_http_server_opts(
    backend: Arc<dyn Backend>,
    preferred_port: u16,
    run_scheduler: bool,
) -> std::io::Result<u16> {
    // Bind synchronously so we can learn (and return) the real port before
    // handing the socket to tokio.
    let listener = std::net::TcpListener::bind(("127.0.0.1", preferred_port))
        .or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0)))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;

    std::thread::Builder::new()
        .name("kasaspace-mcp-http".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[kasaspace-mcp] tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[kasaspace-mcp] listener convert failed: {e}");
                        return;
                    }
                };
                // 스케줄러 백그라운드 타이머 — due 항목을 학생에게 발사(10s 주기). standalone
                // (run_scheduler=false)은 공유 schedule.json 을 소비/영속하면 안 됨(유령버블·유실·레이스).
                if run_scheduler {
                    tokio::spawn(schedule_loop(backend.clone()));
                }
                // ccglass-style 캡처 프록시 — claude 의 Anthropic API 호출을 가로채
                // pane 별 대화(messages[]+SSE)를 모은다. /conversation 으로 노출.
                let conv_store: crate::proxy::ConvStore = Default::default();
                let http_client = reqwest::Client::new();
                let git_backend = backend.clone();
                let diff_backend = backend.clone();
                let commit_backend = backend.clone();
                let push_backend = backend.clone();
                let ai_backend = backend.clone();
                let sessions_backend = backend.clone();
                let board_backend = backend.clone();
                let session_switch_backend = backend.clone();
                let session_new_backend = backend.clone();
                let spawn_student_backend = backend.clone();
                let swap_character_backend = backend.clone();
                let session_close_backend = backend.clone();
                let slash_backend = backend.clone();
                let session_restore_backend = backend.clone();
                let session_rename_backend = backend.clone();
                let recent_sessions_backend = backend.clone();
                let session_resume_backend = backend.clone();
                let session_save_backend = backend.clone();
                let background_agents_backend = backend.clone();
                let session_reset_backend = backend.clone();
                let open_image_backend = backend.clone();
                let open_markdown_backend = backend.clone();
                let panel_open_backend = backend.clone();
                let panel_close_backend = backend.clone();
                let panel_resize_backend = backend.clone();
                let panel_info_backend = backend.clone();
                let terminal_reveal_backend = backend.clone();
                let arona_close_backend = backend.clone();
                let peek_backend = backend.clone();
                let blocks_backend = backend.clone();
                let transcript_backend = backend.clone();
                let transcript_raw_backend = backend.clone();
                let session_transcript_raw_backend = backend.clone();
                let subagents_backend = backend.clone();
                let subagent_transcript_raw_backend = backend.clone();
                let paste_active_backend = backend.clone();
                let layout_backend = backend.clone();
                let tell_god_backend = backend.clone();
                let send_backend = backend.clone();
                let mode_get_backend = backend.clone();
                let mode_set_backend = backend.clone();
                let focus_backend = backend.clone();
                let close_backend = backend.clone();
                let events_backend = backend.clone();
                let messages_backend = backend.clone();
                let list_dir_backend = backend.clone();
                let room_cd_backend = backend.clone();
                let sent_images_backend = backend.clone();
                let pane_tasks_backend = backend.clone();
                let paste_image_backend = backend.clone();
                let git_panel_backend = backend.clone();
                let service = StreamableHttpService::new(
                    move || Ok(KasaspaceTools::new(backend.clone())),
                    Arc::new(LocalSessionManager::default()),
                    Default::default(),
                );
                let app = axum::Router::new()
                    .route(
                        "/git-status",
                        get(move || git_status_handler(git_backend.clone())),
                    )
                    .route(
                        "/git-diff",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            git_diff_handler(diff_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/git-commit",
                        post(move |body: String| {
                            git_commit_handler(commit_backend.clone(), body)
                        }),
                    )
                    .route(
                        "/git-push",
                        post(move || git_push_handler(push_backend.clone())),
                    )
                    .route(
                        "/git-ai-commit",
                        post(move |body: String| {
                            git_ai_commit_handler(ai_backend.clone(), body)
                        }),
                    )
                    .route(
                        "/sessions",
                        get(move || sessions_handler(sessions_backend.clone())),
                    )
                    .route(
                        "/board",
                        get(move || board_handler(board_backend.clone())),
                    )
                    .route(
                        "/transcript",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            transcript_handler(transcript_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/transcript-raw",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            transcript_raw_handler(transcript_raw_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-transcript-raw",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_transcript_raw_handler(session_transcript_raw_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/subagents",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            subagents_handler(subagents_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/subagent-transcript-raw",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            subagent_transcript_raw_handler(subagent_transcript_raw_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/paste-active",
                        post(move |body: String| {
                            paste_active_handler(paste_active_backend.clone(), body)
                        }),
                    )
                    .route(
                        "/layout",
                        get(move || layout_handler(layout_backend.clone())),
                    )
                    .route("/characters", get(characters_handler))
                    .route(
                        "/mode",
                        get(move || mode_get_handler(mode_get_backend.clone())).post(
                            move |q: Query<std::collections::HashMap<String, String>>| {
                                mode_set_handler(mode_set_backend.clone(), q)
                            },
                        ),
                    )
                    .route(
                        "/focus",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            focus_handler(focus_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/close-pane",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            close_pane_handler(close_backend.clone(), q)
                        }),
                    )
                    // /arona-ui(슬래시 없음)는 /arona-ui/ 로 리다이렉트 —
                    // index.html 의 상대경로 assets(./assets/*) 가 디렉토리
                    // 기준으로 풀리려면 trailing slash 가 필요하다.
                    .route(
                        "/arona-ui",
                        get(|| async {
                            axum::response::Redirect::permanent("/arona-ui/")
                        }),
                    )
                    .route("/arona-ui/", get(|| arona_ui_serve(String::new())))
                    .route(
                        "/arona-ui/{*path}",
                        get(|axum::extract::Path(p): axum::extract::Path<String>| {
                            arona_ui_serve(p)
                        }),
                    )
                    .route(
                        "/session-switch",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_switch_handler(session_switch_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-new",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_new_handler(session_new_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-close",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_close_handler(session_close_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/spawn-student",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            spawn_student_handler(spawn_student_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/swap-character",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            swap_character_handler(swap_character_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-restore",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_restore_handler(session_restore_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-rename",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_rename_handler(session_rename_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/recent-sessions",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            recent_sessions_handler(recent_sessions_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-resume",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_resume_handler(session_resume_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/session-save",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_save_handler(session_save_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/background-agents",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            background_agents_handler(background_agents_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/background-kill",
                        post(|q: Query<std::collections::HashMap<String, String>>| {
                            background_kill_handler(q)
                        }),
                    )
                    .route(
                        "/session-reset",
                        post(move || session_reset_handler(session_reset_backend.clone())),
                    )
                    .route(
                        "/open-image",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            open_image_handler(open_image_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/open-markdown",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            open_markdown_handler(open_markdown_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/save-markdown",
                        post(move |body: String| save_markdown_handler(body)),
                    )
                    .route(
                        "/terminal-reveal",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            terminal_reveal_handler(terminal_reveal_backend.clone(), q)
                        }),
                    )
                    .route("/schale-state", get(schale_state_handler))
                    .route(
                        "/arona-close",
                        post(move || arona_close_handler(arona_close_backend.clone())),
                    )
                    .route(
                        "/peek",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            peek_handler(peek_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/blocks",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            blocks_handler(blocks_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/tell-god",
                        post(move |body: String| {
                            tell_god_handler(tell_god_backend.clone(), body)
                        }),
                    )
                    .route(
                        "/send",
                        post(
                            move |q: Query<std::collections::HashMap<String, String>>,
                                  body: String| {
                                send_handler(send_backend.clone(), q, body)
                            },
                        ),
                    )
                    .route(
                        "/events",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            events_handler(events_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/messages",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            messages_handler(messages_backend.clone(), q)
                        }),
                    )
                    .route("/claude-usage", get(claude_usage_handler))
                    .route(
                        "/slash-commands",
                        get(move || slash_commands_handler(slash_backend.clone())),
                    )
                    .route(
                        "/schedule",
                        get(schedule_list_handler).post(|body: String| schedule_add_handler(body)),
                    )
                    .route(
                        "/schedule-delete",
                        post(|q: Query<std::collections::HashMap<String, String>>| {
                            schedule_delete_handler(q)
                        }),
                    )
                    .route(
                        "/image-file",
                        get(image_file_handler),
                    )
                    .route(
                        "/open-file",
                        post(|q: Query<std::collections::HashMap<String, String>>| {
                            open_file_handler(q)
                        }),
                    )
                    .route(
                        "/sent-images",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            sent_images_handler(sent_images_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/pane-tasks",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            pane_tasks_handler(pane_tasks_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/paste-image",
                        post(move |q: Query<std::collections::HashMap<String, String>>, b: Bytes| {
                            paste_image_handler(paste_image_backend.clone(), q, b)
                        }),
                    )
                    .route(
                        "/git-panel",
                        post(move || git_panel_handler(git_panel_backend.clone())),
                    )
                    .route(
                        "/list-dir",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            list_dir_handler(list_dir_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/room-cd",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            room_cd_handler(room_cd_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/panel-open",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            panel_open_handler(panel_open_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/panel-close",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            panel_close_handler(panel_close_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/panel-resize",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            panel_resize_handler(panel_resize_backend.clone(), q)
                        }),
                    )
                    .route(
                        "/panel-info",
                        get(move |q: Query<std::collections::HashMap<String, String>>| {
                            panel_info_handler(panel_info_backend.clone(), q)
                        }),
                    )
                    // 채팅 소스: 캡처 프록시가 모은 pane 대화(turns + 진행 중 streaming).
                    .route(
                        "/conversation",
                        get({
                            let store = conv_store.clone();
                            move |q: Query<std::collections::HashMap<String, String>>| {
                                let store = store.clone();
                                async move {
                                    let pane =
                                        q.get("surface").map(|s| s.trim_start_matches('%')).unwrap_or("");
                                    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
                                    (cors, Json(crate::proxy::conversation_json(&store, pane)))
                                        .into_response()
                                }
                            }
                        }),
                    )
                    // 캡처 프록시: claude 가 ANTHROPIC_BASE_URL=…/p/<pane> 로 보낸 모든
                    // API 호출을 가로채 api.anthropic.com 으로 투명 포워드 + 캡처.
                    .route(
                        "/p/{pane}/{*rest}",
                        any({
                            let store = conv_store.clone();
                            let client = http_client.clone();
                            move |AxPath((pane, rest)): AxPath<(String, String)>,
                                  method: Method,
                                  headers: HeaderMap,
                                  body: Bytes| {
                                crate::proxy::proxy_handler(
                                    store.clone(),
                                    client.clone(),
                                    pane,
                                    rest,
                                    method,
                                    headers,
                                    body,
                                )
                            }
                        }),
                    )
                    .nest_service("/mcp", service);
                if let Err(e) = axum::serve(tokio_listener, app).await {
                    eprintln!("[kasaspace-mcp] serve error: {e}");
                }
            });
        })?;

    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("kasa-mcp-http-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// kasacollab.py mode_path 와 같은 치환이어야 같은 마커를 공유한다.
    #[test]
    fn mode_slug_matches_python_rule() {
        assert_eq!(
            mode_slug(std::path::Path::new("/Users/kasa/Desktop/momewomo/tmuxify")),
            "-Users-kasa-Desktop-momewomo-tmuxify"
        );
        // '.' 포함 경로 — god slug 엣지케이스
        assert_eq!(
            mode_slug(std::path::Path::new("/tmp/app.v1.2/run")),
            "-tmp-app-v1-2-run"
        );
        assert_eq!(mode_slug(std::path::Path::new("/")), "-");
    }

    #[test]
    fn read_mode_defaults_to_solo() {
        let d = temp_dir("read-mode");
        // 파일 없음 → solo
        assert_eq!(read_mode_file(&d.join("missing")), "solo");
        // 쓰레기 값 → solo
        std::fs::write(d.join("garbage"), "banana\n").unwrap();
        assert_eq!(read_mode_file(&d.join("garbage")), "solo");
        // 개행 딸린 god → god (py 의 .strip() 대응)
        std::fs::write(d.join("god"), "god\n").unwrap();
        assert_eq!(read_mode_file(&d.join("god")), "god");
        std::fs::write(d.join("solo"), "solo").unwrap();
        assert_eq!(read_mode_file(&d.join("solo")), "solo");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_mode_atomic_roundtrip() {
        let d = temp_dir("write-mode");
        let p = d.join("rooms").join("-some-room");
        // 부모 디렉토리 없어도 생성
        write_mode_file(&p, "god").unwrap();
        assert_eq!(read_mode_file(&p), "god");
        // tmp 파일이 남지 않는다 (rename 완료)
        let mut tmp = p.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!std::path::Path::new(&tmp).exists());
        // 덮어쓰기 전환
        write_mode_file(&p, "solo").unwrap();
        assert_eq!(read_mode_file(&p), "solo");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn first_valid_json_skips_broken_files() {
        let d = temp_dir("char-json");
        let broken = d.join("broken.json");
        let valid = d.join("valid.json");
        let missing = d.join("missing.json");
        std::fs::write(&broken, "{not json").unwrap();
        std::fs::write(&valid, r#"{"leader":{"name":"아로나"}}"#).unwrap();
        // 깨진 파일·없는 파일은 건너뛰고 첫 유효 JSON 을 집는다
        let got = first_valid_json(&[missing.clone(), broken.clone(), valid.clone()]).unwrap();
        assert_eq!(got["leader"]["name"], "아로나");
        // 전부 무효 → None (핸들러는 404)
        assert!(first_valid_json(&[missing, broken]).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 전역 온보딩 플래그: 경로·기록·마이그레이션 판정. HOME 을 temp 로 격리
    /// (HOME 을 읽는 테스트는 이 하나뿐이라 병렬에서 충돌하지 않는다).
    #[test]
    fn onboarded_flag_path_write_and_migration() {
        let home = temp_dir("onboard-home");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        // 경로: ~/.config/kasaterm/onboarded
        let flag = onboarded_marker_path().unwrap();
        assert_eq!(flag, home.join(".config/kasaterm/onboarded"));

        // 첫 실행: 플래그 없음 + collab 마커 없음 → 온보딩 대상
        assert!(!flag.exists());
        assert!(!any_collab_mode_marker());

        // 모드 선택 = mark_onboarded → 플래그 영속(내용 "1")
        mark_onboarded();
        assert!(flag.exists());
        assert_eq!(std::fs::read_to_string(&flag).unwrap(), "1");

        // 마이그레이션: 플래그 지우고 옛 방 마커 하나 심으면 '첫 실행 아님' 판정
        std::fs::remove_file(&flag).unwrap();
        assert!(!any_collab_mode_marker());
        let room = mode_marker_path(std::path::Path::new("/some/project")).unwrap();
        write_mode_file(&room, "god").unwrap();
        assert!(any_collab_mode_marker());

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
