use super::*;
use crate::oauth_accounts::Provider;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

static ATTEMPT: Mutex<Option<Attempt>> = Mutex::new(None);

#[derive(Clone)]
struct Attempt {
    gateway: String,
    epoch: u64,
    previous: Option<DeviceCred>,
    request: Value,
    link: bool,
    loopback: Option<Arc<Loopback>>,
    /// Gateway capability for an unlinked sign-in waiting for the person's account choice.
    ticket: Option<String>,
    /// Connects work permissions to the signed-in account instead of signing in.
    connect: bool,
}

/// RFC 8252 loopback receiver. The gateway sends the browser here with a one-time code that only
/// this process can redeem, because only it holds the PKCE verifier.
struct Loopback {
    verifier: String,
    redirect_uri: String,
    state: String,
    result: Mutex<Option<Result<String, &'static str>>>,
    stop: AtomicBool,
}

impl Loopback {
    fn bind() -> anyhow::Result<(Arc<Self>, std::net::TcpListener)> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        Ok((
            Arc::new(Self {
                verifier: crate::oauth_accounts::secret(),
                redirect_uri: format!("http://127.0.0.1:{port}/oauth/callback"),
                state: crate::oauth_accounts::secret(),
                result: Mutex::new(None),
                stop: AtomicBool::new(false),
            }),
            listener,
        ))
    }

    fn serve(self: Arc<Self>, listener: std::net::TcpListener) {
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + crate::oauth_accounts::TTL;
        while !self.stop.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    if self.answer(stream) {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    }

    /// True once the attempt's own callback arrived; other visits get a 404 and keep it waiting.
    fn answer(&self, mut stream: std::net::TcpStream) -> bool {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut request = Vec::new();
        let mut chunk = [0; 1024];
        while request.len() < 8192 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&chunk[..n]),
            }
        }
        let line = String::from_utf8_lossy(&request);
        let mut words = line.split_whitespace();
        let received = (words.next() == Some("GET"))
            .then(|| words.next())
            .flatten()
            .and_then(|target| self.callback(target));
        let (status, body) = match &received {
            Some(Ok(_)) => (
                "200 OK",
                "KASA 로그인을 받았어요. 이 탭을 닫고 KASA 로 돌아가세요.",
            ),
            Some(Err(_)) => (
                "200 OK",
                "KASA 로그인을 마치지 못했어요. KASA 에서 다시 시도해 주세요.",
            ),
            None => ("404 Not Found", "Not found"),
        };
        let body =
            format!("<!doctype html><meta charset=\"utf-8\"><title>KASA</title><p>{body}</p>");
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let Some(received) = received else {
            return false;
        };
        if let Ok(mut result) = self.result.lock() {
            *result = Some(received);
        }
        true
    }

    fn callback(&self, target: &str) -> Option<Result<String, &'static str>> {
        if !target.starts_with('/') {
            return None;
        }
        let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
        if url.path() != "/oauth/callback" {
            return None;
        }
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        // A visit without this attempt's state is someone else's page and must not end the attempt.
        if pairs.get("state") != Some(&self.state) {
            return None;
        }
        if let Some(code) = pairs.get("code").filter(|code| opaque(code)) {
            return Some(Ok(code.clone()));
        }
        Some(Err(match pairs.get("error").map(String::as_str) {
            Some("access_denied") => "cancelled",
            _ => "oauth_unavailable",
        }))
    }
}

/// What the gateway supports. Older gateways keep the typed-code flow and create an account for
/// an unlinked identity without asking.
async fn capabilities(gateway: &str) -> Value {
    let Ok(client) = client() else {
        return Value::Null;
    };
    let Ok(response) = client
        .get(format!("{gateway}/relay/oauth/providers"))
        .send()
        .await
    else {
        return Value::Null;
    };
    response.json::<Value>().await.unwrap_or(Value::Null)
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
    // Whether linking GitHub from settings also brings its work permissions. Google links sign-in only.
    let connect = json!({"github":value["redirect_login"] == true && value["connect"]["github"] == true});
    Ok(json!({"state":state,"providers":providers,"connect":connect}))
}

pub(super) async fn start(params: &Value) -> anyhow::Result<Value> {
    anyhow::ensure!(sync_environment_allowed(), "oauth_disabled_in_isolated_run");
    let provider: Provider = serde_json::from_value(params["provider"].clone())
        .map_err(|_| anyhow::anyhow!("bad_provider"))?;
    let mut connect: Vec<String> = params["connect"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|feature| feature.as_str())
        .filter(|feature| *feature == "github.pr")
        .map(str::to_string)
        .collect();
    let link = params["link"].as_bool().unwrap_or(false) || !connect.is_empty();
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
    let mut body = json!({
        "provider":provider,"kind":"desktop","machine_id":machine,"label":crate::mobile::machine_name(),"link":link,
    });
    let capabilities = capabilities(&gateway).await;
    if !link && capabilities["choose_account"] == true {
        body["choose"] = json!(true);
    }
    // Linking GitHub from settings also brings its pull-request permission in the same consent, when
    // the gateway can hold it; an older or unprepared gateway links the sign-in only. Google never
    // asks for more than identity, whatever the gateway answers — mail scopes warn of an unverified app.
    if link
        && connect.is_empty()
        && params["work"] == true
        && provider == Provider::Github
        && capabilities["redirect_login"] == true
        && capabilities["connect"]["github"] == true
    {
        connect = vec!["github.pr".into()];
    }
    if !connect.is_empty() {
        // Provider tokens are only handed over through the PKCE redirect, never the typed code.
        anyhow::ensure!(capabilities["redirect_login"] == true, "update_required");
        anyhow::ensure!(
            capabilities["connect"][provider.name()] == true,
            "setup_required"
        );
        body["connect"] = json!(connect);
    }
    let loopback = if capabilities["redirect_login"] == true {
        let (loopback, listener) = Loopback::bind()?;
        body["code_challenge"] = json!(crate::oauth_accounts::pkce_challenge(&loopback.verifier));
        body["code_challenge_method"] = json!("S256");
        body["redirect_uri"] = json!(loopback.redirect_uri);
        body["state"] = json!(loopback.state);
        Some((loopback, listener))
    } else {
        None
    };
    let mut request = client()?
        .post(format!("{gateway}/relay/oauth/start"))
        .json(&body);
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
    anyhow::ensure!(
        authorization_url == format!("{gateway}/relay/oauth/authorize/{request_id}"),
        "invalid_oauth_response"
    );
    let (request, user_code, listener, loopback) = match loopback {
        Some((loopback, listener)) => (
            json!({"request_id":request_id}),
            None,
            Some(listener),
            Some(loopback),
        ),
        None => {
            let poll_token = value["poll_token"]
                .as_str()
                .filter(|id| opaque(id))
                .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
            let user_code = value["user_code"]
                .as_str()
                .filter(|code| verification_code(code))
                .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
            (
                json!({"provider":provider,"kind":"desktop","machine_id":machine,"request_id":request_id,"poll_token":poll_token}),
                Some(user_code),
                None,
                None,
            )
        }
    };
    let attempt = Attempt {
        gateway,
        epoch,
        previous,
        link,
        request,
        loopback: loopback.clone(),
        ticket: None,
        connect: !connect.is_empty(),
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
        let mut active = ATTEMPT
            .lock()
            .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
        if let Some(old) = active.as_ref().and_then(|old| old.loopback.as_ref()) {
            old.stop.store(true, Ordering::Release);
        }
        *active = Some(attempt);
    }
    if let (Some(loopback), Some(listener)) = (loopback, listener) {
        std::thread::spawn(move || loopback.serve(listener));
    }
    // The UI only needs the one-use browser launch URL, never the poll capability or verifier.
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
        Some("bad_credentials") => "bad_credentials",
        Some("signup_disabled") => "signup_disabled",
        Some("account_disabled") => "account_disabled",
        Some("expired" | "link_expired" | "invalid_grant") => "expired",
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

/// The attempt named by `flow_id`, if this device's credential and gateway have not changed since.
fn active(params: &Value) -> anyhow::Result<Attempt> {
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
    Ok(attempt)
}

pub(super) async fn poll(params: &Value) -> anyhow::Result<Value> {
    let attempt = active(params)?;
    if attempt.ticket.is_some() {
        return Ok(json!({"ok":true,"status":"choose"}));
    }
    let request = match &attempt.loopback {
        Some(loopback) => {
            let received = loopback
                .result
                .lock()
                .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?
                .take();
            let code = match received {
                None => return Ok(json!({"ok":true,"status":"pending"})),
                Some(Ok(code)) => code,
                Some(Err(error)) => {
                    forget(&attempt);
                    anyhow::bail!("{error}");
                }
            };
            client()?
                .post(format!("{}/relay/oauth/token", attempt.gateway))
                .json(&json!({"code":code,"code_verifier":loopback.verifier,"redirect_uri":loopback.redirect_uri}))
        }
        None => client()?
            .post(format!("{}/relay/oauth/poll", attempt.gateway))
            .json(&attempt.request),
    };
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    if attempt.loopback.is_some() && !status.is_success() {
        // The code is spent either way; a retry needs a new browser login.
        forget(&attempt);
    }
    anyhow::ensure!(status.is_success(), "{}", safe_code(&value));
    if value["status"] == "pending" {
        return Ok(json!({"ok":true,"status":"pending"}));
    }
    settle(attempt, value).await
}

/// The person's answer to an unlinked sign-in: a new account, or an existing one by its password.
/// A wrong password keeps the choice open; anything else ends the attempt.
pub(super) async fn choose(params: &Value, claim: bool) -> anyhow::Result<Value> {
    let attempt = active(params)?;
    let ticket = attempt
        .ticket
        .clone()
        .ok_or_else(|| anyhow::anyhow!("expired"))?;
    let (path, body) = if claim {
        let account = params["account"].as_str().unwrap_or("").trim();
        let password = params["password"].as_str().unwrap_or("");
        anyhow::ensure!(
            !account.is_empty() && !password.is_empty(),
            "bad_credentials"
        );
        (
            "claim",
            json!({"ticket":ticket,"account":account,"password":password}),
        )
    } else {
        ("signup", json!({"ticket":ticket}))
    };
    let response = client()?
        .post(format!("{}/relay/oauth/{path}", attempt.gateway))
        .json(&body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
    if !status.is_success() {
        let code = safe_code(&value);
        if !matches!(code, "bad_credentials" | "rate_limited") {
            forget(&attempt);
        }
        anyhow::bail!("{code}");
    }
    anyhow::ensure!(value["status"] == "complete", "invalid_oauth_response");
    settle(attempt, value).await
}

async fn settle(attempt: Attempt, value: Value) -> anyhow::Result<Value> {
    if attempt.connect {
        anyhow::ensure!(
            value["status"] == "connected"
                && attempt
                    .previous
                    .as_ref()
                    .is_some_and(|c| value["account"] == c.account),
            "invalid_oauth_response"
        );
        forget(&attempt);
        let link_error = match value["link_error"].as_str() {
            None => Value::Null,
            Some("already_linked") => json!("already_linked"),
            Some(_) => json!("link_failed"),
        };
        // Only the gateway's own GitHub App page is opened from here.
        let install_url = value["install_url"]
            .as_str()
            .filter(|url| url.starts_with("https://github.com/apps/") && url.len() < 200);
        return Ok(json!({"ok":true,"status":"connected","connection":value["connection"],
            "linked":value["linked"] == true,"link_error":link_error,"install_url":install_url}));
    }
    if !attempt.link && value["status"] == "choose" {
        let ticket = value["ticket"]
            .as_str()
            .filter(|ticket| opaque(ticket))
            .ok_or_else(|| anyhow::anyhow!("invalid_oauth_response"))?;
        let mut active = ATTEMPT
            .lock()
            .map_err(|_| anyhow::anyhow!("oauth_unavailable"))?;
        let held = active
            .as_mut()
            .filter(|active| active.request["request_id"] == attempt.request["request_id"])
            .ok_or_else(|| anyhow::anyhow!("account_changed"))?;
        held.ticket = Some(ticket.into());
        let provider = match value["provider"].as_str() {
            Some("google") => "google",
            Some("github") => "github",
            _ => "",
        };
        // The ticket stays in this process; the UI only learns who signed in and what it may do.
        return Ok(json!({"ok":true,"status":"choose","provider":provider,
            "display":super::display_label(value["display"].as_str()),
            "signup_enabled":value["signup_enabled"] == true}));
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

fn forget(attempt: &Attempt) {
    if let Ok(mut active) = ATTEMPT.lock() {
        if active
            .as_ref()
            .is_some_and(|active| active.request["request_id"] == attempt.request["request_id"])
        {
            *active = None;
        }
    }
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
        if let Some(loopback) = &attempt.loopback {
            // A redirect flow has no server-side cancel capability; it expires unredeemed.
            loopback.stop.store(true, Ordering::Release);
        } else {
            let _ = client()?
                .post(format!("{}/relay/oauth/cancel", attempt.gateway))
                .json(&attempt.request)
                .send()
                .await;
        }
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
            loopback: None,
            ticket: None,
            connect: false,
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
    fn loopback_takes_only_its_own_callback() {
        let (loopback, listener) = Loopback::bind().unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(
            loopback.redirect_uri,
            format!("http://127.0.0.1:{port}/oauth/callback")
        );
        assert!(crate::oauth_accounts::valid_redirect_uri(
            &loopback.redirect_uri
        ));
        let server = std::thread::spawn({
            let loopback = loopback.clone();
            move || loopback.serve(listener)
        });
        let get = |target: String| {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let code = crate::oauth_accounts::secret();
        assert!(
            get(format!("/oauth/callback?code={code}&state=forged")).starts_with("HTTP/1.1 404")
        );
        assert!(get("/favicon.ico".into()).starts_with("HTTP/1.1 404"));
        assert!(loopback.result.lock().unwrap().is_none());
        let page = get(format!(
            "/oauth/callback?code={code}&state={}",
            loopback.state
        ));
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("no-referrer"));
        server.join().unwrap();
        assert_eq!(loopback.result.lock().unwrap().clone(), Some(Ok(code)));
        assert_eq!(
            loopback.callback(&format!(
                "/oauth/callback?error=access_denied&state={}",
                loopback.state
            )),
            Some(Err("cancelled"))
        );
        assert_eq!(
            loopback.callback(&format!(
                "/oauth/callback?code=<script>&state={}",
                loopback.state
            )),
            Some(Err("oauth_unavailable"))
        );
        assert_eq!(
            loopback.callback("http://attacker.example/oauth/callback"),
            None
        );
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
