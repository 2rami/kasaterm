//! `/relay/agent-chains` — Claude 로그인 사슬 맡기기와 접근 토큰 받기(`agent_chains.rs`,
//! docs/agent-chains.md). 계정은 기기 토큰에서만 온다 — 몸통은 계정을 이름 대지 않는다.

use super::*;

const MAX_BODY: usize = 32 * 1024;

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/agent-chains", get(list))
        .route("/relay/agent-chains/seal", axum::routing::post(seal))
}

impl Gate {
    /// 관문이 도는 동안 1분마다 맡긴 사슬을 만료 전에 갱신한다. 사슬이 끊기거나 갱신이 계속 막히면 그
    /// 관문 계정의 폰으로 알린다 — 한 사슬이 막히면 그 계정을 쓰는 모든 기기가 함께 끊긴다.
    pub fn spawn_agent_chains(&self) {
        let Some(service) = self.agent_chains.clone() else { return };
        let gate = self.clone();
        tokio::spawn(async move {
            loop {
                let (_, notices) = service.refresh_due().await;
                for notice in notices {
                    approvals::send_push(&gate, &notice.account, push_payload(&notice));
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }
}

fn push_payload(notice: &crate::agent_chains::Notice) -> serde_json::Value {
    use sha2::Digest as _;
    let collapse = format!("chain-{:x}", sha2::Sha256::digest(notice.key.as_bytes()));
    serde_json::json!({
        "collapse": &collapse[..22],
        "aps": {
            "alert": {"title": notice.title, "body": notice.body},
            "sound": "default",
            "thread-id": "agent-chains",
            "interruption-level": "time-sensitive",
        },
        "kind": "agent_chain",
    })
}

/// (관문 계정, 기기 id, 저장소). 폐기된 기기·막힌 계정은 401.
fn who(
    gate: &Gate,
    headers: &axum::http::HeaderMap,
) -> Result<(String, String, Arc<crate::agent_chains::Service>), axum::response::Response> {
    let Some((me, d)) = gate.device_of(headers) else {
        return Err(agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    if !catalog_device_active(gate, &gate.devices.lock().unwrap(), &me, &d) {
        return Err(agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    let Some(service) = gate.agent_chains.clone() else {
        return Err(agent_accounts_error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"));
    };
    Ok((d.account, me, service))
}

fn answer(result: Result<serde_json::Value, crate::agent_chains::Error>) -> axum::response::Response {
    match result {
        Ok(value) => agent_accounts_response(axum::Json(value)),
        Err(error) => agent_accounts_error(
            StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_GATEWAY),
            error.code(),
        ),
    }
}

async fn list(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    match who(&gate, &headers) {
        Ok((account, _, service)) => answer(service.list(&account).await),
        Err(refusal) => refusal,
    }
}

async fn seal(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (account, device, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(req.into_body(), MAX_BODY)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return agent_accounts_error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
        Err(_) => return agent_accounts_error(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
    };
    let Ok(offer) = serde_json::from_slice::<crate::agent_chains::Offer>(&bytes) else {
        return agent_accounts_error(StatusCode::BAD_REQUEST, "bad_request");
    };
    answer(service.seal(&account, &device, offer).await)
}

#[cfg(test)]
#[path = "gateway_agent_chains/tests.rs"]
mod tests;
