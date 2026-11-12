use super::*;

async fn plan(backend: Arc<dyn Backend>, request: axum::extract::Request) -> axum::response::Response {
    if let Some(denied)=guest_denied(&request) {return denied;}
    let result=tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let mut plan=backend.agent_plan(None)?;
        plan["machine_id"]=serde_json::json!(kasa_collab::board_service::local_id()?);
        plan["account_selection"]=serde_json::json!(1);
        plan["ok"]=serde_json::json!(true);
        Ok(plan)
    }).await;
    let (status,body)=match result {
        Ok(Ok(value))=>(axum::http::StatusCode::OK,value),
        Ok(Err(error))=>(axum::http::StatusCode::CONFLICT,serde_json::json!({"ok":false,"error":error.to_string()})),
        Err(_)=>(axum::http::StatusCode::SERVICE_UNAVAILABLE,serde_json::json!({"ok":false,"error":"account plan unavailable"})),
    };
    (status,[(header::CACHE_CONTROL,"no-store")],Json(body)).into_response()
}

pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let backend=backend.clone();
    axum::Router::new().route("/agents/plan",get(move |request|plan(backend.clone(),request)))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct GuestTrap;
    impl Backend for GuestTrap {
        fn list_workspaces(&self)->anyhow::Result<Vec<kasa_socket::backend::WorkspaceInfo>>{Ok(vec![])}
        fn current_workspace(&self)->anyhow::Result<Option<kasa_socket::backend::WorkspaceInfo>>{Ok(None)}
        fn list_surfaces(&self)->anyhow::Result<Vec<kasa_socket::backend::SurfaceInfo>>{Ok(vec![])}
        fn focus_surface(&self,_:&str)->anyhow::Result<()>{unreachable!()}
        fn split_surface(&self,_:kasa_socket::SplitDirection,_:bool,_:Option<&str>)->anyhow::Result<kasa_socket::backend::SurfaceInfo>{unreachable!()}
        fn send_key(&self,_:Option<&str>,_:&str)->anyhow::Result<()>{unreachable!()}
        fn send_text(&self,_:Option<&str>,_:&str)->anyhow::Result<()>{unreachable!()}
        fn agent_plan(&self,_:Option<&str>)->anyhow::Result<serde_json::Value>{panic!("guest must not observe account inventory")}
    }
    #[tokio::test]
    async fn guest_cannot_observe_account_choices() {
        let mut request=axum::extract::Request::builder().uri("/agents/plan").body(axum::body::Body::empty()).unwrap();
        request.extensions_mut().insert(MobileAuth(kasa_device::mobile::MobileUser {name:"guest".into(),slug:"abcdefghijklmnopqrstuvwxy".into(),created:0,owner:false}));
        assert_eq!(plan(Arc::new(GuestTrap),request).await.status(),axum::http::StatusCode::FORBIDDEN);
    }
}
