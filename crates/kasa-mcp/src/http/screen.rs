//! 기기 화면 거울의 창구 — 브라우저·PC 웹 칸이 여는 보기 페이지 `/screen` 과 프레임 소켓 `/screen/ws`.
//! 뜨기·역압은 `crate::screen_mirror`, 계약은 `docs/screen-mirror.md`.

use super::*;
use super::auth::html_with_token_cookie;

async fn page(Query(q): Query<std::collections::HashMap<String, String>>) -> axum::response::Response {
    html_with_token_cookie(include_str!("../../assets/screen/index.html"), &q)
}

async fn ws(req: axum::extract::Request) -> axum::response::Response {
    // 화면에는 주인의 메일·비밀번호 창까지 다 보인다 — 손님 폰 주소는 막는다.
    if let Some(denied) = guest_denied(&req) {
        return denied;
    }
    if !ws_origin_ok(req.headers()) {
        return (axum::http::StatusCode::FORBIDDEN, "cross-origin websocket refused").into_response();
    }
    let mut tune = crate::screen_mirror::Tune::default();
    if let Ok(Query(q)) = Query::<std::collections::HashMap<String, String>>::try_from_uri(req.uri()) {
        tune.apply(|k| q.get(k)?.parse().ok());
    }
    use axum::extract::FromRequestParts as _;
    let (mut parts, _body) = req.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let source = crate::screen_mirror::installed();
    upgrade.on_upgrade(move |socket| crate::screen_mirror::serve(socket, source, tune))
}

pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        .route("/screen", get(page))
        .route("/screen/ws", get(ws))
}
