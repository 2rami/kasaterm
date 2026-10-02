use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

pub(crate) const TTL: Duration = Duration::from_secs(600);
/// Lifetime of the one-time code handed to an app's redirect URI; the app redeems it immediately.
pub(crate) const CODE_TTL: Duration = Duration::from_secs(120);
/// Private-use URI schemes of first-party apps that may receive a login result (RFC 8252 §7.1).
const APP_SCHEMES: [&str; 2] = ["kasaterm", "nachochat"];
const MAX_PENDING: usize = 256;
/// Wrong passwords a held sign-in survives before it must start over.
const MAX_CLAIM_FAILURES: u8 = 5;
const MAX_RESPONSE: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    Github,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Github => "github",
        }
    }
    fn authorize_url(self) -> &'static str {
        match self {
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::Github => "https://github.com/login/oauth/authorize",
        }
    }
    fn token_url(self) -> &'static str {
        match self {
            Self::Google => "https://oauth2.googleapis.com/token",
            Self::Github => "https://github.com/login/oauth/access_token",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClientConfig {
    pub id: String,
    secret: String,
}

#[derive(Clone, Default)]
pub(crate) struct Config {
    pub origin: String,
    clients: HashMap<Provider, ClientConfig>,
    allow_signup: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_values(|key| std::env::var(key).ok())
    }

    fn from_values(get: impl Fn(&str) -> Option<String>) -> Self {
        let Some(origin) = get("KASA_OAUTH_PUBLIC_ORIGIN").filter(|s| valid_origin(s)) else {
            return Self::default();
        };
        let mut clients = HashMap::new();
        for (provider, prefix) in [(Provider::Google, "GOOGLE"), (Provider::Github, "GITHUB")] {
            if let (Some(id), Some(secret)) = (
                get(&format!("KASA_OAUTH_{prefix}_CLIENT_ID")),
                get(&format!("KASA_OAUTH_{prefix}_CLIENT_SECRET")),
            ) {
                if [id.as_str(), secret.as_str()].iter().all(|s| {
                    !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_whitespace)
                }) {
                    clients.insert(provider, ClientConfig { id, secret });
                }
            }
        }
        Self {
            origin: origin.trim_end_matches('/').into(),
            clients,
            allow_signup: get("KASA_OAUTH_ALLOW_SIGNUP").as_deref() == Some("1"),
        }
    }

    pub fn enabled(&self, provider: Provider) -> bool {
        self.clients.contains_key(&provider)
    }
    pub fn callback(&self, provider: Provider) -> String {
        format!("{}/relay/oauth/{}/callback", self.origin, provider.name())
    }
    pub fn signup_enabled(&self) -> bool {
        self.allow_signup
    }
    pub fn providers(&self, storage_ready: bool) -> Value {
        let providers = [Provider::Google, Provider::Github].map(|provider| {
            let enabled = storage_ready && self.enabled(provider);
            json!({"id":provider.name(),"enabled":enabled,"reason":if enabled { "" } else { "setup_required" }})
        });
        json!({"ok":true,"signup_enabled":self.allow_signup,"redirect_login":true,"choose_account":true,"providers":providers})
    }
}

pub fn valid_origin(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/"
            && url.port().is_none()
    })
}

/// RFC 8252 redirect targets: an any-port loopback IP literal (never `localhost`, which can be
/// re-resolved) or an allowed app scheme. Anything else could deliver the code off the device.
pub fn valid_redirect_uri(value: &str) -> bool {
    value.len() <= 512
        && reqwest::Url::parse(value).is_ok_and(|url| {
            let target = match url.scheme() {
                "http" => {
                    url.port().is_some() && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
                }
                scheme => APP_SCHEMES.contains(&scheme),
            };
            target
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
}

fn unreserved(value: &str, len: std::ops::RangeInclusive<usize>) -> bool {
    len.contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// S256 challenge: base64url SHA-256 without padding.
pub fn valid_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

pub fn valid_verifier(value: &str) -> bool {
    unreserved(value, 43..=128)
}

pub fn valid_client_state(value: &str) -> bool {
    unreserved(value, 1..=128)
}

pub fn pkce_challenge(verifier: &str) -> String {
    challenge(verifier)
}

pub(crate) fn secret() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn digest(value: &str) -> String {
    crate::relay_auth::token_hash(value)
}
fn challenge(value: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(value.as_bytes()))
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    pub provider: Provider,
    pub kind: String,
    pub machine_id: String,
}

/// An app that receives the result at its own redirect URI and proves possession of the PKCE
/// verifier, instead of a person typing a code into the browser.
#[derive(Clone)]
pub(crate) struct Native {
    pub challenge: String,
    pub redirect_uri: String,
    pub state: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Link {
    pub device_id: String,
    pub account: String,
    pub token_hash: String,
}

#[derive(Clone)]
pub(crate) struct Exchange {
    pub provider: Provider,
    verifier: String,
    nonce: String,
    /// Relay-administrator browser sign-in: it never creates accounts or device credentials.
    pub admin: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Identity {
    pub provider: Provider,
    pub subject: String,
    /// Verified Google email or GitHub login, shown to people only; never an identity key.
    pub display: String,
}

/// Sign-up record of an account created by OAuth. Holds no conversation or screen content.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Profile {
    pub provider: Option<Provider>,
    /// Zero when the account predates this record.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    pub last_login: u64,
}

pub(crate) struct AccountInfo {
    pub providers: Vec<Provider>,
    pub profile: Option<Profile>,
    pub oauth_created: bool,
    pub active: bool,
}

fn display_label(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect()
}

enum Outcome {
    Waiting,
    Confirmed,
    Exchanging,
    Ready(Identity),
    Failed(&'static str),
}

struct Pending {
    created: Instant,
    device: Device,
    label: String,
    link: Option<Link>,
    poll_hash: String,
    browser_hash: Option<String>,
    csrf_hash: Option<String>,
    user_code: String,
    confirmation_failures: u8,
    state: String,
    exchange: Exchange,
    outcome: Outcome,
    native: Option<Native>,
    /// Hash and issue time of the code sent to `native.redirect_uri`.
    code: Option<(String, Instant)>,
    /// The client can let the person choose between a new and an existing account.
    choose: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Token {
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Poll {
    pub request_id: String,
    pub poll_token: String,
    pub provider: Provider,
    pub kind: String,
    pub machine_id: String,
}

#[derive(Clone)]
pub(crate) struct Ready {
    pub identity: Identity,
    pub device: Device,
    pub label: String,
    pub link: Option<Link>,
    pub choose: bool,
}

/// A verified identity that is not linked to any account yet, waiting for the person to create
/// an account or name an existing one. Only the device that redeemed the sign-in holds the ticket.
struct Held {
    created: Instant,
    ready: Ready,
    failures: u8,
}

pub(crate) struct BrowserConfirmation {
    pub cookie: String,
    pub csrf: String,
    pub label: String,
    pub machine_id: String,
    pub account: Option<String>,
    pub provider: Provider,
}

pub(crate) struct OAuth {
    pub config: Config,
    pending: Mutex<HashMap<String, Pending>>,
    held: Mutex<HashMap<String, Held>>,
    identities: Mutex<Option<Book>>,
    path: Option<PathBuf>,
    #[cfg(test)]
    pub mock_identity: Mutex<Option<Identity>>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Book {
    #[serde(default)]
    identities: HashMap<String, String>,
    #[serde(default)]
    accounts: HashMap<String, bool>,
    #[serde(default)]
    profiles: HashMap<String, Profile>,
}

impl OAuth {
    pub fn new(path: Option<PathBuf>, config: Config) -> Self {
        let book = path
            .as_ref()
            .and_then(|path| match std::fs::read_to_string(path) {
                Ok(raw) => serde_json::from_str::<Book>(&raw).ok(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Book::default()),
                Err(_) => None,
            });
        Self {
            config,
            pending: Mutex::new(HashMap::new()),
            held: Mutex::new(HashMap::new()),
            identities: Mutex::new(book),
            path,
            #[cfg(test)]
            mock_identity: Mutex::new(None),
        }
    }

    pub fn storage_ready(&self) -> bool {
        self.identities.lock().is_ok_and(|book| book.is_some())
    }
    pub fn active(&self, account: &str) -> bool {
        self.identities.lock().is_ok_and(|book| {
            book.as_ref()
                .is_some_and(|book| book.accounts.get(account) == Some(&true))
        })
    }

    pub fn start(
        &self,
        device: Device,
        label: String,
        link: Option<Link>,
        native: Option<Native>,
        choose: bool,
    ) -> Result<Value, &'static str> {
        if !self.config.enabled(device.provider) || !self.storage_ready() {
            return Err("setup_required");
        }
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        pending.retain(|_, request| request.created.elapsed() < TTL);
        if pending.len() >= MAX_PENDING {
            return Err("too_many_requests");
        }
        let request_id = secret();
        let poll_token = secret();
        let state = secret();
        let code = digest(&secret()).to_uppercase();
        // A redirect flow is redeemed only with its PKCE verifier, so it has no typed code or poll capability.
        let user_code = if native.is_some() {
            String::new()
        } else {
            format!("{}-{}", &code[..4], &code[4..8])
        };
        let exchange = Exchange {
            provider: device.provider,
            verifier: secret(),
            nonce: secret(),
            admin: false,
        };
        let authorization_url =
            format!("{}/relay/oauth/authorize/{request_id}", self.config.origin);
        let mut response = json!({"ok":true,"request_id":request_id,"expires_in":TTL.as_secs(),"authorization_url":authorization_url});
        if native.is_none() {
            response["poll_token"] = json!(poll_token);
            response["user_code"] = json!(user_code);
        }
        pending.insert(
            request_id,
            Pending {
                created: Instant::now(),
                device,
                label,
                link,
                poll_hash: digest(&if native.is_some() {
                    secret()
                } else {
                    poll_token
                }),
                browser_hash: None,
                csrf_hash: None,
                user_code,
                confirmation_failures: 0,
                state,
                exchange,
                outcome: Outcome::Waiting,
                native,
                code: None,
                choose,
            },
        );
        Ok(response)
    }

    /// Browser entry of a redirect flow: binds this browser to the request and goes straight to the
    /// provider. Returns `None` for code-confirmation flows. Re-opening rebinds to the newest browser.
    pub fn redirect(&self, request_id: &str) -> Result<Option<(String, String)>, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let request = pending
            .get_mut(request_id)
            .filter(|r| r.created.elapsed() < TTL)
            .ok_or("expired")?;
        if request.native.is_none() {
            return Ok(None);
        }
        if !matches!(request.outcome, Outcome::Waiting | Outcome::Confirmed) {
            return Err("already_used");
        }
        let url = self.authorization_url(&request.state, &request.exchange)?;
        let cookie = secret();
        request.browser_hash = Some(digest(&cookie));
        request.outcome = Outcome::Confirmed;
        Ok(Some((state_cookie(&request.state, &cookie), url)))
    }

    /// Redeems a redirect flow's one-time code. A presented code is spent even when the verifier or
    /// redirect URI is wrong, so an intercepted code cannot be retried.
    pub fn token(&self, input: &Token) -> Result<Ready, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let hash = digest(&input.code);
        let id = pending
            .iter()
            .find(|(_, request)| request.code.as_ref().is_some_and(|(code, _)| *code == hash))
            .map(|(id, _)| id.clone())
            .ok_or("invalid_grant")?;
        let request = pending.remove(&id).ok_or("invalid_grant")?;
        let (Some(native), Some((_, issued))) = (&request.native, request.code) else {
            return Err("invalid_grant");
        };
        if request.created.elapsed() >= TTL
            || issued.elapsed() >= CODE_TTL
            || native.redirect_uri != input.redirect_uri
            || !valid_verifier(&input.code_verifier)
            || challenge(&input.code_verifier) != native.challenge
        {
            return Err("invalid_grant");
        }
        let Outcome::Ready(identity) = request.outcome else {
            return Err("invalid_grant");
        };
        Ok(Ready {
            identity,
            device: request.device,
            label: request.label,
            link: request.link,
            choose: request.choose,
        })
    }

    pub fn browser(&self, request_id: &str) -> Result<BrowserConfirmation, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let request = pending
            .get_mut(request_id)
            .filter(|r| r.created.elapsed() < TTL)
            .ok_or("expired")?;
        if request.native.is_some() {
            return Err("invalid_request");
        }
        if !matches!(request.outcome, Outcome::Waiting) {
            return Err("already_used");
        }
        let cookie = secret();
        let csrf = secret();
        request.browser_hash = Some(digest(&cookie));
        request.csrf_hash = Some(digest(&csrf));
        Ok(BrowserConfirmation {
            cookie: state_cookie(&request.state, &cookie),
            csrf,
            label: request.label.clone(),
            machine_id: request.device.machine_id.clone(),
            account: request.link.as_ref().map(|link| link.account.clone()),
            provider: request.device.provider,
        })
    }

    pub fn confirm(
        &self,
        request_id: &str,
        cookies: &str,
        csrf: &str,
        user_code: &str,
    ) -> Result<String, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let request = pending
            .get_mut(request_id)
            .filter(|r| r.created.elapsed() < TTL)
            .ok_or("expired")?;
        if request.native.is_some()
            || !matches!(request.outcome, Outcome::Waiting)
            || !browser_matches(request, cookies)
            || request.csrf_hash.as_deref() != Some(digest(csrf).as_str())
        {
            return Err("invalid_confirmation");
        }
        if user_code.trim().to_ascii_uppercase() != request.user_code {
            request.confirmation_failures = request.confirmation_failures.saturating_add(1);
            if request.confirmation_failures >= 5 {
                request.outcome = Outcome::Failed("invalid_confirmation");
            }
            return Err("invalid_confirmation");
        }
        request.outcome = Outcome::Confirmed;
        request.csrf_hash = None;
        self.authorization_url(&request.state, &request.exchange)
    }

    fn authorization_url(&self, state: &str, exchange: &Exchange) -> Result<String, &'static str> {
        let provider = exchange.provider;
        let config = self.config.clients.get(&provider).ok_or("setup_required")?;
        let mut url = reqwest::Url::parse(provider.authorize_url()).map_err(|_| "unavailable")?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", config.id.as_str()),
            ("redirect_uri", &self.config.callback(provider)),
            ("response_type", "code"),
            ("state", state),
            ("code_challenge", &challenge(&exchange.verifier)),
            ("code_challenge_method", "S256"),
            (
                "scope",
                if provider == Provider::Google {
                    "openid email"
                } else {
                    "read:user"
                },
            ),
        ]);
        if provider == Provider::Google {
            url.query_pairs_mut()
                .append_pair("nonce", &exchange.nonce)
                .append_pair("prompt", "select_account");
        }
        Ok(url.to_string())
    }

    /// Browser sign-in to the relay admin page. The browser that starts it is also the one that
    /// receives the result, so the state cookie binds the callback and no app code is involved.
    /// Returns the provider URL and the state cookie.
    pub fn start_admin(&self, provider: Provider) -> Result<(String, String), &'static str> {
        if !self.config.enabled(provider) || !self.storage_ready() {
            return Err("setup_required");
        }
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        pending.retain(|_, request| request.created.elapsed() < TTL);
        if pending.len() >= MAX_PENDING {
            return Err("too_many_requests");
        }
        let state = secret();
        let cookie = secret();
        let exchange = Exchange {
            provider,
            verifier: secret(),
            nonce: secret(),
            admin: true,
        };
        let url = self.authorization_url(&state, &exchange)?;
        let header = state_cookie(&state, &cookie);
        pending.insert(
            secret(),
            Pending {
                created: Instant::now(),
                device: Device {
                    provider,
                    kind: "admin".into(),
                    machine_id: String::new(),
                },
                label: String::new(),
                link: None,
                // Nobody holds this capability, so an admin request can never be polled for a device token.
                poll_hash: digest(&secret()),
                browser_hash: Some(digest(&cookie)),
                csrf_hash: None,
                user_code: String::new(),
                confirmation_failures: 0,
                state,
                exchange,
                outcome: Outcome::Confirmed,
                native: None,
                code: None,
                choose: false,
            },
        );
        Ok((url, header))
    }

    /// Ends an admin sign-in started by `start_admin`; device requests are left untouched.
    pub fn finish_admin(
        &self,
        id: &str,
        result: Result<Identity, &'static str>,
    ) -> Result<Identity, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let request = pending
            .get(id)
            .filter(|request| {
                request.exchange.admin
                    && request.created.elapsed() < TTL
                    && matches!(request.outcome, Outcome::Exchanging)
            })
            .ok_or("invalid_state")?;
        let provider = request.device.provider;
        pending.remove(id);
        let identity = result?;
        if identity.provider != provider {
            return Err("provider_mismatch");
        }
        Ok(identity)
    }

    pub fn callback(
        &self,
        provider: Provider,
        state: &str,
        cookies: &str,
    ) -> Result<(String, Exchange), &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let (id, request) = pending
            .iter_mut()
            .find(|(_, request)| request.state == state && request.device.provider == provider)
            .ok_or("invalid_state")?;
        if request.created.elapsed() >= TTL
            || !matches!(request.outcome, Outcome::Confirmed)
            || !browser_matches(request, cookies)
        {
            return Err("invalid_state");
        }
        // Consume state before network I/O so concurrent callbacks cannot exchange twice.
        request.outcome = Outcome::Exchanging;
        Ok((id.clone(), request.exchange.clone()))
    }

    /// Records the provider result. For a redirect flow, returns where to send the browser: the
    /// app's redirect URI with a fresh one-time code, or with an OAuth error.
    pub fn finish(&self, id: &str, result: Result<Identity, &'static str>) -> Option<String> {
        let mut pending = self.pending.lock().ok()?;
        let request = pending.get_mut(id).filter(|request| {
            !request.exchange.admin
                && request.created.elapsed() < TTL
                && matches!(request.outcome, Outcome::Exchanging)
        })?;
        let outcome = match result {
            Ok(identity) if identity.provider == request.device.provider => {
                Outcome::Ready(identity)
            }
            Ok(_) => Outcome::Failed("provider_mismatch"),
            Err(error) => Outcome::Failed(error),
        };
        let Some(native) = request.native.clone() else {
            request.outcome = outcome;
            return None;
        };
        let mut url = reqwest::Url::parse(&native.redirect_uri).ok()?;
        if let Outcome::Failed(error) = outcome {
            pending.remove(id);
            url.query_pairs_mut().append_pair(
                "error",
                if error == "cancelled" {
                    "access_denied"
                } else {
                    "server_error"
                },
            );
        } else {
            let code = secret();
            request.code = Some((digest(&code), Instant::now()));
            request.outcome = outcome;
            url.query_pairs_mut().append_pair("code", &code);
        }
        if let Some(state) = &native.state {
            url.query_pairs_mut().append_pair("state", state);
        }
        Some(url.to_string())
    }

    pub fn poll(&self, input: &Poll, cancel: bool) -> Result<Option<Ready>, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "unavailable")?;
        let request = pending.get(&input.request_id).ok_or("expired")?;
        if request.poll_hash != digest(&input.poll_token)
            || request.device
                != (Device {
                    provider: input.provider,
                    kind: input.kind.clone(),
                    machine_id: input.machine_id.clone(),
                })
        {
            return Err("invalid_request");
        }
        if request.created.elapsed() >= TTL {
            pending.remove(&input.request_id);
            return Err("expired");
        }
        if cancel {
            pending.remove(&input.request_id);
            return Ok(None);
        }
        match request.outcome {
            Outcome::Waiting | Outcome::Confirmed | Outcome::Exchanging => return Ok(None),
            Outcome::Failed(error) => {
                pending.remove(&input.request_id);
                return Err(error);
            }
            Outcome::Ready(_) => {}
        }
        let request = pending.remove(&input.request_id).ok_or("expired")?;
        let Outcome::Ready(identity) = request.outcome else {
            return Err("invalid_request");
        };
        Ok(Some(Ready {
            identity,
            device: request.device,
            label: request.label,
            link: request.link,
            choose: request.choose,
        }))
    }

    /// Keeps an unlinked sign-in until the person chooses an account. Returns the ticket.
    pub fn hold(&self, ready: Ready) -> Result<String, &'static str> {
        let mut held = self.held.lock().map_err(|_| "unavailable")?;
        held.retain(|_, entry| entry.created.elapsed() < TTL);
        if held.len() >= MAX_PENDING {
            return Err("too_many_requests");
        }
        let ticket = secret();
        held.insert(
            digest(&ticket),
            Held {
                created: Instant::now(),
                ready,
                failures: 0,
            },
        );
        Ok(ticket)
    }

    /// The held sign-in, left in place so a wrong password can be retried.
    pub fn held(&self, ticket: &str) -> Result<Ready, &'static str> {
        let held = self.held.lock().map_err(|_| "unavailable")?;
        held.get(&digest(ticket))
            .filter(|entry| entry.created.elapsed() < TTL)
            .map(|entry| entry.ready.clone())
            .ok_or("expired")
    }

    /// Ends a held sign-in. False when another request already used or ended it.
    pub fn spend(&self, ticket: &str) -> bool {
        self.held
            .lock()
            .is_ok_and(|mut held| held.remove(&digest(ticket)).is_some())
    }

    /// Counts a wrong password; the ticket dies after a few so it cannot be used to guess.
    pub fn miss(&self, ticket: &str) {
        let Ok(mut held) = self.held.lock() else {
            return;
        };
        let key = digest(ticket);
        if let Some(entry) = held.get_mut(&key) {
            entry.failures = entry.failures.saturating_add(1);
            if entry.failures >= MAX_CLAIM_FAILURES {
                held.remove(&key);
            }
        }
    }

    pub fn resolve(
        &self,
        identity: &Identity,
        link: Option<&str>,
        account_exists: impl Fn(&str) -> bool,
    ) -> Result<String, &'static str> {
        if identity.subject.is_empty()
            || identity.subject.len() > 255
            || !identity.subject.is_ascii()
        {
            return Err("invalid_identity");
        }
        let key = format!("{}:{}", identity.provider.name(), identity.subject);
        let now = crate::relay_auth::now_secs();
        let mut guard = self.identities.lock().map_err(|_| "unavailable")?;
        let book = guard.as_ref().ok_or("storage_unavailable")?;
        if let Some(account) = book.identities.get(&key) {
            if link.is_some_and(|link| link != account) {
                return Err("already_linked");
            }
            let account = account.clone();
            if link.is_none() && book.accounts.contains_key(&account) {
                let mut next = book.clone();
                let profile = next
                    .profiles
                    .entry(account.clone())
                    .or_insert_with(|| Profile {
                        provider: Some(identity.provider),
                        ..Profile::default()
                    });
                if !identity.display.is_empty()
                    && (profile.display.is_empty() || profile.provider == Some(identity.provider))
                {
                    profile.display = identity.display.clone();
                }
                profile.last_login = now;
                // The sign-in already succeeded; a failed bookkeeping write must not undo it.
                let _ = self.commit(&mut guard, next);
            }
            return Ok(account);
        }
        if link.is_none() && !self.config.allow_signup {
            return Err("account_not_linked");
        }
        let mut next = book.clone();
        let account = match link {
            Some(account) => account.to_string(),
            None => loop {
                let account = format!(
                    "oauth_{}",
                    uuid::Uuid::new_v4().simple().to_string().get(..24).unwrap()
                );
                if !book.accounts.contains_key(&account) && !account_exists(&account) {
                    next.accounts.insert(account.clone(), true);
                    next.profiles.insert(
                        account.clone(),
                        Profile {
                            provider: Some(identity.provider),
                            created: now,
                            display: identity.display.clone(),
                            last_login: now,
                        },
                    );
                    break account;
                }
            },
        };
        next.identities.insert(key, account.clone());
        self.commit(&mut guard, next)?;
        Ok(account)
    }

    fn commit(&self, guard: &mut Option<Book>, next: Book) -> Result<(), &'static str> {
        let path = self.path.as_ref().ok_or("storage_unavailable")?;
        crate::relay_auth::write_private(
            path,
            &serde_json::to_string(&next).map_err(|_| "storage_unavailable")?,
        )
        .map_err(|_| "storage_unavailable")?;
        *guard = Some(next);
        Ok(())
    }

    /// The account an identity already belongs to. Never creates or links anything.
    pub fn lookup(&self, identity: &Identity) -> Option<String> {
        let key = format!("{}:{}", identity.provider.name(), identity.subject);
        self.identities
            .lock()
            .ok()?
            .as_ref()?
            .identities
            .get(&key)
            .cloned()
    }

    /// Name to show for an OAuth-created account; password accounts keep their chosen name.
    pub fn display_name(&self, account: &str) -> Option<String> {
        let book = self.identities.lock().ok()?;
        book.as_ref()?
            .profiles
            .get(account)
            .map(|profile| profile.display.clone())
            .filter(|display| !display.is_empty())
    }

    pub(crate) fn overview(&self) -> HashMap<String, AccountInfo> {
        let Ok(book) = self.identities.lock() else {
            return HashMap::new();
        };
        let Some(book) = book.as_ref() else {
            return HashMap::new();
        };
        let mut out: HashMap<String, AccountInfo> = HashMap::new();
        for (account, active) in &book.accounts {
            out.insert(
                account.clone(),
                AccountInfo {
                    providers: Vec::new(),
                    profile: book.profiles.get(account).cloned(),
                    oauth_created: true,
                    active: *active,
                },
            );
        }
        for (key, account) in &book.identities {
            let provider = match key.split_once(':').map(|(provider, _)| provider) {
                Some("google") => Provider::Google,
                Some("github") => Provider::Github,
                _ => continue,
            };
            let info = out.entry(account.clone()).or_insert_with(|| AccountInfo {
                providers: Vec::new(),
                profile: None,
                oauth_created: false,
                active: true,
            });
            if !info.providers.contains(&provider) {
                info.providers.push(provider);
            }
        }
        for info in out.values_mut() {
            info.providers.sort_by_key(|provider| provider.name());
        }
        out
    }

    pub async fn exchange(
        &self,
        exchange: &Exchange,
        code: &str,
    ) -> Result<Identity, &'static str> {
        #[cfg(test)]
        if let Some(identity) = self.mock_identity.lock().unwrap().clone() {
            return Ok(identity);
        }
        let config = self
            .config
            .clients
            .get(&exchange.provider)
            .ok_or("setup_required")?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("KASA-OAuth/1")
            .build()
            .map_err(|_| "provider_unavailable")?;
        let token = bounded_json(
            client
                .post(exchange.provider.token_url())
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", config.id.as_str()),
                    ("client_secret", config.secret.as_str()),
                    ("code", code),
                    ("redirect_uri", &self.config.callback(exchange.provider)),
                    ("code_verifier", &exchange.verifier),
                    ("grant_type", "authorization_code"),
                ])
                .send()
                .await
                .map_err(|_| "provider_unavailable")?,
        )
        .await?;
        if !token["token_type"]
            .as_str()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
        {
            return Err("invalid_token");
        }
        match exchange.provider {
            Provider::Google => {
                let jwt = token["id_token"]
                    .as_str()
                    .filter(|token| token.len() < 32 * 1024)
                    .ok_or("invalid_token")?;
                let jwks = bounded_json(
                    client
                        .get("https://www.googleapis.com/oauth2/v3/certs")
                        .send()
                        .await
                        .map_err(|_| "provider_unavailable")?,
                )
                .await?;
                google_identity(
                    jwt,
                    &jwks,
                    &config.id,
                    &exchange.nonce,
                    crate::relay_auth::now_secs(),
                )
            }
            Provider::Github => {
                let token = token["access_token"]
                    .as_str()
                    .filter(|token| !token.is_empty() && token.len() < 4096)
                    .ok_or("invalid_token")?;
                let user = bounded_json(
                    client
                        .get("https://api.github.com/user")
                        .bearer_auth(token)
                        .header("Accept", "application/vnd.github+json")
                        .send()
                        .await
                        .map_err(|_| "provider_unavailable")?,
                )
                .await?;
                github_identity(&user)
            }
        }
    }
}

fn cookie_name(state: &str) -> String {
    format!("__Secure-kasa_oauth_{}", &digest(state)[..16])
}

fn state_cookie(state: &str, cookie: &str) -> String {
    format!(
        "{}={cookie}; Path=/relay/oauth/; Secure; HttpOnly; SameSite=Lax; Max-Age=600",
        cookie_name(state)
    )
}

fn browser_matches(request: &Pending, cookies: &str) -> bool {
    let name = cookie_name(&request.state);
    cookies
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .is_some_and(|(_, cookie)| request.browser_hash.as_deref() == Some(digest(cookie).as_str()))
}

async fn bounded_json(mut response: reqwest::Response) -> Result<Value, &'static str> {
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err("provider_rejected");
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "provider_unavailable")? {
        if data.len() + chunk.len() > MAX_RESPONSE {
            return Err("provider_rejected");
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|_| "provider_rejected")
}

fn github_identity(user: &Value) -> Result<Identity, &'static str> {
    let id = user["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or("invalid_identity")?;
    Ok(Identity {
        provider: Provider::Github,
        subject: id.to_string(),
        display: display_label(user["login"].as_str()),
    })
}

fn google_identity(
    jwt: &str,
    jwks: &Value,
    audience: &str,
    nonce: &str,
    now: u64,
) -> Result<Identity, &'static str> {
    let parts: Vec<_> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err("invalid_token");
    }
    let decode = |part| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| "invalid_token")
    };
    let header: Value = serde_json::from_slice(&decode(parts[0])?).map_err(|_| "invalid_token")?;
    if header["alg"] != "RS256" {
        return Err("invalid_token");
    }
    let kid = header["kid"]
        .as_str()
        .filter(|kid| !kid.is_empty())
        .ok_or("invalid_token")?;
    let key = jwks["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter().find(|key| {
                key["kid"] == kid
                    && key["kty"] == "RSA"
                    && key["alg"] == "RS256"
                    && key["use"] == "sig"
            })
        })
        .ok_or("invalid_token")?;
    let n = decode(key["n"].as_str().ok_or("invalid_token")?)?;
    let e = decode(key["e"].as_str().ok_or("invalid_token")?)?;
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &decode(parts[2])?,
        )
        .map_err(|_| "invalid_token")?;
    let claims: Value = serde_json::from_slice(&decode(parts[1])?).map_err(|_| "invalid_token")?;
    google_claims(&claims, audience, nonce, now)
}

fn google_claims(
    claims: &Value,
    audience: &str,
    nonce: &str,
    now: u64,
) -> Result<Identity, &'static str> {
    if !matches!(
        claims["iss"].as_str(),
        Some("https://accounts.google.com" | "accounts.google.com")
    ) || claims["aud"] != audience
        || claims["nonce"] != nonce
        || claims["email_verified"] != true
        || !claims["exp"].as_u64().is_some_and(|expiry| expiry > now)
        || !claims["iat"]
            .as_u64()
            .is_some_and(|issued| issued <= now.saturating_add(60))
        || claims.get("azp").is_some_and(|azp| azp != audience)
    {
        return Err("invalid_token");
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|sub| !sub.is_empty() && sub.len() <= 255 && sub.is_ascii())
        .ok_or("invalid_identity")?;
    Ok(Identity {
        provider: Provider::Google,
        subject: subject.into(),
        display: display_label(claims["email"].as_str()),
    })
}

#[cfg(test)]
pub(crate) mod tests;
