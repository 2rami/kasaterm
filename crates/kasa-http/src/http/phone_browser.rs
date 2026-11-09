//! `/phone/browser/*` — 카사브라우저 폰 앱이 웹뷰 명령을 가져가고 결과를 올리는 창구(`kasa_browser::phone`).
//! 주인 폰과 이 기계만.

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

/// `GET /phone/browser/next?wait=25` — 다음 명령 `{id, op, arg}`, 기다려도 없으면 204.
async fn next(req: axum::extract::Request) -> axum::response::Response {
    if !owner_phone(&req) {
        return forbidden();
    }
    let wait = query(&req).get("wait").and_then(|w| w.parse::<u64>().ok()).unwrap_or(25).min(25);
    match kasa_browser::phone::next(std::time::Duration::from_secs(wait)).await {
        Some((id, op, arg)) => Json(serde_json::json!({"id": id, "op": op, "arg": arg})).into_response(),
        None => axum::http::StatusCode::NO_CONTENT.into_response(),
    }
}

/// `POST /phone/browser/result?id=` — 화면은 `Content-Type: image/png` 의 바이트, 나머지는 JSON
/// (`{"error": …}` 면 실패). 기다리던 쪽이 이미 포기했으면 410.
async fn result(req: axum::extract::Request) -> axum::response::Response {
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
        Ok(kasa_browser::phone::Outcome::Png(body.to_vec()))
    } else {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => match v.get("error").and_then(|e| e.as_str()) {
                Some(e) => Err(e.to_string()),
                None => Ok(kasa_browser::phone::Outcome::Json(v)),
            },
            Err(e) => Err(format!("폰이 알 수 없는 답을 보냈다: {e}")),
        }
    };
    if kasa_browser::phone::deliver(&id, outcome) {
        Json(serde_json::json!({"ok": true})).into_response()
    } else {
        (axum::http::StatusCode::GONE, Json(serde_json::json!({"ok": false, "error": "nobody_waiting"}))).into_response()
    }
}

pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        .route("/phone/browser/next", get(next))
        .route("/phone/browser/result", post(result).layer(axum::extract::DefaultBodyLimit::max(32 << 20)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::{Request, StatusCode}};
    use std::time::Duration;
    use tower::ServiceExt;

    fn request(method: &str, uri: &str, owner: bool, body: Body) -> Request<Body> {
        let mut req = Request::builder().method(method).uri(uri).body(body).unwrap();
        req.extensions_mut().insert(MobileAuth(kasa_device::mobile::MobileUser {
            name: "fixture".into(), slug: "fixture-only".into(), created: 0, owner,
        }));
        req
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn browser_app_transport_keeps_ownership_and_json_png_roundtrips() {
        let app = routes();
        for (method, uri) in [("GET", "/phone/browser/next?wait=0"), ("POST", "/phone/browser/result?id=fixture")] {
            let response = app.clone().oneshot(request(method, uri, false, Body::empty())).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let old = app.clone().oneshot(request("GET", "/phone/web/next?wait=0", true, Body::empty())).await.unwrap();
        assert_eq!(old.status(), StatusCode::NOT_FOUND);
        let ready = app.clone().oneshot(request("GET", "/phone/browser/next?wait=0", true, Body::empty())).await.unwrap();
        assert_eq!(ready.status(), StatusCode::NO_CONTENT);

        for (op, content_type, bytes) in [
            ("text", "application/json", br#"{"text":"fixture"}"#.as_slice()),
            ("shot", "image/png", b"\x89PNG\r\n\x1a\nfixture".as_slice()),
        ] {
            let caller = std::thread::spawn(move || kasa_browser::phone::call(op, "", Duration::from_secs(5)));
            let response = app.clone().oneshot(request("GET", "/phone/browser/next?wait=5", true, Body::empty())).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            let command: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(command["op"], op);
            let uri = format!("/phone/browser/result?id={}", command["id"].as_str().unwrap());
            let mut result = request("POST", &uri, true, Body::from(bytes));
            result.headers_mut().insert(header::CONTENT_TYPE, content_type.parse().unwrap());
            assert_eq!(app.clone().oneshot(result).await.unwrap().status(), StatusCode::OK);
            match caller.join().unwrap().unwrap() {
                kasa_browser::phone::Outcome::Json(value) => assert_eq!(value["text"], "fixture"),
                kasa_browser::phone::Outcome::Png(value) => assert_eq!(value, bytes),
            }
            let late = request("POST", &uri, true, Body::from("{}"));
            assert_eq!(app.clone().oneshot(late).await.unwrap().status(), StatusCode::GONE);
        }
    }
}
