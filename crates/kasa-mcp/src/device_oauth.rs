use super::*;
use crate::oauth_accounts::Provider;

static ATTEMPT: Mutex<Option<Attempt>> = Mutex::new(None);

#[derive(Clone)]
struct Attempt {
    gateway: String,
    epoch: u64,
    previous: Option<DeviceCred>,
    request: Value,
    link: bool,
}

fn gateway() -> anyhow::Result<String> {
    let gateway = crate::mobile::gateway().ok_or_else(|| anyhow::anyhow!("gateway_off"))?;
    anyhow::ensure!(
        crate::oauth_accounts::valid_origin(&gateway),
        "https_gateway_required"
    );
    Ok(gateway.trim_end_matches('/').into())
}

pub(super) async fn providers() -> anyhow::Result<Value> {
    if !sync_environment_allowed() {
        return Ok(
            json!({"state":"isolated","providers":[{"id":"google","enabled":false},{"id":"github","enabled":false}]}),
        );
    }
    let gateway = gateway()?;
    let response = client()?
        .get(format!("{gateway}/relay/oauth/providers"))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    anyhow::ensure!(response.status().is_success(), "oauth_unavailable");
    let value: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    let providers = ["google", "github"].map(|id| json!({"id":id,"enabled":value["providers"].as_array()
        .is_some_and(|providers| providers.iter().any(|provider| provider["id"] == id && provider["enabled"] == true))}));
    let state = if providers.iter().any(|provider| provider["enabled"] == true) {
        "ready"
    } else {
        "setup_required"
    };
    Ok(json!({"state":state,"providers":providers}))
}

pub(super) async fn start(params: &Value) -> anyhow::Result<Value> {
    anyhow::ensure!(sync_environment_allowed(), "oauth_disabled_in_isolated_run");
    let provider: Provider = serde_json::from_value(params["provider"].clone())
        .map_err(|_| anyhow::anyhow!("bad_provider"))?;
    let link = params["link"].as_bool().unwrap_or(false);
    let gateway = gateway()?;
    let machine =
        crate::mobile::machine_identity().ok_or_else(|| anyhow::anyhow!("machine_id_required"))?;
    let (epoch, previous) = {
        let _guard = CREDENTIALS
            .lock()
            .map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        let previous = current();
        if link {
            anyhow::ensure!(
                previous
                    .as_ref()
                    .is_some_and(|cred| cred.relay.trim_end_matches('/') == gateway),
                "unauthorized"
            );
        }
        (EPOCH.fetch_add(1, Ordering::AcqRel) + 1, previous)
    };
    let mut request = client()?.post(format!("{gateway}/relay/oauth/start")).json(&json!({
        "provider":provider,"kind":"desktop","machine_id":machine,"label":crate::mobile::machine_name(),"link":link,
    }));
    if link {
        request = request.bearer_auth(&previous.as_ref().unwrap().token);
    }
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    anyhow::ensure!(status.is_success(), "{}", safe_code(&value));
    let authorization_url = value["authorization_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
    let request_id = value["request_id"]
        .as_str()
        .filter(|id| opaque(id))
        .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
    let poll_token = value["poll_token"]
        .as_str()
        .filter(|id| opaque(id))
        .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
    let user_code = value["user_code"]
        .as_str()
        .filter(|code| verification_code(code))
        .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
    anyhow::ensure!(
        authorization_url == format!("{gateway}/relay/oauth/authorize/{request_id}"),
        "invalid_oauth_response"
    );
    let attempt = Attempt {
        gateway,
        epoch,
        previous,
        link,
        request: json!({"provider":provider,"kind":"desktop","machine_id":machine,"request_id":request_id,"poll_token":poll_token}),
    };
    {
        let _guard = CREDENTIALS
            .lock()
            .map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        anyhow::ensure!(
            attempt_current(
                &attempt,
                current().as_ref(),
                crate::mobile::gateway().as_deref(),
                EPOCH.load(Ordering::Acquire)
            ),
            "account_changed"
        );
        *ATTEMPT
            .lock()
            .map_err(|_| anyhow::anyhow!("oauth_unavailable"))? = Some(attempt);
    }
    // The UI only needs the one-use browser launch URL, never the poll capability.
    Ok(
        json!({"ok":true,"authorization_url":authorization_url,"flow_id":request_id,"user_code":user_code}),
    )
}

fn opaque(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

fn verification_code(value: &str) -> bool {
    value.len() == 9
        && value.as_bytes()[4] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_hexdigit())
}

fn safe_code(value: &Value) -> &'static str {
    match value["error"].as_str() {
        Some("setup_required") => "setup_required",
        Some("account_not_linked") => "account_not_linked",
        Some("already_linked") => "already_linked",
        Some("cancelled") => "cancelled",
        Some("expired" | "link_expired") => "expired",
        Some("rate_limited") => "rate_limited",
        _ => "oauth_unavailable",
    }
}

fn attempt_current(
    attempt: &Attempt,
    credential: Option<&DeviceCred>,
    gateway: Option<&str>,
    epoch: u64,
) -> bool {
    attempt.epoch == epoch
        && attempt.previous.as_ref() == credential
        && gateway.is_some_and(|gateway| gateway.trim_end_matches('/') == attempt.gateway)
}

pub(super) async fn poll(params: &Value) -> anyhow::Result<Value> {
    let attempt = ATTEMPT
        .lock()
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?
        .clone()
        .ok_or_else(|| anyhow::anyhow!("cancelled"))?;
    anyhow::ensure!(
        params["flow_id"]
            .as_str()
            .is_some_and(|id| attempt.request["request_id"] == id),
        "account_changed"
    );
    {
        let _guard = CREDENTIALS
            .lock()
            .map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        anyhow::ensure!(
            attempt_current(
                &attempt,
                current().as_ref(),
                crate::mobile::gateway().as_deref(),
                EPOCH.load(Ordering::Acquire)
            ),
            "account_changed"
        );
    }
    let response = client()?
        .post(format!("{}/relay/oauth/poll", attempt.gateway))
        .json(&attempt.request)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    anyhow::ensure!(status.is_success(), "{}", safe_code(&value));
    if value["status"] == "pending" {
        return Ok(json!({"ok":true,"status":"pending"}));
    }
    let credential = if attempt.link {
        anyhow::ensure!(
            value["status"] == "linked"
                && attempt
                    .previous
                    .as_ref()
                    .is_some_and(|c| value["account"] == c.account),
            "invalid_oauth_response"
        );
        None
    } else {
        anyhow::ensure!(value["status"] == "complete", "invalid_oauth_response");
        let account = value["account"]
            .as_str()
            .filter(|name| crate::relay_auth::valid_account_name(name))
            .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
        let device_id = value["device_id"]
            .as_str()
            .filter(|id| id.starts_with("dev_") && id.len() == 20)
            .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
        let token = value["token"]
            .as_str()
            .filter(|token| token.strip_prefix("kdt_").is_some_and(opaque))
            .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
        Some(DeviceCred {
            relay: attempt.gateway.clone(),
            account: account.into(),
            device_id: device_id.into(),
            token: token.into(),
            display_name: super::display_label(value["display_name"].as_str()),
        })
    };
    let saved = {
        let _guard = CREDENTIALS
            .lock()
            .map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        if attempt_current(
            &attempt,
            current().as_ref(),
            crate::mobile::gateway().as_deref(),
            EPOCH.load(Ordering::Acquire),
        ) {
            if let Some(credential) = &credential {
                save(credential)?;
                if let Ok(mut rejected) = REJECTED.lock() {
                    *rejected = None;
                }
                crate::agent_accounts::clear_cache();
                crate::account_sync::credentials_changed(
                    attempt.previous.as_ref(),
                    Some(credential),
                );
            }
            *ATTEMPT
                .lock()
                .map_err(|_| anyhow::anyhow!("oauth_unavailable"))? = None;
            true
        } else {
            false
        }
    };
    if !saved {
        if let Some(credential) = credential {
            let _ = client()?
                .post(format!("{}/relay/logout", credential.relay))
                .bearer_auth(&credential.token)
                .send()
                .await;
        }
        anyhow::bail!("account_changed");
    }
    if credential.is_some() {
        crate::uplink::poke();
        crate::account_sync::spawn();
        crate::account_sync::poke();
    }
    Ok(json!({"ok":true,"status":if attempt.link { "linked" } else { "complete" }}))
}

pub(super) async fn cancel(params: &Value) -> anyhow::Result<Value> {
    let attempt = {
        let _guard = CREDENTIALS
            .lock()
            .map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        let mut active = ATTEMPT
            .lock()
            .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
        let attempt = if active.as_ref().is_some_and(|attempt| {
            params["flow_id"]
                .as_str()
                .is_some_and(|id| attempt.request["request_id"] == id)
        }) {
            active.take()
        } else {
            None
        };
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.epoch == EPOCH.load(Ordering::Acquire))
        {
            EPOCH.fetch_add(1, Ordering::AcqRel);
        }
        attempt
    };
    if let Some(attempt) = attempt {
        let _ = client()?
            .post(format!("{}/relay/oauth/cancel", attempt.gateway))
            .json(&attempt.request)
            .send()
            .await;
    }
    Ok(json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_oauth_response_never_overwrites_new_account_or_gateway() {
        let credential = DeviceCred {
            relay: "https://relay.example".into(),
            account: "one".into(),
            device_id: "dev_123".into(),
            token: "private".into(),
            display_name: None,
        };
        let attempt = Attempt {
            gateway: credential.relay.clone(),
            epoch: 4,
            previous: Some(credential.clone()),
            request: Value::Null,
            link: false,
        };
        assert!(attempt_current(
            &attempt,
            Some(&credential),
            Some(&credential.relay),
            4
        ));
        assert!(!attempt_current(
            &attempt,
            Some(&credential),
            Some(&credential.relay),
            5
        ));
        assert!(!attempt_current(&attempt, None, Some(&credential.relay), 4));
        assert!(!attempt_current(
            &attempt,
            Some(&credential),
            Some("https://other.example"),
            4
        ));
        let changed = DeviceCred {
            token: "new private".into(),
            ..credential.clone()
        };
        assert!(!attempt_current(
            &attempt,
            Some(&changed),
            Some(&credential.relay),
            4
        ));
    }
    #[test]
    fn provider_errors_and_capabilities_are_not_reflected() {
        assert_eq!(
            safe_code(&json!({"error":"token=provider-secret"})),
            "oauth_unavailable"
        );
        assert!(!opaque("https://attacker.example"));
        assert!(!opaque("../path"));
        assert!(verification_code("ABCD-1234"));
        assert!(!verification_code("<script>"));
        assert!(!verification_code("ABCD+1234"));
        assert!(!verification_code("ABCD-12345"));
    }
}
