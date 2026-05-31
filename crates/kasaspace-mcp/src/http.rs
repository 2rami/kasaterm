//! Streamable-HTTP serving glue. The host (kasaterm) is a synchronous
//! winit/wgpu app, so we own a small multi-thread tokio runtime on a
//! dedicated background thread and run axum there. The `Backend` is
//! channel-based and `Send + Sync`, so calling it from async handlers on
//! another thread is safe.

use std::sync::Arc;

use agent_socket::backend::{Backend, PanelKind};
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
    let body = match backend.open_preview("image", &path) {
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
    let body = match backend.open_preview("markdown", &path) {
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
                let open_image_backend = backend.clone();
                let open_markdown_backend = backend.clone();
                let panel_open_backend = backend.clone();
                let panel_close_backend = backend.clone();
                let panel_resize_backend = backend.clone();
                let panel_info_backend = backend.clone();
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
