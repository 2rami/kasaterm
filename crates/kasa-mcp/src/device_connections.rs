use super::*;
use std::sync::OnceLock;

/// Approval key of this app process. Only the settings screen's approve path sends it; the socket
/// (`relay.account`) and `kasa-device` never can, so an agent driving the CLI cannot approve
/// its own write even on the same device.
static APPROVER: OnceLock<String> = OnceLock::new();
/// (gateway, device) the key was last registered for; a gateway restart forgets keys.
static REGISTERED: Mutex<Option<(String, String)>> = Mutex::new(None);

fn signed_in() -> anyhow::Result<DeviceCred> {
    anyhow::ensure!(sync_environment_allowed(), "isolated_run");
    current().ok_or_else(|| anyhow::anyhow!("signed_out"))
}

fn segment(value: &str) -> anyhow::Result<&str> {
    anyhow::ensure!(
        (1..=64).contains(&value.len())
            && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid_request"
    );
    Ok(value)
}

/// Body fields a request may carry, copied by name so nothing else reaches the gateway.
fn pick(params: &Value, names: &[&str]) -> Value {
    let mut body = serde_json::Map::new();
    for name in names {
        if let Some(value) = params.get(*name).filter(|value| !value.is_null()) {
            body.insert((*name).into(), value.clone());
        }
    }
    Value::Object(body)
}

async fn call(
    credential: &DeviceCred,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
    approver: Option<&str>,
) -> anyhow::Result<Value> {
    let mut request = client()?
        .request(method, format!("{}/relay/connections{path}", credential.relay))
        .bearer_auth(&credential.token);
    if let Some(key) = approver {
        request = request.header("x-kasa-approver", key);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("gateway_unreachable"))?;
    let status = response.status();
    if status.as_u16() == 404 && path.is_empty() {
        anyhow::bail!("update_required");
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("invalid_response"))?;
    if status.as_u16() == 401 && value["error"] == "unauthorized" {
        reject_token(&credential.token);
    }
    if !status.is_success() {
        let code = value["error"].as_str().unwrap_or("request_failed");
        match value["detail"].as_str() {
            Some(detail) => anyhow::bail!("{code}: {detail}"),
            None => anyhow::bail!("{code}"),
        }
    }
    Ok(value)
}

/// Operations any local caller (settings screen, `kasaterm-cli`, `kasa-device`) may run. Writes
/// only queue; approval is [`approve`].
pub(super) async fn handle(op: &str, params: &Value) -> anyhow::Result<Value> {
    let credential = signed_in()?;
    let post = reqwest::Method::POST;
    match op {
        "connections" => call(&credential, reqwest::Method::GET, "", None, None).await,
        "connections_audit" => {
            let limit = params["limit"].as_u64().unwrap_or(30).min(200);
            call(&credential, reqwest::Method::GET, &format!("/audit?limit={limit}"), None, None).await
        }
        "disconnect" => {
            let id = segment(params["id"].as_str().unwrap_or(""))?;
            call(&credential, reqwest::Method::DELETE, &format!("/{id}"), None, None).await
        }
        "mail_list" => {
            let body = pick(params, &["connection", "query", "max"]);
            call(&credential, post, "/mail/list", Some(body), None).await
        }
        "mail_read" => {
            let body = pick(params, &["connection", "id"]);
            call(&credential, post, "/mail/read", Some(body), None).await
        }
        "mail_send" => {
            let body = pick(params, &["connection", "to", "cc", "subject", "body", "reply_to"]);
            call(&credential, post, "/mail/send", Some(body), None).await
        }
        "pr_create" => {
            let body = pick(params, &["connection", "repo", "base", "head", "title", "body", "draft"]);
            call(&credential, post, "/pr/create", Some(body), None).await
        }
        "reject" => {
            let id = segment(params["id"].as_str().unwrap_or(""))?;
            call(&credential, post, &format!("/pending/{id}/reject"), None, None).await
        }
        other => anyhow::bail!("모르는 동작이에요: {other}"),
    }
}

async fn register(credential: &DeviceCred, key: &str) -> anyhow::Result<()> {
    call(credential, reqwest::Method::POST, "/approver", Some(json!({"key":key})), None).await?;
    if let Ok(mut registered) = REGISTERED.lock() {
        *registered = Some((credential.relay.clone(), credential.device_id.clone()));
    }
    Ok(())
}

async fn approve_async(id: &str, digest: &str) -> anyhow::Result<Value> {
    let credential = signed_in()?;
    let id = segment(id)?;
    let key = APPROVER.get_or_init(crate::oauth_accounts::secret);
    let current = Some((credential.relay.clone(), credential.device_id.clone()));
    if REGISTERED.lock().map(|registered| *registered != current).unwrap_or(true) {
        register(&credential, key).await?;
    }
    let body = json!({"digest":digest});
    let path = format!("/pending/{id}/approve");
    match call(&credential, reqwest::Method::POST, &path, Some(body.clone()), Some(key)).await {
        Err(error) if error.to_string() == "approver_required" => {
            register(&credential, key).await?;
            call(&credential, reqwest::Method::POST, &path, Some(body), Some(key)).await
        }
        result => result,
    }
}

/// Runs a pending write the person approved on the settings screen. `digest` is the one that
/// screen showed. Not reachable through [`handle`] on purpose.
pub fn approve(id: &str, digest: &str) -> anyhow::Result<Value> {
    let (id, digest) = (id.to_string(), digest.to_string());
    std::thread::spawn(move || -> anyhow::Result<Value> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(approve_async(&id, &digest))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("approve thread panicked"))?
}
