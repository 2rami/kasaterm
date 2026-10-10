//! 웹 터미널 곁의 파일·Git·메모·클립보드 창구(`/term/tree`·`file`·`gitcol`·`notes`·`clipboard`).

use super::*;

/// 배치가 `since` 뒤로 바뀔 때까지 매달려 있다가 번호를 돌려준다(롱폴). 관문 우회는
/// 답 머리를 20초까지만 기다리므로 그 안에서 끊는다.
/// 다른 기기의 깃 패널 재료 — 그 기계가 자기 레포를 읽어 그대로 준다(2026-09-17).
async fn term_gitcol_get(
    backend: Arc<dyn Backend>,
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if q.contains_key("schema") || q.contains_key("pane") {
        let query = q.0;
        let result = tokio::task::spawn_blocking(move || crate::git_panel::read(backend.as_ref(), &query))
            .await.unwrap_or_else(|_| serde_json::json!({"schema": crate::git_panel::SCHEMA, "ok": false, "error": "git_unavailable"}));
        return Json(result);
    }
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')).cloned() else {
        return Json(serde_json::json!({ "ok": false, "error": "`path`(절대경로) 가 필요해요" }));
    };
    let commits = q.get("commits").and_then(|v| v.parse().ok()).unwrap_or(20usize).min(200);
    match backend.git_col_view(&path, commits) {
        Ok(view) => Json(serde_json::json!({ "ok": true, "view": view })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// `POST /term/gitop {path, op, message?}` — 다른 기기의 깃 패널이 시키는 일.
///
/// 읽기(`/term/gitcol`)는 있었는데 고치는 길이 없어, 남의 기기 레포를 보면서도 커밋·푸시는
/// 그 기계로 가서 해야 했다(2026-09-21 지시). 경로는 **그 기계의 것**이라 부르는 쪽에서
/// git 을 돌 수 없다 — 시키는 수밖에 없다.
///
/// 되돌리기 어려운 것은 받지 않는다: pull·push·commit(스테이지된 것)뿐이고, 브랜치 전환이나
/// 되감기는 없다. 워킹트리를 여럿이 함께 쓰는 기계에서 그건 남의 pane 을 통째로 끌고 간다.
async fn term_gitop_post(body: axum::body::Bytes) -> impl IntoResponse {
    let err = |m: &str| Json(serde_json::json!({ "ok": false, "error": m }));
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return err("JSON body 가 필요해요");
    };
    let Some(path) = v.get("path").and_then(|p| p.as_str()).filter(|p| p.starts_with('/')) else {
        return err("`path`(절대경로) 가 필요해요");
    };
    let op = v.get("op").and_then(|o| o.as_str()).unwrap_or_default();
    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or_default().trim();
    let path = std::path::Path::new(path).to_path_buf();
    let message = message.to_string();
    let op = op.to_string();
    let done = tokio::task::spawn_blocking(move || match op.as_str() {
        "pull" => Some(crate::git::git_pull(&path)),
        "push" => Some(crate::git::git_push(&path)),
        "commit" if !message.is_empty() => Some(crate::git::git_commit_staged(&path, &message)),
        _ => None,
    })
    .await
    .unwrap_or(None);
    // git 함수는 `{ok, output}` 을 그대로 준다 — 실패 사유(충돌·인증)를 부르는 쪽 토스트가
    // 보여줘야 하므로 통째로 넘긴다.
    match done {
        Some(result) => Json(result),
        None => err("`op` 은 pull·push·commit 중 하나여야 하고, commit 은 `message` 가 필요해요"),
    }
}

/// 폴더 한 층 — 다른 기기의 파일트리가 이걸로 그린다. `.git` 은 빼고, 폴더 먼저.
async fn term_tree_get(
    q: Query<std::collections::HashMap<String, String>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if let Some(denied) = guest_denied(&req) {
        return denied;
    }
    term_tree_list(q).into_response()
}

fn term_tree_list(q: Query<std::collections::HashMap<String, String>>) -> impl IntoResponse {
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')).cloned() else {
        return Json(serde_json::json!({ "ok": false, "error": "`path`(절대경로) 가 필요해요" }));
    };
    let Ok(rd) = std::fs::read_dir(&path) else {
        return Json(serde_json::json!({ "ok": false, "error": "폴더를 못 읽어요" }));
    };
    let mut entries: Vec<(bool, String, bool)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" { return None; }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let is_repo = is_dir && e.path().join(".git").exists();
            Some((is_dir, name, is_repo))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let entries: Vec<serde_json::Value> = entries.into_iter()
        .map(|(is_dir, name, is_repo)| serde_json::json!({ "name": name, "is_dir": is_dir, "is_repo": is_repo }))
        .collect();
    Json(serde_json::json!({ "ok": true, "path": path, "entries": entries }))
}

/// 파일 하나 — 다른 기기에서 열어 보기용. 4MB 까지만.
async fn term_file_get(
    q: Query<std::collections::HashMap<String, String>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if let Some(denied) = guest_denied(&req) {
        return denied;
    }
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')).cloned() else {
        return (axum::http::StatusCode::BAD_REQUEST, "`path`(절대경로) 가 필요해요").into_response();
    };
    match std::fs::metadata(&path) {
        Ok(m) if m.is_file() && m.len() <= 4 * 1024 * 1024 => {}
        Ok(m) if m.is_file() => return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, "4MB 를 넘어요").into_response(),
        _ => return (axum::http::StatusCode::NOT_FOUND, "그런 파일이 없어요").into_response(),
    }
    match std::fs::read(&path) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn term_changes_handler(q: Query<std::collections::HashMap<String, String>>) -> impl IntoResponse {
    let since = q.get("since").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let wait = q.get("wait").and_then(|v| v.parse::<u64>().ok()).unwrap_or(15).min(15);
    let epoch = crate::changes::wait_past(since, std::time::Duration::from_secs(wait)).await;
    Json(serde_json::json!({ "epoch": epoch, "status": crate::changes::status_aware() }))
}

/// `GET /term/path?path=<abs>` — 그 경로가 이 기계에 있나, 그리고 이 기계의 홈.
/// 이사가 「저쪽에 같은 폴더가 없으면 홈에서 띄운다」(2026-09-14 지시)를 고르는
/// 데 쓴다 — 레포를 만들어 맞추는 대신 있는 자리를 묻기만 한다.
async fn term_path_get(
    q: Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(path) = q.get("path").filter(|p| p.starts_with('/')) else {
        return Json(serde_json::json!({ "ok": false, "error": "`path`(절대경로) 가 필요해요" }));
    };
    let home = kasa_socket::home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default();
    Json(serde_json::json!({
        "ok": true,
        "exists": std::path::Path::new(path).is_dir(),
        "home": home,
    }))
}

/// `POST /term/agent-stop?pane=web-…` — 그 세션 셸 아래의 에이전트를 곱게(SIGTERM)
/// 끄고 꺼질 때까지 지켜본다. 역이사의 「출발지 claude 끄기」와 대칭 — SIGKILL 을
/// 안 쓰는 이유도 같다(jsonl 마지막 조각 유실). 인자에서 권한 모드도 읽어 준다:
/// 로컬은 원격 프로세스의 argv 를 볼 손이 없어서, 여기서 읽어 실어 보내야
/// 「옮겨오니 오토모드로 바뀌었다」(2026-08-27 지적의 역방향)가 안 생긴다.
/// 학생 쪽지 목록 — 최근 것부터. 폰 허브의 종 아이콘이 5초마다 읽는다.
async fn term_notes_get() -> impl IntoResponse {
    Json(serde_json::json!(crate::notes::list()))
}

/// 나쵸가 쪽지를 넣는다 — 본문은 notes.rs 의 NoteInput.
async fn term_notes_post(body: Bytes) -> impl IntoResponse {
    let err = |m: String| Json(serde_json::json!({ "ok": false, "error": m }));
    let input: crate::notes::NoteInput = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(format!("쪽지 본문을 못 읽었어요: {e}")),
    };
    match crate::notes::add(input) {
        Some(n) => {
            // 쪽지는 사람이 자리에 없을 때 오는 것이라 폰에도 같이 알린다.
            crate::push::note_arrived(&n.character, &n.kind, &n.summary, &n.pane, Some(&n.url));
            Json(serde_json::json!({ "ok": true, "id": n.id }))
        }
        None => err("pane 과 summary 는 비면 안 돼요".into()),
    }
}

/// 폰이 애플에서 받은 기기 토큰을 맡긴다 — `{token, env: prod|dev}`. 주소의 slug 로
/// 누구 폰인지 안다(없으면 로컬 = 주인).
async fn term_push_token_post(req: axum::extract::Request) -> axum::response::Response {
    let user = req
        .extensions()
        .get::<MobileAuth>()
        .map(|a| a.0.name.clone())
        .unwrap_or_default();
    let body = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
        Ok(b) => b,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })).into_response(),
    };
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let token = v.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
    let env = v.get("env").and_then(serde_json::Value::as_str).unwrap_or("prod");
    let root = v.get("root").and_then(serde_json::Value::as_str).unwrap_or("");
    if v.get("remove").and_then(serde_json::Value::as_bool).unwrap_or(false) {
        crate::push::unregister(token);
        return Json(serde_json::json!({ "ok": true })).into_response();
    }
    let n = crate::push::register(token, env, &user, root);
    Json(serde_json::json!({ "ok": true, "devices": n, "ready": crate::push::configured() })).into_response()
}

/// 읽음 표시 — `{"ids":[1,2]}` 또는 `{"all":true}`.
async fn term_notes_read_post(body: Bytes) -> impl IntoResponse {
    #[derive(serde::Deserialize)]
    struct Req {
        #[serde(default)]
        ids: Vec<u64>,
        #[serde(default)]
        all: bool,
    }
    let req: Req = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": format!("본문을 못 읽었어요: {e}") })),
    };
    let n = crate::notes::mark_read(&req.ids, req.all);
    Json(serde_json::json!({ "ok": true, "marked": n }))
}

/// 지우기 — `{"ids":[1,2]}` 또는 `{"all":true}`. 폰이 옆으로 밀어 지운다.
async fn term_notes_delete_post(body: Bytes) -> impl IntoResponse {
    #[derive(serde::Deserialize)]
    struct Req {
        #[serde(default)]
        ids: Vec<u64>,
        #[serde(default)]
        all: bool,
    }
    let req: Req = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": format!("본문을 못 읽었어요: {e}") })),
    };
    let n = crate::notes::remove(&req.ids, req.all);
    Json(serde_json::json!({ "ok": true, "removed": n }))
}

/// `GET /term/clipboard` — 최근 복사 목록(미리보기·id·비밀 여부). 본문은 안 싣는다.
async fn term_clipboard_list(backend: Arc<dyn Backend>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "items": backend.clipboard_history() }))
}

/// `POST /term/clipboard {text, secret?, from_machine?}` — 폰이나 다른 기계에서 복사한
/// 것을 이 기계 클립보드로. `from_machine` 이 있으면 기계가 밀어 준 것이라 다시
/// 퍼뜨리지 않는다(되돌이 방지).
async fn term_clipboard_post(backend: Arc<dyn Backend>, body: Bytes) -> impl IntoResponse {
    #[derive(serde::Deserialize)]
    struct Req {
        text: String,
        #[serde(default)]
        secret: bool,
        #[serde(default)]
        from_machine: Option<String>,
    }
    let req: Req = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": format!("본문을 못 읽었어요: {e}") })),
    };
    if req.text.trim().is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "빈 글이에요" }));
    }
    let stored = match req.from_machine.as_deref().filter(|m| !m.trim().is_empty()) {
        Some(from) => backend.clipboard_set_from_peer(&req.text, req.secret, from),
        None => backend.clipboard_set_opts(&req.text, req.secret),
    };
    match stored {
        Ok(()) => Json(serde_json::json!({ "ok": true, "chars": req.text.chars().count() })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// `GET /term/clipboard/{id}` — 한 칸의 본문. 폰이 제 클립보드로 가져갈 때.
async fn term_clipboard_item(backend: Arc<dyn Backend>, AxPath(id): AxPath<u64>) -> impl IntoResponse {
    match backend.clipboard_item(id) {
        Ok(text) => Json(serde_json::json!({ "ok": true, "text": text })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// `POST /term/clipboard/pick {id}` — 한 칸을 이 기계 클립보드로 되올린다.
async fn term_clipboard_pick(backend: Arc<dyn Backend>, body: Bytes) -> impl IntoResponse {
    #[derive(serde::Deserialize)]
    struct Req {
        id: u64,
    }
    let req: Req = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": format!("본문을 못 읽었어요: {e}") })),
    };
    match backend.clipboard_pick(req.id) {
        Ok(text) => Json(serde_json::json!({ "ok": true, "chars": text.chars().count() })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// 쪽지에 딸린 pane 사진.
async fn term_notes_image(AxPath(name): AxPath<String>) -> impl IntoResponse {
    let id = name
        .strip_suffix(".png")
        .and_then(|s| s.parse::<u64>().ok());
    match id.and_then(crate::notes::image_bytes) {
        Some(bytes) => (
            axum::http::StatusCode::OK,
            [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "private, max-age=86400")],
            bytes,
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let gitcol_backend = backend.clone();
    let gitcol_wait_backend = backend.clone();
    let clip_backend = backend.clone();
    axum::Router::new()
        .route("/term/changes", get(term_changes_handler))
        .route("/term/gitop", post(term_gitop_post))
        .route(
            "/term/gitcol",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                term_gitcol_get(gitcol_backend.clone(), q)
            }),
        )
        .route(
            "/term/gitcol/wait",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                let backend = gitcol_wait_backend.clone();
                async move { Json(crate::git_panel::wait(backend, q.0).await) }
            }),
        )
        .route("/term/tree", get(term_tree_get))
        .route("/term/file", get(term_file_get))
        .route("/term/path", get(term_path_get))
        // 학생 쪽지 — 나쵸가 넣고 폰 종 목록이 읽는다(notes.rs 머리말).
        .route("/term/notes", get(term_notes_get).post(term_notes_post))
.route("/term/push-token", post(term_push_token_post))
        .route("/term/notes/read", post(term_notes_read_post))
        .route("/term/notes/delete", post(term_notes_delete_post))
        // 클립보드 — 하단바 「최근 복사」 목록을 폰과 나눈다.
        .route(
            "/term/clipboard",
            get({
                let b = clip_backend.clone();
                move || term_clipboard_list(b.clone())
            })
            .post({
                let b = clip_backend.clone();
                move |body| term_clipboard_post(b.clone(), body)
            }),
        )
        .route(
            "/term/clipboard/pick",
            post({
                let b = clip_backend.clone();
                move |body| term_clipboard_pick(b.clone(), body)
            }),
        )
        .route(
            "/term/clipboard/{id}",
            get({
                let b = clip_backend.clone();
                move |p| term_clipboard_item(b.clone(), p)
            }),
        )
        .route("/term/notes/{name}", get(term_notes_image))
}
