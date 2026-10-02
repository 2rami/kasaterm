//! `/relay/connections` — Gmail and GitHub work permissions of the device's account
//! (`connections.rs`, docs/account-connections.md). The account comes only from the device
//! credential; a body never names one.

use super::*;
use crate::connections::{Caller, MailDraft, PrDraft, Write};
use serde::Deserialize;
use serde_json::json;

const MAX_BODY: usize = 256 * 1024;

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/connections", get(list))
        .route("/relay/connections/{id}", axum::routing::delete(disconnect))
        .route("/relay/connections/audit", get(audit))
        .route("/relay/connections/mail/list", axum::routing::post(mail_list))
        .route("/relay/connections/mail/read", axum::routing::post(mail_read))
        .route("/relay/connections/mail/send", axum::routing::post(mail_send))
        .route("/relay/connections/pr/create", axum::routing::post(pr_create))
        .route("/relay/connections/approver", axum::routing::post(approver))
        .route("/relay/connections/pending/{id}/approve", axum::routing::post(approve))
        .route("/relay/connections/pending/{id}/reject", axum::routing::post(reject))
        .layer(axum::middleware::map_response(no_store))
}

async fn no_store(mut response: axum::response::Response) -> axum::response::Response {
    for (key, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(key),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

/// The authenticated device and its account, or the response that refuses the request.
struct Who {
    account: String,
    device: String,
    label: String,
}

impl Who {
    fn caller(&self) -> Caller<'_> {
        Caller {
            account: &self.account,
            device: &self.device,
            label: &self.label,
        }
    }
}

type Refusal = axum::response::Response;

fn who(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<(Who, Arc<crate::connections::Service>), Refusal> {
    let Some((device, rec)) = gate.device_of(headers) else {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    if !gate.account_active(&rec.account) {
        return Err(json_err(StatusCode::UNAUTHORIZED, "account_disabled"));
    }
    let Some(service) = gate.connections.clone() else {
        return Err(json_err(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"));
    };
    Ok((
        Who {
            account: rec.account,
            device,
            label: rec.label,
        },
        service,
    ))
}

async fn read<T: serde::de::DeserializeOwned>(req: axum::extract::Request) -> Result<T, Refusal> {
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), MAX_BODY),
    )
    .await
    .map_err(|_| json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout"))?
    .map_err(|_| json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"))?;
    serde_json::from_slice(&bytes).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad_request"))
}

fn answer(result: Result<serde_json::Value, crate::connections::Error>) -> axum::response::Response {
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => {
            let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut body = json!({"ok":false,"error":error.code()});
            if let Some(detail) = error.detail() {
                body["detail"] = json!(detail);
            }
            (status, axum::Json(body)).into_response()
        }
    }
}

async fn list(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (who, service) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let result = service.list(&who.account).await.map(|mut value| {
        value["available"] = gate.oauth.config.providers(gate.oauth.storage_ready())["connect"].clone();
        value["github_install_url"] = json!(service.github_install_url());
        value
    });
    answer(result)
}

async fn disconnect(
    State(gate): State<Gate>,
    AxPath(id): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let (who, service) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    answer(service.disconnect(&who.caller(), &id).await)
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
}

async fn audit(
    State(gate): State<Gate>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AuditQuery>,
) -> axum::response::Response {
    let (who, service) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let lines = service.audit(&who.account, query.limit.unwrap_or(50).min(200));
    axum::Json(json!({"ok":true,"audit":lines})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailList {
    connection: Option<String>,
    #[serde(default)]
    query: String,
    max: Option<usize>,
}

async fn mail_list(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (who, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let input: MailList = match read(req).await {
        Ok(input) => input,
        Err(refusal) => return refusal,
    };
    answer(
        service
            .mail_list(&who.caller(), input.connection.as_deref(), &input.query, input.max.unwrap_or(10))
            .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailRead {
    connection: Option<String>,
    id: String,
}

async fn mail_read(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (who, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let input: MailRead = match read(req).await {
        Ok(input) => input,
        Err(refusal) => return refusal,
    };
    answer(service.mail_read(&who.caller(), input.connection.as_deref(), &input.id).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailSend {
    connection: Option<String>,
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    subject: String,
    body: String,
    reply_to: Option<String>,
}

async fn mail_send(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (who, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let input: MailSend = match read(req).await {
        Ok(input) => input,
        Err(refusal) => return refusal,
    };
    answer(
        service
            .request(
                &who.caller(),
                input.connection.as_deref(),
                Write::Mail(MailDraft {
                    to: input.to,
                    cc: input.cc,
                    subject: input.subject,
                    body: input.body,
                    reply_to: input.reply_to,
                }),
            )
            .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrCreate {
    connection: Option<String>,
    repo: String,
    base: String,
    head: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    draft: bool,
}

async fn pr_create(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (who, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let input: PrCreate = match read(req).await {
        Ok(input) => input,
        Err(refusal) => return refusal,
    };
    answer(
        service
            .request(
                &who.caller(),
                input.connection.as_deref(),
                Write::Pr(PrDraft {
                    repo: input.repo,
                    base: input.base,
                    head: input.head,
                    title: input.title,
                    body: input.body,
                    draft: input.draft,
                }),
            )
            .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approver {
    key: String,
}

async fn approver(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (who, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let input: Approver = match read(req).await {
        Ok(input) => input,
        Err(refusal) => return refusal,
    };
    answer(service.register_approver(&who.device, &input.key).map(|_| json!({"ok":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approve {
    digest: String,
}

async fn approve(
    State(gate): State<Gate>,
    AxPath(id): AxPath<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let (who, service) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let key = req
        .headers()
        .get("x-kasa-approver")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let input: Approve = match read(req).await {
        Ok(input) => input,
        Err(refusal) => return refusal,
    };
    answer(service.approve(&who.caller(), &key, &id, &input.digest).await)
}

async fn reject(
    State(gate): State<Gate>,
    AxPath(id): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let (who, service) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    answer(service.reject(&who.caller(), &id).await)
}

#[cfg(test)]
#[path = "gateway_connections/tests.rs"]
mod tests;
