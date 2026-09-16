//! `kasa-relay` — 폰 관문 서버. 앱이 업링크로 붙고(`/relay/uplink`) 폰은 `/u/<slug>/…`
//! 로 들어온다 — 배관은 전부 `gateway.rs`. 배포 위치(넷버드망·클러스터)와 무관.
//!
//!   kasa-relay [--port <n>] [--state <파일>]   (기본 8790 · ~/.config/kasaterm/relay-state.json)
//!
//! 세션 중계(`/relay/register`·`/relay/sessions`·`/relay/send`)는 2026-09-16 에 걷어냈다.
//! 그 길은 클로드 전용 유령 명부(SendMessage)에 얹힌 것이라 코덱스가 못 탔고, 기기 간
//! 소통은 이제 `tell`(보드 주소 + `/collab/tell`) 하나다. 관문은 자격을 모른다 — 토큰도
//! 안 본다(공용 문).

use axum::{routing::get, Json, Router};
use serde_json::json;

pub fn router() -> Router {
    Router::new().route("/relay/health", get(|| async { Json(json!({ "ok": true })) }))
}

pub async fn serve(port: u16, _token: Option<String>, state: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let gate = crate::gateway::Gate::new(state);
    let app = router().merge(crate::gateway::router(gate));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let addr = listener.local_addr()?;
    println!("[kasa-relay] listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
