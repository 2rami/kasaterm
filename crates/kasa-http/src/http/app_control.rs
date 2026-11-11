use super::*;

async fn control(backend: Arc<dyn Backend>, request: axum::extract::Request) -> axum::response::Response {
    if let Some(denied) = guest_denied(&request) { return denied; }
    let body = match axum::body::to_bytes(request.into_body(), 8192).await {
        Ok(body) => body,
        Err(_) => return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, Json(serde_json::json!({"ok":false,"error":"request too large"}))).into_response(),
    };
    let parsed = serde_json::from_slice::<kasa_socket::app_control::Request>(&body).map_err(anyhow::Error::from)
        .and_then(|request| { request.validate()?; Ok(request) });
    let request = match parsed {
        Ok(request) => request,
        Err(error) => return (axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({"ok":false,"error":error.to_string()}))).into_response(),
    };
    if let Some(expected) = &request.machine_id {
        if kasa_collab::board_service::local_id().ok().as_ref() != Some(expected) {
            return (axum::http::StatusCode::CONFLICT, Json(serde_json::json!({"ok":false,"error":"target identity changed"}))).into_response();
        }
    }
    let result = tokio::task::spawn_blocking(move || backend.app_control(&request)).await;
    let (status,value) = match result {
        Ok(Ok(value)) => (axum::http::StatusCode::OK,value),
        Ok(Err(error)) => (axum::http::StatusCode::CONFLICT,serde_json::json!({"ok":false,"error":error.to_string()})),
        Err(_) => (axum::http::StatusCode::SERVICE_UNAVAILABLE,serde_json::json!({"ok":false,"error":"app unavailable"})),
    };
    (status, [(header::CACHE_CONTROL,"no-store")], Json(value)).into_response()
}

pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let backend=backend.clone();
    axum::Router::new().route("/app/control",post(move |request| control(backend.clone(),request)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    #[derive(Default)] struct Desk(AtomicUsize);
    impl Backend for Desk {
        fn list_workspaces(&self)->anyhow::Result<Vec<kasa_socket::backend::WorkspaceInfo>>{Ok(vec![])}
        fn current_workspace(&self)->anyhow::Result<Option<kasa_socket::backend::WorkspaceInfo>>{Ok(None)}
        fn list_surfaces(&self)->anyhow::Result<Vec<kasa_socket::backend::SurfaceInfo>>{Ok(vec![])}
        fn focus_surface(&self,_:&str)->anyhow::Result<()>{unreachable!()}
        fn split_surface(&self,_:kasa_socket::SplitDirection,_:bool,_:Option<&str>)->anyhow::Result<kasa_socket::backend::SurfaceInfo>{unreachable!()}
        fn send_key(&self,_:Option<&str>,_:&str)->anyhow::Result<()>{unreachable!()}
        fn send_text(&self,_:Option<&str>,_:&str)->anyhow::Result<()>{unreachable!()}
        fn app_control(&self,_:&kasa_socket::app_control::Request)->anyhow::Result<serde_json::Value>{
            self.0.fetch_add(1,Ordering::Relaxed); Ok(serde_json::json!({"ok":true,"machine_id":"fixture"}))
        }
    }
    fn req(owner:bool, body:&str)->axum::extract::Request {
        let mut request=axum::extract::Request::builder().method("POST").uri("/app/control").body(axum::body::Body::from(body.to_owned())).unwrap();
        request.extensions_mut().insert(MobileAuth(kasa_device::mobile::MobileUser {name:"fixture".into(),slug:"abcdefghijklmnopqrstuvwxy".into(),created:0,owner}));
        request
    }
    #[tokio::test]
    async fn guests_are_denied_before_parsing_or_calling_the_app() {
        let desk=Arc::new(Desk::default());
        for body in ["not-json",r#"{"action":"screen"}"#,r#"{"action":"restart","expect":"1.hash","interrupt":true}"#] {
            assert_eq!(control(desk.clone(),req(false,body)).await.status(),axum::http::StatusCode::FORBIDDEN);
        }
        assert_eq!(desk.0.load(Ordering::Relaxed),0);
    }
    #[tokio::test]
    async fn owner_reads_are_uncached_and_invalid_mutations_never_reach_the_app() {
        let desk=Arc::new(Desk::default());
        let read=control(desk.clone(),req(true,r#"{"action":"inspect"}"#)).await;
        assert_eq!(read.status(),axum::http::StatusCode::OK);
        assert_eq!(read.headers()[header::CACHE_CONTROL],"no-store");
        for body in [r#"{"action":"restart"}"#,r#"{"action":"restore","expect":"1.hash","approved":true}"#] {
            assert_eq!(control(desk.clone(),req(true,body)).await.status(),axum::http::StatusCode::BAD_REQUEST);
        }
        let wrong=control(desk.clone(),req(true,r#"{"action":"inspect","machine_id":"different-machine"}"#)).await;
        assert_eq!(wrong.status(),axum::http::StatusCode::CONFLICT);
        assert_eq!(desk.0.load(Ordering::Relaxed),1);
    }
}
