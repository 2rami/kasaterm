//! 파일·그림·마크다운·URL 열기와 폴더 목록, 방 cwd 옮기기.

use super::*;
use super::arona_ui::mode_slug;

/// `POST /open-file?path=<abs>` — OS 기본 뷰어로 파일 열기(대화창 이미지 클릭 →
/// macOS Preview 등). `~` 확장. macOS=open, Linux=xdg-open, Windows=cmd start.
async fn open_file_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let raw = params.get("path").cloned().unwrap_or_default();
    let path = match raw.strip_prefix("~/") {
        Some(rest) => kasa_socket::home_dir()
            .map(|h| format!("{}/{rest}", h.display()))
            .unwrap_or(raw),
        None => raw,
    };
    if path.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "path required" })));
    }
    let spawned = if cfg!(target_os = "macos") {
        crate::no_window_command("open").arg(&path).spawn()
    } else if cfg!(target_os = "windows") {
        crate::no_window_command("cmd").args(["/C", "start", "", &path]).spawn()
    } else {
        crate::no_window_command("xdg-open").arg(&path).spawn()
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
        Some(rest) => kasa_socket::home_dir()
            .map(|h| format!("{}/{rest}", h.display()))
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
            kasa_socket::collab_root().join(format!("{}__room_{}", mode_slug(&cwd), r))
        }
        _ => kasa_socket::collab_root().join(mode_slug(&cwd)),
    };
    // 세션 경계: since(현재 세션 첫 이벤트 ts, unix sec) 이전 이미지는 이전 대화 잔류물 —
    // 제외(사용자: 이전 pane 이미지가 새 대화에 남던 것). sent-images.jsonl 은 방단위 append-only
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
    // 캡처한 user 메시지(텍스트+이미지)로 말풍선에 뜬다 — sent-images 큰 박스 write 안 함(사용자).
    match backend.paste_image(&surface, body.to_vec()) {
        Ok(()) => (cors, Json(serde_json::json!({ "ok": true }))),
        Err(error) => (cors, Json(serde_json::json!({ "ok": false, "error": error.to_string() }))),
    }
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
/// 떠 있으면 raw `cd` 가 그 프로그램 입력칸에 박히므로(사용자: "프롬프트에 cd~~가 입력돼")
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

/// `GET /open-url?url=<url>&pane=<pid>` — pane 셸의 `open` 셰임·`kasaterm-cli
/// open`·카사크롬 `browser_show_human` 이 부른다. 호스트가 「그 pane 을 보는 거울」로
/// 되돌리거나 직접 연다. 도착지가 「폰」이면 여기서 먼저 임시 터널로 바깥 주소를
/// 만들어 `url` 로 돌려준다 — 부른 쪽(학생)이 그 링크를 답장에 적을 수 있게. 이 기기
/// localhost 이고 새 판 폰 앱이면 터널 없이 그대로 넘긴다(`quicktunnel::in_app_url`).
/// GUI 의 폰 경로는 이미 바깥 주소면 그대로 쪽지에 넣으므로 두 번 세우지 않는다.
async fn open_url_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let url = params.get("url").cloned().unwrap_or_default();
    let pane = if params.get("local").is_some_and(|v| v == "1") {
        Some("__local_browser__")
    } else { params.get("pane").map(|s| s.as_str()).filter(|s| !s.is_empty()) };
    let phone = crate::machines::opens_on_phone();
    let mut tunnel_error = None;
    let in_app = if phone { crate::quicktunnel::in_app_url(&url) } else { None };
    let shown = if let Some(local) = in_app.clone() {
        local
    } else if phone && !url.is_empty() {
        let raw = url.clone();
        match tokio::task::spawn_blocking(move || crate::quicktunnel::public_url(&raw)).await {
            Ok(Ok(public)) => public,
            Ok(Err(e)) => { tunnel_error = Some(e); url.clone() }
            Err(e) => { tunnel_error = Some(e.to_string()); url.clone() }
        }
    } else { url.clone() };
    let body = match backend.open_url(&shown, pane) {
        Ok(()) => {
            let mut body = serde_json::json!({ "ok": true, "url": shown,
                "target": if phone { "phone".to_string() } else { crate::machines::kasachrome_machine() } });
            if let Some(e) = tunnel_error { body["tunnel_error"] = serde_json::json!(e); }
            // 폰 앱 안 Safari 화면이 카사넷(아니면 관문)으로 연다 — 바깥 주소가 없으니 답장에 이 주소를 링크로 적지 않게.
            if in_app.is_some() { body["in_app"] = serde_json::json!(true); }
            body
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

async fn browser_resolve_url_handler(Query(params): Query<std::collections::HashMap<String, String>>) -> impl IntoResponse {
    let selected = crate::machines::kasachrome_machine();
    if params.get("machine").is_some_and(|expected| expected != &selected) {
        return Json(serde_json::json!({"ok": false, "error": "브라우저 기기가 바뀌었어요. 다시 시도해 주세요"}));
    }
    let url = params.get("url").map(String::as_str).unwrap_or("");
    match crate::browser_target::resolve_url(url, &selected).await {
        Ok(url) => Json(serde_json::json!({"ok": true, "url": url, "machine": selected})),
        Err(error) => Json(serde_json::json!({"ok": false, "error": error.to_string()})),
    }
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

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let open_image_backend = backend.clone();
    let open_markdown_backend = backend.clone();
    let open_url_backend = backend.clone();
    let list_dir_backend = backend.clone();
    let room_cd_backend = backend.clone();
    let sent_images_backend = backend.clone();
    let paste_image_backend = backend.clone();
    axum::Router::new()
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
            "/open-url",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                open_url_handler(open_url_backend.clone(), q)
            }),
        )
        .route(
            "/browser/resolve-url",
            get(browser_resolve_url_handler),
        )
        .route(
            "/save-markdown",
            post(move |body: String| save_markdown_handler(body)),
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
            "/paste-image",
            post(move |q: Query<std::collections::HashMap<String, String>>, b: Bytes| {
                paste_image_handler(paste_image_backend.clone(), q, b)
            }).layer(axum::extract::DefaultBodyLimit::max(32 << 20)),
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
}
