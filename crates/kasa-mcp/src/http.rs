//! Streamable-HTTP serving glue. The host (kasaterm) is a synchronous
//! winit/wgpu app, so we own a small multi-thread tokio runtime on a
//! dedicated background thread and run axum there. The `Backend` is
//! channel-based and `Send + Sync`, so calling it from async handlers on
//! another thread is safe.

use std::sync::Arc;

use kasa_socket::backend::{Backend, PanelKind, SplitDirection};
use axum::{
    extract::Query, http::header, response::IntoResponse, routing::get, routing::post, Json,
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
    let mut imgs: Vec<String> = Vec::new();
    if let Some(dir) = find_collab_dir(Some(&cwd)) {
        if let Ok(content) = std::fs::read_to_string(dir.join("sent-images.jsonl")) {
            for line in content.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                let pane = v.get("pane").and_then(|p| p.as_str()).unwrap_or("");
                if !surface.is_empty() && pane != surface {
                    continue;
                }
                if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                    imgs.push(p.to_string());
                }
            }
        }
    }
    if imgs.len() > n {
        imgs.drain(0..imgs.len() - n);
    }
    (cors, Json(serde_json::json!({ "ok": true, "images": imgs })))
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

/// `POST /room-cd?path=<path>` — active pane 셸을 그 경로로 cd(터미널 백엔드).
/// 셸 명령이라 bracketed paste 가 아니라 `cd '<path>'` + CR. claude 가 도는
/// pane 이면 셸 cd 가 아니라 claude 입력으로 가니 무해히 무시되거나 사용자가 정리.
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
    let quoted = path.replace('\'', "'\\''");
    let payload = format!("cd '{quoted}'\r");
    let ok = backend.send_text(None, &payload).is_ok();
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
        Ok(bytes) => (
            axum::http::StatusCode::OK,
            [
                (header::CONTENT_TYPE, static_content_type(&canon)),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
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

/// Body for `POST /spawn`: 새 워커 pane 스펙. 전부 선택적 — 빈 객체면
/// "현재 방에 기본 claude 하나 더".
#[derive(serde::Deserialize)]
struct SpawnReq {
    /// characters.json 의 leader/members 이름. 지정 시 캐릭터 마커를 선점해
    /// claude 래퍼의 assign-character 가 이 캐릭터의 persona 를 입힌다.
    #[serde(default)]
    character: Option<String>,
    /// `claude --model <m>` 로 전달.
    #[serde(default)]
    model: Option<String>,
    /// 새 pane 이 일할 절대경로. 지정 시 `cd <cwd> && claude`.
    #[serde(default)]
    cwd: Option<String>,
}

/// POSIX 셸 single-quote — pane 에 주입하는 cd 경로가 공백·따옴표를 품어도
/// 한 토큰으로 살아남게.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `claude --model x` 같은 비인용 위치로 가는 값의 allowlist. 모델명/캐릭터명에
/// 셸 메타문자가 낄 이유가 없으므로 통째로 거부한다.
fn safe_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// spawn 할 pane 에 보낼 셸 한 줄 조립. `claude` 는 PATH shim(래퍼)이
/// --settings/persona 를 알아서 얹으므로 맨이름 호출이 정답.
fn spawn_command(model: Option<&str>, cwd: Option<&str>) -> String {
    let claude = match model {
        Some(m) => format!("claude --model {m}"),
        None => "claude".to_string(),
    };
    match cwd {
        Some(d) => format!("cd {} && {claude}", sh_quote(d)),
        None => claude,
    }
}

/// 캐릭터 마커 경로 — assign-character.py 와 동일 규약:
/// /tmp/kasaterm-collab/<slug>/character-<pane번호(% 제거)>, 내용 = 이름.
fn character_marker_path(room_cwd: &std::path::Path, surface_id: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp/kasaterm-collab")
        .join(mode_slug(room_cwd))
        .join(format!("character-{}", surface_id.trim_start_matches('%')))
}

/// characters.json 에 이 이름의 캐릭터(leader 또는 member)가 정의돼 있나.
fn character_defined(chars: &serde_json::Value, name: &str) -> bool {
    let leader_is = chars
        .get("leader")
        .and_then(|l| l.get("name"))
        .and_then(|n| n.as_str())
        == Some(name);
    let member_is = chars
        .get("members")
        .and_then(|m| m.as_array())
        .is_some_and(|ms| {
            ms.iter()
                .any(|m| m.get("name").and_then(|n| n.as_str()) == Some(name))
        });
    leader_is || member_is
}

/// `POST /spawn` — split(no-focus) + 새 pane 에 claude 기동(munder spawnPty
/// 대응). body 는 raw JSON 문자열(text/plain) — /git-commit 과 같은 preflight
/// 회피. 흐름은 run_job 패턴: split → (마커 선점·rename best-effort) → send.
async fn spawn_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    let resp = (|| {
        let req: SpawnReq = if body.trim().is_empty() {
            SpawnReq { character: None, model: None, cwd: None }
        } else {
            serde_json::from_str(&body)
                .map_err(|e| format!("bad request body: {e}"))?
        };
        if let Some(m) = req.model.as_deref() {
            if !safe_token(m) {
                return Err(format!("bad model name: {m:?}"));
            }
        }
        // cwd 는 physical 경로로 정규화(canonicalize). pane 안 도구들이
        // os.getcwd()(physical) 기준 slug 로 마커를 찾으므로, /tmp 같은
        // symlink 를 그대로 쓰면 선점 마커가 다른 방에 떨어진다. 존재하지
        // 않는 경로는 cd 도 실패할 것이므로 여기서 거부.
        let req_cwd = match req.cwd.as_deref() {
            Some(d) => {
                if !std::path::Path::new(d).is_absolute() {
                    return Err(format!("cwd must be absolute: {d:?}"));
                }
                let canon = std::fs::canonicalize(d)
                    .map_err(|e| format!("cwd not accessible: {d:?} ({e})"))?;
                Some(canon.to_string_lossy().into_owned())
            }
            None => None,
        };
        // 캐릭터는 characters.json 에 실제 정의된 이름만 — 오타가 마커만
        // 선점하고 persona 는 못 찾는 반쪽 상태를 막는다.
        if let Some(name) = req.character.as_deref() {
            let chars = first_valid_json(&characters_candidate_paths())
                .ok_or_else(|| "characters.json not found".to_string())?;
            if !character_defined(&chars, name) {
                return Err(format!("unknown character: {name:?}"));
            }
        }

        let surf = backend
            .split_surface(SplitDirection::Down, false)
            .map_err(|e| format!("split failed: {e}"))?;
        let surface_id = surf.id;

        // 새 pane 의 방 = cd 후 claude 가 뜰 디렉토리. 마커 slug 기준도 동일.
        let room: std::path::PathBuf = req_cwd
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| resolve_cwd(&backend));
        let mut notes = Vec::new();
        if let Some(name) = req.character.as_deref() {
            let marker = character_marker_path(&room, &surface_id);
            let write = || -> std::io::Result<()> {
                if let Some(dir) = marker.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut tmp = marker.as_os_str().to_owned();
                tmp.push(".tmp");
                let tmp = std::path::PathBuf::from(tmp);
                std::fs::write(&tmp, name)?;
                std::fs::rename(&tmp, &marker)
            };
            match write() {
                // 마커 선점 = assign 의 newly_assigned 경로를 안 타므로 헤더
                // rename 은 여기서 직접 (assign 과 같은 '● <이름>' 표기).
                Ok(()) => match backend.rename_surface(&surface_id, &format!("● {name}")) {
                    Ok(()) => notes.push(format!("character={name}")),
                    Err(e) => notes.push(format!("character={name}, rename skipped ({e})")),
                },
                Err(e) => notes.push(format!("character marker failed ({e})")),
            }
        }

        let mut cmd = spawn_command(req.model.as_deref(), req_cwd.as_deref());
        cmd.push('\n');
        backend
            .send_text(Some(&surface_id), &cmd)
            .map_err(|e| format!("send failed: {e}"))?;
        Ok(serde_json::json!({
            "ok": true,
            "surface_id": surface_id,
            "command": cmd.trim_end(),
            "notes": notes,
        }))
    })()
    .unwrap_or_else(|e: String| serde_json::json!({ "ok": false, "error": e }));
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(resp))
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

/// `POST /session-new` — create a fresh session and switch to it.
async fn session_new_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = match backend.new_session() {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
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

fn persist_sensei_msg(room_cwd: &std::path::Path, surface: &str, text: &str) {
    // 활성 방 디렉터리에 직접 기록(없으면 생성) — 읽기와 달리 존재 여부로 안 거른다.
    let dir = std::path::Path::new("/tmp/kasaterm-collab").join(mode_slug(room_cwd));
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
        "text": text, "ts": now, "read": true
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
    let payload = if submit { submit_payload(&text) } else { text.clone() };
    let resp = match backend.send_text(Some(&surface), &payload) {
        Ok(()) => {
            // 선생님 발신을 messages.jsonl 에 영속(휘발 X) — god/모모톡 가시.
            persist_sensei_msg(&resolve_cwd(&backend), &surface, &text);
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
/// `room_cwd` = 활성 pane cwd(방 해석).
fn collab_messages(room_cwd: &std::path::Path, n: usize) -> Vec<MessageEntry> {
    let Some(dir) = find_collab_dir(Some(room_cwd)) else {
        return Vec::new();
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

/// `GET /messages?n=50` — messages.jsonl 을 캐릭터명 해석 포함 최근 N 개(ts 내림차순).
async fn messages_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let n = params.get("n").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    let messages = collab_messages(&resolve_cwd(&backend), n);
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
            persist_sensei_msg(&resolve_cwd(&backend), &god_pane, &text);
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
                // 스케줄러 백그라운드 타이머 — due 항목을 학생에게 발사(10s 주기).
                tokio::spawn(schedule_loop(backend.clone()));
                let git_backend = backend.clone();
                let diff_backend = backend.clone();
                let commit_backend = backend.clone();
                let push_backend = backend.clone();
                let ai_backend = backend.clone();
                let sessions_backend = backend.clone();
                let board_backend = backend.clone();
                let session_switch_backend = backend.clone();
                let session_new_backend = backend.clone();
                let session_close_backend = backend.clone();
                let session_restore_backend = backend.clone();
                let session_rename_backend = backend.clone();
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
                let transcript_backend = backend.clone();
                let tell_god_backend = backend.clone();
                let send_backend = backend.clone();
                let mode_get_backend = backend.clone();
                let mode_set_backend = backend.clone();
                let spawn_backend = backend.clone();
                let focus_backend = backend.clone();
                let events_backend = backend.clone();
                let messages_backend = backend.clone();
                let list_dir_backend = backend.clone();
                let room_cd_backend = backend.clone();
                let sent_images_backend = backend.clone();
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
                        "/spawn",
                        post(move |body: String| spawn_handler(spawn_backend.clone(), body)),
                    )
                    .route(
                        "/focus",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            focus_handler(focus_backend.clone(), q)
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
                        post(move || session_new_handler(session_new_backend.clone())),
                    )
                    .route(
                        "/session-close",
                        post(move |q: Query<std::collections::HashMap<String, String>>| {
                            session_close_handler(session_close_backend.clone(), q)
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
    fn spawn_command_assembles_cd_and_model() {
        assert_eq!(spawn_command(None, None), "claude");
        assert_eq!(spawn_command(Some("opus"), None), "claude --model opus");
        assert_eq!(
            spawn_command(None, Some("/tmp/a b")),
            "cd '/tmp/a b' && claude"
        );
        assert_eq!(
            spawn_command(Some("claude-fable-5"), Some("/r/x")),
            "cd '/r/x' && claude --model claude-fable-5"
        );
    }

    #[test]
    fn sh_quote_survives_embedded_quote() {
        // it's → 'it'\''s' (닫고-이스케이프-다시염)
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("plain"), "'plain'");
    }

    #[test]
    fn safe_token_rejects_shell_meta() {
        assert!(safe_token("claude-fable-5"));
        assert!(safe_token("opus_4.8"));
        assert!(!safe_token(""));
        assert!(!safe_token("opus; rm -rf /"));
        assert!(!safe_token("a b"));
        assert!(!safe_token("$(boom)"));
    }

    /// assign-character.py 의 my_marker 와 자리·표기가 일치해야 선점이 먹힌다.
    #[test]
    fn character_marker_matches_python_layout() {
        let p = character_marker_path(
            std::path::Path::new("/Users/kasa/Desktop/momewomo/tmuxify"),
            "%7",
        );
        assert_eq!(
            p,
            std::path::PathBuf::from(
                "/tmp/kasaterm-collab/-Users-kasa-Desktop-momewomo-tmuxify/character-7"
            )
        );
    }

    #[test]
    fn character_defined_checks_leader_and_members() {
        let chars: serde_json::Value = serde_json::from_str(
            r#"{"leader":{"name":"아로나"},"members":[{"name":"유우카"},{"name":"시로코"}]}"#,
        )
        .unwrap();
        assert!(character_defined(&chars, "아로나"));
        assert!(character_defined(&chars, "유우카"));
        assert!(!character_defined(&chars, "프라나"));
        // members 없는 단독 leader 구성도
        let solo: serde_json::Value =
            serde_json::from_str(r#"{"leader":{"name":"아로나"}}"#).unwrap();
        assert!(character_defined(&solo, "아로나"));
        assert!(!character_defined(&solo, "유우카"));
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
