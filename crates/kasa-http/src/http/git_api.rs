//! Git 패널 창구 — 상태·diff·커밋·푸시·AI 커밋 메시지.

use super::*;

/// Body for `POST /git-commit`: which files to stage and the message.
#[derive(serde::Deserialize)]
struct CommitReq {
    files: Vec<String>,
    message: String,
}

/// `GET /git-status` — JSON snapshot of the host's current working dir for
/// the webview panel to poll. The wildcard CORS header lets the webview
/// (a different origin) fetch it; the server only binds to 127.0.0.1 so the
/// open origin stays local-only.
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

/// `POST /git-panel` — 아로나 타이틀바 버튼 → 터미널 GUI git 소스컨트롤 패널 토글(사용자).
async fn git_panel_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let ok = backend.toggle_git_panel().is_ok();
    (cors, Json(serde_json::json!({ "ok": ok })))
}

/// Body for `POST /git-ai-commit`: the files the user checked in the panel.
/// Empty → let the AI decide what to include.
#[derive(serde::Deserialize)]
struct AiCommitReq {
    #[serde(default)]
    files: Vec<String>,
}

/// `POST /git-ai-commit` — delegate the commit to the AI. If the active pane
/// runs an agent, inject a commit instruction (with the checked files) so the
/// working agent does the commit; otherwise ask the user to focus an agent
/// pane (agent spawn is phase 2).
///
/// ⚠️ 판정은 **`active_agent`(하네스)** 로 한다. 예전엔 `active_process_name` 에
/// "claude" 가 들었나만 봤는데, codex 는 npm shim 이라 프로세스 이름이 `node` 라서
/// codex pane 에선 버튼이 영영 "claude 가 켜진 pane 에서 눌러주세요" 만 뱉었다.
async fn git_ai_commit_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    // Raw JSON string body (text/plain) to avoid the CORS preflight — see
    // git_commit_handler. Empty/garbage body falls back to "no files".
    let req: AiCommitReq = serde_json::from_str(&body).unwrap_or(AiCommitReq { files: Vec::new() });
    let agent = backend.active_agent();
    let body = if let Some(agent) = agent {
        let msg = if req.files.is_empty() {
            "git 패널에서 AI 커밋을 눌렀어. 지금 작업 디렉토리의 변경사항을 검토하고 적절한 한국어 커밋 메시지로 git add + commit 해줘.\n".to_string()
        } else {
            format!(
                "git 패널에서 AI 커밋을 눌렀어. 체크된 파일은 다음과 같아: {}. 이 파일들만 stage해서 적절한 한국어 커밋 메시지로 commit 해줘.\n",
                req.files.join(", ")
            )
        };
        let _ = backend.send_text(None, &msg);
        serde_json::json!({ "ok": true, "output": format!("작업 중인 {agent}에게 커밋을 요청했어요") })
    } else {
        let proc = backend.active_process_name().unwrap_or_default();
        let who = if proc.is_empty() { "셸".to_string() } else { proc };
        serde_json::json!({ "ok": false, "output": format!("claude·codex가 켜진 pane에서 눌러주세요 (활성: {who})") })
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let git_backend = backend.clone();
    let diff_backend = backend.clone();
    let commit_backend = backend.clone();
    let push_backend = backend.clone();
    let ai_backend = backend.clone();
    let git_panel_backend = backend.clone();
    axum::Router::new()
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
            "/git-panel",
            post(move || git_panel_handler(git_panel_backend.clone())),
        )
}
