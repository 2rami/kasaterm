//! 기기 화면 거울의 창구 — 브라우저·PC 웹 칸이 여는 보기 페이지 `/screen` 과 프레임 소켓 `/screen/ws`.
//! 뜨기·역압은 `kasa_remote::screen_mirror`, 계약은 `docs/screen-mirror.md`.

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
    let mut tune = kasa_remote::screen_mirror::Tune::default();
    let query = Query::<std::collections::HashMap<String, String>>::try_from_uri(req.uri()).map(|Query(q)| q).unwrap_or_default();
    tune.apply(|k| query.get(k)?.parse().ok());
    // 영상 방식(`codec=hevc`) — 이 판이 화면을 압축할 수 있을 때만. 못 하면 JPEG 로 답하고 보는 쪽은 `hello` 에 `codec` 이 없는 것으로 안다.
    let video = query
        .get("codec")
        .filter(|c| matches!(c.as_str(), "hevc" | "h264") && kasa_remote::screen_stream::installed())
        .map(|codec| {
            let num = |k: &str| query.get(k).and_then(|v| v.parse::<f64>().ok());
            kasa_remote::screen_stream::Config::from_json(&serde_json::json!({
                "codec": codec,
                "bitrate": num("mbps").map(|m| (m * 1_000_000.0) as u64),
                "fps": num("fps").map(|f| f as u64),
                "scale": num("scale"),
                "refine": num("refine").map(|r| r as u64),
            }))
        });
    use axum::extract::FromRequestParts as _;
    let (mut parts, _body) = req.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if let Some(config) = video {
        return upgrade.on_upgrade(move |socket| kasa_remote::screen_stream::serve_ws(socket, config));
    }
    let source = kasa_remote::screen_mirror::installed();
    upgrade.on_upgrade(move |socket| kasa_remote::screen_mirror::serve(socket, source, tune))
}

pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        .route("/screen", get(page))
        .route("/screen/ws", get(ws))
}
