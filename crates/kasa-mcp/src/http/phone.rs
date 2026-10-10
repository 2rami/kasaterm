//! 폰 앱·허브 페이지와 폰 사용자 관리(`/mobile/*`), 카사넷 폰 등록.

use super::*;
use super::auth::{ViaPhone, ViaUplink};

/// `POST /kasanet/phone` body `{id}` — 폰이 제 카사넷 id 를 등록한다(`kasanet::allow_phone`). 관문을 거쳐 주인 폰
/// 자격으로 온 것만 받는다: 관문이 기기 토큰(또는 주인 주소)을 확인하고 업링크로 내려보낸 길이다. 카사넷으로 온
/// 등록은 받지 않는다 — 관문에서 폐기된 폰이 직통으로 제 등록을 이어 가면 안 된다.
pub(super) async fn kasanet_phone_handler(req: axum::extract::Request) -> axum::response::Response {
    let ext = req.extensions();
    let owner_phone = ext.get::<ViaUplink>().is_some()
        && ext.get::<ViaPhone>().is_none()
        && ext.get::<MobileAuth>().is_some_and(|a| a.0.owner);
    if !owner_phone {
        return (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({"ok": false, "error": "owner_phone_via_gateway_only"}))).into_response();
    }
    let Ok(body) = axum::body::to_bytes(req.into_body(), 4096).await else {
        return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    };
    let id = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(str::to_string))
        .unwrap_or_default();
    match crate::kasanet::allow_phone(&id) {
        Ok(ttl) => Json(serde_json::json!({"ok": true, "ttl_secs": ttl.as_secs()})).into_response(),
        Err(code) => {
            let status = if code == "bad_id" || code == "id_taken" { axum::http::StatusCode::BAD_REQUEST } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };
            (status, Json(serde_json::json!({"ok": false, "error": code}))).into_response()
        }
    }
}

/// 하단바 「기기」 QR 이 여는 안내 — 앱으로 열기 · 앱 설치 · 웹(허브)에서 보기.
/// 폰 카메라는 주소 하나만 열 수 있어 세 갈래를 한 페이지에 둔다(2026-09-08 지시).
/// 설치 주소는 설정에서 읽어 서버가 심는다 — 페이지가 따로 묻는 왕복을 안 만든다.
async fn app_page() -> impl IntoResponse {
    let install = crate::character::app_install_url().unwrap_or_default();
    let html = include_str!("../../assets/term/app.html")
        .replace("__INSTALL_URL__", &html_escape(&install));
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 폰 허브 — 이 기계와 명부의 다른 기계, 그 pane 목록. 누르면 터미널로.
async fn hub_page() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../assets/term/hub.html"),
    )
}

/// 유저를 더하고 지울 권한 — 이 기계에서 직접(loopback) 왔거나 **주인 주소**로 왔을 때.
/// 남에게 준 주소로는 자기 화면만 보고 목록은 못 본다.
fn mobile_can_manage(req: &axum::extract::Request) -> bool {
    if let Some(a) = req.extensions().get::<MobileAuth>() {
        return a.0.owner;
    }
    !is_remote_peer(req)
}

fn mobile_user_json(u: &crate::mobile::MobileUser) -> serde_json::Value {
    let path = crate::mobile::path_of(u);
    serde_json::json!({
        "name": u.name,
        "owner": u.owner,
        "path": path,
        // 바깥 주소가 켜져 있으면 폰에 보낼 완성 주소까지 — 허브가 127.0.0.1 로 열려
        // 있을 때 location.origin 은 폰에 소용이 없다.
        "url": crate::tunnel::host().map(|h| format!("https://{h}{path}")),
    })
}

/// `GET /mobile/me` — 이 요청이 누구 주소로 왔나 + 관리 가능 여부 + 이 기계 이름.
async fn mobile_me(req: axum::extract::Request) -> axum::response::Response {
    let who = req
        .extensions()
        .get::<MobileAuth>()
        .map(|a| a.0.clone())
        .or_else(|| (!is_remote_peer(&req)).then(crate::mobile::owner).flatten());
    Json(serde_json::json!({
        "ok": true,
        "name": who.as_ref().map(|u| u.name.clone()),
        "owner": who.as_ref().is_some_and(|u| u.owner),
        "can_manage": mobile_can_manage(&req),
        "machine": crate::mobile::machine_name(),
        "tunnel": crate::tunnel::host(),
    }))
    .into_response()
}

fn mobile_query_name(req: &axum::extract::Request) -> Option<String> {
    axum::extract::Query::<std::collections::HashMap<String, String>>::try_from_uri(req.uri())
        .ok()
        .and_then(|q| q.0.get("name").cloned())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
}

/// `GET /mobile/users` — 유저와 주소 목록(주인만).
async fn mobile_users_get(req: axum::extract::Request) -> axum::response::Response {
    if !mobile_can_manage(&req) {
        return (axum::http::StatusCode::FORBIDDEN, "owner only").into_response();
    }
    // 주인이 아직 없으면 여기서 생긴다 — 목록이 비어 보이는 첫 화면을 없앤다.
    let _ = crate::mobile::owner();
    let users: Vec<_> = crate::mobile::users().iter().map(mobile_user_json).collect();
    Json(serde_json::json!({ "ok": true, "users": users })).into_response()
}

/// `POST /mobile/users?name=` — 유저를 더하고 그 주소를 돌려준다. `&rotate=1` 이면
/// 있는 유저의 주소를 새로 뽑는다(샜을 때).
async fn mobile_users_post(req: axum::extract::Request) -> axum::response::Response {
    if !mobile_can_manage(&req) {
        return (axum::http::StatusCode::FORBIDDEN, "owner only").into_response();
    }
    let Some(name) = mobile_query_name(&req) else {
        return (axum::http::StatusCode::BAD_REQUEST, "name 이 필요해요").into_response();
    };
    let rotate = axum::extract::Query::<std::collections::HashMap<String, String>>::try_from_uri(req.uri())
        .ok()
        .and_then(|q| q.0.get("rotate").cloned())
        .is_some_and(|v| v == "1" || v == "true");
    let res = if rotate { crate::mobile::rotate(&name) } else { crate::mobile::add(&name) };
    match res {
        Ok(u) => Json(serde_json::json!({ "ok": true, "user": mobile_user_json(&u) })).into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}

/// `DELETE /mobile/users?name=` — 그 주소가 즉시 죽는다.
async fn mobile_users_delete(req: axum::extract::Request) -> axum::response::Response {
    if !mobile_can_manage(&req) {
        return (axum::http::StatusCode::FORBIDDEN, "owner only").into_response();
    }
    let Some(name) = mobile_query_name(&req) else {
        return (axum::http::StatusCode::BAD_REQUEST, "name 이 필요해요").into_response();
    };
    match crate::mobile::remove(&name) {
        Ok(removed) => Json(serde_json::json!({ "ok": true, "removed": removed })).into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        .route("/kasanet/phone", axum::routing::post(kasanet_phone_handler))
        // 폰 허브·유저별 주소 관리·다른 기계로 넘기는 문(mobile.rs 머리말).
        .route("/hub", get(hub_page))
        .route("/app", get(app_page))
        .route("/mobile/me", get(mobile_me))
        .route(
            "/mobile/users",
            get(mobile_users_get)
                .post(mobile_users_post)
                .delete(mobile_users_delete),
        )
}
