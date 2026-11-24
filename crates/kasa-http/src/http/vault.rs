use std::time::Duration;

use axum::{
    body::{self, Body},
    extract::Request,
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use kasa_device::access::auth::{MobileAuth, ViaPhone};
use kasa_remote::netfwd::VAULT_LOOPBACK_PORT;

const MAX_JSON: usize = 16 * 1024 * 1024;

pub fn routes() -> Router {
    let client = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .build();
    vault_routes(client, format!("http://127.0.0.1:{VAULT_LOOPBACK_PORT}"))
}

fn vault_routes(client: Result<reqwest::Client, reqwest::Error>, source: String) -> Router {
    let client = client.ok();
    let forward = move |req: Request| {
        let client = client.clone();
        let source = source.clone();
        async move { forward(req, client.as_ref(), &source).await }
    };
    Router::new()
        .route("/vault/status", get(forward.clone()))
        .route("/vault/list", get(forward.clone()))
        .route("/vault/file", get(forward.clone()).put(forward.clone()))
        .route("/vault/search", post(forward.clone()))
        .route("/vault/quiz", post(forward))
}

pub(super) fn owner_phone(req: &Request) -> bool {
    // Only the private phone ingress can set ViaPhone.
    req.extensions().get::<ViaPhone>().is_some()
        && req
            .extensions()
            .get::<MobileAuth>()
            .is_some_and(|auth| auth.0.owner)
}

async fn forward(req: Request, client: Option<&reqwest::Client>, source: &str) -> Response {
    if !owner_phone(&req) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(client) = client else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let (head, body) = req.into_parts();
    let Some(path) = head
        .uri
        .path_and_query()
        .and_then(|path| path.as_str().strip_prefix("/vault"))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let has_body = matches!(head.method, Method::POST | Method::PUT);
    let timeout = match head.uri.path() {
        "/vault/quiz" => 630,
        _ if head.method == Method::PUT => 70,
        _ => 22,
    };
    let mut upstream = client
        .request(head.method, format!("{source}{path}"))
        .timeout(Duration::from_secs(timeout));
    if has_body {
        let Ok(body) = body::to_bytes(body, MAX_JSON).await else {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        };
        upstream = upstream
            .header(header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let Ok(mut reply) = upstream.send().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let status = reply.status();
    if status.is_redirection() {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let mut bytes = Vec::new();
    loop {
        match reply.chunk().await {
            Ok(Some(chunk)) if chunk.len() <= MAX_JSON - bytes.len() => bytes.extend(chunk),
            Ok(None) => break,
            Ok(Some(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        }
    }
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from(bytes),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use kasa_device::{access::auth::ViaUplink, mobile::MobileUser};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    fn request(uri: &str, owner: Option<bool>, phone: bool) -> Request {
        let mut req = Request::put(uri)
            .body(Body::from(
                r#"{"path":"리서치/용어.md","text":"수정","base_sha":"old"}"#,
            ))
            .unwrap();
        if let Some(owner) = owner {
            req.extensions_mut().insert(ViaUplink);
            req.extensions_mut().insert(MobileAuth(MobileUser {
                name: "test".into(),
                slug: "test".into(),
                created: 0,
                owner,
            }));
        }
        if phone {
            req.extensions_mut().insert(ViaPhone);
        }
        req
    }

    #[tokio::test]
    async fn vault_requires_owner_phone_and_preserves_conflicts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}", listener.local_addr().unwrap());
        let daemon = Router::new().route(
            "/file",
            axum::routing::put(|uri: axum::http::Uri, Json(data): Json<Value>| async move {
                assert_eq!(uri.query(), Some("dir=a%2Fb"));
                assert_eq!(data["text"], "수정");
                assert_eq!(data["base_sha"], "old");
                (
                    StatusCode::CONFLICT,
                    Json(json!({"error": "conflict", "sha": "new", "text": "최신"})),
                )
            }),
        ).route(
            "/quiz",
            axum::routing::post(|Json(data): Json<Value>| async move {
                assert_eq!(data, json!({"path":"메모리.md","kind":"terms","n":30}));
                Json(json!({"deck_path":"_study/용어.md","cards":3,"title":"용어","commit":"abc123"}))
            }),
        );
        let daemon = tokio::spawn(async move { axum::serve(listener, daemon).await.unwrap() });
        let app = vault_routes(reqwest::Client::builder().no_proxy().build(), source);
        let uri = "/vault/file?dir=a%2Fb";
        for req in [
            request(uri, None, false),
            request(uri, Some(true), false),
            request(uri, Some(false), true),
        ] {
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status(),
                StatusCode::NOT_FOUND
            );
        }
        let res = app
            .clone()
            .oneshot(request(uri, Some(true), true))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
        let body = body::to_bytes(res.into_body(), MAX_JSON).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["text"],
            "최신"
        );
        let quiz = |phone| {
            let mut req = request("/vault/quiz", Some(true), phone);
            *req.method_mut() = Method::POST;
            *req.body_mut() = Body::from(r#"{"path":"메모리.md","kind":"terms","n":30}"#);
            req
        };
        assert_eq!(
            app.clone().oneshot(quiz(false)).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        let response = app.oneshot(quiz(true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body::to_bytes(response.into_body(), MAX_JSON)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["commit"],
            "abc123"
        );
        daemon.abort();
    }
}
