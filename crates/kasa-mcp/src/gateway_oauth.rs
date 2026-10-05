use super::*;
use crate::oauth_accounts::{Device, Link, Native, Poll, Provider, Ready, Token};
use axum::extract::Query;
use serde::Deserialize;
use serde_json::json;

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/oauth/providers", get(providers))
        .route("/relay/oauth/start", axum::routing::post(start))
        .route("/relay/oauth/authorize/{id}", get(authorize).post(confirm))
        .route("/relay/oauth/{provider}/callback", get(callback))
        .route("/relay/oauth/poll", axum::routing::post(poll))
        .route("/relay/oauth/token", axum::routing::post(token))
        .route("/relay/oauth/cancel", axum::routing::post(cancel))
        .route("/relay/oauth/signup", axum::routing::post(signup))
        .route("/relay/oauth/claim", axum::routing::post(claim))
        .layer(axum::middleware::map_response(secure_response))
}

async fn secure_response(mut response: axum::response::Response) -> axum::response::Response {
    // no-referrer turns a navigation form's Origin into null; same-origin keeps CSRF checks usable without cross-origin leakage.
    let referrer_policy = if response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"))
    {
        "same-origin"
    } else {
        "no-referrer"
    };
    for (key, value) in [
        ("cache-control", "no-store"),
        ("pragma", "no-cache"),
        ("referrer-policy", referrer_policy),
        ("x-content-type-options", "nosniff"),
        (
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'; form-action 'self' https://accounts.google.com https://github.com; base-uri 'none'",
        ),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(key),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

async fn body<T: serde::de::DeserializeOwned>(
    req: axum::extract::Request,
) -> Result<T, axum::response::Response> {
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), 8192),
    )
    .await
    .map_err(|_| json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout"))?
    .map_err(|_| json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"))?;
    serde_json::from_slice(&bytes).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad_request"))
}

async fn providers(State(gate): State<Gate>) -> axum::response::Response {
    axum::Json(gate.oauth.config.providers(gate.oauth.storage_ready())).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    provider: Provider,
    kind: String,
    label: String,
    machine_id: String,
    #[serde(default)]
    link: bool,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    /// Ask before an unlinked identity becomes a new account (`choose_account` clients).
    #[serde(default)]
    choose: bool,
    /// Work permissions to connect to the signed-in account instead of a sign-in.
    #[serde(default)]
    connect: Vec<crate::connections::Feature>,
}

/// RFC 8252 + PKCE: an app that names its own redirect URI and S256 challenge skips the typed code.
fn native(input: &Start) -> Result<Option<Native>, &'static str> {
    let (Some(challenge), Some(redirect_uri)) = (&input.code_challenge, &input.redirect_uri) else {
        return if input.code_challenge.is_none()
            && input.redirect_uri.is_none()
            && input.code_challenge_method.is_none()
            && input.state.is_none()
        {
            Ok(None)
        } else {
            Err("invalid_request")
        };
    };
    if input.code_challenge_method.as_deref() != Some("S256")
        || !crate::oauth_accounts::valid_challenge(challenge)
        || input
            .state
            .as_deref()
            .is_some_and(|state| !crate::oauth_accounts::valid_client_state(state))
    {
        return Err("invalid_request");
    }
    if !crate::oauth_accounts::valid_redirect_uri(redirect_uri) {
        return Err("invalid_redirect_uri");
    }
    Ok(Some(Native {
        challenge: challenge.clone(),
        redirect_uri: redirect_uri.clone(),
        state: input.state.clone(),
    }))
}

async fn start(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    if gate.limiter.allow("oauth", &client_ip(&req)).is_err() {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let headers = req.headers().clone();
    let input: Start = match body(req).await {
        Ok(input) => input,
        Err(error) => return error,
    };
    if !matches!(input.kind.as_str(), "desktop" | "phone") || !valid_machine_id(&input.machine_id) {
        return json_err(StatusCode::BAD_REQUEST, "bad_device");
    }
    let native = match native(&input) {
        Ok(native) => native,
        Err(error) => return json_err(StatusCode::BAD_REQUEST, error),
    };
    if input.link && input.choose {
        return json_err(StatusCode::BAD_REQUEST, "invalid_request");
    }
    // Provider tokens reach only an app that proves the redirect with its PKCE verifier, and only
    // for the account whose device asked.
    let connect = if input.connect.is_empty() {
        None
    } else if !input.link
        || native.is_none()
        || input.connect.len() > 3
        || input
            .connect
            .iter()
            .any(|feature| feature.provider() != input.provider)
    {
        return json_err(StatusCode::BAD_REQUEST, "invalid_request");
    } else {
        let mut features = input.connect.clone();
        features.dedup();
        Some(features)
    };
    let link = if input.link {
        let Some((device_id, device)) = gate.device_of(&headers) else {
            return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
        };
        if device.kind != input.kind
            || device
                .machine_id
                .as_deref()
                .is_some_and(|machine| machine != input.machine_id)
            || (device.kind == "desktop" && device.machine_id.is_none())
        {
            return json_err(StatusCode::FORBIDDEN, "device_mismatch");
        }
        Some(Link {
            device_id,
            account: device.account,
            token_hash: device.token_hash,
        })
    } else {
        if headers.contains_key(header::AUTHORIZATION) {
            return json_err(StatusCode::BAD_REQUEST, "explicit_link_required");
        }
        None
    };
    let device = Device {
        provider: input.provider,
        kind: input.kind,
        machine_id: input.machine_id,
    };
    let label = input
        .label
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    match gate.oauth.start(device, label, link, native, input.choose, connect) {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => json_err(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

async fn authorize(
    State(gate): State<Gate>,
    AxPath(id): AxPath<String>,
) -> axum::response::Response {
    match gate.oauth.redirect(&id) {
        Ok(Some((cookie, url))) => {
            return (
                StatusCode::SEE_OTHER,
                [(header::SET_COOKIE, cookie), (header::LOCATION, url)],
            )
                .into_response();
        }
        Ok(None) => {}
        Err(error) => return json_err(StatusCode::BAD_REQUEST, error),
    }
    match gate.oauth.browser(&id) {
        Ok(confirmation) => (
            [(header::SET_COOKIE, confirmation.cookie.clone())],
            axum::response::Html(device_confirmation(&id, &confirmation)),
        )
            .into_response(),
        Err(error) => json_err(StatusCode::BAD_REQUEST, error),
    }
}

fn device_confirmation(
    id: &str,
    confirmation: &crate::oauth_accounts::BrowserConfirmation,
) -> String {
    let destination = confirmation.account.as_deref().map_or_else(
        || "Sign in to KASA".into(),
        |account| {
            format!(
                "Connect a login method to KASA account <strong>{}</strong>",
                html_escape(account)
            )
        },
    );
    // A provider consent page does not identify the requesting KASA device, so every flow verifies it first.
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Verify KASA device</title><body><main><h1>Verify your KASA device</h1><p>{destination} using {}.</p><p>Requested device label: <strong>{}</strong><br>Device ID: <code>{}</code></p><p>Device names are supplied by the requester. Continue only if you started this from your own KASA app. Never use a code or link sent by another person.</p><form method=\"post\" action=\"/relay/oauth/authorize/{}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><label for=\"user_code\">Enter the verification code shown in your KASA app</label><input id=\"user_code\" name=\"user_code\" autocomplete=\"off\" maxlength=\"9\" required><button type=\"submit\">Verify device and continue</button></form></main></body></html>",
        confirmation.provider.name(),
        html_escape(&confirmation.label),
        html_escape(&confirmation.machine_id),
        html_escape(id),
        html_escape(&confirmation.csrf)
    )
}

async fn confirm(
    State(gate): State<Gate>,
    AxPath(id): AxPath<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(gate.oauth.config.origin.as_str())
    {
        return json_err(StatusCode::FORBIDDEN, "invalid_confirmation");
    }
    if req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        != Some("application/x-www-form-urlencoded")
    {
        return json_err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "bad_request");
    }
    let cookies = req
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = match tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), 8192),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return json_err(StatusCode::BAD_REQUEST, "bad_request"),
    };
    let Ok(raw) = std::str::from_utf8(&bytes) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    let mut form = reqwest::Url::parse("https://form.invalid/").unwrap();
    form.set_query(Some(raw));
    let mut fields = HashMap::new();
    for (key, value) in form.query_pairs() {
        if !matches!(key.as_ref(), "csrf" | "user_code")
            || fields
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return json_err(StatusCode::BAD_REQUEST, "bad_request");
        }
    }
    let (Some(csrf), Some(code)) = (fields.get("csrf"), fields.get("user_code")) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    match gate.oauth.confirm(&id, &cookies, csrf, code) {
        Ok(url) => (StatusCode::SEE_OTHER, [(header::LOCATION, url)]).into_response(),
        Err(error) => json_err(StatusCode::BAD_REQUEST, error),
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Deserialize)]
struct Callback {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(gate): State<Gate>,
    AxPath(provider): AxPath<Provider>,
    Query(input): Query<Callback>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if input.state.len() > 128 {
        return json_err(StatusCode::BAD_REQUEST, "invalid_state");
    }
    let cookies = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let (request, exchange) = match gate.oauth.callback(provider, &input.state, cookies) {
        Ok(exchange) => exchange,
        Err(error) => return json_err(StatusCode::BAD_REQUEST, error),
    };
    let result = if input.error.is_some() {
        Err("cancelled")
    } else if let Some(code) = input
        .code
        .filter(|code| !code.is_empty() && code.len() < 8192)
    {
        gate.oauth.exchange(&exchange, &code).await
    } else {
        Err("invalid_code")
    };
    if exchange.admin {
        let result = result.map(|(identity, _)| identity);
        return super::admin::signed_in(&gate, gate.oauth.finish_admin(&request, result));
    }
    let success = result.is_ok();
    if let Some(url) = gate.oauth.finish(&request, result) {
        return (StatusCode::SEE_OTHER, [(header::LOCATION, url)]).into_response();
    }
    let message = if success {
        "KASA sign-in verified. Return to KASA to finish. You can close this tab."
    } else {
        "KASA sign-in was not completed. Return to KASA and try again."
    };
    (
        if success {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        message,
    )
        .into_response()
}

async fn poll(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let input: Poll = match body(req).await {
        Ok(input) => input,
        Err(error) => return error,
    };
    match gate.oauth.poll(&input, false) {
        Ok(None) => axum::Json(json!({"ok":true,"status":"pending"})).into_response(),
        Ok(Some(ready)) if ready.connect.is_some() => connect(&gate, ready).await,
        Ok(Some(ready)) => complete(&gate, ready),
        Err(error) => json_err(StatusCode::BAD_REQUEST, error),
    }
}

async fn token(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    if gate.limiter.allow("oauth", &client_ip(&req)).is_err() {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let input: Token = match body(req).await {
        Ok(input) => input,
        Err(error) => return error,
    };
    match gate.oauth.token(&input) {
        Ok(ready) if ready.connect.is_some() => connect(&gate, ready).await,
        Ok(ready) => complete(&gate, ready),
        Err(error) => json_err(StatusCode::BAD_REQUEST, error),
    }
}

async fn cancel(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let input: Poll = match body(req).await {
        Ok(input) => input,
        Err(error) => return error,
    };
    match gate.oauth.poll(&input, true) {
        Ok(_) => axum::Json(json!({"ok":true,"status":"cancelled"})).into_response(),
        Err(error) => json_err(StatusCode::BAD_REQUEST, error),
    }
}

/// Saves a connect flow's provider tokens to the account of the device that started it, and links
/// the same identity as a sign-in to that account — one consent connects both. An identity that
/// already signs in to another account stays there; the work tokens still land here.
async fn connect(gate: &Gate, ready: Ready) -> axum::response::Response {
    let (Some(link), Some(features), Some(grant)) = (&ready.link, &ready.connect, ready.grant)
    else {
        return json_err(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let Some(connections) = &gate.connections else {
        return json_err(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable");
    };
    let (label, linked) = {
        // Held through identity persistence so revocation cannot race the link, as in `complete`.
        let devices = gate.devices.lock().unwrap();
        let label = match devices.get(&link.device_id).filter(|device| {
            device.revoked_at.is_none()
                && device.account == link.account
                && device.token_hash == link.token_hash
        }) {
            Some(device) if gate.account_active(&link.account) => device.label.clone(),
            _ => return json_err(StatusCode::UNAUTHORIZED, "link_expired"),
        };
        let linked = gate
            .oauth
            .resolve(&ready.identity, Some(&link.account), |name| {
                gate.accounts.exists(name)
            })
            .map(|_| ());
        (label, linked)
    };
    let caller = crate::connections::Caller {
        account: &link.account,
        device: &link.device_id,
        label: &label,
    };
    let installed = match grant.provider {
        Provider::Github => connections.github_installed(&grant.access_token).await,
        Provider::Google => None,
    };
    match connections.store(&caller, grant, features).await {
        Ok(summary) => {
            let mut value = json!({"ok":true,"status":"connected","account":link.account,"connection":summary,
                "linked":linked.is_ok()});
            if let Err(error) = linked {
                value["link_error"] = json!(error);
            }
            if let Some(installed) = installed {
                value["installed"] = json!(installed);
                if !installed {
                    value["install_url"] = json!(connections.github_install_url());
                }
            }
            axum::Json(value).into_response()
        }
        Err(error) => json_err(
            StatusCode::from_u16(error.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            error.code(),
        ),
    }
}

fn complete(gate: &Gate, ready: Ready) -> axum::response::Response {
    if let Some(link) = &ready.link {
        // Hold the device lock through identity persistence so revocation cannot race a link.
        let devices = gate.devices.lock().unwrap();
        if !devices.get(&link.device_id).is_some_and(|device| {
            device.revoked_at.is_none()
                && device.account == link.account
                && device.token_hash == link.token_hash
        }) || !gate.account_active(&link.account)
        {
            return json_err(StatusCode::UNAUTHORIZED, "link_expired");
        }
        return match gate
            .oauth
            .resolve(&ready.identity, Some(&link.account), |name| {
                gate.accounts.exists(name)
            }) {
            Ok(account) => {
                axum::Json(json!({"ok":true,"status":"linked","account":account})).into_response()
            }
            Err(error) => json_err(StatusCode::CONFLICT, error),
        };
    }
    if ready.choose && gate.oauth.lookup(&ready.identity).is_none() {
        let (provider, display) = (ready.identity.provider, ready.identity.display.clone());
        return match gate.oauth.hold(ready) {
            Ok(ticket) => axum::Json(json!({
                "ok":true,"status":"choose","ticket":ticket,"provider":provider.name(),
                "display":display,"signup_enabled":gate.oauth.config.signup_enabled(),
                "expires_in":crate::oauth_accounts::TTL.as_secs(),
            }))
            .into_response(),
            Err(error) => json_err(StatusCode::SERVICE_UNAVAILABLE, error),
        };
    }
    let account = match gate
        .oauth
        .resolve(&ready.identity, None, |name| gate.accounts.exists(name))
    {
        Ok(account) => account,
        Err(error) => return json_err(StatusCode::FORBIDDEN, error),
    };
    sign_in(gate, account, ready)
}

/// Issues a device credential for an account the provider identity now belongs to.
fn sign_in(gate: &Gate, account: String, ready: Ready) -> axum::response::Response {
    if !gate.account_active(&account) {
        return json_err(StatusCode::UNAUTHORIZED, "account_disabled");
    }
    let token = crate::relay_auth::new_token();
    let device_id = crate::relay_auth::new_device_id();
    let machine_id = Some(ready.device.machine_id);
    let kind = ready.device.kind;
    let now = now_secs();
    gate.devices.lock().unwrap().insert(
        device_id.clone(),
        DeviceRec {
            token_hash: crate::relay_auth::token_hash(&token),
            account: account.clone(),
            kind: kind.clone(),
            machine_id: machine_id.clone(),
            label: ready.label,
            created: now,
            last_seen: now,
            revoked_at: None,
        },
    );
    if gate.persist_result().is_err() {
        gate.devices.lock().unwrap().remove(&device_id);
        return json_err(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable");
    }
    // The requester may lose the response or fail to save it; existing credentials remain explicitly revocable.
    let display_name = gate.oauth.display_name(&account);
    axum::Json(json!({"ok":true,"status":"complete","account":account,"display_name":display_name,"device_id":device_id,"token":token})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Signup {
    ticket: String,
}

/// The person chose a new account for a held, unlinked sign-in.
async fn signup(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    if gate.limiter.allow("oauth", &client_ip(&req)).is_err() {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let input: Signup = match body(req).await {
        Ok(input) => input,
        Err(error) => return error,
    };
    let ready = match gate.oauth.held(&input.ticket) {
        Ok(ready) => ready,
        Err(error) => return json_err(StatusCode::BAD_REQUEST, error),
    };
    if !gate.oauth.config.signup_enabled() {
        return json_err(StatusCode::FORBIDDEN, "signup_disabled");
    }
    if !gate.oauth.spend(&input.ticket) {
        return json_err(StatusCode::BAD_REQUEST, "expired");
    }
    match gate
        .oauth
        .resolve(&ready.identity, None, |name| gate.accounts.exists(name))
    {
        Ok(account) => sign_in(&gate, account, ready),
        Err(error) => json_err(StatusCode::FORBIDDEN, error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    ticket: String,
    account: String,
    password: String,
}

/// The person named an existing password account for a held sign-in. The password is asked once;
/// afterwards the provider alone signs in to that account. It shares the password login's lockout.
async fn claim(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let ip = client_ip(&req);
    let input: Claim = match body(req).await {
        Ok(input) => input,
        Err(error) => return error,
    };
    let account = input.account.trim().to_lowercase();
    if gate.limiter.allow(&account, &ip).is_err() {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let ready = match gate.oauth.held(&input.ticket) {
        Ok(ready) => ready,
        Err(error) => return json_err(StatusCode::BAD_REQUEST, error),
    };
    let ok = crate::relay_auth::valid_account_name(&account)
        && !input.password.is_empty()
        && input.password.len() <= 1024
        && {
            let accounts = gate.accounts.clone();
            let (name, password) = (account.clone(), input.password);
            tokio::task::spawn_blocking(move || accounts.check(&name, &password))
                .await
                .unwrap_or(false)
        };
    gate.limiter.record(&account, ok);
    if !ok {
        gate.oauth.miss(&input.ticket);
        return json_err(StatusCode::UNAUTHORIZED, "bad_credentials");
    }
    if !gate.account_active(&account) {
        return json_err(StatusCode::UNAUTHORIZED, "account_disabled");
    }
    if !gate.oauth.spend(&input.ticket) {
        return json_err(StatusCode::BAD_REQUEST, "expired");
    }
    match gate
        .oauth
        .resolve(&ready.identity, Some(&account), |name| gate.accounts.exists(name))
    {
        Ok(account) => sign_in(&gate, account, ready),
        Err(error) => json_err(StatusCode::CONFLICT, error),
    }
}

#[cfg(test)]
#[path = "gateway_oauth/tests.rs"]
mod tests;
