//! 공유방 host 창구 — 폰이 자기 PC 에 「이 대화 게시」「받은 포크를 여기서 열기」를 맡긴다.
//! 실제 일은 앱(`Backend::shared_room_action`)이 하고, 여기는 소유자 확인과 JSON 모양만 맡는다.
//! 손님(다른 계정의 기계 경유 요청)은 원본을 읽거나 새 탭을 열 수 없으니 몸통을 보기 전에 거절한다.

use super::*;

const BODY_LIMIT: usize = 64 << 10;

async fn act(
    backend: Arc<dyn Backend>,
    auth: Option<axum::extract::Extension<MobileAuth>>,
    action: &'static str,
    body: serde_json::Value,
) -> axum::response::Response {
    if auth.as_ref().is_some_and(|auth| !auth.0.0.owner) {
        return (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({"ok": false, "error": "owner_only"}))).into_response();
    }
    let result = tokio::task::spawn_blocking(move || backend.shared_room_action(action, &body)).await;
    let mut value = match result {
        Ok(Ok(value)) if value.is_object() => value,
        Ok(Ok(_)) => serde_json::json!({"ok": false, "error": "shared_response"}),
        Ok(Err(error)) if action == "status" => {
            let _ = error;
            serde_json::json!({"available": false, "authenticated": false})
        }
        Ok(Err(error)) => serde_json::json!({"ok": false, "error": error.to_string()}),
        Err(_) => serde_json::json!({"ok": false, "error": "shared_unavailable"}),
    };
    if value.get("ok").is_none() {
        value["ok"] = serde_json::json!(true);
    }
    let status = if value["ok"] == true { axum::http::StatusCode::OK } else { axum::http::StatusCode::BAD_REQUEST };
    (status, Json(value)).into_response()
}

fn parse(body: &Bytes) -> Result<serde_json::Value, axum::response::Response> {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) if value.is_object() => Ok(value),
        _ => Err((axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({"ok": false, "error": "shared_invalid"}))).into_response()),
    }
}

pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let (status, sources, publish, fork, operation) = (backend.clone(), backend.clone(), backend.clone(), backend.clone(), backend.clone());
    axum::Router::new()
        .route("/shared-rooms/status", get(move |auth| act(status.clone(), auth, "status", serde_json::json!({}))))
        .route("/shared-rooms/sources", get(move |auth| act(sources.clone(), auth, "sources", serde_json::json!({}))))
        .route(
            "/shared-rooms/publish",
            post(move |auth, body: Bytes| {
                let backend = publish.clone();
                async move {
                    match parse(&body) {
                        Ok(body) => act(backend, auth, "publish", body).await,
                        Err(response) => response,
                    }
                }
            })
            .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route(
            "/shared-rooms/fork",
            post(move |auth, body: Bytes| {
                let backend = fork.clone();
                async move {
                    match parse(&body) {
                        Ok(body) => act(backend, auth, "fork", body).await,
                        Err(response) => response,
                    }
                }
            })
            .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route(
            "/shared-rooms/forks/{operation}",
            get(move |auth, AxPath(id): AxPath<String>| act(operation.clone(), auth, "operation", serde_json::json!({"operation_id": id}))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Desk(Mutex<Vec<(String, serde_json::Value)>>);
    impl Backend for Desk {
        fn list_workspaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::WorkspaceInfo>> { Ok(vec![]) }
        fn current_workspace(&self) -> anyhow::Result<Option<kasa_socket::backend::WorkspaceInfo>> { Ok(None) }
        fn list_surfaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::SurfaceInfo>> { Ok(vec![]) }
        fn focus_surface(&self, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
        fn split_surface(&self, _: kasa_socket::SplitDirection, _: bool, _: Option<&str>) -> anyhow::Result<kasa_socket::backend::SurfaceInfo> { anyhow::bail!("unexpected") }
        fn send_key(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
        fn send_text(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
        fn shared_room_action(&self, action: &str, body: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
            self.0.lock().unwrap().push((action.into(), body.clone()));
            Ok(serde_json::json!({"operation_id": "op", "status": "preparing"}))
        }
    }

    struct Old;
    impl Backend for Old {
        fn list_workspaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::WorkspaceInfo>> { Ok(vec![]) }
        fn current_workspace(&self) -> anyhow::Result<Option<kasa_socket::backend::WorkspaceInfo>> { Ok(None) }
        fn list_surfaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::SurfaceInfo>> { Ok(vec![]) }
        fn focus_surface(&self, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
        fn split_surface(&self, _: kasa_socket::SplitDirection, _: bool, _: Option<&str>) -> anyhow::Result<kasa_socket::backend::SurfaceInfo> { anyhow::bail!("unexpected") }
        fn send_key(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
        fn send_text(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
    }

    fn user(owner: bool) -> Option<axum::extract::Extension<MobileAuth>> {
        Some(axum::extract::Extension(MobileAuth(kasa_device::mobile::MobileUser {
            name: "fixture".into(), slug: "abcdefghijklmnopqrstuvwxy".into(), created: 0, owner,
        })))
    }

    async fn body(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn guests_never_reach_the_app() {
        let desk = Arc::new(Desk::default());
        for action in ["status", "sources", "publish", "fork", "operation"] {
            let response = act(desk.clone(), user(false), action, serde_json::json!({})).await;
            assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN, "{action}");
        }
        assert!(desk.0.lock().unwrap().is_empty());
        let response = act(desk.clone(), user(true), "fork", serde_json::json!({"fork_id": "f"})).await;
        assert_eq!(body(response).await, serde_json::json!({"ok": true, "operation_id": "op", "status": "preparing"}));
        let response = act(desk.clone(), None, "operation", serde_json::json!({"operation_id": "op"})).await;
        assert_eq!(body(response).await["ok"], true);
        assert_eq!(desk.0.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn old_backends_answer_status_as_unavailable() {
        let old: Arc<dyn Backend> = Arc::new(Old);
        assert_eq!(body(act(old.clone(), None, "status", serde_json::json!({})).await).await,
            serde_json::json!({"ok": true, "available": false, "authenticated": false}));
        let response = act(old, None, "fork", serde_json::json!({})).await;
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let fork = body(response).await;
        assert_eq!(fork["ok"], false);
    }

    #[test]
    fn bodies_must_be_objects() {
        assert!(parse(&Bytes::from_static(b"[1]")).is_err());
        assert!(parse(&Bytes::from_static(b"not json")).is_err());
        assert!(parse(&Bytes::from_static(b"{\"room_id\":\"r\"}")).is_ok());
    }
}
