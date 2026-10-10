//! 세션 탭·최근 세션·이어 하기·백그라운드 에이전트(`claude agents`) 창구와 claude 바이너리 찾기.

use super::*;

/// `GET /sessions` — JSON snapshot of the tmux-style session tabs for the
/// session panel to poll: `{ count, active }`.
async fn sessions_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let s = backend.sessions();
    let body = serde_json::json!({ "count": s.count, "active": s.active, "saved": s.saved, "labels": s.labels });
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

/// `POST /session-new?character=<name>` — 새 방(윈도우) + 첫 pane 캐릭터 지정 스폰
/// (사용자: 방 추가 시 캐릭터 선택). 미지정이면 아로나 기본. 구 클라이언트의
/// `?god=` 파라미터도 당분간 수용(god 개념 폐기 후 하위호환).
async fn session_new_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let character = params
        .get("character")
        .or_else(|| params.get("god"))
        .filter(|s| !s.is_empty())
        .map(|s| s.as_str())
        .unwrap_or("아로나");
    let body = match backend.new_room(character) {
        Ok(()) => serde_json::json!({ "ok": true, "character": character }),
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

/// 세션 목록에 학생(캐릭터)을 얹는다. `scope` 두 갈래가 같은 모양을 내도록 공통.
fn with_bound_characters(sessions: &[kasa_socket::backend::RecentSession]) -> serde_json::Value {
    let mut arr = serde_json::to_value(sessions).unwrap_or_default();
    if let Some(list) = arr.as_array_mut() {
        for s in list.iter_mut() {
            let bound = s
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(crate::character::session_character);
            if let (Some(ch), Some(obj)) = (bound, s.as_object_mut()) {
                obj.insert("character".into(), serde_json::json!(ch));
            }
        }
    }
    arr
}

/// `GET /recent-sessions?cwd=<abs>&scope=here|all` — recent sessions for the
/// arona-ui resume picker. Newest first:
/// `{ ok, sessions: [{harness, id, label, mtime, cwd, character?}] }`.
///
/// `scope=here`(기본) 는 `cwd`(생략 시 활성 pane 의 cwd) 아래의 세션만. 이쪽도
/// 하네스를 가로지른다 — 같은 폴더에서 codex 로 일한 기록이 프로젝트 목록에
/// 없으면 "여기서 뭘 하다 말았지"에 답이 안 된다. `scope=all` 은 cwd 를 무시하고
/// **하네스 전부**(claude·codex·agy)를
/// 섞어 돌려준다. 목표는 오르카의 「Agent 세션 기록」 처럼 어느 코딩 프로그램의
/// 세션이든 한 목록에서 골라 잇는 것이고, 각 항목의 `harness` 를
/// `/session-resume?harness=` 로 되돌리면 그 프로그램의 이어가기 명령이 나간다.
///
/// `character` 는 세션→학생 영속 바인딩(session_characters.json) — teamName 기록
/// 세션이 claude 자체 /resume 에서 숨겨지는 탓에 이 피커가 사실상 유일한 복원
/// 입구라, 어느 학생의 세션인지 프사·학생색으로 즉시 구분하게 얹는다(사용자).
/// 미바인딩 세션은 필드 생략(웹뷰가 실루엣 폴백).
async fn recent_sessions_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // 하네스별로 이만큼씩 모아 시각순으로 자른다. 기본 20 은 `scope=here` 이 예전부터
    // 쓰던 값이고, 상한을 두는 건 각 하네스 저장소를 그만큼 훑기 때문이다.
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .map_or(20, |n| n.clamp(1, 200));
    if params.get("scope").is_some_and(|s| s == "all") {
        let sessions = kasa_socket::sessions::recent_all_sessions(limit);
        let body = serde_json::json!({ "ok": true, "sessions": with_bound_characters(&sessions) });
        return ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body));
    }
    let cwd = params.get("cwd").filter(|s| !s.is_empty()).map(|s| s.as_str());
    let body = match backend.recent_sessions(cwd) {
        Ok(sessions) => {
            serde_json::json!({ "ok": true, "sessions": with_bound_characters(&sessions) })
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-resume?id=<uuid>&cwd=<abs>&newroom=<bool>&harness=<name>` —
/// open a pane and inject that session's resume command once its shell prompt is
/// up. `newroom=true` opens a fresh window; otherwise it splits the active one.
/// Query params for the same no-preflight reason as session-switch.
///
/// `harness` 는 `/recent-sessions` 가 각 항목에 실어 주는 값을 그대로 되돌려 주면
/// 된다(`claude`/`codex`/`agy`). 없으면 claude — 이 파라미터가 없던 시절의 호출도
/// 그대로 동작한다.
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
    let harness = params
        .get("harness")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("claude");
    let body = if id.is_empty() {
        serde_json::json!({ "ok": false, "error": "missing id" })
    } else {
        match backend.resume_session(&id, cwd.as_deref(), newroom, attach, harness) {
            Ok(()) => serde_json::json!({ "ok": true, "id": id }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `POST /session-save?surface=%N` — foreground claude 를 background daemon 으로
/// detach(←← agents-view 주입). surface 없으면 active pane. "대화 저장하기" — 터미널이
/// 꺼져도 daemon 이 세션을 들고 살아남아 웹뷰에서 계속 보인다(사용자 핵심).
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

/// `claude agents --json --all` 한 벌을 앱 안 여럿이 나눠 쓴다. 부를 때마다 node 를 띄우고
/// 키체인(`security`)을 여는 일이라 한 번에 CPU 280ms·메모리 150MB 남짓이다 — 배경 에이전트
/// 폴러(3초)와 명부 캐시(5초)가 따로 띄우던 시절엔 초당 0.5번이었다(2026-10-06 실측).
/// `max_age` 보다 새것이 있으면 그것을, 아니면 지금 읽어 채운다. 동시에 부르면 하나만 띄우고
/// 나머지는 그 결과를 기다린다. 읽기에 실패하면 `None`(빈 목록과 다르다).
pub fn claude_agents_all(max_age: std::time::Duration) -> Option<std::sync::Arc<Vec<serde_json::Value>>> {
    type Shared = Option<(std::time::Instant, std::sync::Arc<Vec<serde_json::Value>>)>;
    static LAST: std::sync::Mutex<Shared> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = last.as_ref() {
        if at.elapsed() < max_age {
            return Some(list.clone());
        }
    }
    let out = crate::no_window_command(claude_bin()).args(["agents", "--json", "--all"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let value = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()?;
    let list = value
        .as_array()
        .cloned()
        .or_else(|| value.get("agents").and_then(|a| a.as_array().cloned()))?;
    let list = std::sync::Arc::new(list);
    *last = Some((std::time::Instant::now(), list.clone()));
    Some(list)
}

/// Locate the `claude` binary. A GUI app's PATH is minimal (launchd, not the
/// login shell), so PATH lookup alone misses npm-global/local installs — probe
/// the common locations, honoring `CLAUDE_BIN` for an explicit override, and
/// fall back to bare `claude` (PATH) as a last resort.
pub fn claude_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_BIN") {
        if !p.is_empty() {
            return p.into();
        }
    }
    let home = kasa_socket::home_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
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
    let out = crate::no_window_command("ps")
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
    let mut cmd = crate::no_window_command(claude_bin());
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
                            let parent_sid = parent_session_from_pid(pid);
                            if let (Some(parent_sid), Some(obj)) =
                                (parent_sid.as_deref(), a.as_object_mut())
                            {
                                if let Some((pane, _)) =
                                    pane_sids.iter().find(|(_, sid)| sid == parent_sid)
                                {
                                    obj.insert("parentSurface".into(), serde_json::json!(pane));
                                }
                                obj.insert("parentSessionId".into(), serde_json::json!(parent_sid));
                            }
                            // detach 포크는 --agent-name 유실로 이름 없이(name=sid 프리픽스)
                            // 등록된다 — claude 자체 목록은 upstream 한계라, 표시층(웹뷰·
                            // classroom)이 쓰도록 세션→캐릭터 바인딩(자기 sid → 없으면 부모
                            // sid)으로 학생 이름을 복원해 얹는다(사용자: ←← 하면 이름 사라짐).
                            let own_sid = a
                                .get("sessionId")
                                .and_then(|s| s.as_str())
                                .map(str::to_string);
                            let bound = own_sid
                                .as_deref()
                                .and_then(crate::character::session_character)
                                .or_else(|| {
                                    parent_sid
                                        .as_deref()
                                        .and_then(crate::character::session_character)
                                });
                            if let (Some(ch), Some(obj)) = (bound, a.as_object_mut()) {
                                obj.insert("character".into(), serde_json::json!(ch));
                                let nameless =
                                    obj.get("name").and_then(|n| n.as_str()).is_none_or(|n| {
                                        n.is_empty()
                                            || own_sid
                                                .as_deref()
                                                .is_some_and(|s| s.starts_with(n) || n == s)
                                    });
                                if nameless {
                                    obj.insert("name".into(), serde_json::json!(ch));
                                }
                            }
                        }
                    }
                    // 원격 기계의 background 세션도 같은 목록에. 로컬 파싱이 성공한
                    // 경우에만 얹는다 — 실패 분기는 이미 ok:false 라 섞을 자리가 없다.
                    if let Some(arr) = agents.as_array_mut() {
                        arr.extend(crate::remoteboard::background_agents());
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
/// 가 준 것(claude 워커 프로세스). 사용자: 백그라운드 패널에서 세션을 쉽게 정리.
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

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let sessions_backend = backend.clone();
    let session_switch_backend = backend.clone();
    let session_new_backend = backend.clone();
    let session_close_backend = backend.clone();
    let session_restore_backend = backend.clone();
    let session_rename_backend = backend.clone();
    let recent_sessions_backend = backend.clone();
    let session_resume_backend = backend.clone();
    let session_save_backend = backend.clone();
    let background_agents_backend = backend.clone();
    let session_reset_backend = backend.clone();
    axum::Router::new()
        .route(
            "/sessions",
            get(move || sessions_handler(sessions_backend.clone())),
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
}
