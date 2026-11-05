//! `/phone/web/*` — 폰 앱이 웹뷰 명령을 가져가고 결과를 올리는 창구(`crate::phone_web`). 주인 폰과 이 기계만.

use super::*;

/// 주인 주소로 온 폰, 또는 이 기계에서 바로 온 것(시뮬레이터·CLI) — 폰 사용자 관리와 같은 기준.
fn owner_phone(req: &axum::extract::Request) -> bool {
    match req.extensions().get::<MobileAuth>() {
        Some(a) => a.0.owner,
        None => !is_remote_peer(req),
    }
}

fn forbidden() -> axum::response::Response {
    (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({"ok": false, "error": "owner_phone_only"}))).into_response()
}

fn query(req: &axum::extract::Request) -> std::collections::HashMap<String, String> {
    axum::extract::Query::<std::collections::HashMap<String, String>>::try_from_uri(req.uri())
        .map(|q| q.0)
        .unwrap_or_default()
}

/// `GET /phone/web/next?wait=25`(카사텀 폰 앱)·`GET /phone/browser/next?wait=25`(카사브라우저 폰 앱) — 그 앱의 다음
/// 명령 `{id, op, arg}`, 기다려도 없으면 204.
async fn next(target: &'static str, req: axum::extract::Request) -> axum::response::Response {
    if !owner_phone(&req) {
        return forbidden();
    }
    let wait = query(&req).get("wait").and_then(|w| w.parse::<u64>().ok()).unwrap_or(25).min(25);
    match crate::phone_web::next(target, std::time::Duration::from_secs(wait)).await {
        Some((id, op, arg)) => Json(serde_json::json!({"id": id, "op": op, "arg": arg})).into_response(),
        None => axum::http::StatusCode::NO_CONTENT.into_response(),
    }
}

/// `POST /phone/{web,browser}/result?id=` — 화면은 `Content-Type: image/png` 의 바이트, 나머지는 JSON
/// (`{"error": …}` 면 실패). 기다리던 쪽이 이미 포기했거나 그 앱의 명령이 아니면 410.
async fn result(target: &'static str, req: axum::extract::Request) -> axum::response::Response {
    if !owner_phone(&req) {
        return forbidden();
    }
    let Some(id) = query(&req).get("id").cloned().filter(|s| !s.is_empty()) else {
        return (axum::http::StatusCode::BAD_REQUEST, "id 가 필요해요").into_response();
    };
    let png = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("image/png"));
    let Ok(body) = axum::body::to_bytes(req.into_body(), 32 << 20).await else {
        return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    };
    let outcome = if png {
        Ok(crate::phone_web::Outcome::Png(body.to_vec()))
    } else {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => match v.get("error").and_then(|e| e.as_str()) {
                Some(e) => Err(e.to_string()),
                None => Ok(crate::phone_web::Outcome::Json(v)),
            },
            Err(e) => Err(format!("폰이 알 수 없는 답을 보냈다: {e}")),
        }
    };
    if crate::phone_web::deliver(target, &id, outcome) {
        Json(serde_json::json!({"ok": true})).into_response()
    } else {
        (axum::http::StatusCode::GONE, Json(serde_json::json!({"ok": false, "error": "nobody_waiting"}))).into_response()
    }
}

pub(super) fn routes() -> axum::Router {
    let limit = || axum::extract::DefaultBodyLimit::max(32 << 20);
    axum::Router::new()
        .route("/phone/web/next", get(|req| next("kasaterm", req)))
        .route("/phone/web/result", post(|req| result("kasaterm", req)).layer(limit()))
        .route("/phone/browser/next", get(|req| next("browser", req)))
        .route("/phone/browser/result", post(|req| result("browser", req)).layer(limit()))
}
