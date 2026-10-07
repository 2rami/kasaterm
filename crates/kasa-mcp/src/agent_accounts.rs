//! 코딩 에이전트 계정 목록 — 관문 계정 하나에 딸린 Claude·Codex 로그인을 기기끼리 나눈다.
//!
//! 여기서 나누는 것은 **목록뿐**이다(종류·신원·이름·어느 기기에 로그인돼 있나). 2026-09-28 에 두 길을
//! 재 보고 버렸다:
//! - 갱신 토큰을 여러 기기가 나누면 한 번 쓰일 때마다 바뀌어 먼저 갱신한 쪽이 나머지를 로그아웃시킨다.
//! - `claude setup-token` 1년 토큰은 `user:inference` 권한뿐이라 claude.ai 커넥터·Chrome·사용량
//!   조회가 꺼진다. `codex login --with-access-token` 은 Enterprise 전용 에이전트 토큰만 받는다.
//!
//! Claude 자격증명은 이제 관문 하나가 사슬을 쥐고 기기들에 접근 토큰만 나눈다(`agent_chains.rs`).
//! Codex 는 아직 기기마다 로그인한다.
//!
//! 흐름: 기기가 자기 슬롯 중 신원을 아는 것을 올리면(`POST /relay/agent-accounts`) 관문이 같은
//! 계정의 다른 기기 몫과 합쳐 돌려준다. 받은 기기는 없는 계정 자리를 「로그인 필요」로 만든다(앱).
//! 로그인된 기기가 하나도 안 남은 계정은 목록에서 빠지고, 폐기된 기기의 몫도 빠진다.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use crate::device_auth::{DeviceCred, Stamp};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROVIDERS: [&str; 2] = ["claude", "codex"];
/// 한 기기가 한 번에 올릴 수 있는 슬롯 수. 사람이 쓰는 계정 수보다 넉넉하다.
const MAX_LOCAL: usize = 64;
const MAX_FIELD: usize = 200;

/// 이 기기에서 로그인이 확인된 슬롯 하나.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalAccount {
    pub provider: String,
    /// 이 기기의 슬롯 id. 빈 값은 기본 로그인.
    #[serde(default)]
    pub slot: String,
    pub email: String,
    /// Claude 조직명. 한 이메일의 개인 조직과 팀 조직은 한도가 따로라 다른 계정이다.
    #[serde(default)]
    pub org: String,
    /// Codex(ChatGPT) 작업 공간 id. 같은 이메일의 개인·팀 작업 공간을 가른다.
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub plan: String,
    /// 사람이 붙인 별명.
    #[serde(default)]
    pub label: String,
}

impl LocalAccount {
    /// 기기가 달라도 같은 계정이면 같은 값. 슬롯 id 는 기기마다 달라 못 쓴다.
    pub fn key(&self) -> String {
        account_key(&self.provider, &self.email, &self.org, &self.workspace)
    }

    fn valid(&self) -> bool {
        PROVIDERS.contains(&self.provider.as_str())
            && self.email.contains('@')
            && [&self.slot, &self.email, &self.org, &self.workspace, &self.plan, &self.label]
                .iter()
                .all(|s| s.len() <= MAX_FIELD && !s.chars().any(char::is_control))
    }
}

/// 계정 열쇠. Claude 는 진짜 팀 조직이면 조직명, 아니면 이메일(개인 조직 이름은
/// `<이메일>'s Organization` 이라 이메일과 같은 뜻이다). Codex 는 이메일과 작업 공간.
pub fn account_key(provider: &str, email: &str, org: &str, workspace: &str) -> String {
    let email = email.trim().to_lowercase();
    match provider {
        "claude" => {
            let org = org.trim();
            if !org.is_empty() && !org.to_lowercase().contains(&email) {
                format!("claude:{}", org.to_lowercase())
            } else {
                format!("claude:{email}")
            }
        }
        _ if workspace.trim().is_empty() => format!("{provider}:{email}"),
        _ => format!("{provider}:{email}/{}", workspace.trim()),
    }
}

/// 관문이 쥐는 계정 한 줄.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    pub provider: String,
    pub email: String,
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub label: String,
    /// 기기 id → 그 기기의 슬롯과 마지막으로 올린 시각.
    #[serde(default)]
    pub devices: BTreeMap<String, Presence>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Presence {
    pub slot: String,
    pub seen: u64,
}

/// 관문 계정 하나의 목록. 열쇠 → 한 줄.
pub type Book = BTreeMap<String, Entry>;

/// 한 기기가 올린 목록으로 그 기기 몫을 통째로 갈아 끼운다. 안 올린 계정에서는 그 기기가 빠진다
/// — 기기에서 지운 슬롯이 관문에 남지 않게.
pub fn publish(book: &mut Book, device_id: &str, locals: &[LocalAccount], now: u64) -> Result<(), &'static str> {
    if locals.len() > MAX_LOCAL {
        return Err("too_many_accounts");
    }
    if !locals.iter().all(LocalAccount::valid) {
        return Err("bad_account");
    }
    for e in book.values_mut() {
        e.devices.remove(device_id);
    }
    // 한 기기에 같은 계정 슬롯이 둘이면 앞의 것만 — 뒤의 별명이 앞의 것을 덮으면 한 계정이
    // 다른 계정 이름으로 불린다(회사 맥북 실측: 개인 조직으로 다시 로그인된 「사이오닉팀」 슬롯).
    let mut seen = HashSet::new();
    for l in locals {
        if !seen.insert(l.key()) {
            continue;
        }
        let e = book.entry(l.key()).or_default();
        e.provider = l.provider.clone();
        e.email = l.email.trim().to_string();
        e.org = l.org.trim().to_string();
        e.workspace = l.workspace.trim().to_string();
        if !l.plan.trim().is_empty() {
            e.plan = l.plan.trim().to_string();
        }
        if !l.label.trim().is_empty() {
            e.label = l.label.trim().to_string();
        }
        e.devices.insert(device_id.to_string(), Presence { slot: l.slot.clone(), seen: now });
    }
    book.retain(|_, e| !e.devices.is_empty());
    Ok(())
}

/// 기기에 돌려주는 한 줄.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Shared {
    pub key: String,
    pub provider: String,
    pub email: String,
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub devices: Vec<SharedDevice>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedDevice {
    pub device_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub slot: String,
    /// 묻는 기기 자신인가.
    #[serde(default)]
    pub current: bool,
}

impl Shared {
    /// 이 계정에 로그인한 기기 이름들(묻는 기기 제외).
    pub fn elsewhere(&self) -> Vec<&str> {
        self.devices.iter().filter(|d| !d.current).map(|d| d.label.as_str()).collect()
    }
}

/// 살아 있는 기기(폐기 안 된 같은 계정 기기, id → 이름)의 몫만 보인다.
pub fn view(book: &Book, live: &HashMap<String, String>, me: &str) -> Vec<Shared> {
    book.iter()
        .filter_map(|(key, e)| {
            let devices: Vec<SharedDevice> = e
                .devices
                .iter()
                .filter_map(|(id, p)| {
                    live.get(id).map(|label| SharedDevice {
                        device_id: id.clone(),
                        label: label.clone(),
                        slot: p.slot.clone(),
                        current: id == me,
                    })
                })
                .collect();
            (!devices.is_empty()).then(|| Shared {
                key: key.clone(),
                provider: e.provider.clone(),
                email: e.email.clone(),
                org: e.org.clone(),
                workspace: e.workspace.clone(),
                plan: e.plan.clone(),
                label: e.label.clone(),
                devices,
            })
        })
        .collect()
}

/// 폐기된 기기의 몫을 걷는다. 걷은 것이 있으면 `true`.
pub fn prune(book: &mut Book, alive: &HashSet<String>) -> bool {
    let before: usize = book.values().map(|e| e.devices.len()).sum();
    for e in book.values_mut() {
        e.devices.retain(|id, _| alive.contains(id));
    }
    book.retain(|_, e| !e.devices.is_empty());
    before != book.values().map(|e| e.devices.len()).sum::<usize>()
}

// ── 기기 쪽 ─────────────────────────────────────────────────────────────────────

pub(crate) fn settings_path() -> Option<PathBuf> {
    match std::env::var("KASATERM_SETTINGS_FILE") {
        Ok(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => Some(kasa_socket::home_dir()?.join(".config/kasaterm/settings.json")),
    }
}

/// 이 기기에서 신원을 아는 슬롯들. 네트워크는 안 건드린다:
/// Claude 는 앱이 슬롯 토큰으로 확인해 적어 둔 이메일·조직(`claude_account_emails`·`_orgs`),
/// Codex 는 슬롯의 `auth.json` id_token. 신원을 모르는 슬롯(아직 로그인 전)은 빠진다.
///
/// Claude 는 적어 둔 신원만으로 「이 기기」라고 하지 않는다 — 로그인이 풀린 뒤에도 표는
/// 남아서, 풀린 슬롯이 목록에 이 기기 몫으로 올라갔다(2026-10-07 맥북 acct-1·acct-8).
/// 그 슬롯 저장소에 쓸 수 있는 로그인이 남아 있는지 보고, 확실히 없을 때만 뺀다.
///
/// Claude 기본 로그인(`""`)은 고른 슬롯이 없을 때만 넣는다 — 슬롯을 고르면 기본 자리는
/// 그 슬롯을 옮겨 담은 작업대라 같은 계정이고, 적어 둔 기본 신원은 옛 값일 수 있다(`claude_auth.rs`).
pub fn local_snapshot() -> Vec<LocalAccount> {
    if !crate::device_auth::sync_environment_allowed() { return Vec::new(); }
    let Some(path) = settings_path() else { return Vec::new() };
    let settings: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let base = path.parent().map(PathBuf::from);
    let mut out = claude_slots(&settings, |slot| {
        let store = claude_store(base.as_deref(), &settings, slot);
        crate::http::claude_slot_login(store.as_deref().and_then(|p| p.to_str()))
    });
    let default_auth = kasa_socket::home_dir().map(|home| home.join(".codex/auth.json"));
    out.extend(codex_slots(&settings, base.as_deref(), default_auth.as_deref()));
    out
}

pub(crate) fn slots(settings: &Value, key: &str) -> Vec<(String, String)> {
    settings
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let id = v.get("id")?.as_str()?.to_string();
                    let label = v.get("label").and_then(Value::as_str).unwrap_or_default().to_string();
                    (!id.is_empty()).then_some((id, label))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 그 슬롯의 로그인이 실제로 사는 저장소. 기본 로그인과, 작업대에 실려 있는 활성
/// 계정은 claude 의 기본 자리(`None`)다 — 그 계정의 금고는 작업대보다 낡아 있다.
pub(crate) fn claude_store(base: Option<&std::path::Path>, settings: &Value, slot: &str) -> Option<PathBuf> {
    if slot.is_empty() {
        return None;
    }
    let root = base?.join("claude-accounts");
    let active = settings.get("claude_account").and_then(Value::as_str) == Some(slot);
    let on_bench = std::fs::read_to_string(root.join("_active/workbench-stamp.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .is_some_and(|v| v.get("account").and_then(Value::as_str) == Some(slot));
    (!(active && on_bench)).then(|| root.join(slot))
}

fn claude_slots(
    settings: &Value,
    login: impl Fn(&str) -> crate::http::SlotLogin,
) -> Vec<LocalAccount> {
    let remembered = |map: &str, id: &str| {
        settings
            .get(map)
            .and_then(|m| m.get(id))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut ids = slots(settings, "claude_accounts");
    let active = settings.get("claude_account").and_then(Value::as_str).unwrap_or_default();
    if active.is_empty() {
        ids.insert(0, (String::new(), String::new()));
    }
    ids.into_iter()
        .filter_map(|(id, label)| {
            let email = remembered("claude_account_emails", &id);
            if !email.contains('@') || login(&id) == crate::http::SlotLogin::LoggedOut {
                return None;
            }
            Some(LocalAccount {
                provider: "claude".into(),
                org: remembered("claude_account_orgs", &id),
                slot: id,
                email,
                label,
                ..Default::default()
            })
        })
        .collect()
}

fn codex_slots(settings: &Value, base: Option<&std::path::Path>, default_auth: Option<&std::path::Path>) -> Vec<LocalAccount> {
    let mut out = Vec::new();
    if let Some(path) = default_auth {
        if let Some(a) = codex_auth_identity(path) {
            out.push(LocalAccount { slot: String::new(), ..a });
        }
    }
    let Some(base) = base else { return out };
    for (id, label) in slots(settings, "codex_accounts") {
        if let Some(a) = codex_auth_identity(&base.join("codex-accounts").join(&id).join("auth.json")) {
            out.push(LocalAccount { slot: id, label, ..a });
        }
    }
    out
}

/// `auth.json` 의 id_token 에서 신원만 읽는다. 서명은 안 본다 — 쓰임이 「이 슬롯이 누구인가」를
/// 적는 것뿐이고, 토큰이 유효한지는 codex 가 판가름한다. 토큰 값은 어디에도 옮기지 않는다.
pub fn codex_auth_identity(path: &std::path::Path) -> Option<LocalAccount> {
    use base64::Engine as _;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let tokens = v.get("tokens")?;
    let jwt = tokens.get("id_token")?.as_str()?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(jwt.split('.').nth(1)?).ok()?;
    let claims: Value = serde_json::from_slice(&raw).ok()?;
    let email = claims.get("email")?.as_str()?.to_string();
    let auth = claims.get("https://api.openai.com/auth");
    let pick = |k: &str| auth.and_then(|a| a.get(k)).and_then(Value::as_str).unwrap_or_default().to_string();
    let workspace = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| pick("chatgpt_account_id"));
    email.contains('@').then(|| LocalAccount {
        provider: "codex".into(),
        email,
        workspace,
        plan: pick("chatgpt_plan_type"),
        ..Default::default()
    })
}

/// 관문에 이 기기 몫을 올리고 합친 목록을 받는다. 관문 로그인이 없으면 에러.
///
/// 계정 전환·검증 실행에서 옛 토큰을 쓰지 않도록 공통 설정과 같은 인증 경계를 따른다.
pub async fn sync(locals: &[LocalAccount]) -> anyhow::Result<Vec<Shared>> {
    let (cred, stamp) = crate::device_auth::capture()
        .ok_or_else(|| anyhow::anyhow!("계정 동기화를 사용할 수 없어요 — 관문과 로그인 상태를 확인해 주세요"))?;
    sync_bound(&cred, &stamp, None, locals).await
}

async fn sync_bound(cred: &DeviceCred, stamp: &Stamp, generation: Option<u64>, locals: &[LocalAccount]) -> anyhow::Result<Vec<Shared>> {
    let relevant = || generation.is_none_or(|generation| cache().lock()
        .is_ok_and(|cache| cache.matches(stamp, generation)));
    let result = exchange(cred, locals, &|| crate::device_auth::with_current(stamp, || {
        if relevant() { Ok(()) } else { Err("account cache changed".into()) }
    })).await;
    if matches!(result, Err(ExchangeError::Unauthorized)) { crate::device_auth::reject_if(stamp, relevant); }
    result.map_err(|error| anyhow::anyhow!(error.message()))
}

#[derive(Debug, PartialEq, Eq)]
enum ExchangeError { Changed, Unauthorized, Unsupported, Failed }

impl ExchangeError {
    fn message(&self) -> &'static str {
        match self {
            Self::Changed => "계정이 바뀌어 이전 목록을 버렸어요",
            Self::Unauthorized => "관문 로그인이 풀렸어요 — 다시 로그인해 주세요",
            Self::Unsupported => "관문이 계정 목록을 모르는 옛 판이에요",
            Self::Failed => "관문 계정 목록을 받지 못했어요",
        }
    }
}

async fn exchange(cred: &DeviceCred, locals: &[LocalAccount],
    authorize: &(dyn Fn() -> Result<(), String> + Sync)) -> Result<Vec<Shared>, ExchangeError> {
    authorize().map_err(|_| ExchangeError::Changed)?;
    let gateway = cred.relay.trim_end_matches('/').to_string();
    let mut res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build().map_err(|_| ExchangeError::Failed)?
        .post(format!("{gateway}/relay/agent-accounts"))
        .bearer_auth(&cred.token)
        .json(&serde_json::json!({ "accounts": locals }))
        .send()
        .await
        .map_err(|_| ExchangeError::Failed)?;
    authorize().map_err(|_| ExchangeError::Changed)?;
    let status = res.status().as_u16();
    match status {
        200 => {}
        401 | 403 => return Err(ExchangeError::Unauthorized),
        404 | 405 => return Err(ExchangeError::Unsupported),
        _ => return Err(ExchangeError::Failed),
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|_| ExchangeError::Failed)? {
        if body.len() + chunk.len() > 1024 * 1024 { return Err(ExchangeError::Failed); }
        body.extend_from_slice(&chunk);
    }
    authorize().map_err(|_| ExchangeError::Changed)?;
    let v: Value = serde_json::from_slice(&body).map_err(|_| ExchangeError::Failed)?;
    if v["ok"] != true { return Err(ExchangeError::Failed); }
    serde_json::from_value(v["accounts"].clone()).map_err(|_| ExchangeError::Failed)
}

/// 기기 쪽 마지막 관측. 화면은 이것만 읽는다 — 네트워크는 백그라운드 스레드가 탄다.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub shared: Vec<Shared>,
    /// 이 기기에 로그인이 확인된 계정 열쇠. 관문 쪽 표시보다 먼저 믿는다 — 방금 로그인한
    /// 계정이 다음 동기화 전까지 「로그인 필요」로 남지 않게.
    pub local_keys: HashSet<String>,
    pub error: Option<String>,
}

impl Seen {
    /// 이 기기에 없는 계정들.
    pub fn missing<'a>(&'a self, provider: &'a str) -> impl Iterator<Item = &'a Shared> + 'a {
        self.shared.iter().filter(move |a| {
            a.provider == provider && !self.local_keys.contains(&a.key) && !a.devices.iter().any(|d| d.current)
        })
    }
}

#[derive(Default)]
struct CacheCell {
    scope: Option<Stamp>,
    generation: u64,
    at: Option<std::time::Instant>,
    running: bool,
    due: bool,
    seen: Option<Seen>,
}

impl CacheCell {
    fn bind(&mut self, scope: Option<Stamp>) {
        if self.scope != scope {
            *self = Self { scope, generation: self.generation.wrapping_add(1), ..Default::default() };
        }
    }

    fn matches(&self, stamp: &Stamp, generation: u64) -> bool {
        self.scope.as_ref() == Some(stamp) && self.generation == generation
    }

    fn finish(&mut self, stamp: &Stamp, generation: u64, local_keys: HashSet<String>, got: anyhow::Result<Vec<Shared>>) {
        if !self.matches(stamp, generation) { return; }
        let prev = self.seen.take().unwrap_or_default();
        self.seen = Some(match got {
            Ok(shared) => Seen { shared, local_keys, error: None },
            Err(error) => Seen { local_keys, error: Some(error.to_string()), ..prev },
        });
        self.at = Some(std::time::Instant::now());
        self.running = false;
    }
}

fn cache() -> &'static std::sync::Mutex<CacheCell> {
    static C: std::sync::OnceLock<std::sync::Mutex<CacheCell>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 이만큼 지나면 다시 받는다. 다른 기기에서 계정을 늘려도 몇 분 안에 보이면 된다.
const REFRESH: std::time::Duration = std::time::Duration::from_secs(300);

/// 마지막으로 받은 목록. 오래됐거나 `poke` 됐으면 백그라운드로 다시 받는다(부른 자리는 안 기다린다).
/// 관문에 로그인하지 않은 기기는 `None` — 올릴 데가 없다.
///
/// 파일 하나(`device.json`)를 읽으므로 프레임마다 부르지 말고 느린 틱에서 불러라.
pub fn cached() -> Option<Seen> {
    let Some((cred, stamp)) = crate::device_auth::capture() else { clear_cache(); return None };
    crate::device_auth::with_current(&stamp, || {
        let mut c = cache().lock().map_err(|_| "account cache unavailable")?;
        c.bind(Some(stamp.clone()));
        let stale = c.at.is_none_or(|at| at.elapsed() >= REFRESH);
        if (stale || c.due) && !c.running {
            c.running = true;
            c.due = false;
            let stamp = stamp.clone();
            let generation = c.generation;
            std::thread::spawn(move || {
                let got = crate::device_auth::with_current(&stamp, || Ok(local_snapshot()));
                let locals = got.unwrap_or_default();
                let local_keys = locals.iter().map(LocalAccount::key).collect();
                let got = tokio::runtime::Builder::new_current_thread().enable_all().build()
                    .map_err(anyhow::Error::from)
                    .and_then(|runtime| runtime.block_on(sync_bound(&cred, &stamp, Some(generation), &locals)));
                let applied = crate::device_auth::with_current(&stamp, || {
                    cache().lock().map_err(|_| "account cache unavailable")?.finish(&stamp, generation, local_keys, got);
                    Ok(())
                });
                if applied.is_err() {
                    if let Ok(mut c) = cache().lock() {
                        if c.matches(&stamp, generation) { c.bind(None); }
                    }
                }
            });
        }
        Ok(c.seen.clone())
    }).ok().flatten()
}

pub(crate) fn clear_cache() {
    if let Ok(mut c) = cache().lock() { c.bind(None); }
}

pub fn spawn() {
    use std::sync::atomic::{AtomicBool, Ordering};
    if !crate::device_auth::sync_environment_allowed() { return; }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) { return; }
    // Settings need not be open for another device to learn this device's accounts.
    std::thread::spawn(|| loop {
        cached();
        std::thread::sleep(REFRESH);
    });
}

/// 다음 `cached` 때 곧바로 다시 받게 한다 — 로그인·슬롯 추가·제거 뒤에 부른다.
pub fn poke() {
    if let Ok(mut c) = cache().lock() {
        c.due = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_scope_changes_discard_rows_and_late_completions_including_aba() {
        let first = Stamp::fixture_at(1);
        let second = Stamp::fixture_at(2);
        let returned = Stamp::fixture_at(3);
        let rows = vec![Shared { key: "claude:fixture@example.invalid".into(), ..Default::default() }];
        let mut cache = CacheCell::default();
        cache.bind(Some(first.clone()));
        let first_generation = cache.generation;
        cache.finish(&first, first_generation, HashSet::new(), Ok(rows.clone()));
        assert_eq!(cache.seen.as_ref().unwrap().shared, rows);
        cache.bind(Some(second.clone()));
        assert!(cache.seen.is_none());
        cache.running = true;
        cache.finish(&first, first_generation, HashSet::new(), Ok(rows.clone()));
        assert!(cache.seen.is_none());
        assert!(cache.running, "old completion cleared the new request's running marker");
        let second_generation = cache.generation;
        cache.bind(None);
        cache.finish(&second, second_generation, HashSet::new(), Ok(rows.clone()));
        assert!(cache.seen.is_none());
        cache.bind(Some(returned.clone()));
        cache.finish(&first, first_generation, HashSet::new(), Ok(rows.clone()));
        assert!(cache.seen.is_none(), "login back to the first account accepted an old epoch");
        let returned_generation = cache.generation;
        cache.finish(&returned, returned_generation, HashSet::new(), Ok(rows.clone()));
        cache.finish(&returned, returned_generation, HashSet::new(), Err(anyhow::anyhow!("offline")));
        assert_eq!(cache.seen.as_ref().unwrap().shared, rows);
        assert!(cache.seen.as_ref().unwrap().error.is_some());
        cache.bind(None);
        cache.bind(Some(returned.clone()));
        cache.finish(&returned, returned_generation, HashSet::new(), Ok(rows));
        assert!(cache.seen.is_none(), "gateway off/on reused a previous request generation");
    }

    #[test]
    fn isolated_calls_never_load_personal_provider_accounts_or_start_sync() {
        assert!(!crate::device_auth::sync_environment_allowed());
        assert!(local_snapshot().is_empty());
        assert!(cached().is_none());
        spawn();
    }

    #[tokio::test]
    async fn exchange_rejects_late_accounts_malformed_bodies_and_credentials_without_retries() {
        use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
        use axum::{routing::post, Json, Router};
        let active = Arc::new(AtomicBool::new(true));
        let requests = Arc::new(AtomicUsize::new(0));
        let changed = active.clone();
        let count = requests.clone();
        let router = Router::new().route("/relay/agent-accounts", post(move || {
            let changed = changed.clone();
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::AcqRel);
                changed.store(false, Ordering::Release);
                Json(serde_json::json!({"ok":true,"accounts":[]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cred = DeviceCred { relay: format!("http://{}", listener.local_addr().unwrap()),
            account:"fixture".into(), device_id:"fixture".into(), token:"fixture-not-a-real-token".into(), display_name: None };
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let allow = || if active.load(Ordering::Acquire) { Ok(()) } else { Err("changed".into()) };
        assert_eq!(exchange(&cred, &[], &allow).await, Err(ExchangeError::Changed));
        assert_eq!(exchange(&cred, &[], &allow).await, Err(ExchangeError::Changed));
        assert_eq!(requests.load(Ordering::Acquire), 1);
        task.abort();

        for (status, body, expected) in [
            (200, r#"{"ok":true,"accounts":[]}"#.to_string(), Ok(Vec::new())),
            (200, r#"{"ok":true}"#.to_string(), Err(ExchangeError::Failed)),
            (200, "{".to_string(), Err(ExchangeError::Failed)),
            (200, "x".repeat(1024 * 1024 + 1), Err(ExchangeError::Failed)),
            (401, String::new(), Err(ExchangeError::Unauthorized)),
            (403, String::new(), Err(ExchangeError::Unauthorized)),
            (404, String::new(), Err(ExchangeError::Unsupported)),
            (503, String::new(), Err(ExchangeError::Failed)),
        ] {
            let app = Router::new().route("/relay/agent-accounts", post(move || {
                let body = body.clone();
                async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let cred = DeviceCred { relay: format!("http://{}", listener.local_addr().unwrap()), ..cred.clone() };
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            assert_eq!(exchange(&cred, &[], &|| Ok(())).await, expected);
            task.abort();
        }
    }

    fn claude(slot: &str, email: &str, org: &str, label: &str) -> LocalAccount {
        LocalAccount { provider: "claude".into(), slot: slot.into(), email: email.into(), org: org.into(), label: label.into(), ..Default::default() }
    }

    #[test]
    fn personal_and_team_orgs_of_one_email_are_different_accounts() {
        let personal = claude("acct-6", "2rami@sionic.ai", "2rami@sionic.ai's Organization", "");
        let team = claude("acct-5", "2rami@sionic.ai", "Sionic AI", "");
        assert_eq!(personal.key(), "claude:2rami@sionic.ai");
        assert_eq!(team.key(), "claude:sionic ai");
        assert_eq!(claude("", "2Rami@Sionic.ai", "", "").key(), personal.key(), "대소문자·빈 조직도 같은 계정이다");
        assert_eq!(account_key("codex", "a@b.c", "", "ws-1"), "codex:a@b.c/ws-1");
        assert_ne!(account_key("codex", "a@b.c", "", "ws-1"), account_key("codex", "a@b.c", "", "ws-2"));
    }

    #[test]
    fn a_device_owns_only_its_share_and_empty_accounts_drop_out() {
        let mut book = Book::new();
        let mini = [claude("acct-1", "g@gmail.com", "", "지메일"), claude("acct-5", "r@s.ai", "Sionic AI", "사이오닉팀")];
        publish(&mut book, "dev_mini", &mini, 10).unwrap();
        publish(&mut book, "dev_book", &[claude("", "g@gmail.com", "", "")], 11).unwrap();
        let live: HashMap<String, String> =
            [("dev_mini".to_string(), "미니".to_string()), ("dev_book".to_string(), "맥북".to_string())].into();
        let v = view(&book, &live, "dev_book");
        assert_eq!(v.len(), 2);
        let gmail = v.iter().find(|s| s.key == "claude:g@gmail.com").unwrap();
        assert_eq!(gmail.label, "지메일", "빈 별명이 남의 별명을 지우면 안 된다");
        assert_eq!(gmail.elsewhere(), vec!["미니"]);
        assert!(gmail.devices.iter().any(|d| d.current && d.slot.is_empty()));

        let dup = [claude("acct-5", "p@s.ai", "", "개인"), claude("acct-4", "p@s.ai", "", "팀")];
        publish(&mut book, "dev_book", &dup, 11).unwrap();
        let p = &view(&book, &live, "dev_book").into_iter().find(|s| s.key == "claude:p@s.ai").unwrap();
        assert_eq!((p.label.as_str(), p.devices[0].slot.as_str()), ("개인", "acct-5"), "한 기기의 겹친 슬롯은 앞의 것만");
        publish(&mut book, "dev_book", &[claude("", "g@gmail.com", "", "")], 11).unwrap();

        // 미니가 팀 슬롯을 지우고 다시 올리면 그 계정은 목록에서 빠진다.
        publish(&mut book, "dev_mini", &mini[..1], 12).unwrap();
        assert!(view(&book, &live, "dev_book").iter().all(|s| s.key != "claude:sionic ai"));
    }

    #[test]
    fn revoked_devices_disappear_from_the_view_and_prune() {
        let mut book = Book::new();
        publish(&mut book, "dev_old", &[claude("acct-1", "n@naver.com", "", "네이버")], 1).unwrap();
        let live: HashMap<String, String> = [("dev_book".to_string(), "맥북".to_string())].into();
        assert!(view(&book, &live, "dev_book").is_empty(), "폐기된 기기의 계정이 보였다");
        assert!(prune(&mut book, &live.keys().cloned().collect()));
        assert!(book.is_empty());
    }

    #[test]
    fn bad_uploads_are_refused_whole() {
        let mut book = Book::new();
        let ok = claude("acct-1", "g@gmail.com", "", "");
        let bad_provider = LocalAccount { provider: "gemini".into(), ..ok.clone() };
        let no_email = LocalAccount { email: "nobody".into(), ..ok.clone() };
        let control = LocalAccount { label: "a\u{7}b".into(), ..ok.clone() };
        for bad in [bad_provider, no_email, control] {
            assert!(publish(&mut book, "d", &[ok.clone(), bad], 1).is_err());
        }
        assert!(book.is_empty(), "거절된 업로드가 일부라도 들어갔다");
        assert!(publish(&mut book, "d", &vec![ok; MAX_LOCAL + 1], 1).is_err());
    }

    #[test]
    fn snapshot_reads_remembered_identities_and_codex_id_tokens_only() {
        use base64::Engine as _;
        let dir = std::env::temp_dir().join(format!("kasa-agents-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("codex-accounts/codex-1")).unwrap();
        let claims = serde_json::json!({
            "email": "r@s.ai",
            "https://api.openai.com/auth": { "chatgpt_account_id": "ws-team", "chatgpt_plan_type": "team" },
        });
        let jwt = format!("x.{}.y", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string()));
        std::fs::write(
            dir.join("codex-accounts/codex-1/auth.json"),
            serde_json::json!({ "tokens": { "id_token": jwt, "access_token": "secret" } }).to_string(),
        )
        .unwrap();
        let settings = serde_json::json!({
            "claude_accounts": [{"id": "acct-1", "label": "지메일"}, {"id": "acct-9", "label": ""}],
            "claude_account": "acct-1",
            "claude_account_emails": {"": "r@s.ai", "acct-1": "g@gmail.com"},
            "claude_account_orgs": {"acct-1": "g@gmail.com's Organization"},
            "codex_accounts": [{"id": "codex-1", "label": "사이오닉팀"}],
        });
        let claude = claude_slots(&settings, |_| crate::http::SlotLogin::LoggedIn);
        assert_eq!(claude.len(), 1, "로그인 전 자리와 작업대(기본)가 섞였다: {claude:?}");
        assert_eq!((claude[0].slot.as_str(), claude[0].label.as_str()), ("acct-1", "지메일"));
        let codex = codex_slots(&settings, Some(&dir), None);
        let slot = codex.iter().find(|a| a.slot == "codex-1").unwrap();
        assert_eq!((slot.workspace.as_str(), slot.plan.as_str()), ("ws-team", "team"));
        assert!(!serde_json::to_string(&codex).unwrap().contains("secret"), "토큰이 새어 나왔다");

        let bare = serde_json::json!({
            "claude_accounts": [{"id": "acct-9", "label": ""}],
            "claude_account_emails": {"": "r@s.ai"},
        });
        let bare = claude_slots(&bare, |_| crate::http::SlotLogin::LoggedIn);
        assert_eq!((bare.len(), bare[0].slot.as_str()), (1, ""), "고른 슬롯이 없으면 기본 로그인이 이 기기의 계정이다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signed_out_slots_are_not_claimed_for_this_device() {
        use crate::http::SlotLogin;
        let settings = serde_json::json!({
            "claude_accounts": [
                {"id": "acct-5", "label": "개인"}, {"id": "acct-1", "label": "지메일"}, {"id": "acct-8", "label": "네이버"},
            ],
            "claude_account": "acct-5",
            "claude_account_emails": {"acct-5": "r@s.ai", "acct-1": "g@gmail.com", "acct-8": "n@naver.com"},
        });
        let got = claude_slots(&settings, |slot| match slot {
            "acct-1" => SlotLogin::LoggedOut,
            "acct-8" => SlotLogin::Unknown,
            _ => SlotLogin::LoggedIn,
        });
        let slots: Vec<&str> = got.iter().map(|a| a.slot.as_str()).collect();
        assert_eq!(slots, ["acct-5", "acct-8"], "풀린 슬롯은 빠지고, 못 읽은 슬롯은 로그아웃으로 단정하지 않는다");
    }

    #[test]
    fn active_slot_on_the_workbench_is_read_from_the_default_store() {
        let base = std::env::temp_dir().join(format!("kasa-agents-store-{}", uuid::Uuid::new_v4()));
        let settings = serde_json::json!({ "claude_account": "acct-5" });
        assert_eq!(claude_store(Some(&base), &settings, ""), None);
        assert_eq!(claude_store(Some(&base), &settings, "acct-5"), Some(base.join("claude-accounts/acct-5")), "지문이 없으면 금고가 정본");
        std::fs::create_dir_all(base.join("claude-accounts/_active")).unwrap();
        std::fs::write(base.join("claude-accounts/_active/workbench-stamp.json"), r#"{"account":"acct-5","digest":"x"}"#).unwrap();
        assert_eq!(claude_store(Some(&base), &settings, "acct-5"), None, "작업대에 실린 활성 계정은 기본 자리를 본다");
        assert_eq!(claude_store(Some(&base), &settings, "acct-1"), Some(base.join("claude-accounts/acct-1")));
        let _ = std::fs::remove_dir_all(&base);
    }
}
