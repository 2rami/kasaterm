//! 계정 사용량을 상류(`oauth/usage`)보다 빨리 맞추는 관측 저장소. 세 갈래로 들어온다.
//!
//! - **응답 헤더**: 프록시가 `/v1/messages` 응답의 `anthropic-ratelimit-unified-*` 를 본다. 서버는 경고선을 넘은
//!   창만 싣는다(2026-11 실측: `-7d-utilization 0.92`, `-7d-status allowed_warning`, 5시간 창은 없음).
//! - **Claude Code 의 측정**: 칸의 mod 가 `session.measure` 의 `rateLimits` 를 보낸다(`claude_mod`). 상태줄
//!   stdin 의 `rate_limits` 와 같은 값이고 경고선 아래 5시간 창도 실린다(2026-11 실측: `five_hour 8%`). 칸 번호로
//!   오므로 그 칸이 프록시로 보낸 Bearer 에 묶는다.
//! - **다른 기기**: 기기마다 자기 관측을 계정·조직 UUID 로 내보낸다(`export`, `/claude-usage/live`). `peer_loop` 가
//!   새 관측을 본 자리에서 다른 기기로 밀고(POST), 15초마다 당겨 와(GET) 막 켜진 기기도 맞춘다. 기기마다 토큰이
//!   달라 지문으로는 같은 계정을 못 알아본다. 토큰·지문은 기기 밖으로 안 나간다.
//!
//! `oauth/usage` 는 자주 치면 429 이고 맞으면 그 토큰을 쉬게 해, 그동안 화면이 낡은 값이었다. 여기 관측은 상류
//! 호출 없이 들어온다. 창마다 가장 새 관측이 이기고, 초기화 시각이 지난 창은 쓰지 않는다. 같은 계정 조회만
//! 덧입히므로 다른 계정의 숫자가 새지 않는다.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PREFIX: &str = "anthropic-ratelimit-unified-";
/// 이보다 오래된 관측은 덧입히지 않는다 — 그 사이 창이 넘어갔을 수 있다.
const KEEP_FOR: Duration = Duration::from_secs(15 * 60);
/// 다른 기기에서 당겨 오는 주기. 새 관측은 본 기기가 바로 밀어 주므로, 이건 막 켜졌거나 밀기를 놓친 기기를
/// 맞추는 몫이다. 상류가 아니라 내 기기들이라 429 가 없다.
const PEER_EVERY: Duration = Duration::from_secs(15);
/// 밀기 전에 모으는 틈 — 칸 여럿이 몇 ms 사이로 재면 한 번에 보낸다.
const PUSH_GATHER: Duration = Duration::from_millis(250);
/// 한 기기에 묻는 한계. 꺼진 기기가 다른 기기 관측까지 늦추지 않게 짧게 둔다.
const PEER_TIMEOUT: Duration = Duration::from_secs(4);
/// 응답이 없거나 이 창구를 모르는 구판 기기는 이만큼 쉰다.
const PEER_BACKOFF: Duration = Duration::from_secs(60);
pub(crate) const PEER_BODY_LIMIT: usize = 64 * 1024;
const PEER_ACCOUNTS_CAP: usize = 32;
/// 신원을 모르는 토큰의 프로필을 다시 묻기까지. 토큰이 갈리지 않는 한 한 번이면 된다.
const ACCOUNT_ASK_EVERY: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Window {
    pub(crate) pct: f64,
    pub(crate) resets_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Header,
    Measure,
    Peer,
}

#[derive(Debug, Clone)]
struct Seen {
    win: Window,
    at: Instant,
    source: Source,
}

type Windows = HashMap<String, Seen>;
/// 공급자 계정 UUID 와 조직 UUID 쌍 — 기기가 달라도 같은 계정이면 같다.
type Account = (String, String);

#[derive(Default)]
struct Store {
    /// 이 기기에서 본 관측 — Bearer 지문별.
    local: HashMap<u64, Windows>,
    /// 다른 기기가 본 관측 — 계정별.
    peers: HashMap<Account, Windows>,
    /// 칸 번호 → 그 칸이 마지막으로 보낸 Bearer 의 지문.
    panes: HashMap<String, (u64, Instant)>,
    /// 지문 → 계정. 토큰은 한 계정에만 속하므로 한 번 알면 그대로다.
    accounts: HashMap<u64, (Account, Instant)>,
    asked: HashMap<u64, Instant>,
}

impl Store {
    fn prune(&mut self, now: Instant) {
        let fresh = |seen: &Seen| now.duration_since(seen.at) < KEEP_FOR;
        for windows in self.local.values_mut().chain(self.peers.values_mut()) {
            windows.retain(|_, seen| fresh(seen));
        }
        self.local.retain(|_, w| !w.is_empty());
        self.peers.retain(|_, w| !w.is_empty());
        self.panes.retain(|_, (_, at)| now.duration_since(*at) < KEEP_FOR);
        self.accounts.retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(24 * 60 * 60));
        self.asked.retain(|_, at| now.duration_since(*at) < ACCOUNT_ASK_EVERY);
    }

    /// 그 지문들과 같은 계정의 관측을 모두 모아 창마다 가장 새 것을 고른다.
    fn newest(&self, keys: &[u64], now: Instant, now_epoch: u64) -> Windows {
        let accounts: Vec<&Account> = keys.iter().filter_map(|k| self.accounts.get(k).map(|(a, _)| a)).collect();
        let same_account = |k: &u64| keys.contains(k) || self.accounts.get(k).is_some_and(|(a, _)| accounts.contains(&a));
        let local = self.local.iter().filter(|(k, _)| same_account(*k)).map(|(_, w)| w);
        let peers = self.peers.iter().filter(|(a, _)| accounts.contains(a)).map(|(_, w)| w);
        let mut out = Windows::new();
        for (name, seen) in local.chain(peers).flatten() {
            if now.duration_since(seen.at) >= KEEP_FOR || seen.win.resets_at.is_some_and(|at| at <= now_epoch) {
                continue;
            }
            if out.get(name).is_none_or(|best| best.at < seen.at) {
                out.insert(name.clone(), seen.clone());
            }
        }
        out
    }
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

/// 새 관측이 들어왔다는 깃발. 앱 폴러가 `wait_dirty` 로 그 자리에서 깨어 다시 그린다.
fn dirty() -> &'static (Mutex<bool>, Condvar) {
    static DIRTY: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    DIRTY.get_or_init(Default::default)
}

/// 이 기기에서 새로 본 관측이 있다 — `peer_loop` 가 다른 기기로 민다.
fn push_wanted() -> &'static tokio::sync::Notify {
    static PUSH: OnceLock<tokio::sync::Notify> = OnceLock::new();
    PUSH.get_or_init(Default::default)
}

/// 값이 바뀌었다. 다른 기기에서 받은 것은 되밀지 않는다 — 기기끼리 메아리가 돈다.
fn changed(local: bool) {
    let (flag, wake) = dirty();
    if let Ok(mut flag) = flag.lock() {
        *flag = true;
    }
    wake.notify_all();
    if local {
        push_wanted().notify_one();
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn pane_key(pane: &str) -> String {
    pane.trim_start_matches('%').to_owned()
}

fn bearer(authorization: Option<&str>) -> Option<&str> {
    authorization
        .and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// 창 하나를 넣는다. 값이 바뀌었으면 참 — 같은 값이면 관측 시각만 당긴다.
fn put(windows: &mut Windows, name: &str, win: Window, at: Instant, source: Source) -> bool {
    let changed = windows.get(name).is_none_or(|old| old.win != win);
    if windows.get(name).is_none_or(|old| old.at <= at) {
        windows.insert(name.to_owned(), Seen { win, at, source });
    }
    changed
}

fn learn_account(store: &mut Store, key: u64, token: &str, now: Instant) {
    if let Some(id) = super::verified_claude_identity_for_token(token) {
        store.accounts.insert(key, ((id.account_uuid, id.organization_uuid), now));
    }
}

/// 응답 헤더에서 (한도 헤더가 하나라도 있었나, 창별 값)을 꺼낸다. 창 이름은 헤더 가운데 토막(`5h`·`7d`)이고,
/// 사용률이 없는 묶음(`overage-status` 같은 계정 전체 표시)은 창이 아니라 버린다.
fn parse_headers<'a>(headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> (bool, HashMap<String, Window>) {
    let mut pct: HashMap<String, f64> = HashMap::new();
    let mut reset: HashMap<String, u64> = HashMap::new();
    let mut any = false;
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let Some(rest) = name.strip_prefix(PREFIX) else { continue };
        any = true;
        let Some((window, field)) = rest.rsplit_once('-') else { continue };
        let value = value.trim();
        match field {
            // 헤더는 0~1 비율이고, 사용량 응답(`limits[].percent`)은 0~100 이다.
            "utilization" => {
                if let Some(u) = value.parse::<f64>().ok().map(|u| u * 100.0).filter(|p| p.is_finite()) {
                    pct.insert(window.to_owned(), u);
                }
            }
            "reset" => {
                if let Ok(at) = value.parse::<u64>() {
                    reset.insert(window.to_owned(), at);
                }
            }
            _ => {}
        }
    }
    let windows = pct
        .into_iter()
        .map(|(window, pct)| {
            let resets_at = reset.get(&window).copied();
            (window, Window { pct, resets_at })
        })
        .collect();
    (any, windows)
}

/// 프록시가 `/v1/messages` 응답마다 부른다. `authorization` 은 실제로 보낸 값이다(401 재전송이면 바꾼 것).
/// `pane` 은 그 요청이 온 칸 — Claude Code 의 측정(`record_measure`)을 이 Bearer 에 묶는 데 쓴다.
///
/// 한도 헤더가 실린 응답은 그 순간 경고선을 넘은 창 전부를 싣는다 — 헤더로 알던 창이 빠졌으면 경고선 아래로
/// 내려갔다는 뜻이라 지운다. 측정으로 안 창은 경고선과 무관하므로 남긴다.
pub fn record<'a>(pane: &str, authorization: Option<&str>, headers: impl IntoIterator<Item = (&'a str, &'a str)>) {
    let Some(token) = bearer(authorization) else { return };
    let key = super::token_key(token);
    let (any, windows) = parse_headers(headers);
    let Ok(mut store) = store().lock() else { return };
    let now = Instant::now();
    store.prune(now);
    if !pane.is_empty() {
        store.panes.insert(pane_key(pane), (key, now));
    }
    if !store.accounts.contains_key(&key) {
        learn_account(&mut store, key, token, now);
    }
    if !any {
        return;
    }
    let local = store.local.entry(key).or_default();
    let before = local.len();
    local.retain(|name, seen| seen.source != Source::Header || windows.contains_key(name));
    let mut changed = local.len() != before;
    for (name, win) in windows {
        changed |= put(local, &name, win, now, Source::Header);
    }
    if changed {
        self::changed(true);
    }
}

/// Claude Code 가 그 칸에서 잰 한도(`five_hour`·`seven_day`, 0~100, ISO 초기화 시각). 그 칸이 프록시로 요청을
/// 보낸 적이 없으면 어느 계정 것인지 몰라 버린다.
pub fn record_measure<'a>(pane: &str, limits: impl IntoIterator<Item = (&'a str, f64, Option<&'a str>)>) {
    let Ok(mut store) = store().lock() else { return };
    let now = Instant::now();
    store.prune(now);
    let Some(&(key, _)) = store.panes.get(&pane_key(pane)) else { return };
    let local = store.local.entry(key).or_default();
    let mut changed = false;
    for (kind, pct, resets_at) in limits {
        let name = match kind {
            "five_hour" => "5h",
            "seven_day" => "7d",
            _ => continue,
        };
        if !pct.is_finite() || pct < 0.0 {
            continue;
        }
        let win = Window { pct, resets_at: resets_at.and_then(parse_iso_utc) };
        changed |= put(local, name, win, now, Source::Measure);
    }
    if changed {
        self::changed(true);
    }
}

/// 사용량 조회가 실제로 쓴 토큰 — 지문을 계정에 묶어 둔다(메모리의 검증된 신원만 본다).
pub(crate) fn remember_account(token: &str) {
    let Ok(mut store) = store().lock() else { return };
    learn_account(&mut store, super::token_key(token), token, Instant::now());
}

/// 그 토큰의 계정을 아직 모르고, 최근에 묻지도 않았나. 참이면 물은 것으로 적는다.
pub(crate) fn should_ask_account(token: &str) -> bool {
    let key = super::token_key(token);
    let Ok(mut store) = store().lock() else { return false };
    let now = Instant::now();
    store.prune(now);
    if store.accounts.contains_key(&key) || store.asked.contains_key(&key) {
        return false;
    }
    store.asked.insert(key, now);
    true
}

/// 새 관측이 들어왔는가. 읽으면 내려간다.
pub fn take_dirty() -> bool {
    dirty().0.lock().map(|mut flag| std::mem::take(&mut *flag)).unwrap_or(false)
}

/// 새 관측을 `timeout` 까지 기다린다 — 사용량 폴러가 자는 동안 들어온 관측을 그 자리에서 그린다. 들어왔으면 참이고
/// 깃발은 내려간다.
pub fn wait_dirty(timeout: Duration) -> bool {
    let (flag, wake) = dirty();
    let waited = flag.lock().ok().and_then(|flag| wake.wait_timeout_while(flag, timeout, |up| !*up).ok());
    match waited {
        Some((mut flag, _)) => std::mem::take(&mut *flag),
        None => {
            std::thread::sleep(timeout);
            false
        }
    }
}

fn newest(keys: &[u64]) -> Windows {
    store().lock().map(|s| s.newest(keys, Instant::now(), epoch_now())).unwrap_or_default()
}

/// 시험용 — 그 토큰으로 본 창의 사용률(0~100).
#[cfg(any(test, feature = "test-isolation"))]
pub fn observed_pct(token: &str, window: &str) -> Option<f64> {
    newest(&[super::token_key(token)]).get(window).map(|seen| seen.win.pct)
}

/// 그 지문들(과 같은 계정)의 가장 새 관측 시각.
pub(crate) fn latest_at(keys: &[u64]) -> Option<Instant> {
    newest(keys).values().map(|seen| seen.at).max()
}

/// `usage`(oauth/usage 응답 꼴)에 관측을 덧입힌다. `fetched_at` 보다 새 관측만 쓴다 — 방금 받은
/// 상류 값이 관측보다 새로우면 그쪽이 맞다. 덧입혔으면 가장 새 관측 시각을 돌려준다.
pub(crate) fn overlay(keys: &[u64], usage: &mut Value, fetched_at: Option<Instant>) -> Option<Instant> {
    let mut applied: Option<Instant> = None;
    for (window, seen) in newest(keys) {
        if fetched_at.is_some_and(|fetched| fetched >= seen.at) {
            continue;
        }
        let (kind, group, legacy) = match window.as_str() {
            "5h" => ("session", "session", "five_hour"),
            "7d" => ("weekly_all", "weekly", "seven_day"),
            _ => continue,
        };
        apply(usage, kind, group, legacy, &seen.win);
        applied = applied.max(Some(seen.at));
    }
    applied
}

/// `/claude-usage/live` 의 몸 — 이 기기에서 본 관측을 계정별로. 다른 기기에서 받은 것은 되돌려 보내지 않는다.
pub fn export() -> Value {
    let Ok(store) = store().lock() else { return json!({ "v": 1, "accounts": [] }) };
    let now = Instant::now();
    let now_epoch = epoch_now();
    let mut by_account: HashMap<&Account, Windows> = HashMap::new();
    for (key, windows) in &store.local {
        let Some((account, _)) = store.accounts.get(key) else { continue };
        let merged = by_account.entry(account).or_default();
        for (name, seen) in windows {
            if now.duration_since(seen.at) >= KEEP_FOR || seen.win.resets_at.is_some_and(|at| at <= now_epoch) {
                continue;
            }
            if merged.get(name).is_none_or(|best| best.at < seen.at) {
                merged.insert(name.clone(), seen.clone());
            }
        }
    }
    let accounts: Vec<Value> = by_account
        .into_iter()
        .filter(|(_, windows)| !windows.is_empty())
        .map(|((account, org), windows)| {
            let windows: serde_json::Map<String, Value> = windows
                .into_iter()
                .map(|(name, seen)| {
                    let age = now.duration_since(seen.at).as_millis() as u64;
                    (name, json!({ "pct": seen.win.pct, "resets_at": seen.win.resets_at, "age_ms": age }))
                })
                .collect();
            json!({ "account_uuid": account, "organization_uuid": org, "windows": windows })
        })
        .collect();
    json!({ "v": 1, "accounts": accounts })
}

fn valid_uuid(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    let parsed = uuid::Uuid::parse_str(raw).ok().filter(|u| raw.len() == 36 && !u.is_nil())?;
    Some(parsed.to_string())
}

/// 다른 기기의 `export` 를 합친다. 값이 바뀐 창이 있으면 참.
pub(crate) fn merge_peer(body: &Value) -> bool {
    if body["v"] != 1 {
        return false;
    }
    let Some(accounts) = body["accounts"].as_array() else { return false };
    let Ok(mut store) = store().lock() else { return false };
    let now = Instant::now();
    store.prune(now);
    let mut changed = false;
    for entry in accounts.iter().take(PEER_ACCOUNTS_CAP) {
        let (Some(account), Some(org)) = (valid_uuid(&entry["account_uuid"]), valid_uuid(&entry["organization_uuid"])) else {
            continue;
        };
        let Some(windows) = entry["windows"].as_object() else { continue };
        for (name, w) in windows {
            if name != "5h" && name != "7d" {
                continue;
            }
            let (Some(pct), Some(age)) = (w["pct"].as_f64(), w["age_ms"].as_u64()) else { continue };
            if !pct.is_finite() || !(0.0..=1000.0).contains(&pct) || age >= KEEP_FOR.as_millis() as u64 {
                continue;
            }
            let Some(at) = now.checked_sub(Duration::from_millis(age)) else { continue };
            let win = Window { pct, resets_at: w["resets_at"].as_u64() };
            let slot = store.peers.entry((account.clone(), org.clone())).or_default();
            if slot.get(name).is_none_or(|old| old.at < at) {
                changed |= put(slot, name, win, at, Source::Peer);
            }
        }
    }
    if changed {
        self::changed(false);
    }
    changed
}

/// 다른 기기와 관측을 주고받는 루프 — 협업 판이 지금 닿는다고 본 기기마다, 판과 같은 길(직통·관문 우회)로.
/// 이 기기에서 새 관측을 보면 그 자리에서 밀고, `PEER_EVERY` 마다 당겨 온다. 기기마다 따로 띄워 꺼진 기기가 다른
/// 기기를 늦추지 않게 한다. 판 수집기가 없는 실행(검증·홀로 선 서버)에서는 주고받을 곳이 없어 쉰다.
pub async fn peer_loop() {
    let Ok(client) = reqwest::Client::builder()
        .timeout(PEER_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return;
    };
    let mut online: Vec<String> = Vec::new();
    // (기기, 밀기인가) → 이 시각까지 쉰다. 밀기를 모르는 구판 기기도 당기기는 계속 된다.
    let mut resting: HashMap<(String, bool), Instant> = HashMap::new();
    let mut next_pull = tokio::time::Instant::now();
    loop {
        let push = tokio::select! {
            () = push_wanted().notified() => true,
            () = tokio::time::sleep_until(next_pull) => false,
        };
        if push {
            tokio::time::sleep(PUSH_GATHER).await;
        } else {
            online = online_machines();
            next_pull = tokio::time::Instant::now() + PEER_EVERY;
        }
        let mine = push.then(export);
        let now = Instant::now();
        resting.retain(|_, until| now < *until);
        let mut jobs = tokio::task::JoinSet::new();
        for machine in &online {
            if resting.contains_key(&(machine.clone(), push)) {
                continue;
            }
            let Some(base) = kasa_collab::board_service::remote_base(machine) else { continue };
            let (client, mine, machine) = (client.clone(), mine.clone(), machine.clone());
            jobs.spawn(async move { (machine, exchange(&client, &base, mine.as_ref()).await) });
        }
        while let Some(joined) = jobs.join_next().await {
            let Ok((machine, reply)) = joined else { continue };
            match reply {
                Some(Some(theirs)) => {
                    merge_peer(&theirs);
                }
                Some(None) => {}
                None => {
                    resting.insert((machine, push), Instant::now() + PEER_BACKOFF);
                }
            }
        }
    }
}

fn online_machines() -> Vec<String> {
    kasa_collab::board_service::snapshot(&json!({ "scope": "all" }))
        .ok()
        .and_then(|board| {
            board["sources"].as_array().map(|sources| {
                sources
                    .iter()
                    .filter(|s| s["is_local"] != true && s["state"] == "online")
                    .filter_map(|s| s["machine_id"].as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// 한 기기와 한 번 주고받는다. `mine` 이 있으면 밀고(POST) 답 몸은 없다(`Some(None)`). 없으면 그 기기의 관측을
/// 당겨 온다. 닿지 않았거나 거절됐으면 `None`.
async fn exchange(client: &reqwest::Client, base: &str, mine: Option<&Value>) -> Option<Option<Value>> {
    let host = kasa_collab::env::env();
    let url = format!("{}/claude-usage/live", host.route_base(base).trim_end_matches('/'));
    let mut request = match mine {
        Some(body) => client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(body).ok()?),
        None => client.get(url),
    };
    if let Some(token) = host.auth_token(base) {
        request = request.header("x-kasa-token", token);
    }
    let mut response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    if mine.is_some() {
        return Some(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > PEER_BODY_LIMIT {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).ok().map(Some)
}

fn apply(usage: &mut Value, kind: &str, group: &str, legacy: &str, win: &Window) {
    let resets_at = win.resets_at.map(iso_utc);
    // `limits[]` 가 없는 옛 응답에 배열을 새로 만들면 앱이 그 배열만 읽어 다른 창(`five_hour`)을 잃는다.
    if let Some(limits) = usage.get_mut("limits").and_then(Value::as_array_mut) {
        let entry = limits
            .iter_mut()
            .find(|e| e["kind"] == kind && e.get("scope").is_none_or(Value::is_null));
        match entry {
            Some(e) => {
                e["percent"] = json!(win.pct);
                if let Some(at) = &resets_at {
                    e["resets_at"] = json!(at);
                }
                e["is_active"] = json!(true);
            }
            None => limits.push(json!({
                "kind": kind,
                "group": group,
                "percent": win.pct,
                "resets_at": resets_at,
                "scope": null,
                "is_active": true,
            })),
        }
    }
    let Some(map) = usage.as_object_mut() else { return };
    let slot = map.entry(legacy.to_owned()).or_insert(Value::Null);
    if !slot.is_object() {
        *slot = json!({});
    }
    slot["utilization"] = json!(win.pct);
    if let Some(at) = resets_at {
        slot["resets_at"] = json!(at);
    }
}

/// epoch 초 → `2026-11-16T07:00:00+00:00`(사용량 응답의 `resets_at` 꼴).
fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // 하워드 히넌트의 civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `2026-11-11T20:20:00.000Z`·`…+00:00`(UTC 만) → epoch 초. Claude Code 측정의 `resetsAt` 꼴이다.
fn parse_iso_utc(text: &str) -> Option<u64> {
    let (head, tail) = (text.get(..19)?, text.get(19..)?);
    let tail = match tail.strip_prefix('.') {
        Some(frac) => frac.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => tail,
    };
    if tail != "Z" && tail != "+00:00" {
        return None;
    }
    let b = head.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| head.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // 하워드 히넌트의 days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11 실측 응답 헤더 그대로.
    fn measured() -> Vec<(&'static str, &'static str)> {
        vec![
            ("anthropic-ratelimit-unified-7d-reset", "4102444800"),
            ("anthropic-ratelimit-unified-7d-status", "allowed_warning"),
            ("anthropic-ratelimit-unified-7d-surpassed-threshold", "0.75"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.92"),
            ("anthropic-ratelimit-unified-fallback-percentage", "0.5"),
            ("anthropic-ratelimit-unified-overage-disabled-reason", "org_level_disabled"),
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            ("anthropic-ratelimit-unified-representative-claim", "seven_day"),
            ("anthropic-ratelimit-unified-reset", "4102444800"),
            ("anthropic-ratelimit-unified-status", "allowed_warning"),
            ("content-type", "application/json"),
        ]
    }

    fn bind_account(token: &str, account: &str) {
        let mut store = store().lock().unwrap();
        let org = "00000000-0000-4000-8000-0000000000aa".to_owned();
        store.accounts.insert(super::super::token_key(token), ((account.to_owned(), org), Instant::now()));
    }

    #[test]
    fn only_windows_with_a_utilization_are_kept() {
        let got = parse_headers(measured()).1;
        assert_eq!(got.len(), 1, "{got:?}");
        let week = &got["7d"];
        assert!((week.pct - 92.0).abs() < 1e-9);
        assert_eq!(week.resets_at, Some(4_102_444_800));
    }

    #[test]
    fn iso_matches_the_usage_response_shape() {
        assert_eq!(iso_utc(1_794_812_400), "2026-11-16T07:00:00+00:00");
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00+00:00");
    }

    #[test]
    fn measured_reset_times_parse_back_to_epoch() {
        assert_eq!(parse_iso_utc("2026-11-16T07:00:00.000Z"), Some(1_794_812_400));
        assert_eq!(parse_iso_utc("2026-11-16T07:00:00+00:00"), Some(1_794_812_400));
        assert_eq!(parse_iso_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_iso_utc("2026-11-16T16:00:00+09:00"), None, "only UTC is accepted");
        assert_eq!(parse_iso_utc("soon"), None);
    }

    #[test]
    fn overlay_updates_the_matching_window_and_adds_a_missing_session() {
        let token = "tok-overlay-test";
        let mut headers = measured();
        headers.push(("anthropic-ratelimit-unified-5h-utilization", "0.4"));
        headers.push(("anthropic-ratelimit-unified-5h-reset", "4102400000"));
        record("", Some(&format!("Bearer {token}")), headers);
        let key = super::super::token_key(token);
        let mut usage = json!({
            "five_hour": null,
            "seven_day": { "utilization": 90.0, "resets_at": "old" },
            "limits": [
                { "kind": "weekly_all", "group": "weekly", "percent": 90, "resets_at": "old", "scope": null },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 0, "scope": { "model": { "display_name": "Fable" } } }
            ]
        });
        let fetched = Instant::now() - Duration::from_secs(120);
        assert!(overlay(&[key], &mut usage, Some(fetched)).is_some());
        assert_eq!(usage["limits"][0]["percent"], json!(92.0));
        assert_eq!(usage["limits"][0]["resets_at"], "2100-01-01T00:00:00+00:00");
        assert_eq!(usage["limits"][1]["percent"], json!(0), "a model-scoped window is not the 7d header");
        let session = usage["limits"].as_array().unwrap().iter().find(|e| e["kind"] == "session").expect("session added");
        assert_eq!(session["percent"], json!(40.0));
        assert_eq!(usage["five_hour"]["utilization"], json!(40.0));
        assert_eq!(usage["seven_day"]["utilization"], json!(92.0));
    }

    #[test]
    fn newer_upstream_value_or_another_token_is_left_alone() {
        record("", Some("Bearer tok-newer-test"), measured());
        let key = super::super::token_key("tok-newer-test");
        let mut usage = json!({ "limits": [{ "kind": "weekly_all", "percent": 50, "scope": null }] });
        assert!(overlay(&[key], &mut usage, Some(Instant::now() + Duration::from_secs(1))).is_none());
        assert!(overlay(&[super::super::token_key("someone-else")], &mut usage, None).is_none());
        assert_eq!(usage["limits"][0]["percent"], json!(50));
    }

    #[test]
    fn legacy_shape_without_limits_stays_without_limits() {
        record("", Some("Bearer tok-legacy-test"), measured());
        let key = super::super::token_key("tok-legacy-test");
        let mut usage = json!({ "five_hour": { "utilization": 10.0 } });
        assert!(overlay(&[key], &mut usage, None).is_some());
        assert!(usage.get("limits").is_none());
        assert_eq!(usage["seven_day"]["utilization"], json!(92.0));
        assert_eq!(usage["five_hour"]["utilization"], json!(10.0));
    }

    #[test]
    fn non_bearer_or_header_less_responses_record_nothing() {
        record("", None, measured());
        record("", Some("x-api-key sk"), measured());
        record("", Some("Bearer tok-empty-test"), [("content-type", "text/event-stream")]);
        assert!(latest_at(&[super::super::token_key("tok-empty-test")]).is_none());
    }

    /// 헤더는 경고선 아래 5시간 창을 싣지 않는다. 그 칸의 Claude Code 측정이 그 자리를 채운다.
    #[test]
    fn a_pane_measure_fills_the_session_window_headers_leave_out() {
        let token = "tok-measure-test";
        record("%9101", Some(&format!("Bearer {token}")), measured());
        record_measure(
            "%9101",
            [("five_hour", 8.0, Some("2100-01-01T00:00:00.000Z")), ("seven_day", 92.0, None), ("spend_limit", 5.0, None)],
        );
        assert_eq!(observed_pct(token, "5h"), Some(8.0));
        assert_eq!(observed_pct(token, "7d"), Some(92.0));

        // 다음 응답이 7일 창만 다시 실어도 측정으로 안 5시간 창은 남는다.
        record("9101", Some(&format!("Bearer {token}")), measured());
        assert_eq!(observed_pct(token, "5h"), Some(8.0));
    }

    #[test]
    fn a_measure_from_a_pane_that_never_called_the_proxy_is_dropped() {
        record_measure("%9102", [("five_hour", 30.0, None)]);
        assert!(!store().lock().unwrap().panes.contains_key("9102"));
    }

    /// 경고선 아래로 내려가 헤더에서 빠진 창은 옛 헤더 값을 더 덧입히지 않는다.
    #[test]
    fn a_header_window_that_drops_out_is_forgotten() {
        let token = "tok-drop-test";
        record("", Some(&format!("Bearer {token}")), measured());
        assert_eq!(observed_pct(token, "7d"), Some(92.0));
        record("", Some(&format!("Bearer {token}")), [("anthropic-ratelimit-unified-status", "allowed")]);
        assert_eq!(observed_pct(token, "7d"), None);
    }

    #[test]
    fn a_window_past_its_reset_is_not_overlaid() {
        let token = "tok-reset-test";
        record(
            "",
            Some(&format!("Bearer {token}")),
            [("anthropic-ratelimit-unified-5h-utilization", "0.99"), ("anthropic-ratelimit-unified-5h-reset", "1000")],
        );
        let mut usage = json!({ "five_hour": { "utilization": 3.0 } });
        assert!(overlay(&[super::super::token_key(token)], &mut usage, None).is_none());
        assert_eq!(usage["five_hour"]["utilization"], json!(3.0));
    }

    /// 다른 기기에서 받은 관측은 계정 UUID 로 이 기기의 같은 계정 조회에 덧입혀진다 — 토큰이 달라도.
    /// 기기 사이 한 번 주고받기 — 민 관측은 받은 기기의 같은 계정에 들어가고, 당겨 온 몸은 그대로 읽힌다.
    #[tokio::test]
    async fn a_push_lands_on_the_other_machine_and_a_pull_reads_back() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, super::super::routes()).await });
        let account = "33333333-3333-4333-8333-333333333333";
        bind_account("tok-pushed-here", account);
        let mine = json!({ "v": 1, "accounts": [{
            "account_uuid": account,
            "organization_uuid": "00000000-0000-4000-8000-0000000000aa",
            "windows": { "5h": { "pct": 63.0, "resets_at": 4_102_400_000u64, "age_ms": 10 } }
        }]});
        let client = reqwest::Client::new();
        assert_eq!(exchange(&client, &base, Some(&mine)).await, Some(None));
        assert_eq!(observed_pct("tok-pushed-here", "5h"), Some(63.0));
        let pulled = exchange(&client, &base, None).await.flatten().expect("pulled body");
        assert_eq!(pulled["v"], 1);
    }

    /// 앱 폴러는 자는 동안 들어온 관측을 그 자리에서 그린다 — 정기 조회 60초를 기다리지 않는다.
    #[test]
    fn a_new_observation_wakes_the_sleeping_poller() {
        let pane = "%wake-1";
        record(pane, Some("Bearer tok-wake"), std::iter::empty::<(&str, &str)>());
        let measure = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            record_measure(pane, [("five_hour", 12.0, None)]);
        });
        let started = Instant::now();
        assert!(wait_dirty(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(5));
        measure.join().unwrap();
        assert_eq!(observed_pct("tok-wake", "5h"), Some(12.0));
    }

    #[test]
    fn a_peer_observation_reaches_the_same_account_here() {
        let account = "11111111-1111-4111-8111-111111111111";
        let here = "tok-peer-here";
        bind_account(here, account);
        let body = json!({ "v": 1, "accounts": [{
            "account_uuid": account,
            "organization_uuid": "00000000-0000-4000-8000-0000000000aa",
            "windows": { "5h": { "pct": 41.0, "resets_at": 4_102_400_000u64, "age_ms": 2000 }, "spend": { "pct": 1.0, "age_ms": 0 } }
        }]});
        assert!(merge_peer(&body));
        assert!(!merge_peer(&body), "the same value again is not news");
        let mut usage = json!({ "five_hour": { "utilization": 5.0 } });
        assert!(overlay(&[super::super::token_key(here)], &mut usage, Some(Instant::now() - Duration::from_secs(60))).is_some());
        assert_eq!(usage["five_hour"]["utilization"], json!(41.0));
        assert_eq!(observed_pct("tok-peer-unbound", "5h"), None, "a token of an unknown account gets nothing");
    }

    #[test]
    fn export_names_accounts_not_tokens_and_skips_peer_values() {
        let account = "22222222-2222-4222-8222-222222222222";
        let token = "tok-export-test";
        bind_account(token, account);
        record("%9103", Some(&format!("Bearer {token}")), measured());
        let peer_only = "33333333-3333-4333-8333-333333333333";
        merge_peer(&json!({ "v": 1, "accounts": [{
            "account_uuid": peer_only, "organization_uuid": "00000000-0000-4000-8000-0000000000aa",
            "windows": { "7d": { "pct": 10.0, "age_ms": 0 } }
        }]}));
        let out = export();
        let text = out.to_string();
        assert!(!text.contains(token));
        assert!(!text.contains(&super::super::token_key(token).to_string()));
        assert!(!text.contains(peer_only), "a peer value is not echoed back");
        let mine = out["accounts"].as_array().unwrap().iter().find(|a| a["account_uuid"] == account).expect("exported");
        assert_eq!(mine["windows"]["7d"]["pct"], json!(92.0));
        assert!(mine["windows"]["7d"]["age_ms"].as_u64().unwrap() < 60_000);
    }

    #[test]
    fn malformed_peer_bodies_are_ignored() {
        assert!(!merge_peer(&json!({ "v": 2, "accounts": [] })));
        assert!(!merge_peer(&json!({ "v": 1, "accounts": [{ "account_uuid": "nope", "organization_uuid": "x", "windows": {} }] })));
        let account = "44444444-4444-4444-8444-444444444444";
        let org = "00000000-0000-4000-8000-0000000000aa";
        assert!(!merge_peer(&json!({ "v": 1, "accounts": [{ "account_uuid": account, "organization_uuid": org,
            "windows": { "5h": { "pct": f64::MAX, "age_ms": 0 }, "7d": { "pct": 5.0, "age_ms": 3_600_000u64 } } }] })));
    }
}
