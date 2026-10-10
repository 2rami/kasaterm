//! 협업 콘솔 — 보드, 학생에게 보내기(`/send`·채팅), 협업방 이벤트·메시지, `/collab/*` 읽기, tell·나쵸 보고.

use super::*;
use super::arona_ui::mode_slug;

/// `GET /board` — JSON snapshot of every pane's activity (`collab.board`) for
/// the board panel to poll: `{ board: [{surface_id, intent, status, files}] }`.
async fn board_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let board = backend.collab_board().unwrap_or_default();
    // 다른 기계의 학생도 같은 목록에 섞는다 — 「어느 기계에 띄울까」를 매번 생각하지
    // 않으려면 한 화면에 있어야 한다(2026-08-26 지시). 캐시를 읽을 뿐이라 원격이
    // 죽어 있어도 이 응답은 안 느려진다(remoteboard.rs 머리말).
    let mut rows = serde_json::to_value(&board)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    rows.extend(crate::remoteboard::board_rows());
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "board": rows })),
    )
}

/// 선생님(인간) 발신을 messages.jsonl 에 영속한다 — 모모톡 단톡방 가시용.
/// `read=true`: `/send` 로 이미 PTY 전달됐으니 학생 inbox drain 은 막고
/// 기록·표시만 남긴다.
/// claude TUI(Ink)에 텍스트를 *제출까지* 보내는 페이로드. 단순 `\n`(LF)은 Ink 가
/// 입력 내 개행으로 먹어 Enter 제출이 씹힌다(사용자 실측: 텍스트만 입력칸에 남음).
/// cli `tell` 과 동일하게 Ctrl-U(줄 비움) + bracketed paste + `\r`(CR=Enter):
/// handler 의 `split_trailing_submit` 가 끝 `\r` 을 떼어 140ms 후 보내(Ink 가
/// paste 처리를 끝낸 뒤) 제출이 확실히 먹는다.
pub(super) fn submit_payload(text: &str) -> String {
    format!("\x15\x1b[200~{}\x1b[201~\r", text)
}

/// 선생님 발신을 messages.jsonl 에 append. `read=true`: 이미 PTY 로 전달돼 표시·
/// 오케스트레이터 가시용만(학생 inbox drain 막음). `read=false`: 모모톡 inbox 발신 — 받는
/// 에이전트의 drain_unread(to==me·read==false)가 집어 올려 컨텍스트로 받는다.
/// to/to_pane 은 surface(%N) — drain_unread 가 pane id 도 내 주소로 매칭한다.
pub(super) fn persist_sensei_msg(room_cwd: &std::path::Path, surface: &str, text: &str, read: bool, room: Option<&str>) {
    // 활성 방 디렉터리에 직접 기록(없으면 생성) — 읽기와 달리 존재 여부로 안 거른다.
    // 방별 분리(사용자): room 있으면 slug 에 `__room_<id>` — 모모톡 inbox 도 방별 격리.
    let slug = match room {
        Some(r) => format!("{}__room_{}", mode_slug(room_cwd), r),
        None => mode_slug(room_cwd),
    };
    let dir = kasa_socket::collab_root().join(slug);
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

/// `POST /term/chat-send` `{surface, text}` — 거울(다른 기기·폰) 대화 보기의 입력(`docs/mirror-render.md`).
/// 안전한 tell 로 넣는다 — 승인·질문 창이면 기다리고 초안·한글 조합은 안 건드리며, 일하는 칸이면 진행 중인 턴
/// 안으로 들어간다. 그것도 거절되면(막 띄워 신원이 안 선 칸) 옛 `/send` 붙여넣기. mod 칸을 mod 에 맡기던 길은
/// 일하는 내내 묶고 plugin 머리를 붙여 걷었다(2026-10-07).
async fn term_chat_send(backend: Arc<dyn Backend>, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let surface = body["surface"].as_str().unwrap_or("").to_string();
    let text = body["text"].as_str().unwrap_or("").trim().to_string();
    if surface.is_empty() || text.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "surface·text 가 필요해요" }));
    }
    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "surface_id": surface, "message_id": kasa_socket::tell::new_message_id(), "body": text,
        });
        match backend.collab_tell(&params) {
            Ok(receipt) => serde_json::json!({ "ok": true, "via": "tell", "receipt": receipt }),
            Err(refused) => match backend.send_text(Some(&surface), &submit_payload(&text)) {
                Ok(()) => serde_json::json!({ "ok": true, "via": "paste", "tell": refused.to_string() }),
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            },
        }
    })
    .await;
    Json(result.unwrap_or_else(|_| serde_json::json!({ "ok": false, "error": "chat-send worker stopped" })))
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
    // 로만 적는다(사용자: 모모톡은 프롬프트가 아니라 에이전트 inbox). 받는 에이전트는
    // drain_unread 로 컨텍스트에 받고, idle 이면 nudge 가 4s 내 깨운다.
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
            // ⊠ 글리프로 보였다(사용자 리포트). 메뉴 선택·Ctrl 키도 submit=false → 제외.
            // 제어문자는 한 번 더 걸러 영속 텍스트를 깨끗이 유지한다.
            // `nopersist=1`: 학생별 대화 패널의 개인 지시는 그 학생 대화(캡처 프록시)에만
            // 떠야 하는데 persist 하면 모모톡 단톡방에까지 노란버블로 샜다(사용자). 모모톡
            // 발신(모모톡 학생지목)만 persist, 학생별 대화는 nopersist 로 끈다.
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

/// 협업방 디렉터리. `room_cwd`(활성 pane cwd — `/mode`·`/git-status` 와 같은
/// 소스)가 주어지면 **그 방만** 본다: 다른 방의 stale 데이터로 폴백하지 않고,
/// 없으면 None(빈 결과). 예전엔 MCP 프로세스 cwd(보통 `/`)라 slug 불일치 →
/// readdir 첫 dir(엉뚱한 방)을 집어 모모톡/기록에 stale 가 떴다. room_cwd 가
/// 없을 때(헤드리스 등)만 레거시 추정으로 폴백한다.
fn find_collab_dir(room_cwd: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    let base = kasa_socket::collab_root();
    let base = base.as_path();
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
    // 마커 둘째 줄은 주인 pid(sweep 용)라 이름은 첫 줄까지다.
    if let Ok(body) = std::fs::read_to_string(collab_dir.join(format!("character-{n}"))) {
        if let Some(name) = body.lines().next().map(str::trim).filter(|s| !s.is_empty()) {
            return name.to_string();
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
        if let Ok(output) = crate::no_window_command("git")
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
/// `room_cwd` = 활성 pane cwd(방 해석). `room` 있으면 방별 slug(사용자: 방끼리 inbox 격리).
fn collab_messages(room_cwd: &std::path::Path, n: usize, room: Option<&str>) -> Vec<MessageEntry> {
    let dir = match room {
        Some(r) => kasa_socket::collab_root()
            .join(format!("{}__room_{}", mode_slug(room_cwd), r)),
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

/// `GET /messages?n=50` — messages.jsonl 을 캐릭터명 해석 포함 최근 N 개(ts 내림차순).
async fn messages_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let n = params.get("n").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    // 방별 분리(사용자): 활성 방의 messages.jsonl 만 본다. 다른 방 inbox 는 mcp 로만.
    let messages = collab_messages(&resolve_cwd(&backend), n, backend.active_room().as_deref());
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": true, "messages": messages })),
    )
}

async fn collab_read_handler(
    backend: Arc<dyn Backend>, operation: &'static str,
    Query(query): Query<std::collections::HashMap<String,String>>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let params = if let Some(raw) = query.get("params") {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(value) if value.is_object() => value,
            _ => return (StatusCode::BAD_REQUEST,Json(serde_json::json!({"error":"invalid collaboration parameters"}))).into_response(),
        }
    } else {
        let mut value = serde_json::json!({});
        for key in ["scope","since"] { if let Some(text) = query.get(key) { value[key] = serde_json::json!(text); } }
        if let Some(limit) = query.get("limit") {
            match limit.parse::<u64>() {
                Ok(limit) => value["limit"] = serde_json::json!(limit),
                Err(_) => return (StatusCode::BAD_REQUEST,Json(serde_json::json!({"error":"invalid limit"}))).into_response(),
            }
        }
        if operation == "inspect" {
            let mut address = serde_json::json!({});
            for key in ["machine_id","surface_key","surface_id","session_id","instance_id"] {
                if let Some(text) = query.get(key) { address[key] = serde_json::json!(text); }
            }
            value["address"] = address;
        }
        value
    };
    let mut params = params;
    // HTTP 로 온 캡처는 늘 본문으로 돌려준다 — 요청에 실린 경로를 이 기기 디스크에 쓰지 않는다.
    if operation == "capture" {
        params["inline"] = serde_json::json!(true);
        if let Some(object) = params.as_object_mut() { object.remove("path"); }
    }
    match tokio::task::spawn_blocking(move || match operation {
        "snapshot" => backend.collab_snapshot(&params),
        "changes" => backend.collab_changes(&params),
        "inspect" => backend.collab_inspect(&params),
        view => backend.collab_view(view,&params),
    }).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (StatusCode::CONFLICT,Json(serde_json::json!({"error":error.to_string()}))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,Json(serde_json::json!({"error":"collaboration worker failed"}))).into_response(),
    }
}

pub(crate) async fn nacho_report_post(backend: Arc<dyn Backend>, Json(params): Json<serde_json::Value>) -> Json<serde_json::Value> {
    match tokio::task::spawn_blocking(move || backend.nacho_report(&params)).await {
        Ok(Ok(value)) => Json(value),
        Ok(Err(error)) => Json(serde_json::json!({"ok":false,"error":error.to_string()})),
        Err(_) => Json(serde_json::json!({"ok":false,"error":"nacho report worker stopped; check the inbox before re-sending"})),
    }
}

pub(crate) async fn collab_tell_post(backend: Arc<dyn Backend>, Json(params): Json<serde_json::Value>) -> Json<serde_json::Value> {
    match tokio::task::spawn_blocking(move || backend.collab_tell(&params)).await {
        Ok(Ok(value)) => Json(value),
        Ok(Err(error)) => Json(serde_json::json!({"ok":false,"error":error.to_string()})),
        Err(_) => Json(serde_json::json!({"ok":false,"error":"tell worker stopped; inspect receipt before retrying"})),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let board_backend = backend.clone();
    let collab_snapshot_backend = backend.clone();
    let collab_changes_backend = backend.clone();
    let collab_inspect_backend = backend.clone();
    let collab_peek_backend = backend.clone();
    let collab_capture_backend = backend.clone();
    let collab_where_backend = backend.clone();
    let collab_transcript_backend = backend.clone();
    let collab_act_backend = backend.clone();
    let send_backend = backend.clone();
    let chat_send_backend = backend.clone();
    let tell_backend = backend.clone();
    let tell_status_backend = backend.clone();
    let nacho_report_backend = backend.clone();
    let events_backend = backend.clone();
    let messages_backend = backend.clone();
    axum::Router::new()
        .route(
            "/board",
            get(move || board_handler(board_backend.clone())),
        )
        .route("/collab/board", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_snapshot_backend.clone(),"snapshot",q)))
        .route("/collab/changes", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_changes_backend.clone(),"changes",q)))
        .route("/collab/inspect", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_inspect_backend.clone(),"inspect",q)))
        .route("/collab/peek", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_peek_backend.clone(),"peek",q)))
        .route("/collab/capture", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_capture_backend.clone(),"capture",q)))
        .route("/collab/where", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_where_backend.clone(),"where",q)))
        .route("/collab/transcript", get(move |q: Query<std::collections::HashMap<String,String>>|
            collab_read_handler(collab_transcript_backend.clone(),"transcript",q)))
        // 다른 기기가 이 기기 칸·방에 쓰는 일(raw 입력·칸 세우기·옮기기·닫기·이름). tell 과 같은 인증·origin
        // 가드를 탄다. 칸 신원은 `act_service` 가 이 기기 칸에서 다시 확인한다.
        .route("/collab/act", post(move |Json(params): Json<serde_json::Value>| {
            let backend = collab_act_backend.clone();
            async move {
                match tokio::task::spawn_blocking(move || backend.collab_act(&params)).await {
                    Ok(Ok(value)) => Json(value),
                    Ok(Err(error)) => Json(serde_json::json!({"ok":false,"error":error.to_string()})),
                    Err(_) => Json(serde_json::json!({"ok":false,"error":"act worker stopped"})),
                }
            }
        }).layer(axum::extract::DefaultBodyLimit::max(64 * 1024)))
        .route("/term/chat-send", post(move |body: Json<serde_json::Value>| term_chat_send(chat_send_backend.clone(), body)))
        .route("/collab/tell", post(move |Json(params): Json<serde_json::Value>| {
            let backend = tell_backend.clone();
            collab_tell_post(backend,Json(params))
        }).layer(axum::extract::DefaultBodyLimit::max(24 * 1024)))
        .route("/nacho/report", post(move |Json(params): Json<serde_json::Value>| {
            let backend = nacho_report_backend.clone();
            nacho_report_post(backend, Json(params))
        }).layer(axum::extract::DefaultBodyLimit::max(24 * 1024)))
        .route("/collab/tell/status", post(move |Json(params): Json<serde_json::Value>| {
            let backend = tell_status_backend.clone();
            async move {
                match tokio::task::spawn_blocking(move || backend.collab_tell_status(&params)).await {
                    Ok(Ok(value)) => Json(value),
                    Ok(Err(error)) => Json(serde_json::json!({"ok":false,"error":error.to_string()})),
                    Err(_) => Json(serde_json::json!({"ok":false,"error":"receipt worker stopped"})),
                }
            }
        }).layer(axum::extract::DefaultBodyLimit::max(4096)))
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
}

#[cfg(test)]
mod tests {
    /// 거울 대화 입력은 mod 칸이어도 안전한 tell 로 간다 — mod 에 맡기던 길은 일하는 칸에 턴 내내 묶이고 plugin
    /// 머리가 붙었다. tell 이 거절되면(신원이 안 선 칸) 옛 `/send` 붙여넣기.
    #[tokio::test]
    async fn mirror_chat_input_goes_through_the_safe_tell() {
        use super::*;
        #[derive(Default)]
        struct Desk { refuse: bool, tells: std::sync::Mutex<Vec<serde_json::Value>>, pasted: std::sync::Mutex<Vec<String>> }
        impl Backend for Desk {
            fn list_workspaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::WorkspaceInfo>> { Ok(vec![]) }
            fn current_workspace(&self) -> anyhow::Result<Option<kasa_socket::backend::WorkspaceInfo>> { Ok(None) }
            fn list_surfaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::SurfaceInfo>> { Ok(vec![]) }
            fn focus_surface(&self, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
            fn split_surface(&self, _: kasa_socket::SplitDirection, _: bool, _: Option<&str>) -> anyhow::Result<kasa_socket::backend::SurfaceInfo> { anyhow::bail!("unexpected") }
            fn send_key(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
            fn send_text(&self, _: Option<&str>, text: &str) -> anyhow::Result<()> { self.pasted.lock().unwrap().push(text.into()); Ok(()) }
            fn collab_tell(&self, params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
                anyhow::ensure!(!self.refuse, "identity not established");
                self.tells.lock().unwrap().push(params.clone());
                Ok(serde_json::json!({"state": "accepted"}))
            }
        }
        let desk = Arc::new(Desk::default());
        let Json(answer) = term_chat_send(desk.clone(), Json(serde_json::json!({"surface": "%3", "text": " 폰에서 친 말 "}))).await;
        assert_eq!(answer["via"], "tell");
        let tells = desk.tells.lock().unwrap();
        assert_eq!((tells[0]["surface_id"].as_str(), tells[0]["body"].as_str()), (Some("%3"), Some("폰에서 친 말")));
        assert!(tells[0]["message_id"].as_str().is_some_and(|id| id.starts_with("kt1.")));
        assert!(desk.pasted.lock().unwrap().is_empty());

        let fresh = Arc::new(Desk { refuse: true, ..Default::default() });
        let Json(answer) = term_chat_send(fresh.clone(), Json(serde_json::json!({"surface": "%4", "text": "막 뜬 칸"}))).await;
        assert_eq!(answer["via"], "paste");
        assert_eq!(fresh.pasted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn collaboration_read_routes_share_backend_and_origin_guard() {
        use super::*;
        use axum::http::StatusCode;
        let backend: Arc<dyn Backend> = Arc::new(kasa_collab::board_service::synthetic::SyntheticBackend::default());
        let mut router = axum::Router::new();
        for (path,operation) in [("/collab/board","snapshot"),("/collab/changes","changes"),("/collab/inspect","inspect"),
            ("/collab/peek","peek"),("/collab/capture","capture"),("/collab/where","where"),("/collab/transcript","transcript")] {
            let backend = backend.clone();
            router = router.route(path,get(move |q: Query<std::collections::HashMap<String,String>>|
                collab_read_handler(backend.clone(),operation,q)));
        }
        let router = router.layer(axum::middleware::from_fn(origin_guard_mw));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}",listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener,router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::new();
        for path in ["/collab/board","/collab/changes","/collab/inspect","/collab/peek","/collab/capture","/collab/where","/collab/transcript"] {
            let response = client.get(format!("{base}{path}?scope=local")).send().await.unwrap();
            assert_eq!(response.status(),StatusCode::OK);
            assert_eq!(response.json::<serde_json::Value>().await.unwrap()["synthetic"],true);
            let blocked = client.get(format!("{base}{path}")).header("sec-fetch-site","cross-site").send().await.unwrap();
            assert_eq!(blocked.status(),StatusCode::FORBIDDEN);
        }
        // HTTP 로 온 캡처는 요청의 경로를 버리고 본문으로 답한다 — 원격이 이 기기 디스크 아무 데나 쓰게 두지 않는다.
        let shot: serde_json::Value = client.get(format!("{base}/collab/capture"))
            .query(&[("params",r#"{"path":"/tmp/elsewhere.png","address":{}}"#)]).send().await.unwrap().json().await.unwrap();
        assert_eq!(shot["op"],"capture");
        assert_eq!(shot["params"]["inline"],true);
        assert!(shot["params"].get("path").is_none());
        server.abort();
    }
}
