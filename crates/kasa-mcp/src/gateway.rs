//! 관문 — 중계소(`kasa-relay`)에 얹혀 **폰 주소를 기계로 잇는** 쪽. 앱 쪽과 프레임
//! 규약은 `uplink.rs`(그 파일 머리말이 왜 이 구조인지 말한다).
//!
//! - `GET /relay/uplink` (WS): 앱이 붙어 `hello{key, slugs, machine}` 를 보낸다. slug 는
//!   처음 본 키에 묶이고(디스크에 남긴다 — 재시작을 넘게), 다른 키가 같은 slug 를 대면
//!   그 slug 만 거절한다. 같은 slug 가 다시 붙으면 새 연결이 이긴다(앱 재시작).
//! - `/u/<slug>/…` (HTTP·WS 전부): slug 의 업링크를 찾아 요청을 스트림 하나로 내려보내고
//!   답을 그대로 폰에 준다. 업링크가 없으면 「그 기계가 지금 안 붙어 있다」 화면.
//!
//! 관문은 **자격을 모른다.** 페이지·pane·유저 관리는 전부 그 앱이 자기 로컬 서버에서
//! 한다(`mobile.rs`). 여기 있는 건 배관뿐이다 — 릴레이 토큰도 안 본다(공용 문).
//! 대신 배관이 자격을 **새게 하지는 않는다**: 경로가 `/u/<slug>/` 밖으로 못 나가게 하고,
//! 폰이 실어 온 토큰·앱이 심는 쿠키는 어느 쪽으로도 넘기지 않는다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path as AxPath, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

#[path = "gateway_oauth.rs"]
mod oauth;
#[path = "gateway_workspace.rs"]
mod workspace;

use crate::uplink::{
    decode, encode, safe_path, skip_header, BODY, CLOSE, END, HEAD, OPEN, STREAM_QUEUE, WS_BIN, WS_PING, WS_PONG, WS_TEXT,
};

type Frame = (u8, Vec<u8>);

/// 관문은 인증 없이 열려 있다 — 누구나 업링크를 세울 수 있으니 자원에 상한을 둔다.
const MAX_SLUGS_PER_HELLO: usize = 32;
const MAX_UPLINKS_PER_KEY: usize = 8;
const MAX_UPLINKS: usize = 256;
const MAX_STREAMS_PER_UPLINK: usize = 512;
const MAX_BODY: usize = 64 * 1024 * 1024;
/// 이만큼 아무도 안 쓴 주소 묶음은 상태 파일에서 걷는다.
const SLUG_RETENTION_SECS: u64 = 90 * 24 * 3600;

struct Uplink {
    conn: u64,
    machine: String,
    machine_id: String,
    aliases: Vec<String>,
    last_seen: Mutex<Instant>,
    tx: mpsc::Sender<Message>,
    streams: Mutex<HashMap<u32, mpsc::Sender<Frame>>>,
    next: AtomicU32,
    /// 기기 토큰으로 로그인한 연결이면 그 계정·기기. 토큰 없는 옛 앱은 None(주소만 쓴다).
    account: Option<String>,
    device_id: Option<String>,
    owner_slug: Option<String>,
    nacho_app: AtomicBool,
    /// 기기가 폐기되면 이 연결을 끊는다.
    kick: tokio::sync::Notify,
}

/// 로그인한 기기 하나. 토큰은 sha256 만 남긴다.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct DeviceRec {
    token_hash: String,
    account: String,
    /// `desktop` 은 업링크로 붙는 카사텀, `phone` 은 카사모바일.
    kind: String,
    #[serde(default)]
    machine_id: Option<String>,
    #[serde(default)]
    label: String,
    created: u64,
    #[serde(default)]
    last_seen: u64,
    #[serde(default)]
    revoked_at: Option<u64>,
}

const UPLINK_STALE_AFTER: Duration = Duration::from_secs(75);

impl Uplink {
    fn fresh(&self) -> bool {
        self.last_seen
            .lock()
            .is_ok_and(|seen| seen.elapsed() < UPLINK_STALE_AFTER)
    }

    fn touch(&self) {
        if let Ok(mut seen) = self.last_seen.lock() {
            *seen = Instant::now();
        }
    }
}

#[derive(Clone, Copy)]
struct Candidate<'a> {
    index: usize,
    machine: &'a str,
    machine_id: &'a str,
    aliases: &'a [String],
    fresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoutePick {
    /// 대상 기계의 업링크를 직접 쓴다 — `/m/<기계>` 접두를 벗긴다.
    Machine(usize),
    /// 대상 업링크가 없어 기존 최신 업링크의 로컬 direct proxy로 보낸다.
    Fallback(usize),
    Missing,
    Ambiguous,
}

fn pick_route(candidates: &[Candidate<'_>], target: Option<&str>) -> RoutePick {
    let default = candidates
        .iter()
        .rev()
        .find(|candidate| candidate.fresh)
        .map(|candidate| candidate.index);
    let Some(target) = target else {
        return default.map_or(RoutePick::Missing, RoutePick::Fallback);
    };
    let stable_id = target.strip_prefix('~');
    let matching: Vec<&Candidate<'_>> = candidates
        .iter()
        .filter(|candidate| {
            candidate.fresh
                && stable_id.map_or_else(
                    || {
                        candidate.machine == target
                            || candidate.aliases.iter().any(|alias| alias == target)
                    },
                    |id| candidate.machine_id == id,
                )
        })
        .collect();
    let Some(last) = matching.last() else {
        if stable_id.is_some() {
            return RoutePick::Missing;
        }
        return default.map_or(RoutePick::Missing, RoutePick::Fallback);
    };
    if matching
        .iter()
        .any(|candidate| candidate.machine_id != last.machine_id)
    {
        return RoutePick::Ambiguous;
    }
    RoutePick::Machine(last.index)
}

fn live_machines(candidates: &[Candidate<'_>]) -> Vec<crate::uplink::GatewayMachine> {
    let mut by_id: HashMap<&str, crate::uplink::GatewayMachine> = HashMap::new();
    for candidate in candidates.iter().filter(|candidate| {
        candidate.fresh && !candidate.machine.is_empty() && !candidate.machine_id.starts_with("legacy-")
    }) {
        by_id.insert(
            candidate.machine_id,
            crate::uplink::GatewayMachine {
                id: candidate.machine_id.to_string(),
                machine: candidate.machine.to_string(),
                aliases: candidate.aliases.to_vec(),
            },
        );
    }
    let mut machines: Vec<_> = by_id.into_values().collect();
    machines.sort_by(|a, b| a.id.cmp(&b.id));
    machines
}

fn machine_route(rest: &str) -> Option<(&str, &str)> {
    let route = rest.strip_prefix("m/")?;
    let (machine, tail) = route.split_once('/')?;
    (!machine.is_empty() && !tail.is_empty()).then_some((machine, tail))
}

fn candidates_of(uplinks: &[Arc<Uplink>]) -> Vec<Candidate<'_>> {
    uplinks
        .iter()
        .enumerate()
        .map(|(index, uplink)| Candidate {
            index,
            machine: &uplink.machine,
            machine_id: &uplink.machine_id,
            aliases: &uplink.aliases,
            fresh: uplink.fresh(),
        })
        .collect()
}

#[derive(Clone)]
pub struct Gate {
    /// slug → 그 slug 를 알린 **살아 있는 연결들**(뒤가 최신). 하나만 두면 같은
    /// 기계에서 앱이 둘 뜬 경우(검증 리그·재시작 겹침)에 나중 것이 떠나며 먼저 것을
    /// 함께 떼어 버린다 — 2026-09-02 실측: 리그가 붙었다 떠나자 실앱 주소가 502.
    by_slug: Arc<Mutex<HashMap<String, Vec<Arc<Uplink>>>>>,
    /// slug → 키 해시. 처음 온 키가 주인이다.
    keys: Arc<Mutex<HashMap<String, SlugRec>>>,
    /// 살아 있는 업링크 전부(주소가 없는 연결 포함) → (키 해시, 연결). 상한 판정·기기 폐기용.
    live: Arc<Mutex<HashMap<u64, (String, Arc<Uplink>)>>>,
    /// 기기 id → 로그인 기록.
    devices: Arc<Mutex<HashMap<String, DeviceRec>>>,
    /// 관문 계정 → 그 계정 기기들이 올린 코딩 에이전트 계정 목록(`agent_accounts.rs`).
    agents: Arc<Mutex<HashMap<String, crate::agent_accounts::Book>>>,
    accounts: Arc<crate::relay_auth::Accounts>,
    oauth: Arc<crate::oauth_accounts::OAuth>,
    limiter: Arc<crate::relay_auth::Limiter>,
    account_sync: Arc<crate::account_sync::server::Store>,
    /// 계정별 개인비서(키·작업·대화). 상태 폴더가 없으면 `None` — 메모리에만 키를 두지 않는다.
    workspace: Option<Arc<workspace::Service>>,
    auth_changes: tokio::sync::watch::Sender<u64>,
    state_path: Option<PathBuf>,
    state_write: Arc<Mutex<()>>,
    seq: Arc<AtomicU64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SlugRec {
    key_hash: String,
    last_seen: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StateFile {
    version: u32,
    slugs: HashMap<String, SlugRec>,
    #[serde(default)]
    devices: HashMap<String, DeviceRec>,
    #[serde(default)]
    agent_accounts: HashMap<String, crate::agent_accounts::Book>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// v2(`{version, slugs:{slug:{key_hash,last_seen}}}`) 와 v1(slug→키 해시 평면 맵)을 둘 다 읽는다.
/// 읽을 수 없는 파일은 옆으로 치워 두고 빈 채로 뜬다 — 조용히 덮어쓰면 묶음이 흔적 없이 사라진다.
type Loaded = (
    HashMap<String, SlugRec>,
    HashMap<String, DeviceRec>,
    HashMap<String, crate::agent_accounts::Book>,
);

fn load_state(p: &std::path::Path, now: u64) -> Loaded {
    let Ok(raw) = std::fs::read_to_string(p) else {
        return Default::default();
    };
    let (slugs, devices, agents) = if let Ok(v2) = serde_json::from_str::<StateFile>(&raw) {
        (v2.slugs, v2.devices, v2.agent_accounts)
    } else if let Ok(v1) = serde_json::from_str::<HashMap<String, String>>(&raw) {
        let slugs = v1
            .into_iter()
            .map(|(slug, key_hash)| (slug, SlugRec { key_hash, last_seen: now }))
            .collect();
        (slugs, HashMap::new(), HashMap::new())
    } else {
        let aside = p.with_extension(format!("json.corrupt-{now}"));
        let _ = std::fs::rename(p, &aside);
        eprintln!("[gateway] 상태 파일을 못 읽어 {} 로 치웠어요", aside.display());
        return Default::default();
    };
    let slugs = slugs
        .into_iter()
        .filter(|(_, rec)| now.saturating_sub(rec.last_seen) < SLUG_RETENTION_SECS)
        .collect();
    (slugs, devices, agents)
}

impl Gate {
    /// 계정 파일은 상태 파일 옆의 `relay-accounts.json` 이다(`kasa-relay account …` 가 같은 곳을 고친다).
    pub fn new(state_path: Option<PathBuf>) -> Self {
        let accounts = state_path.as_ref().map(|p| p.with_file_name("relay-accounts.json"));
        Self::with_accounts(state_path, accounts)
    }

    pub fn with_accounts(state_path: Option<PathBuf>, accounts_path: Option<PathBuf>) -> Self {
        let (keys, devices, agents) = state_path
            .as_deref()
            .map(|p| load_state(p, now_secs()))
            .unwrap_or_default();
        Self {
            by_slug: Arc::new(Mutex::new(HashMap::new())),
            keys: Arc::new(Mutex::new(keys)),
            live: Arc::new(Mutex::new(HashMap::new())),
            devices: Arc::new(Mutex::new(devices)),
            agents: Arc::new(Mutex::new(agents)),
            accounts: Arc::new(crate::relay_auth::Accounts::new(accounts_path)),
            oauth: Arc::new(crate::oauth_accounts::OAuth::new(
                state_path.as_ref().map(|p| p.with_file_name("relay-oauth-identities.json")),
                crate::oauth_accounts::Config::from_env(),
            )),
            limiter: Arc::new(crate::relay_auth::Limiter::default()),
            account_sync: Arc::new(crate::account_sync::server::Store::new(
                state_path.as_ref().map(|p| p.with_file_name("account-sync")),
            )),
            workspace: workspace::Service::open(state_path.as_deref()),
            auth_changes: tokio::sync::watch::channel(0).0,
            state_path,
            state_write: Arc::new(Mutex::new(())),
            seq: Arc::new(AtomicU64::new(1)),
        }
    }

    /// 임시 파일에 쓰고 fsync 뒤 이름을 바꾼다(0600) — 쓰다 죽어도 반쪽 파일이 남지 않는다.
    fn persist(&self) {
        if let Err(error) = self.persist_result() {
            eprintln!("[gateway] state persistence failed: {error}");
        }
    }

    fn persist_result(&self) -> std::io::Result<()> {
        let Some(p) = &self.state_path else { return Ok(()) };
        // Concurrent requests must not share a temporary file or overwrite a newer snapshot.
        let unavailable = || std::io::Error::other("state lock unavailable");
        let _write = self.state_write.lock().map_err(|_| unavailable())?;
        let slugs = self.keys.lock().map_err(|_| unavailable())?.clone();
        let devices = self.devices.lock().map_err(|_| unavailable())?.clone();
        let agent_accounts = self.agents.lock().map_err(|_| unavailable())?.clone();
        let body = serde_json::to_string_pretty(&StateFile { version: 2, slugs, devices, agent_accounts })
            .map_err(std::io::Error::other)?;
        crate::relay_auth::write_private(p, &body)
    }

    fn account_active(&self, account: &str) -> bool {
        if self.accounts.exists(account) { self.accounts.active(account) } else { self.oauth.active(account) }
    }

    /// `Authorization: Bearer <기기 토큰>` 의 주인. 폐기됐거나 계정이 막혔으면 None.
    fn device_of(&self, headers: &axum::http::HeaderMap) -> Option<(String, DeviceRec)> {
        let token = headers
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?
            .trim();
        self.device_by_token(token)
    }

    fn device_by_token(&self, token: &str) -> Option<(String, DeviceRec)> {
        if !token.starts_with(crate::relay_auth::TOKEN_PREFIX) || token.len() > 200 {
            return None;
        }
        let want = crate::relay_auth::token_hash(token);
        let mut devices = self.devices.lock().unwrap();
        let (id, rec) = devices
            .iter_mut()
            .find(|(_, d)| d.revoked_at.is_none() && d.token_hash == want)?;
        if !self.account_active(&rec.account) {
            return None;
        }
        rec.last_seen = now_secs();
        Some((id.clone(), rec.clone()))
    }

    /// 기기를 폐기하고 그 기기의 업링크를 끊는다.
    fn revoke(&self, device_id: &str) -> bool {
        let done = {
            let mut devices = self.devices.lock().unwrap();
            match devices.get_mut(device_id) {
                Some(d) if d.revoked_at.is_none() => {
                    d.revoked_at = Some(now_secs());
                    true
                }
                _ => false,
            }
        };
        if done {
            self.auth_changes.send_modify(|n| *n = n.wrapping_add(1));
            for (_, up) in self.live.lock().unwrap().values() {
                if up.device_id.as_deref() == Some(device_id) {
                    up.kick.notify_one();
                }
            }
            self.persist();
        }
        done
    }

    /// 이 키가 이 slug 를 써도 되나 — 처음이면 묶고, 아니면 같은 키여야 한다.
    fn claim(&self, slug: &str, key_hash: &str) -> bool {
        let mut k = self.keys.lock().unwrap();
        let now = now_secs();
        match k.get_mut(slug) {
            Some(rec) if rec.key_hash == key_hash => {
                rec.last_seen = now;
                true
            }
            Some(_) => false,
            None => {
                k.insert(slug.to_string(), SlugRec { key_hash: key_hash.to_string(), last_seen: now });
                true
            }
        }
    }

    /// 새 업링크를 받아도 되나 — 전체·키당 상한을 넘으면 거절하고, 받으면 명단에 올린다.
    fn admit(&self, key_hash: &str, up: Arc<Uplink>) -> Result<(), &'static str> {
        let mut live = self.live.lock().unwrap();
        if live.len() >= MAX_UPLINKS {
            return Err("관문에 붙은 기계가 너무 많아요");
        }
        if live.values().filter(|(h, _)| h == key_hash).count() >= MAX_UPLINKS_PER_KEY {
            return Err("같은 열쇠로 붙은 연결이 너무 많아요");
        }
        live.insert(up.conn, (key_hash.to_string(), up));
        Ok(())
    }

    /// 이 계정의 기기가 지금 붙어 있나.
    fn device_online(&self, device_id: &str) -> bool {
        self.live
            .lock()
            .unwrap()
            .values()
            .any(|(_, up)| up.device_id.as_deref() == Some(device_id) && up.fresh())
    }

    /// 지금 붙어 있는 주소 수 — 상태 창구용.
    pub fn live(&self) -> usize {
        self.by_slug.lock().map(|m| m.len()).unwrap_or(0)
    }
}

fn key_hash(key: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(key.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn router(gate: Gate) -> Router {
    Router::new()
        .merge(oauth::routes())
        .merge(workspace::routes())
        .route("/relay/uplink", get(uplink_ws))
        .route("/relay/login", axum::routing::post(login))
        .route("/relay/whoami", get(whoami))
        .route("/relay/devices", get(devices_list))
        .route("/relay/account-sync", get(account_sync_get).patch(account_sync_patch))
        .route("/relay/account/", any(account_proxy_root))
        .route("/relay/account/{*rest}", any(account_proxy))
        .route("/relay/logout", axum::routing::post(logout))
        .route("/relay/devices/{id}/revoke", axum::routing::post(revoke_device))
        .route("/relay/agent-accounts", get(agent_accounts_get).post(agent_accounts_post))
        .route("/u/{slug}", any(need_slash))
        .route("/u/{slug}/", any(proxy_root))
        .route("/u/{slug}/{*rest}", any(proxy))
        .with_state(gate)
}

async fn uplink_ws(State(gate): State<Gate>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |s| uplink_run(gate, s))
}

// ── 계정 로그인과 기기 ──────────────────────────────────────────────────────────

fn json_err(status: StatusCode, code: &str) -> axum::response::Response {
    (status, axum::Json(serde_json::json!({ "ok": false, "error": code }))).into_response()
}

/// 시도 제한에 쓰는 출처. 관문은 cloudflared 뒤(loopback)에 있으니 그때만 CF 헤더를 믿는다 —
/// 직접 붙은 연결이 헤더를 위조해 남의 몫을 깎지 못하게.
fn client_ip(req: &axum::extract::Request) -> String {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0);
    match peer {
        Some(p) if !p.ip().is_loopback() => p.ip().to_string(),
        _ => req
            .headers()
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| "local".into()),
    }
}

fn valid_machine_id(id: &str) -> bool {
    (8..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

#[derive(serde::Deserialize)]
struct LoginBody {
    account: String,
    password: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    machine_id: Option<String>,
}

/// `POST /relay/login` — 아이디·비밀번호로 기기 토큰 하나를 받는다. 같은 기계로 다시 로그인하면
/// 그 기계의 옛 토큰은 폐기된다(기계 하나에 토큰 하나).
async fn login(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let ip = client_ip(&req);
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 16 * 1024).await else {
        return json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
    };
    let Ok(b) = serde_json::from_slice::<LoginBody>(&bytes) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    let account = b.account.trim().to_lowercase();
    let kind = match b.kind.as_str() {
        "" | "desktop" => "desktop",
        "phone" => "phone",
        _ => return json_err(StatusCode::BAD_REQUEST, "bad_kind"),
    };
    let machine_id = b.machine_id.filter(|m| valid_machine_id(m));
    if kind == "desktop" && machine_id.is_none() {
        return json_err(StatusCode::BAD_REQUEST, "machine_id_required");
    }
    let label: String = b.label.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    if let Err(wait) = gate.limiter.allow(&account, &ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(serde_json::json!({ "ok": false, "error": "rate_limited", "retry_after_secs": wait.as_secs().max(1) })),
        )
            .into_response();
    }
    let ok = crate::relay_auth::valid_account_name(&account) && !b.password.is_empty() && b.password.len() <= 1024 && {
        let accounts = gate.accounts.clone();
        let (name, pw) = (account.clone(), b.password);
        tokio::task::spawn_blocking(move || accounts.check(&name, &pw)).await.unwrap_or(false)
    };
    gate.limiter.record(&account, ok);
    if !ok {
        eprintln!("[gateway] 로그인 실패 — 계정 {account:?}, 출처 {ip}");
        return json_err(StatusCode::UNAUTHORIZED, "bad_credentials");
    }
    if kind == "desktop" {
        let old: Vec<String> = gate
            .devices
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, d)| d.revoked_at.is_none() && d.account == account && d.kind == "desktop" && d.machine_id == machine_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in old {
            gate.revoke(&id);
        }
    }
    let token = crate::relay_auth::new_token();
    let device_id = crate::relay_auth::new_device_id();
    let now = now_secs();
    gate.devices.lock().unwrap().insert(
        device_id.clone(),
        DeviceRec {
            token_hash: crate::relay_auth::token_hash(&token),
            account: account.clone(),
            kind: kind.to_string(),
            machine_id,
            label: label.clone(),
            created: now,
            last_seen: now,
            revoked_at: None,
        },
    );
    gate.persist();
    eprintln!("[gateway] 로그인 — 계정 {account}, 기기 {device_id}({kind} {label:?})");
    axum::Json(serde_json::json!({ "ok": true, "account": account, "device_id": device_id, "token": token }))
        .into_response()
}

async fn whoami(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let Some((id, d)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    axum::Json(serde_json::json!({
        "ok": true, "account": d.account, "device_id": id, "kind": d.kind,
        "label": d.label, "machine_id": d.machine_id,
    }))
    .into_response()
}

async fn devices_list(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let Some((me, d)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let mut mine: Vec<(String, DeviceRec)> = gate
        .devices
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, x)| x.account == d.account && x.revoked_at.is_none())
        .map(|(id, x)| (id.clone(), x.clone()))
        .collect();
    mine.sort_by_key(|(_, x)| x.created);
    let list: Vec<serde_json::Value> = mine
        .into_iter()
        .map(|(id, x)| {
            serde_json::json!({
                "device_id": id, "kind": x.kind, "label": x.label, "machine_id": x.machine_id,
                "created": x.created, "last_seen": x.last_seen,
                "online": gate.device_online(&id), "current": id == me,
            })
        })
        .collect();
    axum::Json(serde_json::json!({ "ok": true, "account": d.account, "devices": list })).into_response()
}

fn account_sync_response(
    headers: &axum::http::HeaderMap,
    result: Result<crate::account_sync::schema::Snapshot, crate::account_sync::server::Error>,
) -> axum::response::Response {
    use crate::account_sync::schema::{accepted_keys, visible, KEYS_HEADER};
    use crate::account_sync::server::Error;
    let accepted = accepted_keys(headers.get(KEYS_HEADER).and_then(|v| v.to_str().ok()));
    let result = match result {
        Ok(snapshot) => Ok(visible(snapshot, &accepted)),
        Err(Error::Conflict(snapshot)) => Err(Error::Conflict(visible(snapshot, &accepted))),
        Err(e) => Err(e),
    };
    match result {
        Ok(snapshot) => (
            [(header::CACHE_CONTROL, "no-store")], axum::Json(snapshot),
        ).into_response(),
        Err(Error::Conflict(snapshot)) => (
            StatusCode::CONFLICT, [(header::CACHE_CONTROL, "no-store")],
            axum::Json(serde_json::json!({"error":"revision_conflict", "current":snapshot})),
        ).into_response(),
        Err(Error::Invalid(code)) => json_err(StatusCode::BAD_REQUEST, &code),
        Err(Error::Storage) => json_err(StatusCode::INTERNAL_SERVER_ERROR, "sync_storage_unavailable"),
    }
}

async fn account_sync_get(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let Some((id, device)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let devices = gate.devices.lock().unwrap();
    if !devices.get(&id).is_some_and(|d| d.revoked_at.is_none()) || !gate.account_active(&device.account) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    account_sync_response(&headers, gate.account_sync.get(&device.account))
}

async fn account_sync_patch(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let headers = req.headers().clone();
    if gate.device_of(&headers).is_none() { return json_err(StatusCode::UNAUTHORIZED, "unauthorized"); }
    let bytes = match tokio::time::timeout(Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), crate::account_sync::schema::MAX_BODY)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
        Err(_) => return json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
    };
    let Ok(patch) = serde_json::from_slice::<crate::account_sync::schema::Patch>(&bytes) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    // Authentication is repeated after receiving the body so revocation cannot race a slow upload.
    let Some((id, device)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let devices = gate.devices.lock().unwrap();
    if !devices.get(&id).is_some_and(|d| d.revoked_at.is_none()) || !gate.account_active(&device.account) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    account_sync_response(&headers, gate.account_sync.patch(&device.account, &patch))
}

async fn logout(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let Some((id, _)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    gate.revoke(&id);
    axum::Json(serde_json::json!({ "ok": true })).into_response()
}

/// 같은 계정의 다른 기기를 끊는다(분실·교체).
async fn revoke_device(
    State(gate): State<Gate>,
    AxPath(target): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some((_, d)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let same_account = gate.devices.lock().unwrap().get(&target).is_some_and(|t| t.account == d.account);
    if !same_account || !gate.revoke(&target) {
        return json_err(StatusCode::NOT_FOUND, "no_such_device");
    }
    axum::Json(serde_json::json!({ "ok": true })).into_response()
}

/// 같은 관문 계정의 폐기 안 된 기기들(id → 이름).
fn live_devices(devices: &HashMap<String, DeviceRec>, account: &str) -> HashMap<String, String> {
    devices
        .iter()
        .filter(|(_, d)| d.account == account && d.revoked_at.is_none())
        .map(|(id, d)| (id.clone(), if d.label.is_empty() { id.clone() } else { d.label.clone() }))
        .collect()
}

fn agent_accounts_response(response: impl IntoResponse) -> axum::response::Response {
    let mut response = response.into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert(header::VARY, "Authorization".parse().unwrap());
    response
}

fn agent_accounts_error(status: StatusCode, code: &str) -> axum::response::Response {
    agent_accounts_response(json_err(status, code))
}

fn agent_accounts_reply(gate: &Gate, devices: &HashMap<String, DeviceRec>, account: &str, me: &str) -> axum::response::Response {
    let live = live_devices(devices, account);
    let list = gate
        .agents
        .lock()
        .unwrap()
        .get(account)
        .map(|book| crate::agent_accounts::view(book, &live, me))
        .unwrap_or_default();
    agent_accounts_response(axum::Json(serde_json::json!({ "ok": true, "accounts": list })))
}

fn catalog_device_active(gate: &Gate, devices: &HashMap<String, DeviceRec>, me: &str, device: &DeviceRec) -> bool {
    devices.get(me).is_some_and(|current| current.revoked_at.is_none()
        && current.account == device.account && current.token_hash == device.token_hash)
        && gate.account_active(&device.account)
}

/// `GET /relay/agent-accounts` — 이 관문 계정의 기기들이 올린 코딩 에이전트 계정 목록.
/// 자격증명은 없다 — 종류·신원·이름·어느 기기에 로그인돼 있나뿐.
async fn agent_accounts_get(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let Some((me, d)) = gate.device_of(&headers) else {
        return agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let devices = gate.devices.lock().unwrap();
    if !catalog_device_active(&gate, &devices, &me, &d) {
        return agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    agent_accounts_reply(&gate, &devices, &d.account, &me)
}

/// `POST /relay/agent-accounts {accounts:[…]}` — 이 기기 몫을 갈아 끼우고 합친 목록을 돌려준다.
async fn agent_accounts_post(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let headers = req.headers().clone();
    if gate.device_of(&headers).is_none() {
        return agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let bytes = match tokio::time::timeout(Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), 64 * 1024)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return agent_accounts_error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
        Err(_) => return agent_accounts_error(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
    };
    #[derive(serde::Deserialize)]
    struct Body {
        accounts: Vec<crate::agent_accounts::LocalAccount>,
    }
    let Ok(b) = serde_json::from_slice::<Body>(&bytes) else {
        return agent_accounts_error(StatusCode::BAD_REQUEST, "bad_request");
    };
    let Some((me, d)) = gate.device_of(&headers) else {
        return agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let response = {
        // Revocation shares this lock; a slow upload cannot publish after its device was retired.
        let devices = gate.devices.lock().unwrap();
        if !catalog_device_active(&gate, &devices, &me, &d) {
            return agent_accounts_error(StatusCode::UNAUTHORIZED, "unauthorized");
        }
        let alive: std::collections::HashSet<String> = live_devices(&devices, &d.account).into_keys().collect();
        {
            let mut agents = gate.agents.lock().unwrap();
            let book = agents.entry(d.account.clone()).or_default();
            crate::agent_accounts::prune(book, &alive);
            if let Err(code) = crate::agent_accounts::publish(book, &me, &b.accounts, now_secs()) {
                return agent_accounts_error(StatusCode::BAD_REQUEST, code);
            }
        }
        agent_accounts_reply(&gate, &devices, &d.account, &me)
    };
    gate.persist();
    response
}

struct Hello {
    key: String,
    slugs: Vec<String>,
    machine: String,
    machine_id: Option<String>,
    aliases: Vec<String>,
    /// 로그인한 앱만 싣는다(proto 2).
    device_token: Option<String>,
    owner_slug: Option<String>,
    nacho_app: bool,
}

fn parse_hello(v: &serde_json::Value) -> Option<Hello> {
    if v.get("t")?.as_str()? != "hello" {
        return None;
    }
    let key = v.get("key")?.as_str()?.to_string();
    if key.len() < 16 || key.len() > 200 {
        return None;
    }
    let slugs: Vec<String> = v
        .get("slugs")?
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str())
        .filter(|s| crate::mobile::valid_slug(s))
        .take(MAX_SLUGS_PER_HELLO)
        .map(str::to_string)
        .collect();
    let machine: String = v
        .get("machine")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    let machine_id = v
        .get("machine_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| {
            (8..=128).contains(&value.len())
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
        .map(str::to_string);
    let mut aliases: Vec<String> = v
        .get("machine_aliases")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.chars().count() <= 80
                && !value.chars().any(char::is_control)
        })
        .map(str::to_string)
        .collect();
    aliases.push(machine.clone());
    aliases.sort();
    aliases.dedup();
    let device_token = v
        .get("device_token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty() && t.len() <= 200)
        .map(str::to_string);
    let owner_slug = v.get("owner_slug").and_then(|v| v.as_str())
        .filter(|s| crate::mobile::valid_slug(s) && slugs.iter().any(|slug| slug == s))
        .map(str::to_string);
    let nacho_app = v.get("capabilities").and_then(|v| v.get("nacho_app"))
        .and_then(|v| v.as_bool()).unwrap_or(false);
    Some(Hello { key, slugs, machine, machine_id, aliases, device_token, owner_slug, nacho_app })
}

/// 연결 `conn` 만 그 slug 에서 뗀다 — 같은 slug 의 다른 살아 있는 연결은 남는다.
fn drop_conn(map: &mut HashMap<String, Vec<Arc<Uplink>>>, slug: &str, conn: u64) {
    if let Some(v) = map.get_mut(slug) {
        v.retain(|u| u.conn != conn);
        if v.is_empty() {
            map.remove(slug);
        }
    }
}

async fn uplink_run(gate: Gate, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let first = tokio::time::timeout(Duration::from_secs(10), rx.next()).await;
    let hello = match first {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<serde_json::Value>(t.as_str()).ok(),
        _ => None,
    };
    let Some(Hello { key, slugs, machine, machine_id, aliases, device_token, owner_slug, nacho_app }) = hello.as_ref().and_then(parse_hello)
    else {
        let _ = tx
            .send(Message::Text(r#"{"t":"err","error":"hello 가 없거나 이상해요"}"#.into()))
            .await;
        return;
    };
    // 기기 토큰은 그 기기의 기계에서만 쓴다 — 설정 폴더를 통째로 복사한 다른 기계가 같은
    // 토큰으로 붙으면 거절한다. 거절돼도 앱은 토큰 없이 다시 붙어 주소(폰)는 산다.
    let device = match device_token.as_deref() {
        None => None,
        Some(token) => match gate.device_by_token(token) {
            Some((id, rec))
                if rec.kind == "desktop"
                    && rec.machine_id.as_deref().is_none_or(|m| Some(m) == machine_id.as_deref()) =>
            {
                Some((id, rec.account))
            }
            _ => {
                let _ = tx
                    .send(Message::Text(r#"{"t":"err","error":"device_token_invalid"}"#.into()))
                    .await;
                return;
            }
        },
    };
    let hash = key_hash(&key);
    let conn = gate.seq.fetch_add(1, Ordering::Relaxed);
    // 옛 앱은 stable id를 안 보낸다. 연결별 id로 두면 같은 표시 이름이 둘 뜬 순간
    // 모호함으로 닫히고, 이름만 보고 임의의 기계를 고르는 것보다 안전하다.
    let machine_id = machine_id.unwrap_or_else(|| format!("legacy-{conn}"));
    let (wtx, mut wrx) = mpsc::channel::<Message>(256);
    let (device_id, account) = device.map_or((None, None), |(id, account)| (Some(id), Some(account)));
    let up = Arc::new(Uplink {
        conn,
        machine: machine.clone(),
        machine_id,
        aliases,
        last_seen: Mutex::new(Instant::now()),
        tx: wtx.clone(),
        streams: Mutex::new(HashMap::new()),
        next: AtomicU32::new(1),
        account: account.clone(),
        device_id: device_id.clone(),
        owner_slug: owner_slug.filter(|slug| gate.claim(slug, &hash)),
        nacho_app: AtomicBool::new(nacho_app && account.is_some()),
        kick: tokio::sync::Notify::new(),
    });
    if let Err(why) = gate.admit(&hash, up.clone()) {
        let _ = tx
            .send(Message::Text(serde_json::json!({ "t": "err", "error": why }).to_string().into()))
            .await;
        return;
    }
    let apply = |slugs: &[String]| -> (Vec<String>, Vec<String>) {
        let mut acc = Vec::new();
        let mut rej = Vec::new();
        for s in slugs {
            if gate.claim(s, &hash) {
                let mut map = gate.by_slug.lock().unwrap();
                let v = map.entry(s.clone()).or_default();
                if !v.iter().any(|u| u.conn == conn) {
                    v.push(up.clone());
                }
                acc.push(s.clone());
            } else {
                rej.push(s.clone());
            }
        }
        gate.persist();
        (acc, rej)
    };
    let (acc, rej) = apply(&slugs);
    eprintln!(
        "[gateway] {machine} 붙음(#{conn}) — 주소 {}개{}{}",
        acc.len(),
        if rej.is_empty() { String::new() } else { format!(", 거절 {}개(다른 기계 소유)", rej.len()) },
        account.as_deref().map(|a| format!(", 계정 {a}")).unwrap_or_default(),
    );
    let _ = tx
        .send(Message::Text(
            serde_json::json!({
                "t": "ok", "accepted": acc, "rejected": rej,
                "proto": 2, "account": account, "device_id": device_id,
            })
            .to_string()
            .into(),
        ))
        .await;
    let writer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await;
        loop {
            tokio::select! {
                m = wrx.recv() => match m {
                    // Close 를 보내고 나면 쓸 것이 없다 — 끝나야 폐기 통보가 버려지지 않고 나간다.
                    Some(m @ Message::Close(_)) => { let _ = tx.send(m).await; break }
                    Some(m) => if tx.send(m).await.is_err() { break },
                    None => break,
                },
                _ = tick.tick() => if tx.send(Message::Ping(Vec::new().into())).await.is_err() { break },
            }
        }
    });
    let mut writer = writer;
    let mut mine: Vec<String> = acc;
    loop {
        let m = tokio::select! {
            m = rx.next() => m,
            _ = up.kick.notified() => {
                eprintln!("[gateway] {machine} 기기가 폐기돼 끊음(#{conn})");
                let _ = wtx.send(Message::Text(r#"{"t":"err","error":"device_revoked"}"#.into())).await;
                let _ = wtx.send(Message::Close(None)).await;
                let _ = tokio::time::timeout(Duration::from_secs(2), &mut writer).await;
                break;
            }
        };
        let Some(m) = m else { break };
        match m {
            Ok(Message::Binary(b)) => {
                up.touch();
                let Some((kind, id, payload)) = decode(&b) else { continue };
                let s = up.streams.lock().unwrap().get(&id).cloned();
                if let Some(s) = s {
                    // 기다리지 않는다 — 폰 하나가 안 읽으면 이 기계의 다른 요청이 전부 선다.
                    // 밀린 스트림은 떼어 내고 앱에 CLOSE 로 알린다(떼어 낸 뒤라 guard 는 안 보낸다).
                    match s.try_send((kind, payload.to_vec())) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            up.streams.lock().unwrap().remove(&id);
                            let _ = wtx.try_send(Message::Binary(encode(CLOSE, id, b"overflow").into()));
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            up.streams.lock().unwrap().remove(&id);
                        }
                    }
                }
            }
            Ok(Message::Text(t)) => {
                up.touch();
                // hello 를 다시 보내면 slug 목록 갱신(유저가 늘었다).
                if let Some(Hello { key: k2, slugs: slugs2, device_token, nacho_app, .. }) = serde_json::from_str::<serde_json::Value>(t.as_str())
                    .ok()
                    .as_ref()
                    .and_then(parse_hello)
                {
                    if key_hash(&k2) != hash {
                        continue;
                    }
                    let authenticated = device_token.as_deref().and_then(|token| gate.device_by_token(token))
                        .is_some_and(|(id, rec)| rec.kind == "desktop"
                            && up.device_id.as_deref() == Some(id.as_str())
                            && up.account.as_deref() == Some(rec.account.as_str()));
                    up.nacho_app.store(nacho_app && authenticated, Ordering::Relaxed);
                    let (acc2, rej2) = apply(&slugs2);
                    // 빠진 slug(유저 삭제)는 이 연결에서 뗀다. 잠금은 블록 안에서만 —
                    // 아래 await 를 넘기면 이 future 가 Send 가 아니게 된다.
                    {
                        let mut map = gate.by_slug.lock().unwrap();
                        for old in mine.iter().filter(|s| !acc2.contains(s)) {
                            drop_conn(&mut map, old, conn);
                        }
                    }
                    mine = acc2.clone();
                    let _ = wtx
                        .send(Message::Text(
                            serde_json::json!({ "t": "ok", "accepted": acc2, "rejected": rej2 }).to_string().into(),
                        ))
                        .await;
                }
            }
            Ok(Message::Ping(p)) => {
                up.touch();
                let _ = wtx.send(Message::Pong(p)).await;
            }
            Ok(Message::Pong(_)) => up.touch(),
            Ok(Message::Close(_)) | Err(_) => break,
        }
    }
    writer.abort();
    // 내 slug 만 뗀다 — 그 사이 같은 slug 로 새 연결이 붙었으면 그건 남긴다.
    {
        let mut map = gate.by_slug.lock().unwrap();
        for s in &mine {
            drop_conn(&mut map, s, conn);
        }
    }
    gate.live.lock().unwrap().remove(&conn);
    up.streams.lock().unwrap().clear();
    eprintln!(
        "[gateway] {machine} 떨어짐(#{conn}){}",
        up.account.as_deref().map(|a| format!(" — 계정 {a}")).unwrap_or_default()
    );
}

fn offline_page(slug_ok: bool) -> axum::response::Response {
    let msg = if slug_ok {
        "이 주소의 카사텀이 지금 안 붙어 있어요. 그 기계에서 앱이 켜져 있고 우하단 「● 바깥」이 켜져 있는지 봐 주세요."
    } else {
        "그런 주소가 없어요."
    };
    let html = format!(
        "<!doctype html><html lang=ko><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1,viewport-fit=cover\"><title>kasaterm</title>\
         <body style=\"margin:0;min-height:100dvh;display:flex;align-items:center;justify-content:center;background:#12161c;color:#c8d0d9;font:16px/1.6 -apple-system,system-ui,sans-serif;padding:24px;box-sizing:border-box\">\
         <div style=\"max-width:360px\"><b style=\"display:block;margin-bottom:8px\">kasaterm</b>{msg}</div></body></html>"
    );
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

async fn need_slash(AxPath(slug): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    axum::response::Redirect::temporary(&format!("/u/{slug}/{q}")).into_response()
}

async fn proxy_root(State(gate): State<Gate>, AxPath(slug): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    forward(gate, slug, String::new(), req).await
}

async fn proxy(State(gate): State<Gate>, AxPath((slug, rest)): AxPath<(String, String)>, req: axum::extract::Request) -> axum::response::Response {
    forward(gate, slug, rest, req).await
}

#[derive(Clone)]
struct AccountAccess {
    gate: Gate,
    account: String,
    device_id: String,
    token_hash: String,
}

impl AccountAccess {
    fn valid(&self) -> bool {
        let devices = self.gate.devices.lock().unwrap();
        devices.get(&self.device_id).is_some_and(|d| d.revoked_at.is_none()
            && d.account == self.account && d.token_hash == self.token_hash)
            && self.gate.account_active(&self.account)
    }
}

fn account_access(gate: &Gate, headers: &axum::http::HeaderMap) -> Option<AccountAccess> {
    let device = if headers.contains_key(header::AUTHORIZATION) {
        gate.device_of(headers)
    } else if headers.get(header::UPGRADE).and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        let protocols = headers.get(header::SEC_WEBSOCKET_PROTOCOL)?.to_str().ok()?;
        let token = protocols.split(',').map(str::trim).find_map(|s| s.strip_prefix("kasa-auth."))?;
        gate.device_by_token(token)
    } else { None }?;
    Some(AccountAccess { gate: gate.clone(), account: device.1.account,
        device_id: device.0, token_hash: device.1.token_hash })
}

async fn account_proxy_root(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    account_forward(gate, String::new(), req).await
}

async fn account_proxy(State(gate): State<Gate>, AxPath(rest): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    account_forward(gate, rest, req).await
}

async fn account_forward(gate: Gate, rest: String, req: axum::extract::Request) -> axum::response::Response {
    let Some(access) = account_access(&gate, req.headers()) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    forward_scoped(gate, String::new(), rest, req, Some(access)).await
}

/// 스트림 하나의 수명 — 떨어질 때 관문 표에서 빠지고 앱에 CLOSE 를 알린다.
struct StreamGuard {
    up: Arc<Uplink>,
    id: u32,
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        if self.up.streams.lock().unwrap().remove(&self.id).is_some() {
            let _ = self.up.tx.try_send(Message::Binary(encode(CLOSE, self.id, b"").into()));
        }
    }
}

/// 요청 경로에서 `/u/<slug>/` 뒤의 **원문**(퍼센트 인코딩 그대로). 추출기가 준 `rest` 는
/// 이미 디코딩돼 있어, 그대로 넘기면 `%2e%2e` 가 앱에서 `..` 로 조립된다.
fn raw_rest<'a>(path: &'a str, slug: &str) -> &'a str {
    path.strip_prefix(crate::mobile::PREFIX)
        .and_then(|p| p.strip_prefix(slug))
        .map(|p| p.strip_prefix('/').unwrap_or(p))
        .unwrap_or("")
}

/// 폰이 실어 온 헤더 중 앱으로 넘기지 않는 것. 쿠키·Origin 류는 로컬 관문을 헷갈리게
/// 하고, 토큰류는 **주소 말고 다른 자격**을 관문 너머로 들이게 된다.
fn drop_request_header(k: &str) -> bool {
    skip_header(k)
        || crate::uplink::is_internal_header(k)
        || matches!(
            k,
            "cookie"
                | "origin"
                | "referer"
                | "authorization"
                | "x-kasa-token"
                | "x-forwarded-for"
                | "x-forwarded-proto"
                | "x-forwarded-host"
                | "cf-connecting-ip"
                | "cf-ray"
                | "cf-visitor"
                | "cdn-loop"
        )
        || k.starts_with("sec-")
}

async fn forward(gate: Gate, slug: String, rest: String, req: axum::extract::Request) -> axum::response::Response {
    forward_scoped(gate, slug, rest, req, None).await
}

fn nacho_app_path(rest: &str) -> bool {
    let rest = machine_route(rest).map_or(rest, |(_, rest)| rest);
    rest == "nacho/app" || rest.starts_with("nacho/app/")
}

fn nacho_hub_available(gate: &Gate, up: &Uplink) -> bool {
    if !up.nacho_app.load(Ordering::Relaxed) { return false; }
    let (Some(account), Some(device_id), Some(owner)) = (&up.account, &up.device_id, &up.owner_slug) else {
        return false;
    };
    let device_valid = gate.devices.lock().unwrap().get(device_id).is_some_and(|d|
        d.revoked_at.is_none() && d.kind == "desktop" && d.account == *account
            && d.machine_id.as_deref().is_none_or(|id| id == up.machine_id));
    device_valid && gate.by_slug.lock().unwrap().get(owner)
        .is_some_and(|owners| owners.iter().any(|owner| owner.conn == up.conn))
}

async fn forward_scoped(gate: Gate, slug: String, rest: String, req: axum::extract::Request,
    access: Option<AccountAccess>) -> axum::response::Response {
    if access.is_none() && !crate::mobile::valid_slug(&slug) {
        return offline_page(false);
    }
    let raw = if access.is_some() { req.uri().path().strip_prefix("/relay/account/").unwrap_or("") }
        else { raw_rest(req.uri().path(), &slug) }.to_string();
    if !safe_path(&raw) {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let needs_nacho = access.is_some() && nacho_app_path(&rest);
    let mut uplinks = if let Some(access) = &access {
        if !access.valid() { return json_err(StatusCode::UNAUTHORIZED, "unauthorized"); }
        gate.live.lock().unwrap().values().filter(|(_, up)| up.account.as_deref() == Some(&access.account)
            && up.owner_slug.is_some()).map(|(_, up)| up.clone()).collect::<Vec<_>>()
    } else { gate
        .by_slug
        .lock()
        .ok()
        .and_then(|map| map.get(&slug).cloned())
        .unwrap_or_default() };
    if needs_nacho {
        // Only a device-authenticated hub advertises local credentials; request headers cannot select one.
        uplinks.retain(|up| nacho_hub_available(&gate, up));
    }
    uplinks.sort_by_key(|up| up.conn);
    let candidates = candidates_of(&uplinks);
    let requested_machine = machine_route(&rest).map(|(machine, _)| machine);
    let route = pick_route(&candidates, requested_machine);
    if access.is_some() && (route == RoutePick::Missing
        || requested_machine.is_some() && matches!(route, RoutePick::Fallback(_))) {
        return json_err(StatusCode::SERVICE_UNAVAILABLE,
            if needs_nacho { "nacho_hub_unavailable" } else { "account_device_unavailable" });
    }
    if route == RoutePick::Ambiguous {
        return (
            StatusCode::CONFLICT,
            "같은 이름의 기계가 둘이라 어느 쪽인지 고를 수 없어요",
        )
            .into_response();
    }
    let index = match route {
        RoutePick::Machine(index) | RoutePick::Fallback(index) => index,
        RoutePick::Missing | RoutePick::Ambiguous => {
        // 한 번도 등록된 적 없는 slug 는 「없는 주소」, 등록됐다 떨어진 slug 는 「안 붙어 있음」
        // — 폰에서 할 일이 다르다(주소를 다시 받기 vs 그 기계 앱 켜기).
        let known = gate.keys.lock().map(|k| k.contains_key(&slug)).unwrap_or(false);
        return offline_page(known);
        }
    };
    let up = uplinks[index].clone();
    let slug = if access.is_some() { up.owner_slug.clone().unwrap_or_default() } else { slug };
    // 기계 고르기는 디코딩된 이름으로 하고, 넘기는 건 원문 — `m/<기계>/` 두 조각만 벗긴다.
    let routed_rest = if matches!(route, RoutePick::Machine(_)) {
        raw.splitn(3, '/').nth(2).unwrap_or("")
    } else {
        raw.as_str()
    };
    if up.streams.lock().unwrap().len() >= MAX_STREAMS_PER_UPLINK {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many streams").into_response();
    }
    let live_machines = live_machines(&candidates);
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let path = format!("/{routed_rest}{query}");
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let headers: Vec<serde_json::Value> = req
        .headers()
        .iter()
        .filter(|(k, _)| !drop_request_header(k.as_str()))
        .filter_map(|(k, v)| Some(serde_json::json!([k.as_str(), v.to_str().ok()?])))
        .collect();
    let id = up.next.fetch_add(1, Ordering::Relaxed);
    let (stx, mut srx) = mpsc::channel::<Frame>(STREAM_QUEUE);
    up.streams.lock().unwrap().insert(id, stx);
    let guard = StreamGuard { up: up.clone(), id };
    let open = serde_json::json!({
        "slug": slug, "method": req.method().as_str(), "path": path, "headers": headers,
        "machines": live_machines, "ws": is_ws,
    })
    .to_string();
    if up.tx.send(Message::Binary(encode(OPEN, id, open.as_bytes()).into())).await.is_err() {
        return offline_page(true);
    }
    if is_ws {
        use axum::extract::FromRequestParts as _;
        let (mut parts, _body) = req.into_parts();
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(w) => w,
            Err(e) => return e.into_response(),
        };
        let ws = if access.is_some() { ws.protocols(["kasa-relay-account"]) } else { ws };
        return ws.on_upgrade(move |sock| ws_pipe(guard, srx, sock, access)).into_response();
    }
    // HTTP 바디는 받는 대로 흘려 보낸다 — 통째 모으면 요청 하나가 관문 메모리를 64MB 씩 문다.
    // 상한은 흘린 바이트로 센다. 넘으면 guard 가 떨어지며 앱에 CLOSE 가 간다.
    let (_parts, body) = req.into_parts();
    let mut body = body.into_data_stream();
    let mut sent = 0usize;
    while let Some(chunk) = body.next().await {
        if access.as_ref().is_some_and(|a| !a.valid()) {
            return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
        }
        let Ok(chunk) = chunk else {
            return (StatusCode::BAD_REQUEST, "body read failed").into_response();
        };
        sent += chunk.len();
        if sent > MAX_BODY {
            return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
        }
        for part in chunk.chunks(crate::uplink::CHUNK) {
            if up.tx.send(Message::Binary(encode(BODY, id, part).into())).await.is_err() {
                return offline_page(true);
            }
        }
    }
    if up.tx.send(Message::Binary(encode(END, id, b"").into())).await.is_err() {
        return offline_page(true);
    }
    // 앱의 답 머리 — 30초 안에 안 오면 그 기계가 멈춘 것.
    let head = loop {
        match tokio::time::timeout(Duration::from_secs(30), srx.recv()).await {
            Ok(Some((HEAD, p))) => break serde_json::from_slice::<serde_json::Value>(&p).ok(),
            Ok(Some((CLOSE, _))) | Ok(None) => break None,
            Ok(Some(_)) => continue,
            Err(_) => break None,
        }
    };
    let Some(head) = head else {
        return (StatusCode::BAD_GATEWAY, format!("{} 이(가) 답하지 않았어요", up.machine)).into_response();
    };
    let status = head.get("status").and_then(|s| s.as_u64()).unwrap_or(502) as u16;
    let mut out = axum::response::Response::builder().status(status);
    if let Some(hs) = head.get("headers").and_then(|h| h.as_array()) {
        for kv in hs {
            if let (Some(k), Some(v)) = (kv.get(0).and_then(|x| x.as_str()), kv.get(1).and_then(|x| x.as_str())) {
                // 앱이 심는 쿠키는 그 기계의 원격 토큰이다 — 주소만 아는 폰에 내주면 안 된다.
                if !skip_header(k) && !k.eq_ignore_ascii_case("set-cookie") {
                    out = out.header(k, v);
                }
            }
        }
    }
    let authenticated = access.is_some();
    let stream = async_stream(srx, guard, access);
    let mut response = out.body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    if authenticated {
        response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        response.headers_mut().insert(header::VARY, "Authorization".parse().unwrap());
    }
    response
}

/// BODY 조각을 응답 스트림으로 — END·CLOSE 에서 끝난다. guard 는 스트림과 수명을 같이한다.
fn async_stream(
    srx: mpsc::Receiver<Frame>,
    guard: StreamGuard,
    access: Option<AccountAccess>,
) -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> {
    futures_util::stream::unfold((srx, guard, false, access), |(mut srx, guard, done, access)| async move {
        if done {
            return None;
        }
        // 이 스트림이 버려지면 guard 도 같이 떨어져 CLOSE 가 나간다.
        loop {
            let next = srx.recv().await;
            if access.as_ref().is_some_and(|a| !a.valid()) { return None; }
            match next {
                Some((BODY, p)) => return Some((Ok(p), (srx, guard, false, access))),
                Some((END, _)) | Some((CLOSE, _)) | None => return None,
                Some(_) => continue,
            }
        }
    })
}

async fn ws_pipe(guard: StreamGuard, mut srx: mpsc::Receiver<Frame>, sock: WebSocket, access: Option<AccountAccess>) {
    let up = guard.up.clone();
    let id = guard.id;
    let (mut ctx, mut crx) = sock.split();
    // 앱이 로컬 WS 에 붙었는지(101) 먼저.
    let ok = loop {
        match tokio::time::timeout(Duration::from_secs(20), srx.recv()).await {
            Ok(Some((HEAD, p))) => {
                let v: serde_json::Value = serde_json::from_slice(&p).unwrap_or_default();
                break v.get("status").and_then(|s| s.as_u64()) == Some(101);
            }
            Ok(Some((CLOSE, _))) | Ok(None) | Err(_) => break false,
            Ok(Some(_)) => continue,
        }
    };
    if !ok {
        let _ = ctx
            .send(Message::Text(serde_json::json!({ "t": "gone", "why": "machine ws failed" }).to_string().into()))
            .await;
        return;
    }
    let send = |kind: u8, payload: Vec<u8>| {
        let tx = up.tx.clone();
        async move { tx.send(Message::Binary(encode(kind, id, &payload).into())).await.is_ok() }
    };
    let mut auth_tick = tokio::time::interval(Duration::from_secs(5));
    let mut auth_changes = access.as_ref().map(|a| a.gate.auth_changes.subscribe());
    loop {
        if access.as_ref().is_some_and(|a| !a.valid()) { break; }
        tokio::select! {
            biased;
            _ = auth_tick.tick(), if access.is_some() => {},
            _ = async { match auth_changes.as_mut() {
                Some(changes) => { let _ = changes.changed().await; },
                None => std::future::pending::<()>().await,
            } }, if access.is_some() => {},
            m = crx.next() => match m {
                Some(Ok(Message::Text(t))) => if !send(WS_TEXT, t.as_str().as_bytes().to_vec()).await { break },
                Some(Ok(Message::Binary(b))) => if !send(WS_BIN, b.to_vec()).await { break },
                Some(Ok(Message::Ping(p))) => if !send(WS_PING, p.to_vec()).await { break },
                Some(Ok(Message::Pong(p))) => if !send(WS_PONG, p.to_vec()).await { break },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            },
            f = srx.recv() => match f {
                Some((WS_TEXT, p)) => {
                    let s = String::from_utf8_lossy(&p).to_string();
                    if ctx.send(Message::Text(s.into())).await.is_err() { break }
                }
                Some((WS_BIN, p)) => if ctx.send(Message::Binary(p.into())).await.is_err() { break },
                Some((WS_PING, p)) => if ctx.send(Message::Ping(p.into())).await.is_err() { break },
                Some((WS_PONG, p)) => if ctx.send(Message::Pong(p.into())).await.is_err() { break },
                Some((CLOSE, _)) | None => break,
                Some(_) => {}
            },
        }
    }
    let _ = ctx.close().await;
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_uplink(machine: &str, machine_id: &str, age: Duration) -> Arc<Uplink> {
        let (tx, _rx) = mpsc::channel(1);
        Arc::new(Uplink {
            conn: 1,
            machine: machine.to_string(),
            machine_id: machine_id.to_string(),
            aliases: vec![machine.to_string()],
            last_seen: Mutex::new(Instant::now() - age),
            tx,
            streams: Mutex::new(HashMap::new()),
            next: AtomicU32::new(1),
            account: None,
            device_id: None,
            owner_slug: None,
            nacho_app: AtomicBool::new(false),
            kick: tokio::sync::Notify::new(),
        })
    }

    #[test]
    fn slug_belongs_to_first_key() {
        let g = Gate::new(None);
        assert!(g.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        assert!(g.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        assert!(!g.claim("abcdefghijklmnopqrstuvwxy", "h2"));
        assert!(g.claim("zyxwvutsrqponmlkjihgfedcb", "h2"));
    }

    #[test]
    fn hello_needs_key_and_valid_slugs() {
        let v = serde_json::json!({
            "t":"hello", "key":"0123456789abcdef0123",
            "slugs":["abcdefghijklmnopqrstuvwxy","BAD","short"],
            "machine":"맥북", "machine_id":"machine-macbook-1"
        });
        let h = parse_hello(&v).unwrap();
        assert_eq!(h.slugs, vec!["abcdefghijklmnopqrstuvwxy".to_string()]);
        assert_eq!(h.machine, "맥북");
        assert_eq!(h.machine_id.as_deref(), Some("machine-macbook-1"));
        assert!(h.aliases.contains(&"맥북".to_string()));
        assert!(h.device_token.is_none());
        assert!(!h.nacho_app);
        assert!(parse_hello(&serde_json::json!({"t":"hello","key":"short","slugs":[]})).is_none());
        assert!(parse_hello(&serde_json::json!({"t":"nope"})).is_none());
    }

    #[test]
    fn nacho_capability_requires_a_boolean_and_an_exact_route() {
        let mut hello = serde_json::json!({"t":"hello","key":"0123456789abcdef0123","slugs":[]});
        for value in [serde_json::json!(null), serde_json::json!("true"), serde_json::json!(1), serde_json::json!(false)] {
            hello["capabilities"] = serde_json::json!({"nacho_app":value});
            assert!(!parse_hello(&hello).unwrap().nacho_app);
        }
        hello["capabilities"]["nacho_app"] = true.into();
        assert!(parse_hello(&hello).unwrap().nacho_app);
        for path in ["nacho/app", "nacho/app/events", "m/~mini/nacho/app/messages"] {
            assert!(nacho_app_path(path));
        }
        for path in ["nacho/application", "nacho/read/events", "term/me", "m/~mini/term/me"] {
            assert!(!nacho_app_path(path));
        }
    }

    #[test]
    fn live_machine_route_bypasses_direct_for_http_and_ws_paths() {
        let candidates = [
            Candidate { index: 0, machine: "맥북", machine_id: "book-1", aliases: &[], fresh: true },
            Candidate { index: 1, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
        ];
        assert_eq!(pick_route(&candidates, Some("맥북")), RoutePick::Machine(0));
        assert_eq!(pick_route(&candidates, Some("미니")), RoutePick::Machine(1));
        assert_eq!(machine_route("m/미니/term/panes"), Some(("미니", "term/panes")));
        assert_eq!(machine_route("m/미니/term/ws"), Some(("미니", "term/ws")));
        assert_eq!(live_machines(&candidates).len(), 2);
    }

    #[test]
    fn missing_target_preserves_the_existing_direct_fallback() {
        let candidates = [Candidate {
            index: 0,
            machine: "맥북",
            machine_id: "book-1",
            aliases: &[],
            fresh: true,
        }];
        assert_eq!(pick_route(&candidates, Some("미니")), RoutePick::Fallback(0));
        assert_eq!(pick_route(&candidates, None), RoutePick::Fallback(0));
        assert_eq!(pick_route(&candidates, Some("~unknown-id")), RoutePick::Missing);
    }

    #[test]
    fn display_label_can_route_by_exact_alias_or_stable_id() {
        let aliases = vec!["맥미니".to_string(), "nachoneko".to_string()];
        let candidates = [Candidate {
            index: 0,
            machine: "nachoneko",
            machine_id: "stable-mini-1",
            aliases: &aliases,
            fresh: true,
        }];
        assert_eq!(pick_route(&candidates, Some("맥미니")), RoutePick::Machine(0));
        assert_eq!(
            pick_route(&candidates, Some("~stable-mini-1")),
            RoutePick::Machine(0)
        );
    }

    #[test]
    fn stale_and_ambiguous_routes_fail_closed() {
        let stale = [Candidate {
            index: 0,
            machine: "미니",
            machine_id: "mini-1",
            aliases: &[],
            fresh: false,
        }];
        assert_eq!(pick_route(&stale, Some("미니")), RoutePick::Missing);
        assert!(live_machines(&stale).is_empty());

        let ambiguous = [
            Candidate { index: 0, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
            Candidate { index: 1, machine: "미니", machine_id: "other-mini", aliases: &[], fresh: true },
        ];
        assert_eq!(pick_route(&ambiguous, Some("미니")), RoutePick::Ambiguous);
        assert_eq!(live_machines(&ambiguous).len(), 2);

        let restarted = [
            Candidate { index: 0, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
            Candidate { index: 1, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
        ];
        assert_eq!(pick_route(&restarted, Some("미니")), RoutePick::Machine(1));
        assert_eq!(live_machines(&restarted).len(), 1);
    }

    #[test]
    fn actual_uplink_freshness_expires_without_messages_or_pong() {
        let live = mock_uplink("맥북", "book-1", Duration::from_secs(1));
        let stale = mock_uplink("미니", "mini-1", UPLINK_STALE_AFTER);
        let uplinks = vec![live, stale];
        let candidates = candidates_of(&uplinks);
        assert_eq!(pick_route(&candidates, Some("맥북")), RoutePick::Machine(0));
        assert_eq!(pick_route(&candidates, Some("미니")), RoutePick::Fallback(0));
        assert_eq!(live_machines(&candidates).len(), 1);
    }

    #[test]
    fn uplinks_are_capped_per_key() {
        let g = Gate::new(None);
        let up = |conn: u64| {
            let mut u = Arc::try_unwrap(mock_uplink("맥북", "book-1", Duration::ZERO)).ok().unwrap();
            u.conn = conn;
            Arc::new(u)
        };
        for conn in 0..MAX_UPLINKS_PER_KEY as u64 {
            assert!(g.admit("k1", up(conn)).is_ok());
        }
        assert!(g.admit("k1", up(100)).is_err());
        assert!(g.admit("k2", up(101)).is_ok());
    }

    #[test]
    fn raw_rest_keeps_percent_encoding() {
        let slug = "abcdefghijklmnopqrstuvwxy";
        assert_eq!(raw_rest(&format!("/u/{slug}/%2e%2e/x"), slug), "%2e%2e/x");
        assert_eq!(raw_rest(&format!("/u/{slug}/"), slug), "");
        assert_eq!(raw_rest(&format!("/u/{slug}/m/%EB%AF%B8/term"), slug), "m/%EB%AF%B8/term");
        assert!(!safe_path(raw_rest(&format!("/u/{slug}/%2e%2e/x"), slug)));
    }

    #[test]
    fn state_reads_v1_writes_v2_0600_and_forgets_stale_slugs() {
        let dir = std::env::temp_dir().join(format!("kasa-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("relay-state.json");
        std::fs::write(&p, r#"{"abcdefghijklmnopqrstuvwxy":"h1"}"#).unwrap();
        let g = Gate::new(Some(p.clone()));
        assert!(!g.claim("abcdefghijklmnopqrstuvwxy", "h2"), "v1 묶음을 잃었다");
        g.persist();
        let v2: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v2["version"], 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let old = now_secs() - SLUG_RETENTION_SECS - 1;
        let stale = serde_json::json!({ "version": 2, "slugs": {
            "zyxwvutsrqponmlkjihgfedcb": { "key_hash": "h9", "last_seen": old }
        }});
        std::fs::write(&p, stale.to_string()).unwrap();
        assert!(Gate::new(Some(p.clone())).claim("zyxwvutsrqponmlkjihgfedcb", "h1"), "오래된 묶음이 남았다");
        std::fs::write(&p, "{broken").unwrap();
        assert!(Gate::new(Some(p.clone())).keys.lock().unwrap().is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("corrupt")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 관문 ↔ 가짜 업링크 종단 시험 ─────────────────────────────────────────────

    use tokio_tungstenite::tungstenite::Message as TM;

    const SLUG: &str = "abcdefghijklmnopqrstuvwxy";

    struct FakeUplink {
        opens: Arc<Mutex<Vec<(u32, serde_json::Value)>>>,
        closes: Arc<Mutex<Vec<(u32, Vec<u8>)>>>,
    }

    async fn spawn_relay(gate: Gate) -> std::net::SocketAddr {
        let app = crate::relay::router().merge(router(gate));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap()
        });
        addr
    }

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    /// 업링크 하나를 붙이고 관문의 첫 답(JSON)을 돌려준다.
    async fn hello_uplink(addr: std::net::SocketAddr, hello: serde_json::Value) -> (Ws, serde_json::Value) {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/relay/uplink")).await.unwrap();
        ws.send(TM::Text(hello.to_string().into())).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        let v = serde_json::from_str(first.to_text().unwrap()).unwrap();
        (ws, v)
    }

    /// 관문을 띄우고 가짜 앱 하나를 업링크로 붙인다. `answer` 는 OPEN 마다 불려 답 프레임을 쓴다.
    async fn relay_with_uplink(
        answer: impl Fn(u32, &serde_json::Value, mpsc::UnboundedSender<Vec<u8>>) + Send + Sync + 'static,
    ) -> (std::net::SocketAddr, FakeUplink) {
        let addr = spawn_relay(Gate::new(None)).await;
        let hello = serde_json::json!({
            "t": "hello", "key": "0123456789abcdef0123", "slugs": [SLUG],
            "machine": "맥북", "machine_id": "machine-test-1",
        });
        let fake = fake_uplink_at(addr, hello, answer).await;
        (addr, fake)
    }

    async fn fake_uplink_at(
        addr: std::net::SocketAddr, hello: serde_json::Value,
        answer: impl Fn(u32, &serde_json::Value, mpsc::UnboundedSender<Vec<u8>>) + Send + Sync + 'static,
    ) -> FakeUplink {
        let (ws, ok) = hello_uplink(addr, hello).await;
        assert_eq!(ok["t"], "ok", "{ok}");
        let (mut tx, mut rx) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if tx.send(TM::Binary(frame.into())).await.is_err() {
                    break;
                }
            }
        });
        let fake = FakeUplink { opens: Default::default(), closes: Default::default() };
        let (opens, closes) = (fake.opens.clone(), fake.closes.clone());
        tokio::spawn(async move {
            while let Some(Ok(m)) = rx.next().await {
                let TM::Binary(b) = m else { continue };
                let Some((kind, id, payload)) = decode(&b) else { continue };
                match kind {
                    OPEN => {
                        let v: serde_json::Value = serde_json::from_slice(payload).unwrap();
                        opens.lock().unwrap().push((id, v.clone()));
                        answer(id, &v, out.clone());
                    }
                    CLOSE => closes.lock().unwrap().push((id, payload.to_vec())),
                    _ => {}
                }
            }
        });
        fake
    }

    fn reply(out: &mpsc::UnboundedSender<Vec<u8>>, id: u32, head: serde_json::Value, body: &[u8]) {
        let _ = out.send(encode(HEAD, id, head.to_string().as_bytes()));
        if !body.is_empty() {
            let _ = out.send(encode(BODY, id, body));
        }
        let _ = out.send(encode(END, id, b""));
    }

    #[tokio::test]
    async fn dot_segments_are_refused_before_reaching_the_machine() {
        let (addr, fake) = relay_with_uplink(|id, _, out| reply(&out, id, serde_json::json!({"status":200,"headers":[]}), b"x")).await;
        let status = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            write!(s, "GET /u/{SLUG}/%2e%2e/%2e%2e/version HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").unwrap();
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf.lines().next().unwrap_or("").to_string()
        })
        .await
        .unwrap();
        assert!(status.contains(" 400 "), "{status}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(fake.opens.lock().unwrap().is_empty(), "막혔어야 할 요청이 기계에 닿았다");
    }

    #[tokio::test]
    async fn credentials_do_not_cross_the_gateway_either_way() {
        let (addr, fake) = relay_with_uplink(|id, _, out| {
            let head = serde_json::json!({"status":200,"headers":[["set-cookie","kasa_token=secret"],["x-ok","1"]]});
            reply(&out, id, head, b"hi");
        })
        .await;
        let res = reqwest::Client::new()
            .get(format!("http://{addr}/u/{SLUG}/hub"))
            .header("authorization", "Bearer phone")
            .header("x-kasa-token", "stolen")
            .header("cookie", "kasa_token=stolen")
            .send()
            .await
            .unwrap();
        assert!(res.headers().get("set-cookie").is_none(), "앱의 토큰 쿠키가 폰에 샜다");
        assert_eq!(res.headers().get("x-ok").unwrap(), "1");
        assert_eq!(res.text().await.unwrap(), "hi");
        let opens = fake.opens.lock().unwrap();
        let sent: Vec<String> = opens[0].1["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| kv[0].as_str().unwrap().to_string())
            .collect();
        for h in ["authorization", "x-kasa-token", "cookie"] {
            assert!(!sent.iter().any(|s| s == h), "{h} 가 기계로 넘어갔다: {sent:?}");
        }
    }

    #[tokio::test]
    async fn a_phone_that_stops_reading_does_not_stall_other_requests() {
        let (addr, fake) = relay_with_uplink(|id, open, out| {
            let head = serde_json::json!({"status":200,"headers":[]});
            if open["path"] == "/slow" {
                let _ = out.send(encode(HEAD, id, head.to_string().as_bytes()));
                let chunk = vec![7u8; crate::uplink::CHUNK];
                for _ in 0..(STREAM_QUEUE * 3) {
                    let _ = out.send(encode(BODY, id, &chunk));
                }
            } else {
                reply(&out, id, head, b"fast");
            }
        })
        .await;
        let client = reqwest::Client::new();
        // 머리만 받고 몸통은 안 읽는다 — 폰이 멈춘 것.
        let _slow = client.get(format!("http://{addr}/u/{SLUG}/slow")).send().await.unwrap();
        let fast = tokio::time::timeout(Duration::from_secs(3), async {
            client.get(format!("http://{addr}/u/{SLUG}/fast")).send().await.unwrap().text().await.unwrap()
        })
        .await
        .expect("멈춘 폰 하나가 같은 기계의 다른 요청을 막았다");
        assert_eq!(fast, "fast");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fake.closes.lock().unwrap().iter().any(|(_, why)| why == b"overflow") {
            assert!(Instant::now() < deadline, "밀린 스트림을 끊지 않았다");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // ── 계정 로그인 ─────────────────────────────────────────────────────────────

    fn account_gate(dir: &std::path::Path) -> Gate {
        let accounts = dir.join("relay-accounts.json");
        let mut f = crate::relay_auth::AccountsFile::default();
        f.accounts.insert(
            "geno".into(),
            crate::relay_auth::Account {
                pbkdf2_sha256: crate::relay_auth::hash_password_with("correct horse", 1000),
                created: 1,
                disabled: false,
            },
        );
        crate::relay_auth::save_accounts(&accounts, &f).unwrap();
        Gate::with_accounts(Some(dir.join("relay-state.json")), Some(accounts))
    }

    async fn post(addr: std::net::SocketAddr, path: &str, bearer: Option<&str>, body: serde_json::Value) -> (u16, serde_json::Value) {
        let mut rb = reqwest::Client::new().post(format!("http://{addr}{path}")).json(&body);
        if let Some(t) = bearer {
            rb = rb.bearer_auth(t);
        }
        let r = rb.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    }

    async fn get_json(addr: std::net::SocketAddr, path: &str, bearer: &str) -> (u16, serde_json::Value) {
        let r = reqwest::Client::new().get(format!("http://{addr}{path}")).bearer_auth(bearer).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    }

    fn desktop_hello(machine_id: &str, token: &str) -> serde_json::Value {
        serde_json::json!({
            "t": "hello", "key": "0123456789abcdef0123", "slugs": [], "machine": "맥북",
            "machine_id": machine_id, "device_token": token, "proto": 2,
        })
    }

    #[tokio::test]
    async fn account_sync_isolated_cas_validated_and_revoked_for_desktop_and_phone() {
        let dir = std::env::temp_dir().join(format!("kasa-sync-http-{}", uuid::Uuid::new_v4()));
        drop(account_gate(&dir));
        let accounts_path = dir.join("relay-accounts.json");
        let mut accounts = crate::relay_auth::load_accounts(&accounts_path);
        let other = accounts.accounts["geno"].clone();
        accounts.accounts.insert("other".into(), other);
        crate::relay_auth::save_accounts(&accounts_path, &accounts).unwrap();
        let addr = spawn_relay(Gate::new(Some(dir.join("relay-state.json")))).await;
        let (_, desktop) = post(addr, "/relay/login", None, serde_json::json!({
            "account":"geno","password":"correct horse","machine_id":"sync-desktop","kind":"desktop"
        })).await;
        let token = desktop["token"].as_str().unwrap();
        let (_, phone) = post(addr, "/relay/login", None, serde_json::json!({
            "account":"geno","password":"correct horse","kind":"phone"
        })).await;
        let phone_token = phone["token"].as_str().unwrap();
        let (_, other) = post(addr, "/relay/login", None, serde_json::json!({
            "account":"other","password":"correct horse","kind":"phone"
        })).await;
        assert_eq!(get_json(addr, "/relay/account-sync", "invalid").await.0, 401);
        let patch = |token: String, value: serde_json::Value| async move {
            let reply = reqwest::Client::new().patch(format!("http://{addr}/relay/account-sync"))
                .bearer_auth(token).json(&value).send().await.unwrap();
            (reply.status().as_u16(), reply.json::<serde_json::Value>().await.unwrap())
        };
        let first = serde_json::json!({"expected_revision":0,"settings":{"theme":"graphite"},
            "machines":{"mini":{"label":"Mini","ssh":"user@example.com"}}});
        assert_eq!(patch(token.into(), first.clone()).await.0, 200);
        let shared = get_json(addr, "/relay/account-sync", phone_token).await;
        assert_eq!(shared.0, 200);
        assert_eq!(shared.1["settings"]["theme"], "graphite");
        assert_eq!(get_json(addr, "/relay/account-sync", other["token"].as_str().unwrap()).await.1["revision"], 0);
        assert_eq!(patch(phone_token.into(), first).await.0, 409);
        assert_eq!(patch(phone_token.into(), serde_json::json!({"expected_revision":1,
            "settings":{"password":"do-not-store"},"machines":{}})).await.0, 400);
        assert_eq!(patch(phone_token.into(), serde_json::json!({"expected_revision":1,
            "settings":{},"machines":{},"account":"other"})).await.0, 400);
        assert_eq!(patch(phone_token.into(), serde_json::json!({"expected_revision":1,
            "settings":{"font_size":14},"machines":{}})).await.0, 200);
        assert_eq!(get_json(addr, "/relay/account-sync", token).await.1["settings"],
            serde_json::json!({"theme":"graphite","font_size":14}));
        assert_eq!(post(addr, "/relay/logout", Some(phone_token), serde_json::json!({})).await.0, 200);
        assert_eq!(get_json(addr, "/relay/account-sync", phone_token).await.0, 401);
        assert_eq!(patch(phone_token.into(), serde_json::json!({"expected_revision":2,"settings":{},"machines":{}})).await.0, 401);
        assert_eq!(get_json(addr, "/relay/account-sync", token).await.1["revision"], 2);
    }

    #[tokio::test]
    async fn account_phone_proxy_authenticates_http_ws_and_closes_on_revocation() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let dir = std::env::temp_dir().join(format!("kasa-account-proxy-{}", uuid::Uuid::new_v4()));
        let gate = account_gate(&dir);
        let addr = spawn_relay(gate).await;
        let (_, desktop) = post(addr, "/relay/login", None, serde_json::json!({
            "account":"geno","password":"correct horse","machine_id":"account-mac-a"
        })).await;
        let (_, phone) = post(addr, "/relay/login", None, serde_json::json!({
            "account":"geno","password":"correct horse","kind":"phone"
        })).await;
        let token = phone["token"].as_str().unwrap();
        let mut hello = desktop_hello("account-mac-a", desktop["token"].as_str().unwrap());
        hello["slugs"] = serde_json::json!([SLUG]);
        hello["owner_slug"] = SLUG.into();
        let fake = fake_uplink_at(addr, hello, |id, open, out| {
            if open["ws"] == true {
                let _ = out.send(encode(HEAD, id, br#"{"status":101}"#));
                let _ = out.send(encode(WS_TEXT, id, b"ready"));
            } else {
                reply(&out, id, serde_json::json!({"status":200,"headers":[["content-type","application/json"]]}),
                    br#"{"account_view":true}"#);
            }
        }).await;
        assert_eq!(get_json(addr, "/relay/account/term/me", "invalid").await.0, 401);
        assert_eq!(reqwest::get(format!("http://{addr}/relay/account/term/me?token=not-a-credential"))
            .await.unwrap().status().as_u16(), 401);
        assert_eq!(get_json(addr, "/relay/account/term/me", token).await.1["account_view"], true);
        let count = fake.opens.lock().unwrap().len();
        assert_eq!(get_json(addr, "/relay/account/m/~another-account-machine/term/me", token).await.0, 503);
        assert_eq!(fake.opens.lock().unwrap().len(), count);
        {
            let opens = fake.opens.lock().unwrap();
            assert_eq!(opens[0].1["slug"], SLUG);
            assert_eq!(opens[0].1["path"], "/term/me");
            assert!(opens[0].1["headers"].as_array().unwrap().iter().all(|h| h[0] != "authorization"));
        }
        let mut request = format!("ws://{addr}/relay/account/term/ws").into_client_request().unwrap();
        request.headers_mut().insert(header::SEC_WEBSOCKET_PROTOCOL,
            format!("kasa-relay-account, kasa-auth.{token}").parse().unwrap());
        let (mut ws, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(response.headers()[header::SEC_WEBSOCKET_PROTOCOL], "kasa-relay-account");
        assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap(), "ready");
        assert_eq!(post(addr, "/relay/logout", Some(token), serde_json::json!({})).await.0, 200);
        let closed = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.expect("revoked socket remained open");
        assert!(matches!(closed, Some(Ok(TM::Close(_))) | None));
        assert_eq!(get_json(addr, "/relay/account/term/me", token).await.0, 401);
    }

    #[tokio::test]
    async fn account_nacho_selects_only_its_authenticated_capable_hub() {
        let dir = std::env::temp_dir().join(format!("kasa-nacho-hub-{}", uuid::Uuid::new_v4()));
        drop(account_gate(&dir));
        let accounts_path = dir.join("relay-accounts.json");
        let mut accounts = crate::relay_auth::load_accounts(&accounts_path);
        accounts.accounts.insert("other".into(), accounts.accounts["geno"].clone());
        crate::relay_auth::save_accounts(&accounts_path, &accounts).unwrap();
        let gate = Gate::new(Some(dir.join("relay-state.json")));
        let addr = spawn_relay(gate.clone()).await;
        let mut credentials = Vec::new();
        for (account, machine) in [("geno", "nacho-macbook"), ("geno", "nacho-mini"), ("other", "other-hub") ] {
            let (status, device) = post(addr, "/relay/login", None, serde_json::json!({
                "account":account,"password":"correct horse","machine_id":machine
            })).await;
            assert_eq!(status, 200);
            credentials.push(device);
        }
        let token = credentials[0]["token"].as_str().unwrap();
        let mut hello = desktop_hello("nacho-macbook", token);
        hello["slugs"] = serde_json::json!([SLUG]);
        hello["owner_slug"] = SLUG.into();
        let book = fake_uplink_at(addr, hello, |id, _, out|
            reply(&out, id, serde_json::json!({"status":200}), br#"{"hub":"book"}"#)).await;
        let mut other_hello = desktop_hello("other-hub", credentials[2]["token"].as_str().unwrap());
        other_hello["key"] = "different-machine-key-123".into();
        other_hello["slugs"] = serde_json::json!(["zyxwvutsrqponmlkjihgfedcb"]);
        other_hello["owner_slug"] = "zyxwvutsrqponmlkjihgfedcb".into();
        other_hello["capabilities"] = serde_json::json!({"nacho_app":true});
        let other = fake_uplink_at(addr, other_hello, |id, _, out|
            reply(&out, id, serde_json::json!({"status":200}), br#"{"hub":"other"}"#)).await;
        let legacy = fake_uplink_at(addr, serde_json::json!({
            "t":"hello","key":"legacy-machine-key-123","machine_id":"legacy-hub",
            "slugs":["qwertyuiopasdfghjklzxcvbnm"],"owner_slug":"qwertyuiopasdfghjklzxcvbnm",
            "capabilities":{"nacho_app":true},"account":"geno"
        }), |id, _, out| reply(&out, id, serde_json::json!({"status":200}), b"legacy")).await;
        assert!(!gate.live.lock().unwrap().values().find(|(_, up)| up.machine_id == "legacy-hub")
            .unwrap().1.nacho_app.load(Ordering::Relaxed));

        let path = "/relay/account/nacho/app/events";
        assert_eq!(get_json(addr, path, "invalid").await.0, 401);
        let missing = reqwest::Client::new().get(format!("http://{addr}{path}?nacho_app=true"))
            .bearer_auth(token).header("x-kasa-nacho-app", "true").send().await.unwrap();
        assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(missing.json::<serde_json::Value>().await.unwrap()["error"], "nacho_hub_unavailable");
        assert!(book.opens.lock().unwrap().is_empty());
        assert!(other.opens.lock().unwrap().is_empty());
        assert!(legacy.opens.lock().unwrap().is_empty());
        assert_eq!(get_json(addr, "/relay/account/term/me", token).await.1["hub"], "book");
        assert_eq!(get_json(addr, &format!("/u/{SLUG}/nacho/app/events"), token).await.1["hub"], "book");
        let book_opens = book.opens.lock().unwrap().len();

        let mut mini_hello = desktop_hello("nacho-mini", credentials[1]["token"].as_str().unwrap());
        mini_hello["slugs"] = serde_json::json!([SLUG]);
        mini_hello["owner_slug"] = SLUG.into();
        mini_hello["capabilities"] = serde_json::json!({"nacho_app":true});
        let mini = fake_uplink_at(addr, mini_hello, |id, _, out|
            reply(&out, id, serde_json::json!({"status":200}), br#"{"hub":"mini"}"#)).await;
        assert_eq!(get_json(addr, path, token).await.1["hub"], "mini");
        assert_eq!(get_json(addr, "/relay/account/m/~nacho-macbook/nacho/app/events", token).await.0, 503);
        assert_eq!(get_json(addr, "/relay/account/m/~other-hub/nacho/app/events", token).await.0, 503);
        let opens = mini.opens.lock().unwrap();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].1["slug"], SLUG);
        assert_eq!(opens[0].1["path"], "/nacho/app/events");
        assert!(opens[0].1["headers"].as_array().unwrap().iter().all(|h| h[0] != "authorization"));
        drop(opens);
        assert_eq!(book.opens.lock().unwrap().len(), book_opens);
        assert!(other.opens.lock().unwrap().is_empty());
        assert!(legacy.opens.lock().unwrap().is_empty());

        gate.devices.lock().unwrap().get_mut(credentials[1]["device_id"].as_str().unwrap())
            .unwrap().revoked_at = Some(now_secs());
        assert_eq!(get_json(addr, path, token).await.0, 503);
        assert_eq!(mini.opens.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn nacho_capability_refresh_cannot_authenticate_a_legacy_socket() {
        let dir = std::env::temp_dir().join(format!("kasa-nacho-refresh-{}", uuid::Uuid::new_v4()));
        let gate = account_gate(&dir);
        let addr = spawn_relay(gate.clone()).await;
        let (_, device) = post(addr, "/relay/login", None, serde_json::json!({
            "account":"geno","password":"correct horse","machine_id":"refresh-hub"
        })).await;
        let mut hello = desktop_hello("refresh-hub", device["token"].as_str().unwrap());
        hello["slugs"] = serde_json::json!([SLUG]);
        hello["owner_slug"] = SLUG.into();
        hello["capabilities"] = serde_json::json!({"nacho_app":true});
        let mut anonymous = hello.clone();
        anonymous.as_object_mut().unwrap().remove("device_token");
        let (mut ws, ok) = hello_uplink(addr, anonymous).await;
        assert_eq!(ok["t"], "ok");
        ws.send(TM::Text(hello.to_string().into())).await.unwrap();
        let refreshed = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap().unwrap().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(refreshed.to_text().unwrap()).unwrap()["t"], "ok");
        assert!(gate.live.lock().unwrap().values().all(|(_, up)| up.account.is_none()
            && !up.nacho_app.load(Ordering::Relaxed)));
    }

    #[tokio::test]
    async fn device_login_binds_one_machine_and_revocation_drops_its_uplink() {
        let dir = std::env::temp_dir().join(format!("kasa-login-{}", uuid::Uuid::new_v4()));
        let addr = spawn_relay(account_gate(&dir)).await;
        let book = serde_json::json!({"account":"geno","password":"correct horse","machine_id":"machine-book-1","label":"맥북"});
        let wrong = serde_json::json!({"account":"geno","password":"nope","machine_id":"machine-book-1"});
        assert_eq!(post(addr, "/relay/login", None, wrong).await.0, 401);
        assert_eq!(post(addr, "/relay/login", None, serde_json::json!({"account":"geno","password":"correct horse"})).await.0, 400, "기계 없는 데스크톱 로그인이 통했다");
        let (status, v) = post(addr, "/relay/login", None, book.clone()).await;
        assert_eq!(status, 200, "{v}");
        let token = v["token"].as_str().unwrap().to_string();
        let desktop_id = v["device_id"].as_str().unwrap().to_string();
        assert_eq!(get_json(addr, "/relay/whoami", &token).await.1["account"], "geno");

        let (_, other) = hello_uplink(addr, desktop_hello("machine-mini-1", &token)).await;
        assert_eq!(other["error"], "device_token_invalid", "다른 기계가 이 기기 토큰으로 붙었다");
        let (mut ws, ok) = hello_uplink(addr, desktop_hello("machine-book-1", &token)).await;
        assert_eq!((ok["t"].as_str(), ok["account"].as_str()), (Some("ok"), Some("geno")), "{ok}");
        let (_, list) = get_json(addr, "/relay/devices", &token).await;
        let me = list["devices"].as_array().unwrap().iter().find(|d| d["device_id"] == desktop_id.as_str()).cloned().unwrap();
        assert_eq!((me["online"].as_bool(), me["current"].as_bool()), (Some(true), Some(true)), "{list}");

        let (_, phone) = post(addr, "/relay/login", None, serde_json::json!({"account":"geno","password":"correct horse","kind":"phone","label":"폰"})).await;
        let phone_token = phone["token"].as_str().unwrap();
        let (status, _) = post(addr, &format!("/relay/devices/{desktop_id}/revoke"), Some(phone_token), serde_json::json!({})).await;
        assert_eq!(status, 200);
        let kicked = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(Ok(m)) = ws.next().await {
                if m.to_text().is_ok_and(|t| t.contains("device_revoked")) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(kicked, "폐기한 기기의 업링크가 안 끊겼다");
        assert_eq!(get_json(addr, "/relay/whoami", &token).await.0, 401);
        let (_, again) = hello_uplink(addr, desktop_hello("machine-book-1", &token)).await;
        assert_eq!(again["error"], "device_token_invalid", "폐기된 토큰으로 다시 붙었다");

        let state = std::fs::read_to_string(dir.join("relay-state.json")).unwrap();
        assert!(!state.contains(&token), "토큰 원문이 상태 파일에 남았다");
        assert!(state.contains(&crate::relay_auth::token_hash(&token)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn logging_in_again_on_a_machine_retires_its_old_token_and_disabling_stops_all() {
        let dir = std::env::temp_dir().join(format!("kasa-login-{}", uuid::Uuid::new_v4()));
        let addr = spawn_relay(account_gate(&dir)).await;
        let book = serde_json::json!({"account":"geno","password":"correct horse","machine_id":"machine-book-1"});
        let first = post(addr, "/relay/login", None, book.clone()).await.1["token"].as_str().unwrap().to_string();
        let second = post(addr, "/relay/login", None, book).await.1["token"].as_str().unwrap().to_string();
        assert_eq!(get_json(addr, "/relay/whoami", &first).await.0, 401, "같은 기계의 옛 토큰이 살아 있다");
        assert_eq!(get_json(addr, "/relay/whoami", &second).await.0, 200);

        let p = dir.join("relay-accounts.json");
        let mut f = crate::relay_auth::load_accounts(&p);
        f.accounts.get_mut("geno").unwrap().disabled = true;
        crate::relay_auth::save_accounts(&p, &f).unwrap();
        let later = std::time::SystemTime::now() + Duration::from_secs(2);
        let _ = std::fs::File::options().write(true).open(&p).and_then(|file| file.set_modified(later));
        assert_eq!(get_json(addr, "/relay/whoami", &second).await.0, 401, "막은 계정의 토큰이 통했다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_account_lists_are_shared_per_relay_account_and_follow_revocation() {
        let dir = std::env::temp_dir().join(format!("kasa-agents-{}", uuid::Uuid::new_v4()));
        let addr = spawn_relay(account_gate(&dir)).await;
        let login = |machine: &str, label: &str| {
            serde_json::json!({"account":"geno","password":"correct horse","machine_id":machine,"label":label})
        };
        let mini = post(addr, "/relay/login", None, login("machine-mini-1", "미니")).await.1;
        let book = post(addr, "/relay/login", None, login("machine-book-1", "맥북")).await.1;
        let (mini_token, mini_id) = (mini["token"].as_str().unwrap(), mini["device_id"].as_str().unwrap());
        let book_token = book["token"].as_str().unwrap();

        let slots = serde_json::json!({ "accounts": [
            {"provider":"claude","slot":"acct-1","email":"g@gmail.com","org":"g@gmail.com's Organization","label":"지메일"},
            {"provider":"codex","slot":"codex-1","email":"r@s.ai","workspace":"ws-team","plan":"team","label":"사이오닉팀"},
        ]});
        assert_eq!(post(addr, "/relay/agent-accounts", None, slots.clone()).await.0, 401, "토큰 없이 목록을 올렸다");
        assert_eq!(get_json(addr, "/relay/agent-accounts", "kdt_nope").await.0, 401);
        let (status, v) = post(addr, "/relay/agent-accounts", Some(mini_token), slots).await;
        assert_eq!(status, 200, "{v}");

        let (_, seen) = post(addr, "/relay/agent-accounts", Some(book_token), serde_json::json!({"accounts": []})).await;
        let list = seen["accounts"].as_array().unwrap();
        assert_eq!(list.len(), 2, "{seen}");
        let gmail = list.iter().find(|a| a["key"] == "claude:g@gmail.com").unwrap();
        assert_eq!(gmail["label"], "지메일");
        assert_eq!(gmail["devices"][0]["label"], "미니");
        assert_eq!(gmail["devices"][0]["current"], false);
        assert!(!seen.to_string().contains("kdt_"), "기기 토큰이 목록에 섞였다");

        let bad = serde_json::json!({"accounts": [{"provider":"gemini","email":"x@y.z"}]});
        assert_eq!(post(addr, "/relay/agent-accounts", Some(book_token), bad).await.0, 400);

        let state = std::fs::read_to_string(dir.join("relay-state.json")).unwrap();
        assert!(state.contains("claude:g@gmail.com"), "목록이 상태 파일에 안 남았다");
        drop(state);

        let (status, _) = post(addr, &format!("/relay/devices/{mini_id}/revoke"), Some(book_token), serde_json::json!({})).await;
        assert_eq!(status, 200);
        let (_, after) = get_json(addr, "/relay/agent-accounts", book_token).await;
        assert_eq!(after["accounts"].as_array().unwrap().len(), 0, "폐기한 기기의 계정이 남았다: {after}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_round_trip() {
        let dir = std::env::temp_dir().join(format!("kasa-gate-{}", uuid::Uuid::new_v4()));
        let p = dir.join("relay-state.json");
        let g = Gate::new(Some(p.clone()));
        assert!(g.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        g.persist();
        let g2 = Gate::new(Some(p));
        assert!(!g2.claim("abcdefghijklmnopqrstuvwxy", "h2"));
        assert!(g2.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn operational_state_preserves_agent_catalogs_devices_and_slugs_with_separate_settings_sync() {
        let dir = std::env::temp_dir().join(format!("kasa-state-merge-{}", uuid::Uuid::new_v4()));
        let path = dir.join("relay-state.json");
        let fixture = serde_json::json!({
            "version": 2,
            "slugs": {SLUG: {"key_hash":"synthetic-uplink-hash", "last_seen":now_secs()}},
            "devices": {
                "dev_fixture": {"token_hash":"synthetic-token-hash", "account":"fixture", "kind":"desktop",
                    "machine_id":"fixture-machine", "label":"Fixture desktop", "created":1, "last_seen":2, "revoked_at":null},
                "dev_retired": {"token_hash":"retired-token-hash", "account":"fixture", "kind":"phone",
                    "machine_id":null, "label":"Retired phone", "created":1, "last_seen":2, "revoked_at":3}
            },
            "agent_accounts": {"fixture": {"codex:person@example.test/fixture-workspace": {
                "provider":"codex", "email":"person@example.test", "org":"", "workspace":"fixture-workspace",
                "plan":"team", "label":"Fixture login", "devices":{"dev_fixture":{"slot":"codex-fixture", "seen":4}}
            }}}
        });
        crate::relay_auth::write_private(&path, &fixture.to_string()).unwrap();
        let gate = Gate::new(Some(path.clone()));
        let patch = serde_json::from_value(serde_json::json!({
            "expected_revision":0, "settings":{"theme":"graphite", "mobile_theme_mode":"dark"},
            "machines":{"fixture":{"label":"Fixture desktop", "ssh":"person@example.test"}}
        })).unwrap();
        let snapshot = gate.account_sync.patch("fixture", &patch).unwrap();
        gate.persist();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, fixture, "saving the merged relay discarded operational state");
        assert!(saved.get("settings").is_none(), "settings leaked into the operational state format");
        let reloaded = Gate::new(Some(path.clone()));
        assert_eq!(serde_json::to_value(reloaded.account_sync.get("fixture").unwrap()).unwrap(),
            serde_json::to_value(snapshot).unwrap());
        assert_eq!(reloaded.account_sync.get("another-account").unwrap().revision, 0);
        reloaded.persist();
        let twice: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(twice, fixture, "reloading and saving discarded an agent catalog or device");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_catalog_persistence_keeps_every_completed_mutation() {
        let dir = std::env::temp_dir().join(format!("kasa-state-writers-{}", uuid::Uuid::new_v4()));
        let path = dir.join("relay-state.json");
        let gate = Gate::new(Some(path.clone()));
        let barrier = Arc::new(std::sync::Barrier::new(12));
        let workers: Vec<_> = (0..12).map(|index| {
            let gate = gate.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let name = format!("fixture-{index}");
                gate.agents.lock().unwrap().insert(name.clone(), Default::default());
                gate.keys.lock().unwrap().insert(name, SlugRec { key_hash:"fixture".into(), last_seen:now_secs() });
                barrier.wait();
                gate.persist();
            })
        }).collect();
        for worker in workers { worker.join().unwrap(); }
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["agent_accounts"].as_object().unwrap().len(), 12);
        assert_eq!(saved["slugs"].as_object().unwrap().len(), 12);
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn catalog_fixture_device(gate: &Gate) -> (String, axum::http::HeaderMap) {
        let token = crate::relay_auth::new_token();
        gate.devices.lock().unwrap().insert("dev_catalog_fixture".into(), DeviceRec {
            token_hash: crate::relay_auth::token_hash(&token), account: "geno".into(), kind: "desktop".into(),
            machine_id: Some("catalog-fixture-machine".into()), label: "Fixture".into(),
            created: 1, last_seen: 1, revoked_at: None,
        });
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        (token, headers)
    }

    fn assert_catalog_no_store(response: &axum::response::Response) {
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::VARY], "Authorization");
    }

    async fn catalog_upload_after_auth_change(disable_account: bool) {
        let dir = std::env::temp_dir().join(format!("kasa-catalog-race-{}", uuid::Uuid::new_v4()));
        let gate = account_gate(&dir);
        let (_, headers) = catalog_fixture_device(&gate);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let body = axum::body::Body::from_stream(futures_util::stream::once(async move {
            let _ = started_tx.send(());
            resume_rx.await.unwrap();
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                br#"{"accounts":[{"provider":"codex","email":"person@example.test","slot":"fixture"}]}"#))
        }));
        let mut request = axum::extract::Request::new(body);
        *request.headers_mut() = headers.clone();
        let task = tokio::spawn(agent_accounts_post(State(gate.clone()), request));
        tokio::time::timeout(Duration::from_secs(2), started_rx).await.unwrap().unwrap();
        if disable_account {
            let path = dir.join("relay-accounts.json");
            let mut accounts = crate::relay_auth::load_accounts(&path);
            accounts.accounts.get_mut("geno").unwrap().disabled = true;
            crate::relay_auth::save_accounts(&path, &accounts).unwrap();
            std::fs::File::options().write(true).open(&path).unwrap()
                .set_modified(std::time::SystemTime::now() + Duration::from_secs(2)).unwrap();
        } else {
            assert!(gate.revoke("dev_catalog_fixture"));
        }
        resume_tx.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_catalog_no_store(&response);
        assert!(gate.agents.lock().unwrap().is_empty(), "retired credentials mutated the catalog");
        let response = agent_accounts_get(State(gate), headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_catalog_no_store(&response);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_catalog_rejects_device_revoked_while_receiving_body() {
        catalog_upload_after_auth_change(false).await;
    }

    #[tokio::test]
    async fn agent_catalog_rejects_account_disabled_while_receiving_body() {
        catalog_upload_after_auth_change(true).await;
    }

    #[tokio::test]
    async fn agent_catalog_bounds_upload_size_and_duration_without_caching_errors() {
        let dir = std::env::temp_dir().join(format!("kasa-catalog-limits-{}", uuid::Uuid::new_v4()));
        let gate = account_gate(&dir);
        let (_, headers) = catalog_fixture_device(&gate);
        let bodies = [
            (axum::body::Body::from(vec![b'x'; 64 * 1024 + 1]), StatusCode::PAYLOAD_TOO_LARGE),
            (axum::body::Body::from("not-json"), StatusCode::BAD_REQUEST),
            (axum::body::Body::from_stream(futures_util::stream::pending::<Result<axum::body::Bytes, std::io::Error>>()),
                StatusCode::REQUEST_TIMEOUT),
        ];
        for (body, expected) in bodies {
            let mut request = axum::extract::Request::new(body);
            *request.headers_mut() = headers.clone();
            let response = tokio::time::timeout(Duration::from_secs(12), agent_accounts_post(State(gate.clone()), request))
                .await.expect("an idle catalog upload did not time out");
            assert_eq!(response.status(), expected);
            assert_catalog_no_store(&response);
        }
        assert!(gate.agents.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_catalog_http_isolates_accounts_and_disables_response_caching() {
        let dir = std::env::temp_dir().join(format!("kasa-catalog-scope-{}", uuid::Uuid::new_v4()));
        drop(account_gate(&dir));
        let accounts_path = dir.join("relay-accounts.json");
        let mut accounts = crate::relay_auth::load_accounts(&accounts_path);
        accounts.accounts.insert("other".into(), accounts.accounts["geno"].clone());
        crate::relay_auth::save_accounts(&accounts_path, &accounts).unwrap();
        let addr = spawn_relay(Gate::new(Some(dir.join("relay-state.json")))).await;
        let mut tokens = Vec::new();
        for account in ["geno", "other"] {
            let (status, login) = post(addr, "/relay/login", None, serde_json::json!({
                "account":account, "password":"correct horse", "kind":"phone"
            })).await;
            assert_eq!(status, 200);
            tokens.push(login["token"].as_str().unwrap().to_string());
        }
        let client = reqwest::Client::new();
        for (token, label) in tokens.iter().zip(["First catalog", "Second catalog"]) {
            let response = client.post(format!("http://{addr}/relay/agent-accounts")).bearer_auth(token)
                .json(&serde_json::json!({"accounts":[{"provider":"codex", "email":"shared@example.test", "label":label}]}))
                .send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::VARY], "Authorization");
            assert_eq!(response.json::<serde_json::Value>().await.unwrap()["accounts"][0]["label"], label);
        }
        for (token, label) in tokens.iter().zip(["First catalog", "Second catalog"]) {
            let response = client.get(format!("http://{addr}/relay/agent-accounts")).bearer_auth(token)
                .send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let value = response.json::<serde_json::Value>().await.unwrap();
            assert_eq!(value["accounts"].as_array().unwrap().len(), 1);
            assert_eq!(value["accounts"][0]["label"], label);
        }
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let response = client.request(method, format!("http://{addr}/relay/agent-accounts"))
                .json(&serde_json::json!({"accounts":[]})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
