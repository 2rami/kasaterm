use super::{
    expires, fingerprint, local_slots, load_settings, map_path, now_ms, opaque, text, Held, Local, Read, Slot, Stores,
    LINEAGE, LINEAGE_TTL_MS,
};
use axum::http::{header, HeaderMap, HeaderValue};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;

const RECOVER_LIMIT: Duration = Duration::from_secs(8);
const POLL_EVERY: Duration = Duration::from_millis(40);

#[derive(Clone)]
pub struct Ticket {
    key: String,
    fingerprint: String,
    expires_at: u64,
    generation: String,
    stamp: crate::device_auth::Stamp,
}

impl Ticket {
    /// 요청을 보내기 전에만 원래 Bearer 를 확실한 관리 사슬에 묶을 수 있다. 이후에는 헤더를 다시 믿지 않는다.
    pub fn issue(headers: &HeaderMap) -> Option<Self> {
        if !crate::device_auth::sync_environment_allowed() {
            return None;
        }
        ledger().issue(headers)
    }

    /// 새 토큰 탐색은 같은 사슬끼리 합치고, 각 재시도 직전 Store 재검증은 겹치지 않게 한 줄로 세운다.
    pub async fn recover(&self) -> Option<Recovered> {
        if !crate::device_auth::sync_environment_allowed() {
            return None;
        }
        ledger()
            .recover_with_limit(self, Arc::new(LocalIo), RECOVER_LIMIT)
            .await
    }
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ticket([redacted])")
    }
}

#[derive(Clone)]
pub struct Recovered {
    authorization: HeaderValue,
}

impl Recovered {
    /// 민감 표시를 호출부에서도 강제해 응답·진단 Debug 에 새 Bearer 가 나타나지 않게 한다.
    pub fn authorization(&self) -> HeaderValue {
        let mut value = self.authorization.clone();
        value.set_sensitive(true);
        value
    }

    fn from_token(token: &str) -> Option<Self> {
        if !opaque(token) {
            return None;
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}")).ok()?;
        authorization.set_sensitive(true);
        Some(Self { authorization })
    }
}

impl fmt::Debug for Recovered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Recovered([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mark {
    fingerprint: String,
    expires_at: u64,
}

#[derive(Clone)]
struct Observed {
    generation: String,
    updated: u64,
    current: Mark,
    lineage: Vec<Mark>,
}

#[derive(Default)]
struct State {
    stamp: Option<crate::device_auth::Stamp>,
    managed: BTreeSet<String>,
    observed: BTreeMap<String, Observed>,
}

#[derive(Default)]
struct Flight {
    active: Mutex<Option<Arc<Round>>>,
    io: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
struct Round {
    result: Mutex<RoundResult>,
    notify: Notify,
}

#[derive(Clone, Default)]
enum RoundResult {
    #[default]
    Pending,
    Complete(Option<SharedResult>),
}

struct FlightLeader {
    flight: Arc<Flight>,
    round: Arc<Round>,
    armed: bool,
}

impl FlightLeader {
    fn complete(mut self, result: Option<SharedResult>) -> Option<Recovered> {
        let recovered = result.as_ref().map(|result| result.recovered.clone());
        let saved = if let Ok(mut state) = self.round.result.lock() {
            *state = RoundResult::Complete(result);
            true
        } else {
            false
        };
        if let Ok(mut active) = self.flight.active.lock() {
            if active.as_ref().is_some_and(|round| Arc::ptr_eq(round, &self.round)) {
                *active = None;
            }
        }
        self.armed = !saved;
        self.round.notify.notify_waiters();
        saved.then_some(recovered).flatten()
    }
}

impl Drop for FlightLeader {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut state) = self.round.result.lock() {
            *state = RoundResult::Complete(None);
        }
        if let Ok(mut active) = self.flight.active.lock() {
            if active.as_ref().is_some_and(|round| Arc::ptr_eq(round, &self.round)) {
                *active = None;
            }
        }
        self.round.notify.notify_waiters();
    }
}

#[derive(Clone)]
struct SharedResult {
    expected: Mark,
    recovered: Recovered,
    binding: SlotBinding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SlotBinding {
    id: String,
    store: Option<std::path::PathBuf>,
}

impl Flight {
    fn enter(self: &Arc<Self>) -> Option<(Arc<Round>, bool)> {
        let mut active = self.active.lock().ok()?;
        if let Some(round) = active.as_ref() {
            return Some((round.clone(), false));
        }
        let round = Arc::new(Round::default());
        *active = Some(round.clone());
        Some((round, true))
    }
}

fn flight_in_use(flight: &Arc<Flight>) -> bool {
    Arc::strong_count(flight) > 1 || flight.active.lock().is_ok_and(|active| active.is_some())
}

#[derive(Clone, Default)]
pub(crate) struct Ledger {
    state: Arc<Mutex<State>>,
    flights: Arc<Mutex<HashMap<String, Arc<Flight>>>>,
}

pub(crate) fn ledger() -> &'static Ledger {
    static LEDGER: OnceLock<Ledger> = OnceLock::new();
    LEDGER.get_or_init(Ledger::default)
}

fn valid_generation(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_key(value: &str) -> bool {
    value.starts_with("claude:") && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    if headers.contains_key("x-api-key") {
        return None;
    }
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") || parts.next().is_some() || !opaque(token) {
        return None;
    }
    Some(token)
}

impl Ledger {
    pub(crate) fn begin(&self, stamp: &crate::device_auth::Stamp) {
        let changed = self.state.lock().is_ok_and(|mut state| {
            if state.stamp.as_ref() == Some(stamp) {
                return false;
            }
            *state = State { stamp: Some(stamp.clone()), ..State::default() };
            true
        });
        if changed {
            if let Ok(mut flights) = self.flights.lock() {
                flights.retain(|_, flight| flight_in_use(flight));
            }
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = State::default();
        }
        if let Ok(mut flights) = self.flights.lock() {
            flights.retain(|_, flight| flight_in_use(flight));
        }
    }

    pub(crate) fn manage(&self, keys: Vec<String>) {
        let managed: BTreeSet<String> = keys.into_iter().filter(|key| valid_key(key)).collect();
        if let Ok(mut state) = self.state.lock() {
            state.managed = managed.clone();
        }
        if let Ok(mut flights) = self.flights.lock() {
            flights.retain(|key, flight| managed.contains(key) || flight_in_use(flight));
        }
    }

    pub(crate) fn observe(&self, chains: &[Held]) {
        self.observe_at(chains, now_ms());
    }

    fn observe_at(&self, chains: &[Held], now: u64) {
        let mut observed = BTreeMap::new();
        for chain in chains {
            let Some(access) = chain.access.as_ref() else { continue };
            if chain.state != "ok"
                || !valid_key(&chain.key)
                || !valid_generation(&chain.generation)
                || !opaque(&access.access_token)
                || access.expires_at <= now
            {
                continue;
            }
            let lineage = if chain.updated.saturating_add(LINEAGE_TTL_MS) < now {
                Vec::new()
            } else {
                chain
                    .lineage
                    .iter()
                    .filter(|spent| {
                        spent.expires_at > now
                            && spent.generation == chain.generation
                            && super::valid_fingerprint(&spent.fingerprint)
                    })
                    .rev()
                    .take(LINEAGE)
                    .map(|spent| Mark {
                        fingerprint: spent.fingerprint.clone(),
                        expires_at: spent.expires_at,
                    })
                    .collect()
            };
            observed.insert(
                chain.key.clone(),
                Observed {
                    generation: chain.generation.clone(),
                    updated: chain.updated,
                    current: Mark {
                        fingerprint: fingerprint(&access.access_token),
                        expires_at: access.expires_at,
                    },
                    lineage,
                },
            );
        }
        if let Ok(mut state) = self.state.lock() {
            state.observed = observed;
        }
    }

    fn issue(&self, headers: &HeaderMap) -> Option<Ticket> {
        self.issue_at(headers, now_ms())
    }

    fn issue_at(&self, headers: &HeaderMap, now: u64) -> Option<Ticket> {
        let token = bearer(headers)?;
        let target = fingerprint(token);
        let state = self.state.lock().ok()?;
        let stamp = state.stamp.clone()?;
        let mut found: Option<Ticket> = None;
        for (key, chain) in state.observed.iter().filter(|(key, _)| state.managed.contains(*key)) {
            let mut marks = chain.lineage.iter().filter(|mark| {
                chain.updated.saturating_add(LINEAGE_TTL_MS) >= now && mark.expires_at > now
            });
            let matched = if chain.current.fingerprint == target && chain.current.expires_at > now {
                Some(&chain.current)
            } else {
                marks.find(|mark| mark.fingerprint == target)
            };
            let Some(mark) = matched else { continue };
            if found.is_some() {
                return None;
            }
            found = Some(Ticket {
                key: key.clone(),
                fingerprint: target.clone(),
                expires_at: mark.expires_at,
                generation: chain.generation.clone(),
                stamp: stamp.clone(),
            });
        }
        found
    }

    fn check(&self, ticket: &Ticket) -> MemoryCheck {
        self.check_at(ticket, now_ms())
    }

    fn check_at(&self, ticket: &Ticket, now: u64) -> MemoryCheck {
        if ticket.expires_at <= now || !valid_generation(&ticket.generation) {
            return MemoryCheck::Stop;
        }
        let Ok(state) = self.state.lock() else { return MemoryCheck::Stop };
        if state.stamp.as_ref() != Some(&ticket.stamp) || !state.managed.contains(&ticket.key) {
            return MemoryCheck::Stop;
        }
        let Some(chain) = state.observed.get(&ticket.key) else { return MemoryCheck::Stop };
        if chain.generation != ticket.generation {
            return MemoryCheck::Stop;
        }
        let original_is_current = chain.current.fingerprint == ticket.fingerprint
            && chain.current.expires_at == ticket.expires_at;
        let original_is_lineage = chain.updated.saturating_add(LINEAGE_TTL_MS) >= now
            && chain.lineage.iter().any(|mark| {
                mark.fingerprint == ticket.fingerprint && mark.expires_at == ticket.expires_at
            });
        if !original_is_current && !original_is_lineage {
            return MemoryCheck::Stop;
        }
        if chain.current.expires_at > ticket.expires_at
            && chain.current.expires_at > now
            && chain.current.fingerprint != ticket.fingerprint
        {
            MemoryCheck::Ready(chain.current.clone())
        } else {
            MemoryCheck::Wait
        }
    }

    fn flight(&self, key: &str) -> Option<Arc<Flight>> {
        let mut flights = self.flights.lock().ok()?;
        Some(flights.entry(key.to_string()).or_default().clone())
    }

    async fn stamp_current(io: Arc<dyn RecoveryIo>, stamp: crate::device_auth::Stamp) -> bool {
        tokio::task::spawn_blocking(move || io.stamp_current(&stamp)).await.unwrap_or(false)
    }

    async fn shared_current(
        &self,
        ticket: &Ticket,
        io: Arc<dyn RecoveryIo>,
        flight: Arc<Flight>,
        shared: &SharedResult,
    ) -> bool {
        if self.check(ticket) != MemoryCheck::Ready(shared.expected.clone()) {
            return false;
        }
        let for_io = ticket.clone();
        let binding = shared.binding.clone();
        let expected = shared.expected.clone();
        let (sent, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _guard = flight.io.clone().lock_owned().await;
            if sent.is_closed() {
                return;
            }
            let current = tokio::task::spawn_blocking(move || io.grant_current(&for_io, &binding, &expected))
                .await
                .unwrap_or(false);
            let delivered = sent.send(current);
            drop(flight);
            let _ = delivered;
        });
        let current = received.await.unwrap_or(false);
        current && self.check(ticket) == MemoryCheck::Ready(shared.expected.clone())
    }

    async fn recover_with_limit(
        &self,
        ticket: &Ticket,
        io: Arc<dyn RecoveryIo>,
        limit: Duration,
    ) -> Option<Recovered> {
        let deadline = tokio::time::Instant::now() + limit;
        tokio::time::timeout_at(deadline, self.recover_shared(ticket, io, deadline))
            .await
            .ok()
            .flatten()
    }

    async fn recover_shared(
        &self,
        ticket: &Ticket,
        io: Arc<dyn RecoveryIo>,
        deadline: tokio::time::Instant,
    ) -> Option<Recovered> {
        if matches!(self.check(ticket), MemoryCheck::Stop) {
            return None;
        }
        let flight = self.flight(&ticket.key)?;
        let (round, leader) = flight.enter()?;
        if leader {
            let work = self.clone();
            let work_ticket = ticket.clone();
            let work_io = io.clone();
            let work_round = round.clone();
            let work_flight = flight.clone();
            let leader = FlightLeader { flight: flight.clone(), round: work_round, armed: true };
            tokio::spawn(async move {
                let result = work.recover_once(&work_ticket, work_io, work_flight, deadline).await;
                let _ = leader.complete(result);
            });
        }
        loop {
            let notified = round.notify.notified();
            let result = round.result.lock().ok()?.clone();
            match result {
                RoundResult::Pending => tokio::time::timeout_at(deadline, notified).await.ok()?,
                RoundResult::Complete(None) => return None,
                RoundResult::Complete(Some(shared)) => {
                    return self
                        .shared_current(ticket, io, flight.clone(), &shared)
                        .await
                        .then_some(shared.recovered);
                }
            }
        }
    }

    async fn recover_once(
        &self,
        ticket: &Ticket,
        io: Arc<dyn RecoveryIo>,
        flight: Arc<Flight>,
        deadline: tokio::time::Instant,
    ) -> Option<SharedResult> {
        if !Self::stamp_current(io.clone(), ticket.stamp.clone()).await {
            return None;
        }
        super::poke();
        loop {
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            match self.check(ticket) {
                MemoryCheck::Stop => return None,
                MemoryCheck::Wait => {}
                MemoryCheck::Ready(expected) => {
                    let io = io.clone();
                    let probe_ticket = ticket.clone();
                    let for_probe = expected.clone();
                    let _guard = flight.io.clone().lock_owned().await;
                    let probe = tokio::task::spawn_blocking(move || io.probe(&probe_ticket, &for_probe)).await.ok()?;
                    match probe {
                        Probe::Stop => return None,
                        Probe::Pending => {}
                        Probe::Found(recovered, binding) => {
                            let shared = SharedResult { expected, recovered, binding };
                            if tokio::time::Instant::now() >= deadline {
                                return None;
                            }
                            return (self.check(ticket) == MemoryCheck::Ready(shared.expected.clone()))
                                .then_some(shared);
                        }
                    }
                }
            }
            let next = (tokio::time::Instant::now() + POLL_EVERY).min(deadline);
            tokio::time::sleep_until(next).await;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MemoryCheck {
    Stop,
    Wait,
    Ready(Mark),
}

enum Probe {
    Stop,
    Pending,
    Found(Recovered, SlotBinding),
}

trait RecoveryIo: Send + Sync {
    fn stamp_current(&self, stamp: &crate::device_auth::Stamp) -> bool;
    fn probe(&self, ticket: &Ticket, expected: &Mark) -> Probe;
    fn grant_current(&self, ticket: &Ticket, binding: &SlotBinding, expected: &Mark) -> bool;
}

struct LocalIo;

impl RecoveryIo for LocalIo {
    fn stamp_current(&self, stamp: &crate::device_auth::Stamp) -> bool {
        crate::device_auth::stamp_is_current(stamp)
    }

    fn probe(&self, ticket: &Ticket, expected: &Mark) -> Probe {
        if !self.stamp_current(&ticket.stamp) {
            return Probe::Stop;
        }
        let Some((slots, map)) = local_scope() else { return Probe::Stop };
        probe_slots(ticket, expected, &slots, &map, &Local)
    }

    fn grant_current(&self, ticket: &Ticket, binding: &SlotBinding, expected: &Mark) -> bool {
        if !self.stamp_current(&ticket.stamp) {
            return false;
        }
        let Some((slots, map)) = local_scope() else { return false };
        if !grant_matches(ticket, binding, expected, &slots, &map, &Local) {
            return false;
        }
        local_scope().is_some_and(|(slots, map)| binding_current(ticket, binding, &slots, &map, &Local))
            && self.stamp_current(&ticket.stamp)
    }
}

fn local_scope() -> Option<(Vec<Slot>, BTreeMap<String, String>)> {
    let (path, settings) = load_settings()?;
    let raw = std::fs::read_to_string(map_path()?).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let map = serde_json::from_value::<BTreeMap<String, String>>(value["slots"].clone()).ok()?;
    Some((local_slots(path.parent(), &settings), map))
}

fn binding_current(
    ticket: &Ticket,
    binding: &SlotBinding,
    slots: &[Slot],
    map: &BTreeMap<String, String>,
    stores: &dyn Stores,
) -> bool {
    slots.iter().any(|slot| {
        slot.id == binding.id
            && slot.store == binding.store
            && map.get(&slot.id) == Some(&ticket.key)
            && stores.same(slot)
    })
}

fn grant_matches(
    ticket: &Ticket,
    binding: &SlotBinding,
    expected: &Mark,
    slots: &[Slot],
    map: &BTreeMap<String, String>,
    stores: &dyn Stores,
) -> bool {
    let Some(slot) = slots.iter().find(|slot| {
        slot.id == binding.id
            && slot.store == binding.store
            && map.get(&slot.id) == Some(&ticket.key)
    }) else {
        return false;
    };
    if !stores.same(slot) {
        return false;
    }
    let Read::Present(doc) = stores.read(slot) else { return false };
    let current = text(&doc, "refreshToken").is_none()
        && text(&doc, "accessToken").is_some_and(|token| {
            fingerprint(token) == expected.fingerprint && expires(&doc) == expected.expires_at
        });
    current && stores.same(slot)
}

fn probe_slots(
    ticket: &Ticket,
    expected: &Mark,
    slots: &[Slot],
    map: &BTreeMap<String, String>,
    stores: &dyn Stores,
) -> Probe {
    let mut found = None;
    let mut pending = false;
    let mut mapped = false;
    for slot in slots.iter().filter(|slot| map.get(&slot.id) == Some(&ticket.key)) {
        mapped = true;
        match stores.read(slot) {
            Read::Unknown => pending = true,
            Read::Absent => {}
            Read::Present(doc) => {
                if !stores.same(slot) {
                    return Probe::Stop;
                }
                if text(&doc, "refreshToken").is_some() {
                    return Probe::Stop;
                }
                let Some(token) = text(&doc, "accessToken") else { continue };
                let mark = Mark { fingerprint: fingerprint(token), expires_at: expires(&doc) };
                if mark == *expected {
                    let Some(recovered) = Recovered::from_token(token) else { return Probe::Stop };
                    let binding = SlotBinding { id: slot.id.clone(), store: slot.store.clone() };
                    if found.is_none() {
                        found = Some((recovered, binding));
                    }
                } else if mark.fingerprint == ticket.fingerprint && mark.expires_at == ticket.expires_at {
                    pending = true;
                } else {
                    return Probe::Stop;
                }
            }
        }
    }
    if !mapped {
        return Probe::Stop;
    }
    found.map_or_else(
        || if pending { Probe::Pending } else { Probe::Stop },
        |(recovered, binding)| Probe::Found(recovered, binding),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_chains::{Access, Spent};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Condvar;

    const KEY: &str = "claude:fixture@example.invalid";
    const GENERATION: &str = "0123456789abcdef0123456789abcdef";

    fn headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
        headers
    }

    fn held(current: &str, current_expires: u64, old: &str, old_expires: u64) -> Held {
        Held {
            key: KEY.into(),
            state: "ok".into(),
            access: Some(Access { access_token: current.into(), expires_at: current_expires, ..Access::default() }),
            updated: now_ms(),
            generation: GENERATION.into(),
            lineage: vec![Spent {
                fingerprint: fingerprint(old),
                expires_at: old_expires,
                generation: GENERATION.into(),
            }],
            ..Held::default()
        }
    }

    fn ready(old: &str, new: &str) -> (Ledger, Ticket, Mark) {
        let ledger = Ledger::default();
        ledger.begin(&crate::device_auth::Stamp::fixture());
        ledger.manage(vec![KEY.into()]);
        let old_expires = now_ms() + 60_000;
        let new_expires = old_expires + 60_000;
        ledger.observe(&[held(new, new_expires, old, old_expires)]);
        let ticket = ledger.issue(&headers(old)).unwrap();
        let expected = Mark { fingerprint: fingerprint(new), expires_at: new_expires };
        (ledger, ticket, expected)
    }

    #[test]
    fn issue_accepts_only_one_known_managed_bearer() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        assert!(!format!("{ticket:?}").contains("old-token"));

        let mut api = headers("old-token");
        api.insert("x-api-key", HeaderValue::from_static("api-key"));
        assert!(ledger.issue(&api).is_none());

        let mut multiple = headers("old-token");
        multiple.append(header::AUTHORIZATION, HeaderValue::from_static("Bearer old-token"));
        assert!(ledger.issue(&multiple).is_none());
        assert!(ledger.issue(&headers("unknown-token")).is_none());

        let mut basic = HeaderMap::new();
        basic.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert!(ledger.issue(&basic).is_none());

        let mut other_org = held(
            "other-new-token",
            now_ms() + 120_000,
            "old-token",
            ticket.expires_at,
        );
        other_org.key = "claude:fixture team".into();
        ledger.manage(vec![KEY.into(), other_org.key.clone()]);
        ledger.observe(&[
            held("new-token", now_ms() + 120_000, "old-token", ticket.expires_at),
            other_org,
        ]);
        assert!(ledger.issue(&headers("old-token")).is_none());

        ledger.manage(Vec::new());
        assert!(ledger.issue(&headers("old-token")).is_none());
    }

    #[test]
    fn public_issue_is_disabled_in_test_isolation() {
        assert!(Ticket::issue(&headers("old-token")).is_none());
    }

    #[tokio::test]
    async fn a_preissued_ticket_cannot_enter_local_io_in_test_isolation() {
        let (_ledger, ticket, _) = ready("old-token", "new-token");
        assert!(ticket.recover().await.is_none());
    }

    struct FakeIo {
        calls: AtomicUsize,
        grants: AtomicUsize,
        current: AtomicBool,
        token: String,
        delay: Duration,
        allowed_grants: usize,
        logout_during_probe: bool,
    }

    impl RecoveryIo for FakeIo {
        fn stamp_current(&self, _: &crate::device_auth::Stamp) -> bool {
            self.current.load(Ordering::SeqCst)
        }

        fn probe(&self, _: &Ticket, _: &Mark) -> Probe {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            if self.logout_during_probe {
                self.current.store(false, Ordering::SeqCst);
            }
            Probe::Found(
                Recovered::from_token(&self.token).unwrap(),
                SlotBinding { id: "fixture-slot".into(), store: None },
            )
        }

        fn grant_current(&self, _: &Ticket, _: &SlotBinding, _: &Mark) -> bool {
            let grant = self.grants.fetch_add(1, Ordering::SeqCst);
            self.current.load(Ordering::SeqCst) && grant < self.allowed_grants
        }
    }

    #[tokio::test]
    async fn concurrent_recovery_is_singleflight_and_returns_a_sensitive_header() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(FakeIo {
            calls: AtomicUsize::new(0),
            grants: AtomicUsize::new(0),
            current: AtomicBool::new(true),
            token: "new-token".into(),
            delay: Duration::from_millis(50),
            allowed_grants: usize::MAX,
            logout_during_probe: false,
        });
        let (one, two) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_secs(1)),
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_secs(1)),
        );
        assert_eq!(io.calls.load(Ordering::SeqCst), 1);
        assert_eq!(io.grants.load(Ordering::SeqCst), 2, "each caller skipped its final grant check");
        for recovered in [one.unwrap(), two.unwrap()] {
            let authorization = recovered.authorization();
            assert!(authorization.is_sensitive());
            assert_eq!(authorization.to_str().unwrap(), "Bearer new-token");
            assert!(!format!("{recovered:?}").contains("new-token"));
        }
    }

    #[tokio::test]
    async fn leader_rechecks_the_device_stamp_after_reading_the_new_token() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(FakeIo {
            calls: AtomicUsize::new(0),
            grants: AtomicUsize::new(0),
            current: AtomicBool::new(true),
            token: "new-token".into(),
            delay: Duration::ZERO,
            allowed_grants: usize::MAX,
            logout_during_probe: true,
        });
        assert!(ledger.recover_with_limit(&ticket, io.clone(), Duration::from_secs(1)).await.is_none());
        assert_eq!(io.calls.load(Ordering::SeqCst), 1);
        assert_eq!(io.grants.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn every_waiter_rechecks_slot_permission_after_the_shared_read() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(FakeIo {
            calls: AtomicUsize::new(0),
            grants: AtomicUsize::new(0),
            current: AtomicBool::new(true),
            token: "new-token".into(),
            delay: Duration::from_millis(50),
            allowed_grants: 1,
            logout_during_probe: false,
        });
        let (one, two) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_secs(1)),
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_secs(1)),
        );
        assert_eq!(io.calls.load(Ordering::SeqCst), 1, "the follower repeated the keychain read");
        assert_eq!(io.grants.load(Ordering::SeqCst), 2);
        assert_eq!([one.is_some(), two.is_some()].into_iter().filter(|present| *present).count(), 1);
    }

    struct BlockingIo {
        calls: AtomicUsize,
        active: AtomicUsize,
        released: (Mutex<bool>, Condvar),
    }

    impl BlockingIo {
        fn release(&self) {
            if let Ok(mut released) = self.released.0.lock() {
                *released = true;
                self.released.1.notify_all();
            }
        }
    }

    impl RecoveryIo for BlockingIo {
        fn stamp_current(&self, _: &crate::device_auth::Stamp) -> bool {
            true
        }

        fn probe(&self, _: &Ticket, _: &Mark) -> Probe {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let waited = match self.released.0.lock() {
                Ok(released) => self.released.1.wait_while(released, |released| !*released).is_ok(),
                Err(_) => false,
            };
            self.active.fetch_sub(1, Ordering::SeqCst);
            if !waited {
                return Probe::Stop;
            }
            Probe::Found(
                Recovered::from_token("new-token").unwrap(),
                SlotBinding { id: "fixture-slot".into(), store: None },
            )
        }

        fn grant_current(&self, _: &Ticket, _: &SlotBinding, _: &Mark) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn timed_out_callers_cannot_accumulate_detached_blocking_probes() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(BlockingIo {
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            released: (Mutex::new(false), Condvar::new()),
        });
        let started = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while io.active.load(Ordering::SeqCst) == 0 {
                assert!(tokio::time::Instant::now() < deadline, "blocking probe never started");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let (first, ()) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(30)),
            started,
        );
        assert!(first.is_none());
        assert_eq!((io.calls.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (1, 1));

        let release = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!((io.calls.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (1, 1));
            io.release();
        };
        let (one, two, three, ()) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(200)),
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(200)),
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(200)),
            release,
        );
        assert!(one.is_none() && two.is_none() && three.is_none());
        assert_eq!((io.calls.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (1, 0));

        let recovered = ledger
            .recover_with_limit(&ticket, io.clone(), Duration::from_secs(1))
            .await;
        assert!(recovered.is_some(), "the cancelled caller left the chain permanently locked");
        assert_eq!((io.calls.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (2, 0));
    }

    struct BlockingGrantIo {
        probes: AtomicUsize,
        grants: AtomicUsize,
        active: AtomicUsize,
        released: (Mutex<bool>, Condvar),
    }

    impl BlockingGrantIo {
        fn release(&self) {
            if let Ok(mut released) = self.released.0.lock() {
                *released = true;
                self.released.1.notify_all();
            }
        }
    }

    impl RecoveryIo for BlockingGrantIo {
        fn stamp_current(&self, _: &crate::device_auth::Stamp) -> bool {
            true
        }

        fn probe(&self, _: &Ticket, _: &Mark) -> Probe {
            self.probes.fetch_add(1, Ordering::SeqCst);
            Probe::Found(
                Recovered::from_token("new-token").unwrap(),
                SlotBinding { id: "fixture-slot".into(), store: None },
            )
        }

        fn grant_current(&self, _: &Ticket, _: &SlotBinding, _: &Mark) -> bool {
            self.grants.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let waited = match self.released.0.lock() {
                Ok(released) => self.released.1.wait_while(released, |released| !*released).is_ok(),
                Err(_) => false,
            };
            self.active.fetch_sub(1, Ordering::SeqCst);
            waited
        }
    }

    #[tokio::test]
    async fn final_store_reads_stay_serial_after_callers_time_out() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(BlockingGrantIo {
            probes: AtomicUsize::new(0),
            grants: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            released: (Mutex::new(false), Condvar::new()),
        });
        let started = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while io.active.load(Ordering::SeqCst) == 0 {
                assert!(tokio::time::Instant::now() < deadline, "final Store read never started");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let (one, two, ()) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(30)),
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(30)),
            started,
        );
        assert!(one.is_none() && two.is_none());
        assert_eq!(io.probes.load(Ordering::SeqCst), 1);
        assert_eq!((io.grants.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (1, 1));

        io.release();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while io.active.load(Ordering::SeqCst) != 0 {
            assert!(tokio::time::Instant::now() < deadline, "cancelled Store read never finished");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let recovered = ledger
            .recover_with_limit(&ticket, io.clone(), Duration::from_secs(1))
            .await;
        assert!(recovered.is_some());
        assert_eq!(io.probes.load(Ordering::SeqCst), 2);
        assert_eq!((io.grants.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (2, 0));
    }

    #[tokio::test]
    async fn outstanding_grant_keeps_the_same_gate_across_manage_removal_and_readd() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(BlockingGrantIo {
            probes: AtomicUsize::new(0),
            grants: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            released: (Mutex::new(false), Condvar::new()),
        });
        let started = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while io.active.load(Ordering::SeqCst) == 0 {
                assert!(tokio::time::Instant::now() < deadline, "first grant never started");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let (first, ()) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(30)),
            started,
        );
        assert!(first.is_none());
        assert_eq!((io.probes.load(Ordering::SeqCst), io.grants.load(Ordering::SeqCst)), (1, 1));

        ledger.manage(Vec::new());
        ledger.manage(vec![KEY.into()]);
        let release = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(io.probes.load(Ordering::SeqCst), 1, "a new gate allowed the next probe to overlap");
            assert_eq!((io.grants.load(Ordering::SeqCst), io.active.load(Ordering::SeqCst)), (1, 1));
            io.release();
        };
        let (recovered, ()) = tokio::join!(
            ledger.recover_with_limit(&ticket, io.clone(), Duration::from_secs(1)),
            release,
        );
        assert!(recovered.is_some());
        assert_eq!((io.probes.load(Ordering::SeqCst), io.grants.load(Ordering::SeqCst)), (2, 2));
        assert_eq!(io.active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_new_round_cannot_erase_the_completed_round_seen_by_old_waiters() {
        let flight = Arc::new(Flight::default());
        let (first, first_leader) = flight.enter().unwrap();
        let (waiter, waiter_leader) = flight.enter().unwrap();
        assert!(first_leader && !waiter_leader && Arc::ptr_eq(&first, &waiter));
        let expected = Mark { fingerprint: fingerprint("new-token"), expires_at: now_ms() + 60_000 };
        let shared = SharedResult {
            expected,
            recovered: Recovered::from_token("new-token").unwrap(),
            binding: SlotBinding { id: "fixture-slot".into(), store: None },
        };
        let leader = FlightLeader { flight: flight.clone(), round: first, armed: true };
        assert!(leader.complete(Some(shared)).is_some());

        let (second, second_leader) = flight.enter().unwrap();
        assert!(second_leader);
        let preserved = waiter.result.lock().unwrap().clone();
        assert!(matches!(preserved, RoundResult::Complete(Some(_))));
        drop(FlightLeader { flight, round: second, armed: true });
    }

    #[tokio::test]
    async fn generation_stamp_and_chain_changes_close_recovery() {
        let (ledger, ticket, _) = ready("old-token", "new-token");
        let io = Arc::new(FakeIo {
            calls: AtomicUsize::new(0),
            grants: AtomicUsize::new(0),
            current: AtomicBool::new(true),
            token: "new-token".into(),
            delay: Duration::ZERO,
            allowed_grants: usize::MAX,
            logout_during_probe: false,
        });
        let mut changed = held("newer-token", now_ms() + 180_000, "old-token", ticket.expires_at);
        changed.generation = "fedcba9876543210fedcba9876543210".into();
        changed.lineage[0].generation = changed.generation.clone();
        ledger.observe(&[changed]);
        assert!(ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(100)).await.is_none());
        assert_eq!(io.calls.load(Ordering::SeqCst), 0);

        let (ledger, ticket, _) = ready("old-token", "new-token");
        ledger.begin(&crate::device_auth::Stamp::fixture_at(1));
        assert!(ledger.recover_with_limit(&ticket, io.clone(), Duration::from_millis(100)).await.is_none());

        let (ledger, ticket, _) = ready("old-token", "new-token");
        ledger.observe(&[Held { key: KEY.into(), state: "reconnect_required".into(), ..Held::default() }]);
        assert!(ledger.recover_with_limit(&ticket, io, Duration::from_millis(100)).await.is_none());
    }

    #[test]
    fn legacy_metadata_and_expired_lineage_never_issue_a_ticket() {
        let ledger = Ledger::default();
        ledger.begin(&crate::device_auth::Stamp::fixture());
        ledger.manage(vec![KEY.into()]);
        let mut legacy = held("new-token", now_ms() + 60_000, "old-token", now_ms() + 30_000);
        legacy.generation.clear();
        ledger.observe(&[legacy]);
        assert!(ledger.issue(&headers("old-token")).is_none());

        let expired = held("new-token", now_ms() + 60_000, "old-token", now_ms().saturating_sub(1));
        ledger.observe(&[expired]);
        assert!(ledger.issue(&headers("old-token")).is_none());

        let mut stale = held("new-token", now_ms() + 60_000, "old-token", now_ms() + 30_000);
        stale.updated = now_ms().saturating_sub(LINEAGE_TTL_MS + 1);
        ledger.observe(&[stale]);
        assert!(ledger.issue(&headers("old-token")).is_none());
    }

    #[test]
    fn missed_hint_keeps_lineage_for_two_pull_cycles_but_never_past_grace_or_original_expiry() {
        const MINUTE: u64 = 60_000;
        let rotated = 1_000_000;
        let old_expires = rotated + 60 * MINUTE;
        let new_expires = rotated + 8 * 60 * MINUTE;
        let mut chain = held("new-token", new_expires, "old-token", old_expires);
        chain.updated = rotated;

        let ledger = Ledger::default();
        ledger.begin(&crate::device_auth::Stamp::fixture());
        ledger.manage(vec![KEY.into()]);
        let after_missed_pull = rotated + 5 * MINUTE + 30_000;
        ledger.observe_at(&[chain.clone()], after_missed_pull);
        let ticket = ledger.issue_at(&headers("old-token"), after_missed_pull).unwrap();
        assert_eq!(ledger.check_at(&ticket, after_missed_pull), MemoryCheck::Ready(Mark {
            fingerprint: fingerprint("new-token"),
            expires_at: new_expires,
        }));

        let after_grace = rotated + LINEAGE_TTL_MS + 1;
        ledger.observe_at(&[chain], after_grace);
        assert!(ledger.issue_at(&headers("old-token"), after_grace).is_none());

        let expired_at = rotated + 5 * MINUTE;
        let mut expired = held("new-token", new_expires, "old-token", expired_at);
        expired.updated = rotated;
        ledger.observe_at(&[expired], after_missed_pull);
        assert!(ledger.issue_at(&headers("old-token"), after_missed_pull).is_none());
    }

    struct MemoryStores(Mutex<std::collections::HashMap<String, serde_json::Value>>);

    impl Stores for MemoryStores {
        fn read(&self, slot: &Slot) -> Read {
            self.0
                .lock()
                .unwrap()
                .get(&slot.id)
                .cloned()
                .map_or(Read::Absent, Read::Present)
        }

        fn same(&self, _: &Slot) -> bool {
            true
        }
    }

    struct SwitchingStores {
        doc: serde_json::Value,
        same: AtomicBool,
    }

    impl Stores for SwitchingStores {
        fn read(&self, _: &Slot) -> Read {
            self.same.store(false, Ordering::SeqCst);
            Read::Present(self.doc.clone())
        }

        fn same(&self, _: &Slot) -> bool {
            self.same.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn a_deleted_slot_cannot_recover_through_a_stale_map_or_the_current_identity() {
        let (_ledger, ticket, expected) = ready("old-token", "new-token");
        let stores = MemoryStores(Mutex::new(std::collections::HashMap::from([(
            "current".into(),
            serde_json::json!({"claudeAiOauth":{"accessToken":"new-token","expiresAt":expected.expires_at}}),
        )])));
        let slots = vec![Slot {
            id: "current".into(),
            store: None,
            table_key: Some(KEY.into()),
            bench: false, stamped: None,
        }];
        let stale = BTreeMap::from([("deleted".into(), KEY.into())]);
        assert!(matches!(probe_slots(&ticket, &expected, &slots, &stale, &stores), Probe::Stop));
    }

    #[test]
    fn slot_selection_changing_during_the_keychain_read_refuses_the_grant() {
        let (_ledger, ticket, expected) = ready("old-token", "new-token");
        let slot = Slot { id: "managed".into(), store: Some("fixture-store".into()), table_key: None, bench: false, stamped: None };
        let map = BTreeMap::from([(slot.id.clone(), KEY.into())]);
        let stores = SwitchingStores {
            doc: serde_json::json!({"claudeAiOauth":{"accessToken":"new-token","expiresAt":expected.expires_at}}),
            same: AtomicBool::new(true),
        };
        assert!(matches!(probe_slots(&ticket, &expected, &[slot], &map, &stores), Probe::Stop));
    }

    #[test]
    fn completed_grants_require_the_bound_slot_in_the_latest_settings_and_map() {
        let (_ledger, ticket, _) = ready("old-token", "new-token");
        let slot = Slot { id: "managed".into(), store: Some("fixture-store".into()), table_key: None, bench: false, stamped: None };
        let map = BTreeMap::from([(slot.id.clone(), KEY.into())]);
        let stores = MemoryStores(Mutex::new(std::collections::HashMap::new()));
        let binding = SlotBinding { id: slot.id.clone(), store: slot.store.clone() };
        assert!(binding_current(&ticket, &binding, &[slot], &map, &stores));
        assert!(!binding_current(&ticket, &binding, &[], &map, &stores));
        let stale = BTreeMap::from([("deleted".into(), KEY.into())]);
        let selected = Slot { id: "current".into(), store: None, table_key: Some(KEY.into()), bench: false, stamped: None };
        assert!(!binding_current(&ticket, &binding, &[selected], &stale, &stores));
    }

    #[test]
    fn completed_leader_result_is_refused_after_the_bound_store_changes() {
        let (_ledger, ticket, expected) = ready("old-token", "new-token");
        let slot = Slot { id: "managed".into(), store: Some("fixture-store".into()), table_key: None, bench: false, stamped: None };
        let slots = vec![slot];
        let map = BTreeMap::from([("managed".into(), KEY.into())]);
        let stores = MemoryStores(Mutex::new(std::collections::HashMap::from([(
            "managed".into(),
            serde_json::json!({"claudeAiOauth":{"accessToken":"new-token","expiresAt":expected.expires_at}}),
        )])));
        let binding = match probe_slots(&ticket, &expected, &slots, &map, &stores) {
            Probe::Found(_, binding) => binding,
            _ => panic!("leader did not find the synthetic token"),
        };
        assert!(grant_matches(&ticket, &binding, &expected, &slots, &map, &stores));

        stores.0.lock().unwrap().remove("managed");
        assert!(!grant_matches(&ticket, &binding, &expected, &slots, &map, &stores));

        stores.0.lock().unwrap().insert(
            "managed".into(),
            serde_json::json!({"claudeAiOauth":{"accessToken":"another-token","expiresAt":expected.expires_at}}),
        );
        assert!(!grant_matches(&ticket, &binding, &expected, &slots, &map, &stores));

        stores.0.lock().unwrap().insert(
            "managed".into(),
            serde_json::json!({"claudeAiOauth":{"accessToken":"new-token","refreshToken":"new-login","expiresAt":expected.expires_at}}),
        );
        assert!(!grant_matches(&ticket, &binding, &expected, &slots, &map, &stores));
    }
}
