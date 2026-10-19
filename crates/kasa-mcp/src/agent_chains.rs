//! Claude 로그인 사슬을 관문 하나가 돌리고, 기기들은 짧은 접근 토큰만 받아 쓴다(docs/agent-chains.md).
//!
//! 갱신 토큰은 한 번 쓰면 바뀐다. 여러 자리가 같은 사슬을 돌리면 먼저 쓴 쪽이 나머지를 로그아웃시키므로
//! 사슬은 관문만 쥔다. 기기 슬롯에는 갱신 토큰 없이 접근 토큰만 둔다 — 그러면 그 슬롯의 claude 는 만료
//! 직전에도 스스로 갱신하지 않고, 401 을 받아도 저장소를 지우지 않는다(claude 2.1.292 실측). 접근 토큰은
//! 정식 로그인과 권한이 같아 커넥터·사용량·신원이 그대로 돈다 — `setup-token` 과 다른 점이다.
//!
//! 토큰은 로그·감사 기록·argv 에 남기지 않는다. 키체인 쓰기는 claude 처럼 `security -i` 표준입력으로 한다.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// claude CLI 의 공개 OAuth 클라이언트 — 갱신된 토큰을 CLI 가 그대로 이어 쓴다.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// 관문 갱신 실패를 만료 전에 여러 번 재시도할 여유다. 성공하면 옛 토큰은 즉시 폐기되므로 배포는 별도 힌트/pull이 맡는다.
const REFRESH_AHEAD_MS: u64 = 3 * 60 * 60 * 1000;
/// 맡긴 직후에는 갱신하지 않는다 — 맡긴 기기의 claude 가 키체인 캐시(30초)로 옛 갱신 토큰을 잠깐 더 쥔다.
const SEAL_SETTLE_MS: u64 = 2 * 60 * 1000;
/// 이보다 덜 남은 사슬은 맡기지 않는다 — claude 는 만료 5분 안쪽에서 스스로 갱신하므로 겹치지 않게 넉넉히.
const OFFER_MIN_LEFT_MS: u64 = 30 * 60 * 1000;
/// 갱신이 일시 실패하면 이만큼 쉬고 다시 — 매분 두드리면 429 로 더 막힌다.
const RETRY_MS: u64 = 5 * 60 * 1000;
/// 접근 토큰이 이만큼 안 남았는데 아직 갱신을 못 했으면 사람에게 알린다 — 관문은 3시간 앞서 갱신하므로
/// 여기까지 왔다면 몇 번을 내리 실패했거나 관문이 멈춘 것이다.
const ALERT_LEFT_MS: u64 = 60 * 60 * 1000;
const MAX_CHAINS: usize = 16;
const MAX_TOKEN: usize = 4096;
const MAX_RESPONSE: usize = 1024 * 1024;
const AUDIT_ROTATE: u64 = 1024 * 1024;
/// 사슬마다 기억하는 이전 접근 토큰 지문 수. 갱신은 몇 시간에 한 번이라 claude 의 30초 캐시를 넘기에 넉넉하다.
const LINEAGE: usize = 4;
/// 힌트를 놓쳐도 두 번의 5분 pull 안에는 옛 Bearer 를 같은 세대의 새 토큰에 결속할 수 있어야 한다.
const LINEAGE_TTL_MS: u64 = 10 * 60 * 1000;

pub(crate) mod recovery;
pub use recovery::{Recovered, Ticket};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 접근 토큰의 지문. 관문과 기기가 같은 값을 낸다 — 관문은 사슬이 낸 이전 토큰을 이것으로만 기억해 알리고,
/// 기기는 401 난 Bearer 가 어느 사슬의 것이었는지 토큰 없이 가린다(`agent_chains/recovery.rs`).
pub(crate) fn fingerprint(token: &str) -> String {
    use sha2::Digest as _;
    let mut hash = sha2::Sha256::new();
    hash.update(b"kasa.agent-chains.access\0");
    hash.update(token.as_bytes());
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn valid_fingerprint(fp: &str) -> bool {
    fp.len() == 64 && fp.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// ── 관문 쪽 ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
struct Chain {
    email: String,
    #[serde(default)]
    org: String,
    access_token: String,
    expires_at: u64,
    refresh_token: String,
    #[serde(default)]
    refresh_expires_at: u64,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    subscription_type: Option<String>,
    #[serde(default)]
    rate_limit_tier: Option<String>,
    sealed_at: u64,
    updated: u64,
    /// 일시 실패 뒤 다음 시도 시각.
    #[serde(default)]
    retry_at: u64,
    /// 서버가 갱신 토큰을 거부했다 — 어느 기기에서든 다시 로그인해야 이어진다.
    #[serde(default)]
    broken: bool,
    /// 「곧 끊긴다」 알림을 이미 보냈다. 갱신에 성공하면 내린다.
    #[serde(default)]
    warned: bool,
    /// 봉인 세대. 새로 맡을 때마다 바뀌고 갱신으로는 안 바뀐다 — 다시 로그인해 맡긴 사슬의 토큰을 끊긴
    /// 옛 사슬의 401 에 내주지 않게 기기가 이것으로 가른다. 이 기능 전에 맡긴 사슬은 비어 있다가 처음 볼 때 붙는다.
    #[serde(default)]
    generation: String,
    /// 이 세대가 앞서 낸 접근 토큰들(오래된 것부터). 갱신하면 옛 토큰은 곧바로 죽는데 도는 claude 는 30초
    /// 캐시로 그것을 더 보낸다 — 기기가 그 401 을 이 사슬의 것으로 알아보게 알려 준다. 끊기면 비운다.
    #[serde(default)]
    lineage: Vec<Spent>,
    /// 관문이 공급자 프로필에서 확인한 계정·조직 UUID. 팀 열쇠는 조직명이라 열쇠가 같아도 사람이 다를 수 있다 —
    /// 기기는 이것으로 받을 토큰의 주인을 가른다. 이 기능 전에 맡긴 사슬은 다음 갱신 때 채운다.
    #[serde(default)]
    account_uuid: String,
    #[serde(default)]
    organization_uuid: String,
}

/// 사슬이 앞서 낸 접근 토큰 하나 — 지문과 그 토큰의 원래 만료만, 토큰 값은 없다.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Spent {
    pub fingerprint: String,
    pub expires_at: u64,
    /// 같은 계정이 다시 봉인된 뒤 옛 지문이 새 사슬에 붙지 않게 한다.
    #[serde(default)]
    pub generation: String,
}

/// 공급자의 계정·조직 UUID 쌍 — 토큰의 주인을 가르는 유일한 근거. 이메일·조직명·사슬 열쇠는 이름일 뿐이다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Who {
    pub account_uuid: String,
    pub organization_uuid: String,
}

impl Who {
    pub(crate) fn new(account: &str, organization: &str) -> Option<Self> {
        let parse = |raw: &str| {
            let id = uuid::Uuid::parse_str(raw).ok()?;
            (raw.len() == 36 && !id.is_nil()).then(|| id.to_string())
        };
        Some(Self { account_uuid: parse(account)?, organization_uuid: parse(organization)? })
    }

    fn from_profile(profile: &Value) -> Option<Self> {
        let get = |path: &str| profile.pointer(path).and_then(Value::as_str).unwrap_or_default();
        Self::new(get("/account/uuid"), get("/organization/uuid"))
    }

    fn of_held(chain: &Held) -> Option<Self> {
        Self::new(&chain.account_uuid, &chain.organization_uuid)
    }
}

fn new_generation() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 세대 없이 맡겨진 사슬(이 기능 전)에 세대를 붙인다. 붙였으면 true.
fn settle_generations(book: &mut Book) -> bool {
    let mut changed = false;
    for chain in book.values_mut().filter(|c| c.generation.is_empty() && !c.broken) {
        chain.generation = new_generation();
        changed = true;
    }
    changed
}

impl Chain {
    fn access(&self) -> Access {
        Access {
            access_token: self.access_token.clone(),
            expires_at: self.expires_at,
            scopes: self.scopes.clone(),
            subscription_type: self.subscription_type.clone(),
            rate_limit_tier: self.rate_limit_tier.clone(),
        }
    }

    fn label(&self) -> String {
        let org = self.org.trim();
        if org.is_empty() || org.to_lowercase().contains(&self.email.to_lowercase()) {
            self.email.clone()
        } else {
            format!("{org}({})", self.email)
        }
    }

    fn who(&self) -> Option<Who> {
        Who::new(&self.account_uuid, &self.organization_uuid)
    }

    fn due(&self, now: u64) -> bool {
        !self.broken
            && now >= self.sealed_at + SEAL_SETTLE_MS
            && now >= self.retry_at
            && self.expires_at < now + REFRESH_AHEAD_MS
    }
}

/// 관문 계정 하나가 맡긴 사슬들. 계정 열쇠(`agent_accounts::account_key`) → 사슬.
type Book = BTreeMap<String, Chain>;

/// 기기가 맡기는 사슬. 이름은 claude 키체인의 `claudeAiOauth` 와 같다.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    #[serde(default)]
    pub refresh_token_expires_at: u64,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
    #[serde(default)]
    pub rate_limit_tier: Option<String>,
}

/// 기기에 건네는 접근 토큰 한 벌 — 갱신 토큰은 없다.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    pub access_token: String,
    pub expires_at: u64,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
    #[serde(default)]
    pub rate_limit_tier: Option<String>,
}

/// 관문이 쥔 사슬 하나를 기기에서 본 모양.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Held {
    pub key: String,
    pub email: String,
    #[serde(default)]
    pub org: String,
    /// `ok` 또는 `reconnect_required`.
    pub state: String,
    #[serde(default)]
    pub access: Option<Access>,
    /// 관문이 마지막으로 맡거나 갱신한 시각(ms). 첫 실제 갱신을 확인할 때 본다.
    #[serde(default)]
    pub updated: u64,
    /// 봉인 세대(`Chain::generation`). 옛 관문은 안 보낸다 — 비어 있으면 기기는 이 사슬의 401 을 살리지 않는다.
    #[serde(default)]
    pub generation: String,
    /// 이 세대의 이전 접근 토큰(`Chain::lineage`).
    #[serde(default)]
    pub lineage: Vec<Spent>,
    /// 관문이 공급자에서 확인한 UUID(`Chain::account_uuid`). 옛 관문은 안 보낸다 — 기기가 직접 확인한다.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_uuid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organization_uuid: String,
}

/// 관문이 사람에게 알릴 일 — 그 관문 계정의 폰으로 간다(`gateway_agent_chains.rs`).
pub(crate) struct Notice {
    pub account: String,
    /// 사슬 열쇠 — 같은 사슬의 알림은 폰에서 한 자리를 갈아 끼운다.
    pub key: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    /// 맡긴 접근 토큰으로 신원을 못 읽었다(만료·폐기).
    Rejected,
    Provider,
    Storage,
    Full,
    /// 같은 열쇠의 사슬이 다른 공급자 계정이다 — 팀 열쇠(조직명)가 겹친 경우. 어느 쪽도 바꾸지 않는다.
    Conflict,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "bad_request",
            Self::Rejected => "token_rejected",
            Self::Provider => "provider_unavailable",
            Self::Storage => "storage_unavailable",
            Self::Full => "too_many_chains",
            Self::Conflict => "identity_conflict",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid => 400,
            // 401 이 아니다 — 기기는 401 을 「관문 로그인이 풀렸다」로 읽는다.
            Self::Rejected => 422,
            Self::Provider => 502,
            Self::Storage => 503,
            Self::Full | Self::Conflict => 409,
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

#[derive(Clone)]
pub(crate) struct Endpoints {
    pub token: String,
    pub profile: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            token: "https://platform.claude.com/v1/oauth/token".into(),
            profile: "https://api.anthropic.com/api/oauth/profile".into(),
        }
    }
}

pub struct Service {
    vault: crate::sealed::Vault,
    audit: PathBuf,
    /// 사슬 읽고-고치고-쓰기를 한 줄로 세운다. 갱신 토큰은 한 번만 쓸 수 있어 두 갱신이 겹치면 안 된다.
    books: tokio::sync::Mutex<()>,
    pub(crate) endpoints: Endpoints,
    http: reqwest::Client,
}

fn opaque(token: &str) -> bool {
    (1..=MAX_TOKEN).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_scope(scope: &str) -> bool {
    (1..=64).contains(&scope.len()) && scope.bytes().all(|b| b.is_ascii_graphic())
}

async fn bounded(mut response: reqwest::Response) -> Result<Value, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Provider)? {
        if body.len() + chunk.len() > MAX_RESPONSE {
            return Err(Error::Provider);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Error::Provider)
}

impl Service {
    pub(crate) fn open(state_path: Option<&Path>) -> Option<Arc<Self>> {
        let directory = state_path?.with_file_name("agent-chains");
        let vault = match crate::sealed::Vault::open(directory.clone(), "kasa.agent-chains.v1") {
            Ok(vault) => vault,
            Err(_) => {
                eprintln!("[gateway] agent chains disabled: storage_unavailable");
                return None;
            }
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("KASA-AgentChains/1")
            .build()
            .ok()?;
        Some(Arc::new(Self {
            vault,
            audit: directory.join("audit.jsonl"),
            books: tokio::sync::Mutex::new(()),
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

    fn record(&self, account: &str, device: &str, action: &str, key: &str, result: &str) {
        let line = json!({"at":crate::relay_auth::now_secs(),"account":account,"device":device,
            "action":action,"key":key,"result":result});
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

    /// 그 접근 토큰의 주인(이메일, 조직명, 계정·조직 UUID). 기기가 적어 보낸 신원은 믿지 않는다.
    async fn identity(&self, access_token: &str) -> Result<(String, String, Option<Who>), Error> {
        let response = self
            .http
            .get(&self.endpoints.profile)
            .bearer_auth(access_token)
            .header("anthropic-beta", "oauth-2025-04-20")
            .send()
            .await
            .map_err(|_| Error::Provider)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(Error::Rejected),
            _ => return Err(Error::Provider),
        }
        let profile = bounded(response).await?;
        let email = profile
            .pointer("/account/email")
            .and_then(Value::as_str)
            .filter(|email| email.contains('@') && email.len() <= 200)
            .ok_or(Error::Provider)?;
        let org = profile
            .pointer("/organization/name")
            .and_then(Value::as_str)
            .filter(|org| org.len() <= 200 && !org.chars().any(char::is_control))
            .unwrap_or_default();
        Ok((email.to_string(), org.to_string(), Who::from_profile(&profile)))
    }

    /// 기기가 사슬을 맡긴다. 같은 계정의 멀쩡한 사슬이 이미 있으면 그것을 지키고 그 접근 토큰을
    /// 돌려준다(`held: true`) — 기기는 자기 사슬을 버리고 관문 것을 쓴다.
    pub async fn seal(&self, account: &str, device: &str, offer: Offer) -> Result<Value, Error> {
        let now = now_ms();
        if !opaque(&offer.access_token)
            || !opaque(&offer.refresh_token)
            || offer.expires_at <= now + 10 * 60 * 1000
            || offer.scopes.len() > 16
            || !offer.scopes.iter().all(|scope| valid_scope(scope))
            || !offer.scopes.iter().any(|scope| scope == "user:inference")
        {
            return Err(Error::Invalid);
        }
        let (email, org, who) = self.identity(&offer.access_token).await?;
        let key = crate::agent_accounts::account_key("claude", &email, &org, "");
        let _guard = self.books.lock().await;
        let mut book = self.load(account)?;
        if let Some(existing) = book.get(&key).filter(|c| !c.broken && c.expires_at > now + 10 * 60 * 1000) {
            if existing.who().zip(who.as_ref()).is_some_and(|(held, offered)| &held != offered) {
                self.record(account, device, "seal", &key, "identity_conflict");
                return Err(Error::Conflict);
            }
            self.record(account, device, "seal", &key, "held");
            let held = existing.who();
            return Ok(json!({"ok":true,"key":key,"held":true,"access":existing.access(),
                "account_uuid":held.as_ref().map(|w| w.account_uuid.as_str()),
                "organization_uuid":held.as_ref().map(|w| w.organization_uuid.as_str())}));
        }
        if !book.contains_key(&key) && book.len() >= MAX_CHAINS {
            return Err(Error::Full);
        }
        let chain = Chain {
            email,
            org,
            access_token: offer.access_token,
            expires_at: offer.expires_at,
            refresh_token: offer.refresh_token,
            refresh_expires_at: offer.refresh_token_expires_at,
            scopes: offer.scopes,
            subscription_type: offer.subscription_type,
            rate_limit_tier: offer.rate_limit_tier,
            sealed_at: now,
            updated: now,
            retry_at: 0,
            broken: false,
            warned: false,
            generation: new_generation(),
            lineage: Vec::new(),
            account_uuid: who.as_ref().map(|w| w.account_uuid.clone()).unwrap_or_default(),
            organization_uuid: who.as_ref().map(|w| w.organization_uuid.clone()).unwrap_or_default(),
        };
        let access = chain.access();
        book.insert(key.clone(), chain);
        self.save(account, &book)?;
        self.record(account, device, "seal", &key, "sealed");
        Ok(json!({"ok":true,"key":key,"held":false,"access":access,
            "account_uuid":who.as_ref().map(|w| w.account_uuid.as_str()),
            "organization_uuid":who.as_ref().map(|w| w.organization_uuid.as_str())}))
    }

    /// 이 관문 계정이 맡긴 사슬들과 그 접근 토큰.
    pub async fn list(&self, account: &str) -> Result<Value, Error> {
        let _guard = self.books.lock().await;
        let mut book = self.load(account)?;
        let stored = book.clone();
        // 붙인 세대를 못 남기면 다음에 다른 세대를 보이게 된다 — 그럴 바엔 세대 없이 보낸다(기기는 복구를 끈다).
        if settle_generations(&mut book) && self.save(account, &book).is_err() {
            book = stored;
        }
        let now = now_ms();
        let chains: Vec<Held> = book
            .iter()
            .map(|(key, chain)| Held {
                key: key.clone(),
                email: chain.email.clone(),
                org: chain.org.clone(),
                state: if chain.broken { "reconnect_required" } else { "ok" }.into(),
                access: (!chain.broken).then(|| chain.access()),
                updated: chain.updated,
                generation: if chain.broken { String::new() } else { chain.generation.clone() },
                lineage: if chain.broken || chain.updated.saturating_add(LINEAGE_TTL_MS) < now {
                    Vec::new()
                } else {
                    chain
                        .lineage
                        .iter()
                        .filter(|spent| {
                            spent.expires_at > now
                                && spent.generation == chain.generation
                                && valid_fingerprint(&spent.fingerprint)
                        })
                        .rev()
                        .take(LINEAGE)
                        .cloned()
                        .collect()
                },
                account_uuid: chain.account_uuid.clone(),
                organization_uuid: chain.organization_uuid.clone(),
            })
            .collect();
        Ok(json!({"ok":true,"chains":chains}))
    }

    /// 만료가 다가온 사슬을 갱신한다. 갱신한 수, 사람에게 알릴 일, 그리고 기기가 다시 받아 가야 할 관문
    /// 계정(갱신·끊김이 봉인 저장소에 저장까지 된 것만)을 돌려준다.
    pub(crate) async fn refresh_due(&self) -> (usize, Vec<Notice>, Vec<String>) {
        let mut refreshed = 0;
        let mut notices = Vec::new();
        let mut changed_accounts = Vec::new();
        for account in self.vault.accounts() {
            let _guard = self.books.lock().await;
            let Ok(mut book) = self.load(&account) else { continue };
            let now = now_ms();
            let due: Vec<String> = book.iter().filter(|(_, c)| c.due(now)).map(|(k, _)| k.clone()).collect();
            if due.is_empty() {
                continue;
            }
            settle_generations(&mut book);
            let mut changed = false;
            for key in due {
                let chain = book[&key].clone();
                match self.refresh(&chain, now).await {
                    Ok(next) => {
                        book.insert(key.clone(), next);
                        refreshed += 1;
                        changed = true;
                        self.record(&account, "", "refresh", &key, "ok");
                    }
                    Err(Error::Rejected) => {
                        let chain = book.get_mut(&key).expect("key came from the book");
                        chain.broken = true;
                        chain.access_token.clear();
                        chain.refresh_token.clear();
                        chain.lineage.clear();
                        chain.updated = now;
                        changed = true;
                        self.record(&account, "", "refresh", &key, "reconnect_required");
                        notices.push(Notice {
                            account: account.clone(),
                            key: key.clone(),
                            title: "Claude 로그인이 끊겼어요".into(),
                            body: format!("{} — 아무 기기에서 한 번 다시 로그인하면 모든 기기가 이어져요", chain.label()),
                        });
                    }
                    Err(error) => {
                        let chain = book.get_mut(&key).expect("key came from the book");
                        chain.retry_at = now + RETRY_MS;
                        eprintln!("[agent-chains] 갱신 일시 실패({}) — {}분 뒤 다시", error.code(), RETRY_MS / 60_000);
                        if !chain.warned && chain.expires_at < now + ALERT_LEFT_MS {
                            chain.warned = true;
                            self.record(&account, "", "refresh", &key, "stalled");
                            notices.push(Notice {
                                account: account.clone(),
                                key: key.clone(),
                                title: "Claude 로그인 갱신이 막혔어요".into(),
                                body: format!("{} — {}분 안에 갱신 못 하면 모든 기기에서 끊겨요",
                                    chain.label(), chain.expires_at.saturating_sub(now) / 60_000),
                            });
                        }
                    }
                }
            }
            if self.save(&account, &book).is_err() {
                eprintln!("[agent-chains] 갱신 결과 저장 실패 — 다음 갱신은 옛 토큰으로 나간다");
            } else if changed {
                changed_accounts.push(account.clone());
            }
        }
        (refreshed, notices, changed_accounts)
    }

    async fn refresh(&self, chain: &Chain, now: u64) -> Result<Chain, Error> {
        let mut body = json!({"grant_type":"refresh_token","refresh_token":chain.refresh_token,"client_id":CLIENT_ID});
        if !chain.scopes.is_empty() {
            body["scope"] = json!(chain.scopes.join(" "));
        }
        let response = self
            .http
            .post(&self.endpoints.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| Error::Provider)?;
        let status = response.status().as_u16();
        let value = bounded(response).await.unwrap_or(Value::Null);
        if status == 401 || (status == 400 && value["error"] == "invalid_grant") {
            return Err(Error::Rejected);
        }
        if status != 200 {
            eprintln!("[agent-chains] 토큰 창구 응답 {status}");
            return Err(Error::Provider);
        }
        let access = value["access_token"].as_str().filter(|t| opaque(t)).ok_or(Error::Provider)?;
        let expires_in = value["expires_in"].as_u64().filter(|s| *s > 0).ok_or(Error::Provider)?;
        let mut next = chain.clone();
        next.access_token = access.into();
        next.expires_at = now + expires_in * 1000;
        // 회전된 갱신 토큰을 반드시 남긴다 — 옛것은 이미 쓴 값이다.
        if let Some(rotated) = value["refresh_token"].as_str().filter(|t| opaque(t)) {
            next.refresh_token = rotated.into();
        }
        if let Some(secs) = value["refresh_token_expires_in"].as_u64() {
            next.refresh_expires_at = now + secs * 1000;
        }
        if let Some(scope) = value["scope"].as_str() {
            let scopes: Vec<String> = scope.split(' ').filter(|s| valid_scope(s)).map(str::to_string).collect();
            if !scopes.is_empty() {
                next.scopes = scopes;
            }
        }
        next.lineage.retain(|spent| {
            spent.expires_at > now
                && spent.generation == next.generation
                && valid_fingerprint(&spent.fingerprint)
        });
        next.lineage.push(Spent {
            fingerprint: fingerprint(&chain.access_token),
            expires_at: chain.expires_at,
            generation: chain.generation.clone(),
        });
        let spare = next.lineage.len().saturating_sub(LINEAGE);
        next.lineage.drain(..spare);
        next.updated = now;
        next.retry_at = 0;
        next.warned = false;
        if next.who().is_none() {
            // 기능 전에 맡긴 사슬: 갓 받은 토큰으로 주인을 확인한다. 열쇠가 어긋나면 비워 두어 기기가 직접 확인하게 한다.
            if let Ok((email, org, Some(who))) = self.identity(&next.access_token).await {
                if crate::agent_accounts::account_key("claude", &email, &org, "")
                    == crate::agent_accounts::account_key("claude", &next.email, &next.org, "")
                {
                    next.account_uuid = who.account_uuid;
                    next.organization_uuid = who.organization_uuid;
                }
            }
        }
        Ok(next)
    }
}

// ── 기기 쪽 ─────────────────────────────────────────────────────────────────────

/// 그 슬롯 저장소의 `claudeAiOauth`.
fn oauth(doc: &Value) -> Option<&serde_json::Map<String, Value>> {
    doc.get("claudeAiOauth")?.as_object()
}

fn text<'a>(doc: &'a Value, field: &str) -> Option<&'a str> {
    oauth(doc)?.get(field)?.as_str().filter(|s| !s.is_empty())
}

fn expires(doc: &Value) -> u64 {
    oauth(doc).and_then(|o| o.get("expiresAt")).and_then(Value::as_u64).unwrap_or(0)
}

/// 맡길 만한 사슬인가 — 갱신 토큰이 있고 접근 토큰이 넉넉히 남았다.
fn offer_of(doc: &Value, now: u64) -> Option<Offer> {
    let refresh = text(doc, "refreshToken")?;
    let access = text(doc, "accessToken")?;
    if expires(doc) <= now + OFFER_MIN_LEFT_MS {
        return None;
    }
    let o = oauth(doc)?;
    let string = |field: &str| o.get(field).and_then(Value::as_str).map(str::to_string);
    Some(Offer {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: expires(doc),
        refresh_token_expires_at: o.get("refreshTokenExpiresAt").and_then(Value::as_u64).unwrap_or(0),
        scopes: o
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default(),
        subscription_type: string("subscriptionType"),
        rate_limit_tier: string("rateLimitTier"),
    })
}

/// 관문 토큰으로 채울 자리인가. 자기 갱신 토큰이 있는 슬롯은 건드리지 않는다 — 그건 맡길 차례를
/// 기다리거나 claude 가 막 회전시키는 중이다. 빈 갱신 토큰("")은 claude 가 죽은 사슬에 남기는 표시라
/// 없는 것으로 본다.
fn wants_fill(doc: Option<&Value>, access: &Access, now: u64) -> bool {
    if doc.and_then(|d| text(d, "refreshToken")).is_some() {
        return false;
    }
    let mine = doc.filter(|d| text(d, "accessToken").is_some()).map_or(0, expires);
    access.expires_at > now
        && access.expires_at > mine
        && doc.and_then(|d| text(d, "accessToken")) != Some(access.access_token.as_str())
}

/// 그 자리에 접근 토큰을 싣는다. 갱신 토큰 칸은 **지운다** — 빈 문자열로 두면 claude 가 죽은 사슬로
/// 읽는다. 같은 항목의 다른 칸(MCP 서버 OAuth 등)은 그대로 둔다.
fn with_access(doc: Option<&Value>, access: &Access) -> Value {
    let mut doc = doc.filter(|d| d.is_object()).cloned().unwrap_or_else(|| json!({}));
    let mut o = doc.get("claudeAiOauth").and_then(Value::as_object).cloned().unwrap_or_default();
    o.remove("refreshToken");
    o.remove("refreshTokenExpiresAt");
    o.insert("accessToken".into(), json!(access.access_token));
    o.insert("expiresAt".into(), json!(access.expires_at));
    if !access.scopes.is_empty() {
        o.insert("scopes".into(), json!(access.scopes));
    }
    if let Some(kind) = &access.subscription_type {
        o.insert("subscriptionType".into(), json!(kind));
    }
    if let Some(tier) = &access.rate_limit_tier {
        o.insert("rateLimitTier".into(), json!(tier));
    }
    doc["claudeAiOauth"] = Value::Object(o);
    doc
}

/// 저장소 한 칸을 읽은 결과. 잠긴 키체인처럼 모르는 경우는 「없음」과 갈라야 한다 — 없음으로
/// 읽고 덮으면 살아 있는 로그인을 지운다.
pub(crate) enum Read {
    Present(Value),
    Absent,
    Unknown,
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn bounded_child_output(
    mut child: std::process::Child,
    timeout: Duration,
    limit: usize,
) -> std::io::Result<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Read as _;
    use std::sync::mpsc::TryRecvError;

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("child stdout missing"));
    };
    let (sent, received) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("kasa-chain-store-read".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(limit as u64 + 1).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sent.send(result);
        });
    if let Err(error) = reader {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut status = None;
    let mut output = None;
    loop {
        match received.try_recv() {
            Ok(Ok(bytes)) if bytes.len() <= limit => output = Some(bytes),
            Ok(Ok(_)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "child output too large"));
            }
            Ok(Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Err(TryRecvError::Disconnected) if output.is_none() => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "child output reader stopped"));
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(done) => status = done,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        if status.is_some() && output.is_some() {
            return Ok((status.expect("checked"), output.take().expect("checked")));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "child output timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "macos")]
fn read_store(store: Option<&Path>) -> Read {
    let dir = store.and_then(Path::to_str);
    let Some(user) = crate::http::keychain_user() else { return Read::Unknown };
    let service = crate::http::claude_keychain_service(dir);
    let Ok(child) = crate::no_window_command("security")
        .args(["find-generic-password", "-a", &user, "-w", "-s", &service])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Read::Unknown;
    };
    let Ok((status, stdout)) = bounded_child_output(child, Duration::from_secs(5), 8 * 1024 * 1024) else {
        return Read::Unknown;
    };
    match status {
        status if status.success() => match serde_json::from_slice(stdout.trim_ascii()) {
            Ok(v) => Read::Present(v),
            Err(_) => Read::Unknown,
        },
        // 44 = errSecItemNotFound.
        status if status.code() == Some(44) => Read::Absent,
        _ => Read::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn write_store(store: Option<&Path>, doc: &Value) -> bool {
    use std::io::Write as _;
    let Some(user) = crate::http::keychain_user() else { return false };
    let Ok(body) = serde_json::to_string(doc) else { return false };
    let hex: String = body.bytes().map(|b| format!("{b:02x}")).collect();
    let service = crate::http::claude_keychain_service(store.and_then(Path::to_str));
    let line = format!("add-generic-password -U -a \"{user}\" -s \"{service}\" -X \"{hex}\"\n");
    // `security -i` 의 한 줄 상한(claude 와 같은 4032). 넘으면 claude 는 argv 로 넘기지만 우리는 토큰을
    // argv 에 싣지 않는다 — 그 자리는 건너뛴다.
    if line.len() > 4032 {
        eprintln!("[agent-chains] 키체인 항목이 너무 커서 건너뜀");
        return false;
    }
    let Ok(mut child) = crate::no_window_command("security")
        .arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(line.as_bytes()).is_ok());
    if !wrote {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    bounded_child_output(child, Duration::from_secs(5), 64 * 1024)
        .is_ok_and(|(status, _)| status.success())
}

#[cfg(not(target_os = "macos"))]
fn credentials_file(store: Option<&Path>) -> Option<PathBuf> {
    match store {
        Some(dir) => Some(dir.join(".credentials.json")),
        None => Some(kasa_socket::home_dir()?.join(".claude/.credentials.json")),
    }
}

#[cfg(not(target_os = "macos"))]
fn read_store(store: Option<&Path>) -> Read {
    let Some(path) = credentials_file(store) else { return Read::Unknown };
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_or(Read::Unknown, Read::Present),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::Absent,
        Err(_) => Read::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
fn write_store(store: Option<&Path>, doc: &Value) -> bool {
    let Some(path) = credentials_file(store) else { return false };
    let Ok(body) = serde_json::to_string(doc) else { return false };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    crate::relay_auth::write_private(&path, &body).is_ok()
}

/// 이 기기의 Claude 슬롯 하나 — 실제 저장소와, 적어 둔 신원으로 만든 계정 열쇠.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    pub id: String,
    pub store: Option<PathBuf>,
    pub table_key: Option<String>,
    /// 고른 슬롯이 작업대(기본 자리)에 실려 있다. 작업대는 바깥 `/login` 이 언제든 다른 계정으로 바꿀 수 있어
    /// 슬롯의 계정을 알려 주지 못한다 — 여기서 본 토큰으로는 슬롯 매핑을 배우거나 바꾸지 않는다.
    pub bench: bool,
    /// 작업대 소유 표식이 이 슬롯 이름으로 적어 둔 UUID — 앱이 금고를 작업대에 실을 때 그 토큰으로 확인한 값이다.
    /// 막 로그인해 곧바로 고른 슬롯은 아직 매핑이 없으니 이것으로 작업대의 주인을 증명한다.
    pub stamped: Option<Who>,
}

/// 슬롯 저장소를 읽고 쓰는 자리. 앱은 키체인(맥)·자격증명 파일이고, 시험은 메모리다 — 시험이
/// 사용자 키체인을 건드리면 안 된다.
pub(crate) trait Stores {
    fn read(&self, slot: &Slot) -> Read;
    // Read-only recovery probes cannot acquire a write capability.
    fn compare_and_write(&self, _slot: &Slot, _expected: Option<&Value>, _doc: &Value) -> bool { false }
    /// 쓰기 직전에 그 슬롯이 아직 같은 저장소를 가리키나 — 그 사이 계정을 바꿨으면 작업대에는
    /// 다른 계정이 실려 있다.
    fn same(&self, slot: &Slot) -> bool;
}

struct Local;

impl Stores for Local {
    fn read(&self, slot: &Slot) -> Read {
        read_store(slot.store.as_deref())
    }

    fn same(&self, slot: &Slot) -> bool {
        load_settings().is_some_and(|(path, settings)| {
            local_slots(path.parent(), &settings).iter().any(|current|
                current == slot)
        })
    }
}

struct GuardedLocal<'a>(&'a crate::device_auth::Stamp);

impl Stores for GuardedLocal<'_> {
    fn read(&self, slot: &Slot) -> Read { Local.read(slot) }
    fn same(&self, slot: &Slot) -> bool { Local.same(slot) }

    fn compare_and_write(&self, slot: &Slot, expected: Option<&Value>, doc: &Value) -> bool {
        // Phone promotion holds this same device lock; a delayed pull must not erase a new login.
        crate::device_auth::with_current(self.0, || {
            if !self.same(slot) || !matches_document(self.read(slot), expected) { return Ok(false); }
            Ok(write_store(slot.store.as_deref(), doc))
        }).unwrap_or(false)
    }
}

fn matches_document(current: Read, expected: Option<&Value>) -> bool {
    match (current, expected) {
        (Read::Present(current), Some(expected)) => &current == expected,
        (Read::Absent, None) => true,
        _ => false,
    }
}

fn load_settings() -> Option<(PathBuf, Value)> {
    let path = crate::agent_accounts::settings_path()?;
    let settings = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok())?;
    Some((path, settings))
}

fn local_slots(base: Option<&Path>, settings: &Value) -> Vec<Slot> {
    let remembered = |map: &str, id: &str| {
        settings.get(map).and_then(|m| m.get(id)).and_then(Value::as_str).unwrap_or_default().to_string()
    };
    let stamp = base
        .and_then(|base| std::fs::read_to_string(base.join("claude-accounts/_active/workbench-stamp.json")).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_default();
    let stamped = |id: &str| {
        (!id.is_empty() && stamp["account"].as_str() == Some(id))
            .then(|| Who::new(stamp["identity"]["account_uuid"].as_str()?, stamp["identity"]["organization_uuid"].as_str()?))
            .flatten()
    };
    let mut ids: Vec<String> = crate::agent_accounts::slots(settings, "claude_accounts").into_iter().map(|(id, _)| id).collect();
    if settings.get("claude_account").and_then(Value::as_str).unwrap_or_default().is_empty() {
        ids.insert(0, String::new());
    }
    ids.into_iter()
        .map(|id| {
            let email = remembered("claude_account_emails", &id);
            let table_key = email
                .contains('@')
                .then(|| crate::agent_accounts::account_key("claude", &email, &remembered("claude_account_orgs", &id), ""));
            let store = crate::agent_accounts::claude_store(base, settings, &id);
            let bench = !id.is_empty() && store.is_none();
            let stamped = if bench { stamped(&id) } else { None };
            Slot { id, store, table_key, bench, stamped }
        })
        .collect()
}

fn map_path() -> Option<PathBuf> {
    Some(crate::agent_accounts::settings_path()?.with_file_name("agent-chains.json"))
}

/// 슬롯에 마지막으로 실은 토큰의 지문과 그 주인 UUID. 토큰은 없다. 관문이 사슬을 갈면 옛 토큰은 곧바로 죽어
/// 프로필로 다시 확인할 수 없다 — 우리가 실은 그 바이트가 그대로면 이 기록이 주인을 증명한다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Owner {
    pub key: String,
    pub fingerprint: String,
    #[serde(flatten)]
    pub who: Who,
}

#[derive(Default)]
pub(crate) struct Learned {
    pub keys: BTreeMap<String, String>,
    pub owners: BTreeMap<String, Owner>,
}

fn load_file() -> Value {
    map_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

/// 슬롯 id → 관문 사슬 열쇠. 맡기거나 받아 온 자리를 적는다. 토큰은 없다.
fn load_map() -> BTreeMap<String, String> {
    serde_json::from_value(load_file()["slots"].clone()).unwrap_or_default()
}

fn load_owners() -> BTreeMap<String, Owner> {
    let file = load_file();
    let Some(owners) = file.get("owners").and_then(Value::as_object) else { return BTreeMap::new() };
    // 한 줄이 깨졌다고 나머지 증명을 버리지 않는다.
    owners.iter().filter_map(|(id, v)| Some((id.clone(), serde_json::from_value(v.clone()).ok()?))).collect()
}

fn save_map(map: &BTreeMap<String, String>, owners: &BTreeMap<String, Owner>) {
    if let Some(path) = map_path() {
        let _ = crate::relay_auth::write_private(&path, &json!({"slots": map, "owners": owners}).to_string());
    }
}

/// 화면용 마지막 관측 — 열쇠와 상태뿐, 토큰은 오래 쥐지 않는다.
fn seen() -> &'static std::sync::Mutex<Vec<(String, String)>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<Vec<(String, String)>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// 관문이 그 계정 사슬을 멀쩡히 쥐고 있나(마지막 관측).
pub fn held(key: &str) -> bool {
    seen().lock().is_ok_and(|s| s.iter().any(|(k, state)| k == key && state == "ok"))
}

/// 그 슬롯을 관문 사슬로 채우게 한다 — 「다른 기기에 있는 계정」을 이 기기에 붙일 때.
pub fn adopt(slot: &str, key: &str) {
    let mut map = load_map();
    let mut owners = load_owners();
    map.insert(slot.to_string(), key.to_string());
    owners.remove(slot);
    save_map(&map, &owners);
    poke();
}

fn waker() -> &'static (std::sync::Mutex<bool>, std::sync::Condvar) {
    static WAKER: (std::sync::Mutex<bool>, std::sync::Condvar) = (std::sync::Mutex::new(false), std::sync::Condvar::new());
    &WAKER
}

/// 다음 주기를 기다리지 않고 곧바로 한 번 돈다 — 로그인·슬롯 추가 뒤, 관문이 사슬이 바뀌었다고 알릴 때.
/// 관문은 갱신하자마자 옛 접근 토큰을 죽이므로 이 깨움이 늦은 만큼 claude 칸이 401 을 본다.
pub fn poke() {
    let (poked, wake) = waker();
    if let Ok(mut poked) = poked.lock() {
        *poked = true;
        wake.notify_all();
    }
}

/// 다음 주기까지 쉰다. `poke` 가 오면 곧바로 깬다.
fn rest(period: Duration) {
    let (poked, wake) = waker();
    let Ok(guard) = poked.lock() else { return std::thread::sleep(period) };
    if let Ok((mut guard, _)) = wake.wait_timeout_while(guard, period, |poked| !*poked) {
        *guard = false;
    }
}

#[derive(Clone)]
struct Sealed {
    key: String,
    access: Access,
    /// 관문에 이미 같은 계정 사슬이 있어 우리 것을 받지 않았다 — 내준 토큰은 그 사슬의 것이다.
    held: bool,
    who: Option<Who>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyncError {
    Unauthorized,
    Unsupported,
    Failed,
}

pub(crate) struct Gateway<'a> {
    base: String,
    token: &'a str,
    http: reqwest::Client,
    profile: String,
    /// 이번 주기에 확인한 토큰 지문 → 주인. 같은 토큰을 한 주기에 두 번 묻지 않는다.
    proofs: std::sync::Mutex<HashMap<String, Option<Who>>>,
}

impl<'a> Gateway<'a> {
    pub(crate) fn new(base: &str, token: &'a str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http,
            profile: Endpoints::default().profile,
            proofs: Default::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_profile(mut self, profile: &str) -> Self {
        self.profile = profile.to_string();
        self
    }

    /// 그 접근 토큰의 주인 UUID. 막 만료·폐기된 토큰이면 모른다(None) — 그때는 덮지 않는다.
    async fn prove(&self, token: &str) -> Option<Who> {
        let fp = fingerprint(token);
        if let Some(known) = self.proofs.lock().ok().and_then(|p| p.get(&fp).cloned()) {
            return known;
        }
        #[cfg(not(test))]
        if let Some(cached) = crate::verified_claude_identity_for_token(token) {
            return Who::new(&cached.account_uuid, &cached.organization_uuid);
        }
        let found = async {
            let response = self.http.get(&self.profile).bearer_auth(token)
                .header("anthropic-beta", "oauth-2025-04-20").send().await.ok()?;
            if response.status().as_u16() != 200 {
                return None;
            }
            Who::from_profile(&bounded(response).await.ok()?)
        }.await;
        if let Ok(mut proofs) = self.proofs.lock() {
            proofs.insert(fp, found.clone());
        }
        found
    }

    /// 관문이 내준 사슬 토큰의 주인 — 관문이 공급자에서 확인한 UUID, 옛 관문이면 그 (갓 받은) 토큰으로 직접.
    async fn chain_who(&self, chain: &Held) -> Option<Who> {
        match Who::of_held(chain) {
            Some(who) => Some(who),
            None => self.prove(&chain.access.as_ref()?.access_token).await,
        }
    }

    /// 저장소에 지금 든 토큰의 주인. 우리가 실은 바이트 그대로면 소유 기록, 관문 사슬의 현재·이전 토큰이면 그 사슬,
    /// 아니면 프로필. 아무것도 증명하지 못하면 None — 부르는 쪽은 덮지 않는다.
    async fn current_who(&self, token: &str, owner: Option<&Owner>, chain: &Held) -> Option<Who> {
        let fp = fingerprint(token);
        if let Some(owner) = owner.filter(|o| o.fingerprint == fp) {
            return Some(owner.who.clone());
        }
        let issued = chain.access.as_ref().is_some_and(|a| a.access_token == token)
            || (!chain.generation.is_empty()
                && chain.lineage.iter().any(|spent| spent.fingerprint == fp && spent.generation == chain.generation));
        if issued {
            return self.chain_who(chain).await;
        }
        self.prove(token).await
    }

    async fn call(&self, request: reqwest::RequestBuilder) -> Result<Value, SyncError> {
        let response = request.bearer_auth(self.token).send().await.map_err(|_| SyncError::Failed)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(SyncError::Unauthorized),
            404 | 405 => return Err(SyncError::Unsupported),
            _ => return Err(SyncError::Failed),
        }
        let value = bounded(response).await.map_err(|_| SyncError::Failed)?;
        if value["ok"] != true {
            return Err(SyncError::Failed);
        }
        Ok(value)
    }

    async fn seal(&self, offer: &Offer) -> Result<Sealed, SyncError> {
        let value = self.call(self.http.post(format!("{}/relay/agent-chains/seal", self.base)).json(offer)).await?;
        let key = value["key"].as_str().ok_or(SyncError::Failed)?.to_string();
        let access = serde_json::from_value(value["access"].clone()).map_err(|_| SyncError::Failed)?;
        let text = |field: &str| value[field].as_str().unwrap_or_default();
        Ok(Sealed {
            key,
            access,
            held: value["held"] == true,
            who: Who::new(text("account_uuid"), text("organization_uuid")),
        })
    }

    async fn list(&self) -> Result<Vec<Held>, SyncError> {
        let value = self.call(self.http.get(format!("{}/relay/agent-chains", self.base))).await?;
        serde_json::from_value(value["chains"].clone()).map_err(|_| SyncError::Failed)
    }
}

/// 한 주기: 갱신 토큰이 있는 슬롯은 맡기고, 관문이 쥔 계정의 슬롯은 접근 토큰을 받아 채운다. 어느 쪽이든 저장소를
/// 바꾸기 전에 지금 든 토큰과 실을 토큰의 주인 UUID 가 같음을 증명해야 한다 — 증명이 없으면 덮지 않는다.
/// 이번에 새로 안 슬롯 → 사슬 열쇠·소유 기록과, 관문이 보여 준 사슬 목록을 돌려준다.
pub(crate) async fn sync_once(
    gateway: &Gateway<'_>,
    slots: &[Slot],
    map: &BTreeMap<String, String>,
    owners: &BTreeMap<String, Owner>,
    stores: &dyn Stores,
    authorized: &(dyn Fn() -> bool + Sync),
    ledger: &recovery::Ledger,
) -> Result<(Learned, Vec<Held>), SyncError> {
    let mut learned = Learned::default();
    // 네트워크가 막혀도 삭제된 실제 슬롯이 이전 성공 때의 복구 권한을 계속 갖지 않게 먼저 줄인다.
    ledger.manage(managed_keys(slots, map, &learned.keys));
    let now = now_ms();
    // 같은 사슬이 두 자리에 있으면 한 번만 맡긴다(갱신 토큰 → 결과).
    let mut offered: HashMap<String, Sealed> = HashMap::new();
    for slot in slots {
        let Read::Present(doc) = stores.read(slot) else { continue };
        let Some(offer) = offer_of(&doc, now) else { continue };
        // 작업대의 로그인은 그 슬롯의 것이라고 증명될 때만 맡긴다. 바깥 `/login` 의 다른 계정을 넘기고 갱신 토큰을
        // 지우면 그 로그인은 이 기기에서 슬롯 없이 사라진다 — 그대로 두면 계정 화면이 따로 들이게 안내한다.
        let recorded = map.get(&slot.id).and(owners.get(&slot.id)).map(|owner| &owner.who);
        if slot.bench && recorded.is_none() && slot.stamped.is_none() {
            continue;
        }
        let own = gateway.prove(&offer.access_token).await;
        if slot.bench && (own.is_none() || (own.as_ref() != recorded && own != slot.stamped)) {
            continue;
        }
        let sealed = match offered.get(&offer.refresh_token) {
            Some(done) => done.clone(),
            None => match gateway.seal(&offer).await {
                Ok(done) => {
                    offered.insert(offer.refresh_token.clone(), done.clone());
                    done
                }
                // 이 사슬 하나의 문제(토큰 거부·관문 일시 실패·같은 열쇠의 다른 계정)면 다음 슬롯으로 — 로그인은 그대로 둔다.
                Err(SyncError::Failed) => continue,
                Err(error) => return Err(error),
            },
        };
        // 관문이 받지 않고 자기 사슬을 내줬으면 그 주인이 우리 로그인과 같을 때만 갈아 끼운다. 다르거나 모르면
        // 우리 갱신 토큰을 그대로 둔다 — 관문은 그것을 받지 않았으니 두 곳이 같은 사슬을 돌리지 않는다.
        let who = if sealed.held {
            let held = match sealed.who.clone() {
                Some(who) => Some(who),
                None => gateway.prove(&sealed.access.access_token).await,
            };
            if held.is_none() || held != own {
                continue;
            }
            held
        } else {
            sealed.who.clone().or(own)
        };
        if !authorized() || !stores.same(slot) {
            continue;
        }
        // 그 사이 claude 가 갱신했으면 맡긴 사슬은 이미 낡았다 — 다음 주기에 새것을 맡긴다.
        let Read::Present(current) = stores.read(slot) else { continue };
        if text(&current, "refreshToken") != Some(offer.refresh_token.as_str()) {
            continue;
        }
        if stores.compare_and_write(slot, Some(&current), &with_access(Some(&current), &sealed.access)) {
            eprintln!("[agent-chains] {} 슬롯 사슬을 관문에 맡겼어요", display(&slot.id));
            learned.keys.insert(slot.id.clone(), sealed.key.clone());
            if let Some(who) = who {
                let fingerprint = fingerprint(&sealed.access.access_token);
                learned.owners.insert(slot.id.clone(), Owner { key: sealed.key.clone(), fingerprint, who });
            }
        }
    }
    let chains = gateway.list().await?;
    if let Ok(mut s) = seen().lock() {
        *s = chains.iter().map(|c| (c.key.clone(), c.state.clone())).collect();
    }
    ledger.manage(managed_keys(slots, map, &learned.keys));
    ledger.observe(&chains);
    for slot in slots {
        let mapped = learned.keys.get(&slot.id).or(map.get(&slot.id)).cloned();
        // 적어 둔 표시 신원은 후보를 찾는 힌트일 뿐이다 — 아래에서 지금 든 토큰의 주인으로 증명돼야 쓴다. 작업대에는 안 쓴다.
        let key = match (&mapped, &slot.table_key) {
            (Some(key), _) => key.clone(),
            (None, Some(key)) if !slot.bench => key.clone(),
            _ => continue,
        };
        let Some(chain) = chains.iter().find(|c| c.key == key && c.state == "ok") else { continue };
        let Some(access) = chain.access.as_ref() else { continue };
        let before = match stores.read(slot) {
            Read::Present(doc) => Some(doc),
            Read::Absent => None,
            Read::Unknown => continue,
        };
        let owner = learned.owners.get(&slot.id).or(owners.get(&slot.id)).cloned();
        let token = before.as_ref().and_then(|d| text(d, "accessToken")).map(str::to_string);
        if !wants_fill(before.as_ref(), access, now) {
            // 받을 것이 없어도 사슬 토큰을 그대로 들고 있으면 그 주인을 적어 둔다 — 관문이 사슬을 갈면 옛 토큰은
            // 곧바로 죽어 프로필로 못 묻는다. 이 기록이 그때 덮을 근거가 된다(이 기능 전 기기가 처음 돌 때도).
            if owner.is_none() && mapped.is_some() && token.as_deref() == Some(access.access_token.as_str()) {
                if let Some(who) = gateway.chain_who(chain).await.filter(|_| authorized() && stores.same(slot)) {
                    learned.owners.insert(slot.id.clone(), Owner { key, fingerprint: fingerprint(&access.access_token), who });
                }
            }
            continue;
        }
        if !authorized() || !stores.same(slot) {
            continue;
        }
        let Some(incoming) = gateway.chain_who(chain).await else { continue };
        let proven = match &token {
            Some(token) => gateway.current_who(token, owner.as_ref(), chain).await.as_ref() == Some(&incoming),
            // 빈 자리·껍데기는 잃을 자격이 없다. 그래도 사람이 고르거나 맡긴 매핑이 있어야 채운다.
            None => mapped.is_some(),
        };
        // 열쇠(팀은 조직명)가 같아도 슬롯에 적힌 주인과 UUID 가 다르면 다른 사람이다.
        let pinned = owner.as_ref().map(|owner| &owner.who).or(slot.stamped.as_ref());
        if !proven || pinned.is_some_and(|who| who != &incoming) {
            eprintln!("[agent-chains] {} 슬롯은 주인을 증명하지 못해 관문 토큰을 싣지 않았어요", display(&slot.id));
            continue;
        }
        let current = match stores.read(slot) {
            Read::Present(doc) => Some(doc),
            Read::Absent => None,
            Read::Unknown => continue,
        };
        if current.as_ref().and_then(|d| text(d, "accessToken")) != token.as_deref()
            || !wants_fill(current.as_ref(), access, now)
        {
            continue;
        }
        if stores.compare_and_write(slot, current.as_ref(), &with_access(current.as_ref(), access)) {
            eprintln!("[agent-chains] {} 슬롯에 관문 접근 토큰을 실었어요", display(&slot.id));
            if mapped.as_ref() != Some(&key) && !slot.bench {
                learned.keys.insert(slot.id.clone(), key.clone());
            }
            learned.owners.insert(slot.id.clone(), Owner { key, fingerprint: fingerprint(&access.access_token), who: incoming });
        }
    }
    Ok((learned, chains))
}

/// 삭제된 슬롯의 map 흔적이나 표시용 신원으로는 401 복구 대상을 만들 수 없게 실제 슬롯과 정확한 map만 교차한다.
fn managed_keys(
    slots: &[Slot],
    map: &BTreeMap<String, String>,
    learned: &BTreeMap<String, String>,
) -> Vec<String> {
    slots
        .iter()
        .filter_map(|slot| learned.get(&slot.id).or(map.get(&slot.id)).cloned())
        .collect()
}

/// 데스크톱이 사람에게 알릴 일.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub body: String,
}

/// 관문에서 받아 쓰는 슬롯이 곧 끊기거나 끊겼는지. 관문이 멈추면 관문은 알리지 못하므로 기기가 본다.
/// (같은 계정 열쇠, 알림)을 돌려준다 — 같은 계정 슬롯이 둘이어도 한 번만 알린다.
pub(crate) fn device_alerts(
    slots: &[Slot],
    map: &BTreeMap<String, String>,
    stores: &dyn Stores,
    chains: Option<&[Held]>,
    now: u64,
) -> BTreeMap<String, Alert> {
    let mut out = BTreeMap::new();
    for slot in slots {
        let Some(key) = map.get(&slot.id) else { continue };
        let Read::Present(doc) = stores.read(slot) else { continue };
        if text(&doc, "refreshToken").is_some() || text(&doc, "accessToken").is_none() {
            continue;
        }
        let label = key.strip_prefix("claude:").unwrap_or(key);
        let chain = chains.and_then(|c| c.iter().find(|c| &c.key == key));
        if chain.is_some_and(|c| c.state == "reconnect_required") {
            out.insert(format!("broken|{key}"), Alert {
                title: "Claude 로그인이 끊겼어요".into(),
                body: format!("{label} — 아무 기기에서 한 번 다시 로그인하면 모든 기기가 이어져요"),
            });
            continue;
        }
        let left = expires(&doc);
        if left >= now + ALERT_LEFT_MS {
            continue;
        }
        let why = if chains.is_none() { "관문에 닿지 않아" } else { "관문이 갱신하지 못해" };
        let body = if left > now {
            format!("{label} — {why} {}분 뒤 이 기기에서 끊겨요", (left - now) / 60_000)
        } else {
            format!("{label} — {why} 이 기기에서 끊겼어요")
        };
        out.insert(format!("stalled|{key}"), Alert { title: "Claude 로그인 갱신이 막혔어요".into(), body });
    }
    out
}

/// 새로 생긴 알림만 쌓는다. 풀린 것은 잊어 다음에 다시 생기면 또 알린다.
fn raise(current: BTreeMap<String, Alert>) {
    static RAISED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let Ok(mut raised) = RAISED.lock() else { return };
    let fresh: Vec<Alert> = current.iter().filter(|(k, _)| !raised.contains(k)).map(|(_, a)| a.clone()).collect();
    *raised = current.into_keys().collect();
    if !fresh.is_empty() {
        for alert in &fresh {
            eprintln!("[agent-chains] 알림: {} — {}", alert.title, alert.body);
        }
        if let Ok(mut pending) = pending().lock() {
            pending.extend(fresh);
        }
    }
}

fn pending() -> &'static std::sync::Mutex<Vec<Alert>> {
    static PENDING: std::sync::Mutex<Vec<Alert>> = std::sync::Mutex::new(Vec::new());
    &PENDING
}

/// 아직 사람에게 안 보인 알림 — 앱이 데스크톱 알림·토스트로 띄운다.
pub fn take_alerts() -> Vec<Alert> {
    pending().lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default()
}

/// 이 기기 슬롯으로 한 주기를 돌고, 새로 안 자리를 적는다. 적기 전에 파일을 다시 읽는다 — 그 사이
/// 설정 화면이 `adopt` 로 적은 자리를 덮지 않게.
async fn sync_local(gateway: &Gateway<'_>, stamp: &crate::device_auth::Stamp) -> Result<(), SyncError> {
    let Some((path, settings)) = load_settings() else {
        recovery::ledger().manage(Vec::new());
        return Ok(());
    };
    let slots = local_slots(path.parent(), &settings);
    let mut map = load_map();
    let owners = load_owners();
    let authorized = || crate::device_auth::stamp_is_current(stamp);
    let (outcome, chains) =
        match sync_once(gateway, &slots, &map, &owners, &GuardedLocal(stamp), &authorized, recovery::ledger()).await {
            Ok((learned, chains)) => {
                if !learned.keys.is_empty() || !learned.owners.is_empty() {
                    // 관문 응답을 기다리는 사이 로그아웃·슬롯 삭제·재로그인·계정 전환이 있었을 수 있다. 같은 기기 로그인
                    // 세대 잠금 안에서 지금 설정의 슬롯이 이 주기에 본 그대로인지, 소유 기록이 가리키는 토큰이 아직 그
                    // 자리에 있는지 다시 보고 맞는 것만 적는다.
                    let saved = crate::device_auth::with_current(stamp, || {
                        let (path, settings) = load_settings().ok_or("settings unavailable")?;
                        let now = local_slots(path.parent(), &settings);
                        let learned = still_current(learned, &slots, &now, &Local);
                        let mut map = load_map();
                        let mut owners = load_owners();
                        map.extend(learned.keys);
                        owners.extend(learned.owners);
                        owners.retain(|id, _| now.iter().any(|slot| &slot.id == id));
                        save_map(&map, &owners);
                        Ok(map)
                    });
                    map = saved.unwrap_or_else(|_| load_map());
                }
                (Ok(()), Some(chains))
            }
            Err(error) => (Err(error), None),
        };
    raise(device_alerts(&slots, &map, &Local, chains.as_deref(), now_ms()));
    outcome
}

/// 한 주기에 배운 것 중 저장할 때도 맞는 것만 — 슬롯이 이 주기에 본 그대로(같은 저장소·작업대 여부·표식)이고,
/// 소유 기록이 가리키는 토큰이 아직 그 자리에 있을 것. 지연된 응답이 삭제·재로그인된 슬롯에 옛 주인을 적지 않게.
pub(crate) fn still_current(learned: Learned, seen: &[Slot], now: &[Slot], stores: &dyn Stores) -> Learned {
    let unchanged = |id: &String| seen.iter().find(|slot| &slot.id == id).is_some_and(|slot| now.contains(slot));
    let holds = |id: &String, owner: &Owner| {
        now.iter().find(|slot| &slot.id == id).is_some_and(|slot| match stores.read(slot) {
            Read::Present(doc) => text(&doc, "accessToken").map(fingerprint).as_deref() == Some(owner.fingerprint.as_str()),
            _ => false,
        })
    };
    let owners: BTreeMap<_, _> = learned.owners.into_iter()
        .filter(|(id, owner)| unchanged(id) && holds(id, owner)).collect();
    // A mapping learned before a relogin is stale even if the slot path and label did not change.
    let keys = learned.keys.into_iter()
        .filter(|(id, key)| owners.get(id).is_some_and(|owner| &owner.key == key)).collect();
    Learned { keys, owners }
}

/// 관문 로그인이 없을 때도 관문 토큰으로 도는 슬롯은 끊기기 전에 알린다.
fn alert_offline() {
    let Some((path, settings)) = load_settings() else { return };
    let map = load_map();
    if !map.is_empty() {
        raise(device_alerts(&local_slots(path.parent(), &settings), &map, &Local, None, now_ms()));
    }
}

fn display(id: &str) -> &str {
    if id.is_empty() { "기본" } else { id }
}

/// 앱이 도는 동안 5분마다(`poke` 면 곧바로) 한 번씩 돈다. 관문 로그인이 없거나 옛 관문이면 조용히 쉰다.
pub fn spawn() {
    use std::sync::atomic::{AtomicBool, Ordering};
    if !crate::device_auth::sync_environment_allowed() {
        return;
    }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        let mut unsupported_said = false;
        loop {
            // 관문·네트워크가 안 닿아 이번 주기를 놓쳤으면 5분이 아니라 30초 뒤 다시 — 기기엔 갱신 토큰이 없어
            // 받기를 놓친 시간만큼 claude 칸이 만료 토큰으로 멈춘다(잠들었다 깬 맥북, 2026-10-09).
            let mut missed = false;
            if let Some((cred, stamp)) = crate::device_auth::capture() {
                recovery::ledger().begin(&stamp);
                let gateway = Gateway::new(&cred.relay, &cred.token);
                match runtime.block_on(sync_local(&gateway, &stamp)) {
                    Ok(()) => unsupported_said = false,
                    Err(SyncError::Unauthorized) => {
                        recovery::ledger().clear();
                        crate::device_auth::reject_if(&stamp, || true);
                    }
                    Err(SyncError::Unsupported) => {
                        recovery::ledger().clear();
                        if !unsupported_said {
                            unsupported_said = true;
                            eprintln!("[agent-chains] 관문이 사슬 맡기기를 모르는 옛 판이에요 — 기기마다 로그인을 그대로 써요");
                        }
                    }
                    Err(SyncError::Failed) => missed = true,
                }
            } else {
                recovery::ledger().clear();
                alert_offline();
            }
            rest(Duration::from_secs(if missed { 30 } else { 300 }));
        }
    });
}

#[cfg(test)]
pub(crate) mod tests;
