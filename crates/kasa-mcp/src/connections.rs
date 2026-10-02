//! Work permissions attached to a KASA account — Gmail and GitHub pull requests — held and used by
//! the gateway (docs/account-connections.md).
//!
//! Provider tokens never leave the gateway. Devices ask for an action with their device credential;
//! reads run at once, writes wait as a pending write until a person approves it on an app screen.
//! Every action leaves one audit line without message bodies or tokens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::oauth_accounts::{Config, Provider};

/// How long a pending write waits for a person.
pub const PENDING_TTL: u64 = 24 * 60 * 60;
const MAX_PENDING: usize = 50;
const MAX_CONNECTIONS: usize = 16;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_MAIL_TEXT: usize = 100 * 1024;
const AUDIT_ROTATE: u64 = 2 * 1024 * 1024;
const GMAIL_READ: &str = "https://www.googleapis.com/auth/gmail.readonly";
const GMAIL_SEND: &str = "https://www.googleapis.com/auth/gmail.send";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Feature {
    #[serde(rename = "mail.read")]
    MailRead,
    #[serde(rename = "mail.send")]
    MailSend,
    #[serde(rename = "github.pr")]
    GithubPr,
}

impl Feature {
    pub fn provider(self) -> Provider {
        match self {
            Self::MailRead | Self::MailSend => Provider::Google,
            Self::GithubPr => Provider::Github,
        }
    }
    /// Google scope behind a mail feature. GitHub App permissions are set on the app instead.
    pub fn scope(self) -> Option<&'static str> {
        match self {
            Self::MailRead => Some(GMAIL_READ),
            Self::MailSend => Some(GMAIL_SEND),
            Self::GithubPr => None,
        }
    }
}

/// Tokens a provider returned for a connect flow. Lives only between the provider callback and the
/// requesting device's redemption, then in the sealed book.
#[derive(Clone, Debug)]
pub struct Grant {
    pub provider: Provider,
    pub subject: String,
    pub display: String,
    pub access_token: String,
    pub access_expires: u64,
    pub refresh_token: Option<String>,
    pub refresh_expires: u64,
    pub scopes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Connection {
    id: String,
    provider: Provider,
    subject: String,
    display: String,
    features: Vec<Feature>,
    access_token: String,
    /// Unix seconds; zero means the token does not expire.
    access_expires: u64,
    refresh_token: Option<String>,
    refresh_expires: u64,
    created: u64,
    updated: u64,
    /// The provider refused the stored tokens; only reconnecting fixes it.
    #[serde(default)]
    broken: bool,
}

impl Connection {
    fn summary(&self) -> Value {
        json!({"id":self.id,"provider":self.provider.name(),"display":self.display,
            "features":self.features,"state":if self.broken { "reconnect_required" } else { "ok" },
            "created":self.created,"updated":self.updated})
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailDraft {
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Gmail message id this replies to; threading headers are looked up when sending.
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrDraft {
    pub repo: String,
    pub base: String,
    pub head: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Write {
    Mail(MailDraft),
    Pr(PrDraft),
}

impl Write {
    fn feature(&self) -> Feature {
        match self {
            Self::Mail(_) => Feature::MailSend,
            Self::Pr(_) => Feature::GithubPr,
        }
    }
    fn digest(&self) -> String {
        hex(&sha2::Sha256::digest(serde_json::to_vec(self).unwrap_or_default()))
    }
    /// What an audit line may say about the write: no body, no subject, no addresses.
    fn target(&self) -> Value {
        match self {
            Self::Mail(mail) => json!({"recipients":mail.to.len() + mail.cc.len(),"reply":mail.reply_to.is_some()}),
            Self::Pr(pr) => json!({"repo":pr.repo,"base":pr.base,"head":pr.head,"draft":pr.draft}),
        }
    }
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Mail(mail) => {
                let addresses = mail.to.iter().chain(&mail.cc);
                if mail.to.is_empty()
                    || mail.to.len() + mail.cc.len() > 50
                    || addresses.clone().any(|address| !valid_address(address))
                    || mail.subject.chars().count() > 300
                    || mail.subject.chars().any(char::is_control)
                    || mail.body.len() > 200 * 1024
                    || mail.reply_to.as_deref().is_some_and(|id| !valid_message_id(id))
                {
                    return Err(Error::Invalid);
                }
            }
            Self::Pr(pr) => {
                if !valid_repo(&pr.repo)
                    || !valid_branch(&pr.base)
                    || !valid_head(&pr.head)
                    || pr.title.trim().is_empty()
                    || pr.title.chars().count() > 256
                    || pr.title.chars().any(char::is_control)
                    || pr.body.len() > 64 * 1024
                {
                    return Err(Error::Invalid);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    id: String,
    connection: String,
    write: Write,
    digest: String,
    device: String,
    device_label: String,
    created: u64,
}

impl Pending {
    fn view(&self, connections: &[Connection]) -> Value {
        let via = connections
            .iter()
            .find(|connection| connection.id == self.connection);
        json!({"id":self.id,"connection":self.connection,
            "provider":via.map(|c| c.provider.name()),"display":via.map(|c| c.display.as_str()),
            "write":self.write,"digest":self.digest,"device":self.device,
            "device_label":self.device_label,"created":self.created,
            "expires":self.created + PENDING_TTL})
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    connections: Vec<Connection>,
    #[serde(default)]
    pending: Vec<Pending>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    NotFound,
    /// More than one connection could serve the request; the caller must name one.
    ChooseConnection,
    FeatureMissing,
    Reconnect,
    ApproverRequired,
    DigestMismatch,
    NotAccessible,
    /// The provider refused the request itself; carries its short reason.
    Rejected(String),
    Provider,
    /// The send left the gateway but no answer came back; it may or may not have gone out.
    Unknown,
    Setup,
    Storage,
    Full,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::NotFound => "not_found",
            Self::ChooseConnection => "connection_required",
            Self::FeatureMissing => "feature_missing",
            Self::Reconnect => "reconnect_required",
            Self::ApproverRequired => "approver_required",
            Self::DigestMismatch => "content_changed",
            Self::NotAccessible => "repo_not_accessible",
            Self::Rejected(_) => "provider_rejected",
            Self::Provider => "provider_unavailable",
            Self::Unknown => "result_unknown",
            Self::Setup => "setup_required",
            Self::Storage => "storage_unavailable",
            Self::Full => "too_many_pending",
        }
    }
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid => 400,
            Self::NotFound => 404,
            Self::ChooseConnection | Self::Reconnect | Self::DigestMismatch => 409,
            Self::FeatureMissing | Self::ApproverRequired | Self::NotAccessible => 403,
            Self::Rejected(_) => 422,
            Self::Provider | Self::Unknown => 502,
            Self::Setup | Self::Storage => 503,
            Self::Full => 429,
        }
    }
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Rejected(reason) => Some(reason),
            _ => None,
        }
    }
}

impl From<crate::sealed::Error> for Error {
    fn from(error: crate::sealed::Error) -> Self {
        match error {
            crate::sealed::Error::Invalid => Self::Invalid,
            crate::sealed::Error::Storage => Self::Storage,
        }
    }
}

/// Who asked, as the gateway authenticated it.
pub struct Caller<'a> {
    pub account: &'a str,
    pub device: &'a str,
    pub label: &'a str,
}

#[derive(Clone)]
pub(crate) struct Endpoints {
    pub google_token: String,
    pub google_revoke: String,
    pub gmail: String,
    pub github_token: String,
    pub github_api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            google_token: "https://oauth2.googleapis.com/token".into(),
            google_revoke: "https://oauth2.googleapis.com/revoke".into(),
            gmail: "https://gmail.googleapis.com/gmail/v1/users/me".into(),
            github_token: "https://github.com/login/oauth/access_token".into(),
            github_api: "https://api.github.com".into(),
        }
    }
}

pub struct Service {
    vault: crate::sealed::Vault,
    audit: PathBuf,
    /// Serializes read-modify-write of the sealed books, including token refresh: a GitHub
    /// refresh token is single-use, so two refreshes must never race.
    books: tokio::sync::Mutex<()>,
    /// Device id → hash of the approval key its app registered for this gateway run. Memory only:
    /// a gateway restart asks the apps to register again.
    approvers: Mutex<HashMap<String, String>>,
    config: Config,
    pub(crate) endpoints: Endpoints,
    http: reqwest::Client,
}

impl Service {
    pub(crate) fn open(state_path: Option<&Path>, config: Config) -> Option<Arc<Self>> {
        let directory = state_path?.with_file_name("connections");
        let vault = match crate::sealed::Vault::open(directory.clone(), "kasa.connections.v1") {
            Ok(vault) => vault,
            Err(_) => {
                eprintln!("[gateway] account connections disabled: storage_unavailable");
                return None;
            }
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("KASA-Connections/1")
            .build()
            .ok()?;
        Some(Arc::new(Self {
            vault,
            audit: directory.join("audit.jsonl"),
            books: tokio::sync::Mutex::new(()),
            approvers: Mutex::new(HashMap::new()),
            config,
            endpoints: Endpoints::default(),
            http,
        }))
    }

    fn load(&self, account: &str) -> Result<Book, Error> {
        Ok(self.vault.read::<Book>(account)?.unwrap_or_default())
    }

    fn save(&self, account: &str, book: &Book) -> Result<(), Error> {
        Ok(self.vault.write(account, book)?)
    }

    fn record(&self, caller: &Caller, via: &str, action: &str, target: Value, result: &str) {
        let line = json!({"at":crate::relay_auth::now_secs(),"account":caller.account,"device":caller.device,
            "via":via,"action":action,"target":target,"result":result});
        if std::fs::metadata(&self.audit).is_ok_and(|meta| meta.len() > AUDIT_ROTATE) {
            let _ = std::fs::rename(&self.audit, self.audit.with_extension("jsonl.1"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        if let Ok(mut file) = options.open(&self.audit) {
            use std::io::Write as _;
            let _ = writeln!(file, "{line}");
        }
    }

    /// The account's newest audit lines, newest first.
    pub fn audit(&self, account: &str, limit: usize) -> Vec<Value> {
        let mut lines: Vec<Value> = [self.audit.with_extension("jsonl.1"), self.audit.clone()]
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .flat_map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect::<Vec<_>>()
            })
            .filter(|line| line["account"] == account)
            .collect();
        lines.reverse();
        lines.truncate(limit);
        lines
    }

    pub async fn list(&self, account: &str) -> Result<Value, Error> {
        let _guard = self.books.lock().await;
        let mut book = self.load(account)?;
        if prune(&mut book) {
            self.save(account, &book)?;
        }
        Ok(json!({"ok":true,
            "connections":book.connections.iter().map(Connection::summary).collect::<Vec<_>>(),
            "pending":book.pending.iter().map(|pending| pending.view(&book.connections)).collect::<Vec<_>>()}))
    }

    /// Saves a grant from a connect flow; reconnecting the same provider identity replaces it.
    pub async fn store(
        &self,
        caller: &Caller<'_>,
        grant: Grant,
        requested: &[Feature],
    ) -> Result<Value, Error> {
        let features: Vec<Feature> = requested
            .iter()
            .copied()
            .filter(|feature| {
                feature.provider() == grant.provider
                    && feature
                        .scope()
                        .is_none_or(|scope| grant.scopes.iter().any(|granted| granted == scope))
            })
            .collect();
        let _guard = self.books.lock().await;
        let mut book = self.load(caller.account)?;
        let now = crate::relay_auth::now_secs();
        let existing = book
            .connections
            .iter()
            .position(|c| c.provider == grant.provider && c.subject == grant.subject);
        if existing.is_none() && book.connections.len() >= MAX_CONNECTIONS {
            return Err(Error::Full);
        }
        let connection = Connection {
            id: existing.map_or_else(
                || format!("con_{}", &crate::oauth_accounts::secret()[..16]),
                |index| book.connections[index].id.clone(),
            ),
            provider: grant.provider,
            subject: grant.subject,
            display: grant.display,
            features,
            access_token: grant.access_token,
            access_expires: grant.access_expires,
            refresh_token: grant.refresh_token,
            refresh_expires: grant.refresh_expires,
            created: existing.map_or(now, |index| book.connections[index].created),
            updated: now,
            broken: false,
        };
        let summary = connection.summary();
        match existing {
            Some(index) => book.connections[index] = connection,
            None => book.connections.push(connection),
        }
        self.save(caller.account, &book)?;
        drop(_guard);
        self.record(caller, "device", "connect", json!({"provider":summary["provider"]}), "ok");
        Ok(summary)
    }

    /// Revokes at the provider first, then forgets the tokens even when the provider is unreachable.
    pub async fn disconnect(&self, caller: &Caller<'_>, id: &str) -> Result<Value, Error> {
        let guard = self.books.lock().await;
        let mut book = self.load(caller.account)?;
        let index = book
            .connections
            .iter()
            .position(|connection| connection.id == id)
            .ok_or(Error::NotFound)?;
        let connection = book.connections.remove(index);
        book.pending.retain(|pending| pending.connection != id);
        self.save(caller.account, &book)?;
        drop(guard);
        let revoked = self.revoke(&connection).await;
        self.record(
            caller,
            "device",
            "disconnect",
            json!({"provider":connection.provider.name()}),
            if revoked { "ok" } else { "provider_revoke_unconfirmed" },
        );
        Ok(json!({"ok":true,"provider_revoked":revoked}))
    }

    async fn revoke(&self, connection: &Connection) -> bool {
        let request = match connection.provider {
            Provider::Google => self.http.post(&self.endpoints.google_revoke).form(&[(
                "token",
                connection
                    .refresh_token
                    .as_deref()
                    .unwrap_or(&connection.access_token),
            )]),
            Provider::Github => {
                let Some((id, secret)) = self.config.credentials(Provider::Github, true) else {
                    return false;
                };
                self.http
                    .delete(format!("{}/applications/{id}/grant", self.endpoints.github_api))
                    .basic_auth(id, Some(secret))
                    .header("Accept", "application/vnd.github+json")
                    .json(&json!({"access_token":connection.access_token}))
            }
        };
        request
            .send()
            .await
            .is_ok_and(|response| response.status().is_success() || response.status() == 404)
    }

    /// A usable access token for the connection that serves `feature`, refreshing it when it is
    /// about to expire. `id` picks a connection when the account has several.
    async fn token(
        &self,
        account: &str,
        id: Option<&str>,
        feature: Feature,
    ) -> Result<(String, Connection), Error> {
        let _guard = self.books.lock().await;
        let mut book = self.load(account)?;
        let index = pick(&book.connections, id, feature)?;
        let connection = &book.connections[index];
        if connection.broken {
            return Err(Error::Reconnect);
        }
        if !connection.features.contains(&feature) {
            return Err(Error::FeatureMissing);
        }
        let now = crate::relay_auth::now_secs();
        if connection.access_expires == 0 || connection.access_expires > now + 60 {
            return Ok((connection.access_token.clone(), connection.clone()));
        }
        match self.refresh(connection).await {
            Ok(refreshed) => {
                book.connections[index] = refreshed;
                self.save(account, &book)?;
            }
            Err(Error::Reconnect) => {
                let connection = &mut book.connections[index];
                connection.broken = true;
                connection.access_token.clear();
                connection.refresh_token = None;
                self.save(account, &book)?;
                return Err(Error::Reconnect);
            }
            Err(error) => return Err(error),
        }
        let connection = &book.connections[index];
        Ok((connection.access_token.clone(), connection.clone()))
    }

    async fn refresh(&self, connection: &Connection) -> Result<Connection, Error> {
        let refresh = connection.refresh_token.as_deref().ok_or(Error::Reconnect)?;
        let (id, secret) = self
            .config
            .credentials(connection.provider, true)
            .ok_or(Error::Setup)?;
        let url = match connection.provider {
            Provider::Google => &self.endpoints.google_token,
            Provider::Github => &self.endpoints.github_token,
        };
        let response = self
            .http
            .post(url)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", id),
                ("client_secret", secret),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
            ])
            .send()
            .await
            .map_err(|_| Error::Provider)?;
        let status = response.status();
        let value = bounded(response).await?;
        // GitHub answers refresh failures with 200 and an `error` field.
        if status.is_client_error() || value.get("error").is_some() {
            return Err(Error::Reconnect);
        }
        if !status.is_success() {
            return Err(Error::Provider);
        }
        let now = crate::relay_auth::now_secs();
        let access = value["access_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() < 4096)
            .ok_or(Error::Provider)?;
        let mut next = connection.clone();
        next.access_token = access.into();
        next.access_expires = value["expires_in"].as_u64().map_or(0, |secs| now + secs);
        // Google keeps the refresh token; GitHub rotates it on every use.
        if let Some(rotated) = value["refresh_token"].as_str().filter(|t| !t.is_empty()) {
            next.refresh_token = Some(rotated.into());
            next.refresh_expires = value["refresh_token_expires_in"]
                .as_u64()
                .map_or(0, |secs| now + secs);
        }
        next.updated = now;
        Ok(next)
    }

    pub async fn mail_list(
        &self,
        caller: &Caller<'_>,
        connection: Option<&str>,
        query: &str,
        max: usize,
    ) -> Result<Value, Error> {
        if query.len() > 500 || query.chars().any(char::is_control) {
            return Err(Error::Invalid);
        }
        let (token, via) = self.token(caller.account, connection, Feature::MailRead).await?;
        let max = max.clamp(1, 25).to_string();
        let mut url = reqwest::Url::parse(&format!("{}/messages", self.endpoints.gmail))
            .map_err(|_| Error::Provider)?;
        url.query_pairs_mut().append_pair("maxResults", &max);
        if !query.is_empty() {
            url.query_pairs_mut().append_pair("q", query);
        }
        let listed = self.google_get(&token, url).await?;
        let mut messages = Vec::new();
        for id in listed["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message["id"].as_str())
            .filter(|id| valid_message_id(id))
        {
            let mut url = reqwest::Url::parse(&format!("{}/messages/{id}", self.endpoints.gmail))
                .map_err(|_| Error::Provider)?;
            url.query_pairs_mut().append_pair("format", "metadata");
            for header in ["From", "To", "Subject", "Date"] {
                url.query_pairs_mut().append_pair("metadataHeaders", header);
            }
            let message = self.google_get(&token, url).await?;
            let headers = headers(&message["payload"]);
            messages.push(json!({"id":id,"thread_id":message["threadId"],
                "from":headers.get("from"),"to":headers.get("to"),"subject":headers.get("subject"),
                "date":headers.get("date"),"snippet":message["snippet"],
                "unread":message["labelIds"].as_array().is_some_and(|labels| labels.iter().any(|l| l == "UNREAD"))}));
        }
        self.record(caller, "device", "mail.list", json!({"count":messages.len()}), "ok");
        Ok(json!({"ok":true,"connection":via.id,"display":via.display,"messages":messages}))
    }

    pub async fn mail_read(
        &self,
        caller: &Caller<'_>,
        connection: Option<&str>,
        id: &str,
    ) -> Result<Value, Error> {
        if !valid_message_id(id) {
            return Err(Error::Invalid);
        }
        let (token, via) = self.token(caller.account, connection, Feature::MailRead).await?;
        let mut url = reqwest::Url::parse(&format!("{}/messages/{id}", self.endpoints.gmail))
            .map_err(|_| Error::Provider)?;
        url.query_pairs_mut().append_pair("format", "full");
        let message = self.google_get(&token, url).await?;
        let headers = headers(&message["payload"]);
        let mut attachments = Vec::new();
        attachment_names(&message["payload"], &mut attachments);
        self.record(caller, "device", "mail.read", json!({"message":id}), "ok");
        Ok(json!({"ok":true,"connection":via.id,"display":via.display,"id":id,
            "thread_id":message["threadId"],"from":headers.get("from"),"to":headers.get("to"),
            "cc":headers.get("cc"),"subject":headers.get("subject"),"date":headers.get("date"),
            "text":body_text(&message["payload"]),"attachments":attachments}))
    }

    async fn google_get(&self, token: &str, url: reqwest::Url) -> Result<Value, Error> {
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::Provider)?;
        match response.status().as_u16() {
            200 => bounded(response).await,
            401 => Err(Error::Reconnect),
            403 => Err(Error::FeatureMissing),
            404 => Err(Error::NotFound),
            _ => Err(Error::Provider),
        }
    }

    /// Queues a write for a person to approve. The caller learns only its id.
    pub async fn request(
        &self,
        caller: &Caller<'_>,
        connection: Option<&str>,
        write: Write,
    ) -> Result<Value, Error> {
        write.validate()?;
        let _guard = self.books.lock().await;
        let mut book = self.load(caller.account)?;
        prune(&mut book);
        let index = pick(&book.connections, connection, write.feature())?;
        let via = &book.connections[index];
        if via.broken {
            return Err(Error::Reconnect);
        }
        if !via.features.contains(&write.feature()) {
            return Err(Error::FeatureMissing);
        }
        if book.pending.len() >= MAX_PENDING {
            return Err(Error::Full);
        }
        let pending = Pending {
            id: format!("pw_{}", &crate::oauth_accounts::secret()[..16]),
            connection: via.id.clone(),
            digest: write.digest(),
            write,
            device: caller.device.into(),
            device_label: caller.label.chars().take(60).collect(),
            created: crate::relay_auth::now_secs(),
        };
        let view = pending.view(&book.connections);
        let target = pending.write.target();
        let action = feature_action(pending.write.feature());
        book.pending.push(pending);
        self.save(caller.account, &book)?;
        drop(_guard);
        self.record(caller, "device", action, target, "pending");
        Ok(json!({"ok":true,"status":"pending","pending":view}))
    }

    /// Remembers the approval key an app screen holds for this device until the gateway restarts.
    pub fn register_approver(&self, device: &str, key: &str) -> Result<(), Error> {
        if !opaque(key) {
            return Err(Error::Invalid);
        }
        self.approvers
            .lock()
            .map_err(|_| Error::Storage)?
            .insert(device.into(), crate::relay_auth::token_hash(key));
        Ok(())
    }

    fn approver_ok(&self, device: &str, key: &str) -> bool {
        self.approvers.lock().is_ok_and(|approvers| {
            approvers.get(device).is_some_and(|hash| *hash == crate::relay_auth::token_hash(key))
        })
    }

    pub async fn reject(&self, caller: &Caller<'_>, id: &str) -> Result<Value, Error> {
        let guard = self.books.lock().await;
        let mut book = self.load(caller.account)?;
        let index = book
            .pending
            .iter()
            .position(|pending| pending.id == id)
            .ok_or(Error::NotFound)?;
        let pending = book.pending.remove(index);
        self.save(caller.account, &book)?;
        drop(guard);
        self.record(caller, "device", "reject", pending.write.target(), "ok");
        Ok(json!({"ok":true}))
    }

    /// Runs a pending write a person approved on an app screen. The digest is what that screen
    /// showed, so a write changed after it was shown is refused.
    pub async fn approve(
        &self,
        caller: &Caller<'_>,
        approver: &str,
        id: &str,
        digest: &str,
    ) -> Result<Value, Error> {
        if !self.approver_ok(caller.device, approver) {
            return Err(Error::ApproverRequired);
        }
        let pending = {
            let _guard = self.books.lock().await;
            let mut book = self.load(caller.account)?;
            prune(&mut book);
            let index = book
                .pending
                .iter()
                .position(|pending| pending.id == id)
                .ok_or(Error::NotFound)?;
            if book.pending[index].digest != digest {
                return Err(Error::DigestMismatch);
            }
            let pending = book.pending.remove(index);
            self.save(caller.account, &book)?;
            pending
        };
        let target = pending.write.target();
        let action = feature_action(pending.write.feature());
        let result = self.execute(caller.account, &pending).await;
        match &result {
            Ok(_) => self.record(caller, "approver", action, target, "ok"),
            Err(Outcome::Before(error)) => {
                // Nothing left the gateway, so the person can approve it again after fixing it.
                let _guard = self.books.lock().await;
                if let Ok(mut book) = self.load(caller.account) {
                    book.pending.push(pending.clone());
                    let _ = self.save(caller.account, &book);
                }
                self.record(caller, "approver", action, target, error.code());
            }
            Err(Outcome::After(error)) => self.record(caller, "approver", action, target, error.code()),
        }
        result.map_err(|outcome| match outcome {
            Outcome::Before(error) | Outcome::After(error) => error,
        })
    }

    async fn execute(&self, account: &str, pending: &Pending) -> Result<Value, Outcome> {
        let feature = pending.write.feature();
        let (token, via) = self
            .token(account, Some(&pending.connection), feature)
            .await
            .map_err(Outcome::Before)?;
        match &pending.write {
            Write::Mail(mail) => self.send_mail(&token, &via, mail).await,
            Write::Pr(pr) => self.create_pr(&token, pr).await,
        }
    }

    async fn send_mail(&self, token: &str, via: &Connection, mail: &MailDraft) -> Result<Value, Outcome> {
        let mut thread = None;
        let mut references = None;
        if let Some(reply_to) = &mail.reply_to {
            // gmail.send cannot read the original; threading needs the read permission too, and
            // without it the reply still goes out, just as a new conversation.
            if via.features.contains(&Feature::MailRead) {
                let mut url = reqwest::Url::parse(&format!("{}/messages/{reply_to}", self.endpoints.gmail))
                    .map_err(|_| Outcome::Before(Error::Provider))?;
                url.query_pairs_mut()
                    .append_pair("format", "metadata")
                    .append_pair("metadataHeaders", "Message-ID")
                    .append_pair("metadataHeaders", "References");
                let original = self.google_get(token, url).await.map_err(Outcome::Before)?;
                let headers = headers(&original["payload"]);
                thread = original["threadId"].as_str().map(str::to_string);
                if let Some(message_id) = headers.get("message-id").filter(|v| !v.contains(['\r', '\n'])) {
                    let chain = headers
                        .get("references")
                        .filter(|v| !v.contains(['\r', '\n']))
                        .map_or_else(|| message_id.clone(), |refs| format!("{refs} {message_id}"));
                    references = Some((message_id.clone(), chain));
                }
            }
        }
        let raw = mime(via, mail, references.as_ref());
        let mut body = json!({"raw":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)});
        if let Some(thread) = thread {
            body["threadId"] = json!(thread);
        }
        let response = self
            .http
            .post(format!("{}/messages/send", self.endpoints.gmail))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|_| Outcome::After(Error::Unknown))?;
        match response.status().as_u16() {
            200 => {
                let sent = bounded(response).await.map_err(Outcome::After)?;
                Ok(json!({"ok":true,"status":"sent","id":sent["id"],"thread_id":sent["threadId"]}))
            }
            401 => Err(Outcome::Before(Error::Reconnect)),
            403 => Err(Outcome::Before(Error::FeatureMissing)),
            400 => Err(Outcome::Before(Error::Rejected(provider_message(response).await))),
            _ => Err(Outcome::After(Error::Unknown)),
        }
    }

    async fn create_pr(&self, token: &str, pr: &PrDraft) -> Result<Value, Outcome> {
        let response = self
            .http
            .post(format!("{}/repos/{}/pulls", self.endpoints.github_api, pr.repo))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&json!({"title":pr.title,"head":pr.head,"base":pr.base,"body":pr.body,"draft":pr.draft}))
            .send()
            .await
            .map_err(|_| Outcome::After(Error::Unknown))?;
        match response.status().as_u16() {
            201 => {
                let created = bounded(response).await.map_err(Outcome::After)?;
                Ok(json!({"ok":true,"status":"created","number":created["number"],"url":created["html_url"]}))
            }
            401 => Err(Outcome::Before(Error::Reconnect)),
            403 | 404 => Err(Outcome::Before(Error::NotAccessible)),
            422 => Err(Outcome::Before(Error::Rejected(provider_message(response).await))),
            _ => Err(Outcome::After(Error::Unknown)),
        }
    }

    pub fn github_install_url(&self) -> Option<String> {
        self.config
            .github_app_slug()
            .map(|slug| format!("https://github.com/apps/{slug}/installations/new"))
    }
}

/// Where a failed write stopped: before anything was sent (safe to approve again), or after the
/// request left the gateway (it may have happened; do not resend blindly).
enum Outcome {
    Before(Error),
    After(Error),
}

fn feature_action(feature: Feature) -> &'static str {
    match feature {
        Feature::MailSend => "mail.send",
        Feature::GithubPr => "pr.create",
        Feature::MailRead => "mail.read",
    }
}

fn prune(book: &mut Book) -> bool {
    let now = crate::relay_auth::now_secs();
    let before = book.pending.len();
    book.pending
        .retain(|pending| pending.created + PENDING_TTL > now);
    before != book.pending.len()
}

fn pick(connections: &[Connection], id: Option<&str>, feature: Feature) -> Result<usize, Error> {
    if let Some(id) = id {
        return connections
            .iter()
            .position(|connection| connection.id == id && connection.provider == feature.provider())
            .ok_or(Error::NotFound);
    }
    let mut candidates = connections
        .iter()
        .enumerate()
        .filter(|(_, connection)| {
            connection.provider == feature.provider() && connection.features.contains(&feature)
        });
    match (candidates.next(), candidates.next()) {
        (Some((index, _)), None) => Ok(index),
        (Some(_), Some(_)) => Err(Error::ChooseConnection),
        (None, _) if connections.iter().any(|c| c.provider == feature.provider()) => {
            Err(Error::FeatureMissing)
        }
        (None, _) => Err(Error::NotFound),
    }
}

async fn bounded(mut response: reqwest::Response) -> Result<Value, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err(Error::Provider);
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Provider)? {
        if data.len() + chunk.len() > MAX_RESPONSE {
            return Err(Error::Provider);
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|_| Error::Provider)
}

/// The provider's own short explanation of a refused write (GitHub validation, Gmail bad request).
async fn provider_message(response: reqwest::Response) -> String {
    let value = bounded(response).await.unwrap_or(Value::Null);
    let mut parts: Vec<String> = Vec::new();
    for text in [&value["message"], &value["error"]["message"]] {
        if let Some(text) = text.as_str() {
            parts.push(text.into());
        }
    }
    for error in value["errors"].as_array().into_iter().flatten() {
        if let Some(text) = error["message"].as_str() {
            parts.push(text.into());
        }
    }
    let joined = parts.join(" · ");
    joined.chars().filter(|c| !c.is_control()).take(300).collect()
}

fn headers(payload: &Value) -> HashMap<String, String> {
    payload["headers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|header| {
            Some((
                header["name"].as_str()?.to_ascii_lowercase(),
                header["value"].as_str()?.chars().take(2000).collect(),
            ))
        })
        .collect()
}

fn decode_part(data: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data.trim_end_matches('='))
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn find_part<'a>(payload: &'a Value, mime: &str) -> Option<&'a Value> {
    if payload["mimeType"] == mime && payload["body"]["data"].is_string() {
        return Some(payload);
    }
    payload["parts"]
        .as_array()?
        .iter()
        .find_map(|part| find_part(part, mime))
}

/// Plain text of a message: its text/plain part, or its HTML part with tags removed.
fn body_text(payload: &Value) -> String {
    let text = if let Some(part) = find_part(payload, "text/plain") {
        decode_part(part["body"]["data"].as_str().unwrap_or_default()).unwrap_or_default()
    } else if let Some(part) = find_part(payload, "text/html") {
        strip_html(&decode_part(part["body"]["data"].as_str().unwrap_or_default()).unwrap_or_default())
    } else {
        String::new()
    };
    let mut end = text.len().min(MAX_MAIL_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let name = tag.trim_start_matches('/').split_whitespace().next().unwrap_or("");
                if matches!(name.to_ascii_lowercase().as_str(), "br" | "p" | "div" | "tr" | "li") {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn attachment_names(payload: &Value, out: &mut Vec<String>) {
    if let Some(name) = payload["filename"].as_str().filter(|name| !name.is_empty()) {
        out.push(name.chars().filter(|c| !c.is_control()).take(200).collect());
    }
    for part in payload["parts"].as_array().into_iter().flatten() {
        attachment_names(part, out);
    }
}

fn encoded_word(text: &str) -> String {
    if text.is_ascii() {
        text.into()
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(text)
        )
    }
}

/// RFC 5322 message for `gmail.send`. Header values were validated free of line breaks.
fn mime(via: &Connection, mail: &MailDraft, references: Option<&(String, String)>) -> Vec<u8> {
    let mut message = String::new();
    if valid_address(&via.display) {
        message.push_str(&format!("From: {}\r\n", via.display));
    }
    message.push_str(&format!("To: {}\r\n", mail.to.join(", ")));
    if !mail.cc.is_empty() {
        message.push_str(&format!("Cc: {}\r\n", mail.cc.join(", ")));
    }
    message.push_str(&format!("Subject: {}\r\n", encoded_word(&mail.subject)));
    if let Some((message_id, chain)) = references {
        message.push_str(&format!("In-Reply-To: {message_id}\r\nReferences: {chain}\r\n"));
    }
    message.push_str("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=\"UTF-8\"\r\nContent-Transfer-Encoding: base64\r\n\r\n");
    let body = base64::engine::general_purpose::STANDARD.encode(mail.body.as_bytes());
    for line in body.as_bytes().chunks(76) {
        message.push_str(std::str::from_utf8(line).unwrap_or_default());
        message.push_str("\r\n");
    }
    message.into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn opaque(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

fn valid_address(value: &str) -> bool {
    let Some((local, domain)) = value.rsplit_once('@') else {
        return false;
    };
    value.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && value
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && !"<>,;:\"()[]\\".contains(c))
}

fn valid_message_id(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_name(value: &str) -> bool {
    (1..=100).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && value != "."
        && value != ".."
}

fn valid_repo(value: &str) -> bool {
    value
        .split_once('/')
        .is_some_and(|(owner, name)| valid_name(owner) && valid_name(name))
}

fn valid_branch(value: &str) -> bool {
    (1..=255).contains(&value.len())
        && !value.starts_with('-')
        && !value.contains("..")
        && value
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && !"~^:?*[\\".contains(c))
}

fn valid_head(value: &str) -> bool {
    match value.split_once(':') {
        Some((owner, branch)) => valid_name(owner) && valid_branch(branch),
        None => valid_branch(value),
    }
}

#[cfg(test)]
pub(crate) mod tests;
