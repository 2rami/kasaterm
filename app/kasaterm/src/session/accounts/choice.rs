//! 기기들이 같은 Claude 계정으로 모인다 — 관문 공통 설정의 순서 있는 선택(`kasa_mcp::account_sync::choice`,
//! docs/agent-account-selection.md). 슬롯 번호·별명은 기기마다 달라 선택은 공급자 UUID 쌍으로만 맞춘다.
//! 이메일·조직명은 관문 사슬을 찾고 화면에 보이는 힌트다.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use kasa_mcp::account_sync::choice;
use serde_json::Value;

use super::sync::{AccountIdentity, SyncNotice};
use crate::App;

/// 고른 계정이 어느 길로든 바뀌면 오른다. 자동 한도 전환은 판단을 시작할 때의 세대에 묶인다 — 그 사이 사람이
/// 고르거나 다른 기기를 따라 바뀌었으면 옛 사용량 응답으로 계정을 옮기지 않는다.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// 다른 기기를 따라 바꾼 시각(초). 떠나온 계정의 사용량이 아직 남아 있어 그 뒤 5분은 자동 전환을 쉰다.
static FOLLOWED_AT: AtomicU64 = AtomicU64::new(0);
const FOLLOW_REST_SECS: u64 = 300;
/// 신원 증명이 없는 슬롯이 있을 때 「이 기기에 없음」으로 단정하기 전에 기다리는 동기화 주기 수(15초 간격).
const WAIT_PASSES: u32 = 4;

pub(crate) fn selection_generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub(super) fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// 작업대에 지금 든 로그인의 증명된 신원과 그 관측 시각. 키체인 읽기는 자식 프로세스라 동기화 스레드(적용 훅)가
/// 재 두고, GUI 스레드의 수렴은 이 값만 본다.
struct BenchObservation {
    at: std::time::Instant,
    generation: u64,
    epoch: u64,
    identity: Option<AccountIdentity>,
}

impl BenchObservation {
    fn current(&self, generation: u64, epoch: u64) -> bool {
        self.at.elapsed() < BENCH_FRESH && self.generation == generation && self.epoch == epoch
    }
}

static BENCH: Mutex<Option<BenchObservation>> = Mutex::new(None);
const BENCH_FRESH: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) fn observe_workbench_identity() {
    let generation = selection_generation();
    let epoch = kasa_mcp::device_auth::change_epoch();
    let found = crate::claude_auth::verified_workbench_identity().and_then(AccountIdentity::from_verified);
    if let Ok(mut bench) = BENCH.lock() {
        *bench = Some(BenchObservation { at: std::time::Instant::now(), generation, epoch, identity: found });
    }
}

fn observed_bench() -> Option<AccountIdentity> {
    BENCH.lock().ok()?.as_ref()
        .filter(|observed| observed.current(selection_generation(), kasa_mcp::device_auth::change_epoch()))
        .and_then(|observed| observed.identity.clone())
}

pub(crate) fn followed_recently() -> bool {
    now_secs() < FOLLOWED_AT.load(Ordering::Acquire).saturating_add(FOLLOW_REST_SECS)
}

/// 신원 증명을 기다리는 이 기기의 선택. 증명이 오면 고를 때 본 revision 으로 낸다 — 기다리는 사이 다른 기기가 고른
/// 것이 있으면 관문이 이 선택을 낡은 것으로 거절한다. 고를 때의 KASA 계정·로그인 세대에 묶여, 그 사이 다른 KASA
/// 계정으로 로그인했으면 버린다.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    slot: String,
    seen: u64,
    generation: u64,
    account: String,
    epoch: u64,
}

impl Pending {
    fn current(&self, selected: &str, account: Option<&str>, epoch: u64, generation: u64) -> bool {
        self.generation == generation && self.slot == selected && account == Some(self.account.as_str()) && self.epoch == epoch
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Waiting(u32),
    /// 관문 계정 목록·사슬 관측을 기다린다(부팅 직후엔 아직 없다).
    Listing(u32),
    Adopting(String),
    /// 전환을 시도했다가 이 이유로 멈췄다. 사람이 풀어야 하는 이유면 같은 정본에서 다시 시도하지 않는다.
    Stopped(&'static str),
    Shown,
}

#[derive(Default)]
struct State {
    scope: Option<(String, u64)>,
    pending: Option<Pending>,
    attempt: Option<(u64, Step)>,
    notice: Option<SyncNotice>,
    /// 마지막으로 본 정본 revision — 새 정본이 오면 다른 계정의 지난 P2P 결과를 걷는다.
    seen_rev: Option<u64>,
}

impl State {
    fn align_scope(&mut self, account: Option<&str>, epoch: u64) {
        let scope = account.map(|account| (account.to_owned(), epoch));
        if self.scope != scope {
            // Revisions are account-local; a stopped attempt from another login can have the same number.
            *self = Self { scope, ..Self::default() };
        }
    }

    fn observe_revision(&mut self, rev: u64) -> (bool, bool) {
        let fresh = self.seen_rev.replace(rev) != Some(rev);
        let supersedes = fresh && self.pending.as_ref().is_none_or(|pending| rev > pending.seen);
        (fresh, supersedes)
    }
}

fn state() -> &'static Mutex<State> {
    static STATE: Mutex<State> = Mutex::new(State { scope: None, pending: None, attempt: None, notice: None, seen_rev: None });
    &STATE
}

fn set_step(rev: u64, step: Step) {
    if let Ok(mut state) = state().lock() {
        state.attempt = Some((rev, step));
    }
}

pub(super) fn notice() -> Option<SyncNotice> {
    let state = state().lock().ok()?;
    state.scope.as_ref().filter(|(_, epoch)| *epoch == kasa_mcp::device_auth::change_epoch())?;
    state.notice.clone()
}

fn show(summary: &str, details: String, failed: bool) {
    if let Ok(mut state) = state().lock() {
        state.notice = Some(SyncNotice { summary: summary.into(), details, failed, pending: !failed });
    }
}

fn settle() {
    if let Ok(mut state) = state().lock() {
        state.attempt = None;
        state.notice = None;
    }
}

fn paths() -> Option<kasa_mcp::account_sync::local::Paths> {
    crate::settings::account_sync_allowed().then(kasa_mcp::account_sync::local::Paths::current).flatten()
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Follow {
    Nothing,
    Switch(String),
    /// 고른 슬롯은 맞는데 작업대(실제로 도는 claude 가 쓰는 자리)의 신원을 아직 모른다.
    Confirm,
    Wait,
    Missing,
}

/// 정본 선택을 이 기기의 어느 슬롯이 따르나. 완료는 고른 슬롯의 금고와 작업대가 둘 다 그 UUID 일 때다 — 바깥
/// `/login` 이 작업대만 바꿨으면 같은 슬롯을 다시 골라 보존·검증을 탄다. 같은 UUID 의 슬롯이 여럿이면 같은 계정이라
/// 앞의 것을 고른다(별명은 기기마다 다르고 공유하지 않는다). 증명이 아직 없는 슬롯이 있으면 없다고 단정하지 않는다.
pub(super) fn follow(
    want: &AccountIdentity,
    selected: &str,
    slots: &[String],
    identity_of: impl Fn(&str) -> Option<AccountIdentity>,
    bench: Option<&AccountIdentity>,
) -> Follow {
    if !selected.is_empty() && identity_of(selected).as_ref() == Some(want) {
        return match bench {
            Some(bench) if bench == want => Follow::Nothing,
            Some(_) => Follow::Switch(selected.into()),
            None => Follow::Confirm,
        };
    }
    let mut unknown = false;
    for slot in slots.iter().filter(|slot| !slot.is_empty()) {
        match identity_of(slot) {
            Some(found) if &found == want => return Follow::Switch(slot.clone()),
            Some(_) => {}
            None => unknown = true,
        }
    }
    if unknown { Follow::Wait } else { Follow::Missing }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PeerRequest {
    Apply,
    /// 순서 있는 선택이 이미 있다 — 옛 P2P 요청을 그대로 적용하면 관문의 순서를 우회해 새 선택을 되돌릴 수 있다.
    Hint,
}

/// 다른 기기가 보낸 `claude-account-sync-v1`. 이 기기에 순서 있는 선택(답 받은 것이든 낼 것이든)이 있으면 그와
/// 같은 신원일 때만 적용하고, 아니면 동기화를 깨우는 힌트로만 쓴다. 선택이 없을 때(옛 관문)만 옛 동작이다.
pub(super) fn peer_request(local: Option<&Value>, requested: &AccountIdentity, waiting_for_proof: bool) -> PeerRequest {
    // A new local choice does not reach the shared key until its identity proof arrives.
    if waiting_for_proof { return PeerRequest::Hint; }
    match local.filter(|value| choice::valid(value)) {
        None => PeerRequest::Apply,
        Some(value) if AccountIdentity::from_choice(value).as_ref() == Some(requested) => PeerRequest::Apply,
        Some(_) => PeerRequest::Hint,
    }
}

/// 같은 정본에서 멈춘 전환을 다시 시도할까. 신원·보존 증명을 기다린 것이면 다시, 사람이 계정 화면에서 풀어야 하는
/// 것(바깥 로그인을 들일 슬롯 선택 등)이면 새 선택이 올 때까지 기다린다 — 15초마다 같은 안내를 다시 띄우지 않게.
fn retry_after(code: &str) -> bool {
    matches!(code, "account_sync_identity_unverified" | "account_sync_preservation_pending")
}

impl App {
    /// `claude-account-sync-v1` 을 그대로 적용해도 되나. 아니면 동기화를 깨우고 관문 순서로 맞춘다.
    pub(crate) fn peer_choice_applies(&self, requested: &AccountIdentity) -> bool {
        let settings = crate::socket::read_settings();
        let account = kasa_mcp::account_sync::current_account();
        let local = account.as_ref().and_then(|account| choice::owned(&settings, account));
        let waiting = state().lock().map_or(true, |state| state.pending.as_ref().is_some_and(|pending|
            pending.current(&self.set_claude_account, account.as_deref(), kasa_mcp::device_auth::change_epoch(), selection_generation())));
        let applies = peer_request(local, requested, waiting) == PeerRequest::Apply;
        if !applies {
            kasa_mcp::account_sync::poke();
        }
        applies
    }

    /// 사람이나 자동 전환이 고른 계정을 다른 기기에 알린다. 다른 기기를 따라 바꾼 것은 다시 내지 않는다.
    pub(super) fn record_claude_choice(&mut self, slot: &str, identity: Option<&AccountIdentity>) {
        let Some(paths) = paths() else { return };
        let Some(account) = kasa_mcp::account_sync::current_account() else { return };
        let seen = kasa_mcp::account_sync::seen_revision(&paths);
        let Ok(mut state) = state().lock() else { return };
        state.align_scope(Some(&account), kasa_mcp::device_auth::change_epoch());
        state.notice = None;
        state.attempt = None;
        state.pending = None;
        match identity {
            Some(identity) => {
                drop(state);
                publish(&paths, &account, seen, identity);
            }
            None => {
                state.pending = Some(Pending {
                    slot: slot.into(),
                    seen,
                    generation: selection_generation(),
                    account,
                    epoch: kasa_mcp::device_auth::change_epoch(),
                });
            }
        }
    }

    /// 동기화 주기마다(관문 응답을 적용한 뒤) 부른다. 증명을 기다리던 이 기기의 선택을 내고, 정본이 다른 계정이면
    /// 같은 UUID 의 슬롯으로 바꾸거나 관문 사슬을 새 슬롯에 받아 증명 뒤 고른다.
    pub(crate) fn converge_claude_choice(&mut self) {
        let Some(paths) = paths() else { return };
        let account = kasa_mcp::account_sync::current_account();
        if let Ok(mut state) = state().lock() {
            state.align_scope(account.as_deref(), kasa_mcp::device_auth::change_epoch());
        }
        let Some(account) = account else { return };
        let settings = crate::socket::read_settings();
        if let Some(value) = choice::owned(&settings, &account) {
            if let (Some(rev), Some(want)) = (choice::rev(value), AccountIdentity::from_choice(value)) {
                let (fresh, supersedes) = state().lock().map_or((false, false), |mut state| state.observe_revision(rev));
                // Invalidate old usage decisions even when following needs a new slot or an identity proof first.
                if supersedes { bump_generation(); }
                if fresh {
                    super::sync::drop_report_unless(|slot| crate::settings::account_sync_identity(slot).as_ref() == Some(&want));
                }
            }
        }
        self.publish_pending_choice(&paths, Some(&account));
        let settings = crate::socket::read_settings();
        let Some(value) = choice::owned(&settings, &account) else { return };
        let Some(rev) = choice::rev(value) else { return };
        if state().lock().map_or(true, |state| state.pending.is_some()) {
            return;
        }
        let Some(want) = AccountIdentity::from_choice(value) else { return };
        let step = state().lock().ok().and_then(|state| state.attempt.clone()).filter(|(at, _)| *at == rev).map(|(_, step)| step);
        // 받아 둔 슬롯이 있으면 그 상태가 먼저다 — 다른 슬롯의 신원 대기가 받은 계정 불일치·실패 안내를 덮지 않게.
        if let Some(Step::Adopting(slot)) = &step {
            return self.check_adopted(slot, &want, rev);
        }
        let slots: Vec<String> = self.set_claude_accounts.iter().map(|account| account.id.clone()).collect();
        let bench = observed_bench();
        let (email, org) = want.email_org();
        let name = display_name(email, org);
        match follow(&want, &self.set_claude_account, &slots, crate::settings::account_sync_identity, bench.as_ref()) {
            Follow::Nothing => settle(),
            Follow::Switch(slot) => {
                if !matches!(step, Some(Step::Stopped(code)) if !retry_after(code)) {
                    self.follow_claude_choice(&slot, &want, rev);
                }
            }
            Follow::Confirm => {
                crate::settings::request_account_sync_identity(&self.set_claude_account);
                show("다른 기기 계정 확인 중", format!("「{name}」 — 지금 쓰는 로그인의 신원을 확인하고 있어요"), false);
            }
            Follow::Wait if !matches!(step, Some(Step::Waiting(n)) if n >= WAIT_PASSES) => {
                for slot in &slots {
                    crate::settings::request_account_sync_identity(slot);
                }
                let n = match step { Some(Step::Waiting(n)) => n + 1, _ => 1 };
                set_step(rev, Step::Waiting(n));
                show("다른 기기 계정 확인 중", format!("「{name}」 — 이 기기 계정들의 신원을 확인하고 있어요"), false);
            }
            Follow::Wait | Follow::Missing => match step {
                Some(Step::Shown) => {}
                Some(Step::Listing(tries)) => self.prepare_claude_choice(&want, rev, tries),
                _ => self.prepare_claude_choice(&want, rev, 0),
            },
        }
    }

    fn publish_pending_choice(&mut self, paths: &kasa_mcp::account_sync::local::Paths, account: Option<&str>) {
        let Ok(mut state) = state().lock() else { return };
        let Some(pending) = state.pending.clone() else { return };
        if !pending.current(&self.set_claude_account, account, kasa_mcp::device_auth::change_epoch(), selection_generation()) {
            state.pending = None;
            return;
        }
        let Some(identity) = observed_bench() else {
            crate::settings::request_account_sync_identity(&pending.slot);
            return;
        };
        if crate::settings::account_sync_identity(&pending.slot).is_some_and(|slot| slot != identity) {
            return;
        }
        state.pending = None;
        drop(state);
        publish(paths, &pending.account, pending.seen, &identity);
    }

    fn follow_claude_choice(&mut self, slot: &str, want: &AccountIdentity, rev: u64) {
        self.account_switch_from_peer = true;
        let result = self.apply_claude_account_switch_checked(slot, Some(want));
        self.account_switch_from_peer = false;
        match result {
            Ok((_, to_label, restarted, deferred, focused, live)) => {
                FOLLOWED_AT.store(now_secs(), Ordering::Release);
                self.account_flash = Some(std::time::Instant::now());
                self.set_toast(format!(
                    "다른 기기를 따라 {}",
                    crate::session::account_switch_toast(&to_label, false, restarted, deferred, focused, live)
                ));
                settle();
                self.chrome_dirty = true;
            }
            Err(error) => {
                show("다른 기기 계정 따라가기 멈춤", format!("{}: {}", self.claude_account_display(slot), error.failure_message()),
                    !retry_after(error.failure_code()));
                set_step(rev, Step::Stopped(error.failure_code()));
            }
        }
    }

    fn check_adopted(&mut self, slot: &str, want: &AccountIdentity, rev: u64) {
        let (email, org) = want.email_org();
        let name = display_name(email, org);
        if !self.set_claude_accounts.iter().any(|account| account.id == slot) {
            show("다른 기기 계정 받기 멈춤", format!("「{name}」 — 받던 슬롯이 지워졌어요"), true);
            return set_step(rev, Step::Shown);
        }
        match crate::settings::account_sync_identity(slot) {
            Some(found) if &found == want => self.follow_claude_choice(slot, want, rev),
            Some(_) => {
                show("다른 기기 계정 받기 멈춤", format!("관문에서 받은 「{name}」 로그인이 다른 계정이에요 — 그 슬롯은 고르지 않았어요"), true);
                set_step(rev, Step::Shown);
            }
            None => {
                crate::settings::request_account_sync_identity(slot);
                show("다른 기기 계정 받는 중", format!("「{name}」 — 관문에서 받은 로그인의 신원을 확인하고 있어요"), false);
            }
        }
    }

    /// 같은 UUID 의 슬롯이 없다. 관문이 그 계정 사슬을 멀쩡히 쥐고 있으면 공식 받기 길로 새 슬롯을 준비한다 —
    /// 확인된 다른 계정 슬롯은 건드리지 않고, 받은 토큰의 UUID 가 맞아야 고른다(`check_adopted`).
    fn prepare_claude_choice(&mut self, want: &AccountIdentity, rev: u64, tries: u32) {
        if crate::settings::hidden_login_running() {
            return;
        }
        let (email, org) = want.email_org();
        let name = display_name(email, org);
        let key = kasa_mcp::agent_accounts::account_key("claude", email, org, "");
        let state = kasa_mcp::agent_chains::chain_state(&key);
        if state.as_deref() == Some("ok") {
            if let Some(slot) = self.adopt_held_claude_chain(&key) {
                show("다른 기기 계정 받는 중", format!("관문에서 「{name}」 로그인을 새 슬롯에 받아요 — 신원을 확인한 뒤 고릅니다"), false);
                return set_step(rev, Step::Adopting(slot));
            }
        }
        if state.as_deref() == Some("reconnect_required") {
            show("다른 기기 계정 로그인 필요", format!("「{name}」 관문 로그인이 끊겼어요 — 아무 기기에서 한 번 다시 로그인하면 이어져요"), true);
            return set_step(rev, Step::Shown);
        }
        // 관문 사슬·계정 목록은 각자 주기로 받아 온다. 막 켜졌으면 아직 없으니 몇 주기 더 본 뒤에 없다고 말한다.
        if tries + 1 < WAIT_PASSES {
            kasa_mcp::agent_chains::poke();
            kasa_mcp::agent_accounts::poke();
            show("다른 기기 계정 확인 중", format!("「{name}」 — 관문에서 그 계정 로그인을 찾고 있어요"), false);
            return set_step(rev, Step::Listing(tries + 1));
        }
        let details = if state.is_some() {
            format!("「{name}」 — 관문 계정 목록에서 찾지 못했어요. 이 기기에서 로그인해 주세요")
        } else {
            format!("「{name}」 — 이 기기에 같은 계정이 없고 관문 사슬도 없어요. 이 기기에서 로그인해 주세요")
        };
        show("다른 기기 계정 없음", details, true);
        set_step(rev, Step::Shown);
    }
}

fn display_name(email: &str, org: &str) -> String {
    if !org.is_empty() && !org.to_lowercase().contains(&email.to_lowercase()) { org.into() } else { email.into() }
}

fn publish(paths: &kasa_mcp::account_sync::local::Paths, account: &str, seen: u64, identity: &AccountIdentity) {
    let (account_uuid, organization_uuid) = identity.uuids();
    let (email, org) = identity.email_org();
    if let Err(error) = kasa_mcp::account_sync::choose_for(paths, account, seen, account_uuid, organization_uuid, email, org) {
        eprintln!("[account-choice] 선택을 적지 못했어요: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: &str = "00000000-0000-4000-8000-0000000000f0";

    fn who(account: &str, email: &str) -> AccountIdentity {
        AccountIdentity::from_verified(kasa_mcp::VerifiedClaudeIdentity {
            account_uuid: account.into(),
            organization_uuid: ORG.into(),
            email: email.into(),
            org: "Shown name".into(),
        })
        .unwrap()
    }

    const SIONIC: &str = "00000000-0000-4000-8000-0000000000a1";
    const GMAIL: &str = "00000000-0000-4000-8000-0000000000a2";
    const TEAM: &str = "00000000-0000-4000-8000-0000000000a3";

    #[test]
    fn following_matches_provider_uuids_never_labels_or_slot_numbers() {
        let sionic = who(SIONIC, "person@example.test");
        let gmail = who(GMAIL, "person@example.test");
        let slots: Vec<String> = ["acct-1", "acct-5", "acct-6"].map(String::from).to_vec();
        // The mini today: acct-1 and acct-6 are both Gmail (acct-6's label still names the Sionic account).
        let mini = |id: &str| match id {
            "acct-5" => Some(who(TEAM, "x@example.test")),
            "acct-1" | "acct-6" => Some(gmail.clone()),
            _ => None,
        };
        assert_eq!(follow(&sionic, "acct-6", &slots, mini, Some(&gmail)), Follow::Missing);
        assert_eq!(follow(&gmail, "acct-6", &slots, mini, Some(&gmail)), Follow::Nothing);
        assert_eq!(follow(&gmail, "acct-9", &slots, mini, None), Follow::Switch("acct-1".into()), "duplicates pick the first proven slot");
        let renamed = who(SIONIC, "renamed@example.test");
        let book = |id: &str| Some(if id == "acct-5" { renamed.clone() } else { gmail.clone() });
        assert_eq!(follow(&sionic, "", &slots, book, None), Follow::Switch("acct-5".into()), "a renamed email is the same account");
        assert_eq!(follow(&sionic, "acct-1", &slots, |id| (id == "acct-1").then(|| gmail.clone()), None), Follow::Wait);
    }

    #[test]
    fn an_outside_login_on_the_workbench_is_not_a_finished_follow() {
        let sionic = who(SIONIC, "person@example.test");
        let gmail = who(GMAIL, "person@example.test");
        let slots: Vec<String> = ["acct-5"].map(String::from).to_vec();
        let vault = |_: &str| Some(sionic.clone());
        assert_eq!(follow(&sionic, "acct-5", &slots, vault, Some(&sionic)), Follow::Nothing);
        assert_eq!(follow(&sionic, "acct-5", &slots, vault, Some(&gmail)), Follow::Switch("acct-5".into()),
            "the vault matched while Claude ran on another login");
        assert_eq!(follow(&sionic, "acct-5", &slots, vault, None), Follow::Confirm, "an unproven workbench was called done or adopted over");
        assert!(retry_after("account_sync_identity_unverified") && !retry_after("account_sync_workbench_changed"));
    }

    #[test]
    fn a_choice_waiting_for_proof_is_dropped_after_a_kasa_sign_in_or_selection_change() {
        let pending = Pending { slot: "acct-5".into(), seen: 7, generation: 9, account: "x".into(), epoch: 3 };
        assert!(pending.current("acct-5", Some("x"), 3, 9));
        assert!(!pending.current("acct-5", Some("y"), 3, 9), "a later KASA account would publish it");
        assert!(!pending.current("acct-5", None, 3, 9));
        assert!(!pending.current("acct-5", Some("x"), 4, 9), "signed out and back in");
        assert!(!pending.current("acct-6", Some("x"), 3, 9));
        assert!(!pending.current("acct-5", Some("x"), 3, 10), "the selection moved on while waiting");
    }

    #[test]
    fn a_late_v1_request_cannot_bypass_the_ordered_choice() {
        let sionic = who(SIONIC, "person@example.test");
        let gmail = who(GMAIL, "person@example.test");
        assert_eq!(peer_request(None, &sionic, false), PeerRequest::Apply, "an older relay keeps the legacy path");
        let canonical = choice::new_intent(GMAIL, ORG, "person@example.test", "Shown name", 12, &[]).map(|mut v| {
            v["rev"] = 13.into();
            v
        }).unwrap();
        assert_eq!(peer_request(Some(&canonical), &sionic, false), PeerRequest::Hint, "a delayed request undid the newer choice");
        assert_eq!(peer_request(Some(&canonical), &gmail, false), PeerRequest::Apply);
        assert_eq!(peer_request(Some(&canonical), &gmail, true), PeerRequest::Hint,
            "the old canonical choice cancelled a new local selection waiting for proof");
        assert_eq!(peer_request(None, &gmail, true), PeerRequest::Hint);
        let mine = choice::new_intent(SIONIC, ORG, "person@example.test", "Shown name", 13, &[]).unwrap();
        assert_eq!(peer_request(Some(&mine), &gmail, false), PeerRequest::Hint, "a request overrode this device's own unanswered choice");
    }

    #[test]
    fn workbench_observation_cannot_prove_a_later_selection_or_login() {
        let observed = BenchObservation { at: std::time::Instant::now(), generation: 4, epoch: 6,
            identity: Some(who(GMAIL, "old@example.test")) };
        assert!(observed.current(4, 6));
        assert!(!observed.current(5, 6), "a queued observation preceded the new selection");
        assert!(!observed.current(4, 7), "the KASA login changed while the observation was queued");
    }

    #[test]
    fn a_new_canonical_choice_invalidates_old_usage_before_following_finishes() {
        let pending = Pending { slot: "acct-5".into(), seen: 10, generation: 7, account: "x".into(), epoch: 3 };
        let mut state = State { pending: Some(pending), ..State::default() };
        assert_eq!(state.observe_revision(10), (true, false), "the old canonical value preceded the manual choice");
        assert_eq!(state.observe_revision(10), (false, false));
        assert_eq!(state.observe_revision(11), (true, true), "a later remote choice must invalidate old decisions before adoption finishes");
        assert_eq!(state.observe_revision(11), (false, false));
    }

    #[test]
    fn another_kasa_login_does_not_inherit_a_stopped_attempt_at_the_same_revision() {
        let mut state = State { scope: Some(("x".into(), 3)), seen_rev: Some(12),
            attempt: Some((12, Step::Stopped("account_sync_workbench_changed"))),
            pending: Some(Pending { slot: "acct-5".into(), seen: 12, generation: 7, account: "x".into(), epoch: 3 }),
            notice: Some(SyncNotice { summary: "old".into(), details: "old".into(), failed: true, pending: false }) };
        state.align_scope(Some("x"), 3);
        assert!(state.pending.is_some() && state.attempt.is_some());
        state.align_scope(Some("y"), 3);
        assert!(state.pending.is_none() && state.attempt.is_none() && state.notice.is_none() && state.seen_rev.is_none());
        state.attempt = Some((12, Step::Shown));
        state.align_scope(Some("y"), 4);
        assert!(state.attempt.is_none());
        state.align_scope(None, 5);
        assert!(state.scope.is_none());
    }

    #[test]
    fn a_follow_rests_the_usage_poller_and_moves_the_generation() {
        let before = selection_generation();
        bump_generation();
        assert_ne!(selection_generation(), before);
        FOLLOWED_AT.store(now_secs(), Ordering::Release);
        assert!(followed_recently());
        FOLLOWED_AT.store(0, Ordering::Release);
        assert!(!followed_recently());
    }
}
