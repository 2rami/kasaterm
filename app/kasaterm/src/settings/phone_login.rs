use super::*;
use crate::claude_auth::phone_credentials::{self, Snapshot, Stage, Staged};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(600);
const MAX_FLOWS: usize = 32;

#[derive(Clone)]
pub(crate) struct Ready(String);

impl std::fmt::Debug for Ready {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PhoneLoginReady([redacted])")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    #[serde(deserialize_with = "deserialize_nonce")]
    nonce: uuid::Uuid,
    machine_id: String,
}

fn deserialize_nonce<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<uuid::Uuid, D::Error> {
    let value = String::deserialize(deserializer)?;
    uuid::Uuid::parse_str(&value).map_err(|_| serde::de::Error::custom("invalid login nonce"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Starting,
    Authorizing,
    Code,
    Verifying,
    Confirm,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Authorizing => "awaiting_authorization",
            Self::Code => "awaiting_code",
            Self::Verifying => "verifying",
            Self::Confirm => "needs_confirmation",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Expired
        )
    }
}

#[derive(Clone)]
struct Binding {
    slot: String,
    machine: String,
    label: String,
    email: String,
    org: String,
    identity: Option<crate::session::AccountIdentity>,
}

struct State {
    id: String,
    nonce: uuid::Uuid,
    binding: Binding,
    deadline: Instant,
    expires_at_ms: u64,
    phase: Phase,
    error: Option<&'static str>,
    authorization_url: Option<String>,
    confirmation: Option<String>,
    confirmation_accepted: bool,
    actual: Option<kasa_mcp::VerifiedClaudeIdentity>,
    runtime_updated: bool,
    runtime_error: Option<&'static str>,
    workers: usize,
    cleanup_pending: bool,
}

impl State {
    fn new(
        id: String,
        request: &StartRequest,
        binding: Binding,
        now: Instant,
        expires_at_ms: u64,
    ) -> Self {
        Self {
            id,
            nonce: request.nonce,
            binding,
            deadline: now + TTL,
            expires_at_ms,
            phase: Phase::Starting,
            error: None,
            authorization_url: None,
            confirmation: None,
            confirmation_accepted: false,
            actual: None,
            runtime_updated: false,
            runtime_error: None,
            workers: 1,
            cleanup_pending: false,
        }
    }

    fn view(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "flow_id":self.id,"slot_id":self.binding.slot,"machine_id":self.binding.machine,
            "label":self.binding.label,"email":self.actual.as_ref().map_or(self.binding.email.as_str(), |i| &i.email),
            "org":self.actual.as_ref().map_or(self.binding.org.as_str(), |i| &i.org),
            "state":self.phase.name(),"expires_at_ms":self.expires_at_ms,
        });
        if matches!(self.phase, Phase::Authorizing | Phase::Code) {
            if let Some(url) = &self.authorization_url {
                value["authorization_url"] = url.clone().into();
            }
        }
        if let Some(error) = self.error {
            value["error_code"] = error.into();
        }
        if let Some(actual) = &self.actual {
            value["account_uuid"] = actual.account_uuid.clone().into();
            value["organization_uuid"] = actual.organization_uuid.clone().into();
        }
        if self.phase == Phase::Confirm {
            value["expected_email"] = self.binding.email.clone().into();
            value["expected_org"] = self.binding.org.clone().into();
            value["confirmation_nonce"] = self.confirmation.clone().unwrap_or_default().into();
        }
        if self.phase == Phase::Succeeded {
            value["runtime_updated"] = self.runtime_updated.into();
            if let Some(error) = self.runtime_error {
                value["runtime_error_code"] = error.into();
            }
        }
        value
    }

    fn finish(&mut self, phase: Phase, error: Option<&'static str>) {
        if !self.phase.terminal() {
            self.cleanup_pending = true;
        }
        self.phase = phase;
        self.error = error;
        self.authorization_url = None;
        self.confirmation = None;
    }

    fn lease_releasable(&self) -> bool {
        self.phase.terminal() && self.workers == 0 && !self.cleanup_pending
    }

    fn worker_finished(&mut self) -> bool {
        self.workers = self.workers.saturating_sub(1);
        self.lease_releasable()
    }

    fn cleanup_finished(&mut self) -> bool {
        self.cleanup_pending = false;
        self.lease_releasable()
    }

    fn check(
        &mut self,
        current: Result<&Binding, &'static str>,
        owner_current: bool,
        lease_owned: bool,
        now: Instant,
    ) -> Result<(), &'static str> {
        let error = match current {
            Ok(current) if !same_binding(&self.binding, current) => {
                Some("phone_login_target_changed")
            }
            Err(error) => Some(error),
            _ if !owner_current => Some("phone_login_owner_changed"),
            _ => None,
        };
        if let Some(error) = error {
            self.finish(Phase::Failed, Some(error));
            return Err(error);
        }
        self.check_liveness(owner_current, lease_owned, now);
        Ok(())
    }

    fn check_liveness(&mut self, owner_current: bool, lease_owned: bool, now: Instant) {
        if self.phase.terminal() {
            return;
        }
        if now >= self.deadline {
            self.finish(Phase::Expired, Some("phone_login_expired"));
        } else if !owner_current {
            self.finish(Phase::Failed, Some("phone_login_owner_changed"));
        } else if !lease_owned {
            self.finish(Phase::Cancelled, Some("phone_login_cancelled"));
        }
    }

    fn action(
        &mut self,
        action: Action<'_>,
        input: Option<&mut dyn Write>,
    ) -> Result<Effect, &'static str> {
        match action {
            Action::Status => {}
            Action::Cancel => {
                if !self.phase.terminal() {
                    self.finish(Phase::Cancelled, None);
                }
            }
            Action::Code(code) => {
                if self.phase == Phase::Verifying {
                    return Ok(Effect::None);
                }
                if !matches!(self.phase, Phase::Authorizing | Phase::Code) {
                    return Err("phone_login_not_waiting");
                }
                let code = code.trim();
                if code.len() > 2048 || !super::looks_like_login_code(code) {
                    return Err("phone_login_invalid_code");
                }
                let input = input.ok_or("phone_login_not_waiting")?;
                if input
                    .write_all(code.as_bytes())
                    .and_then(|_| input.write_all(b"\n"))
                    .and_then(|_| input.flush())
                    .is_err()
                {
                    self.finish(Phase::Failed, Some("phone_login_cli_failed"));
                } else {
                    self.phase = Phase::Verifying;
                    self.authorization_url = None;
                }
            }
            Action::Confirm(nonce) => {
                if self.phase != Phase::Confirm || self.confirmation.as_deref() != Some(nonce) {
                    return Err("phone_login_confirmation_invalid");
                }
                self.confirmation = None;
                self.confirmation_accepted = true;
                self.phase = Phase::Verifying;
                self.workers += 1;
                return Ok(Effect::VerifyConfirmed);
            }
        }
        Ok(Effect::None)
    }

    fn verified(&mut self, actual: kasa_mcp::VerifiedClaudeIdentity, confirmed: bool) -> Effect {
        if self.phase.terminal() {
            return Effect::None;
        }
        if confirmed
            && (!self.confirmation_accepted
                || self.actual.as_ref().is_none_or(|before| {
                    before.account_uuid != actual.account_uuid
                        || before.organization_uuid != actual.organization_uuid
                }))
        {
            self.finish(Phase::Failed, Some("phone_login_identity_mismatch"));
            return Effect::None;
        }
        let decision = identity_decision(&self.binding, &actual);
        self.actual = Some(actual);
        match decision {
            IdentityDecision::Reject => {
                self.finish(Phase::Failed, Some("phone_login_identity_mismatch"))
            }
            IdentityDecision::Confirm if !confirmed => {
                self.phase = Phase::Confirm;
                self.confirmation = Some(uuid::Uuid::new_v4().to_string());
            }
            _ => {
                self.phase = Phase::Verifying;
                return Effect::Ready;
            }
        }
        Effect::None
    }

    fn output(&mut self, output: Output) {
        if self.phase.terminal() {
            return;
        }
        match output {
            Output::Url(url)
                if matches!(
                    self.phase,
                    Phase::Starting | Phase::Authorizing | Phase::Code
                ) =>
            {
                self.authorization_url = Some(url);
                self.phase = Phase::Authorizing;
            }
            Output::Prompt if self.phase == Phase::Authorizing => self.phase = Phase::Code,
            Output::Overflow => self.finish(Phase::Failed, Some("phone_login_output_limit")),
            Output::InvalidUrl => {
                self.finish(Phase::Failed, Some("phone_login_authorization_url_invalid"))
            }
            _ => {}
        }
    }

    fn commit(
        &mut self,
        current: Result<&Binding, &'static str>,
        owner_current: bool,
        lease_owned: bool,
        now: Instant,
        actual: Option<kasa_mcp::VerifiedClaudeIdentity>,
        selected: bool,
        promote: impl FnOnce() -> Result<bool, &'static str>,
    ) -> Option<kasa_mcp::VerifiedClaudeIdentity> {
        if self.phase != Phase::Verifying {
            return None;
        }
        if self
            .check(current, owner_current, lease_owned, now)
            .is_err()
            || self.phase.terminal()
        {
            return None;
        }
        let Some(actual) = actual else {
            self.finish(Phase::Failed, Some("phone_login_identity_unverified"));
            return None;
        };
        if self.actual.as_ref().is_none_or(|before| {
            before.account_uuid != actual.account_uuid
                || before.organization_uuid != actual.organization_uuid
        }) {
            self.finish(Phase::Failed, Some("phone_login_identity_mismatch"));
            return None;
        }
        let decision = identity_decision(current.expect("binding checked"), &actual);
        if decision == IdentityDecision::Reject
            || (decision == IdentityDecision::Confirm && !self.confirmation_accepted)
        {
            self.finish(Phase::Failed, Some("phone_login_identity_mismatch"));
            return None;
        }
        match promote() {
            Ok(updated) => {
                self.runtime_updated = updated;
                self.runtime_error = (selected && !updated).then_some("runtime_update_pending");
                self.binding.email = actual.email.clone();
                self.binding.org = actual.org.clone();
                self.binding.identity =
                    crate::session::AccountIdentity::from_verified(actual.clone());
                self.actual = Some(actual.clone());
                self.finish(Phase::Succeeded, None);
                Some(actual)
            }
            Err(error) => {
                self.finish(Phase::Failed, Some(error));
                None
            }
        }
    }
}

enum Action<'a> {
    Status,
    Cancel,
    Code(&'a str),
    Confirm(&'a str),
}

impl<'a> Action<'a> {
    fn parse(action: &str, argument: &'a str) -> Result<Self, &'static str> {
        Ok(match action {
            "claude-phone-login-status" => Self::Status,
            "claude-phone-login-cancel" => Self::Cancel,
            "claude-phone-login-code" => Self::Code(argument),
            "claude-phone-login-confirm" => Self::Confirm(argument),
            _ => return Err("phone_login_action_unknown"),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Effect {
    None,
    VerifyConfirmed,
    Ready,
}

fn replay_id<'a>(
    states: impl Iterator<Item = &'a State>,
    request: &StartRequest,
    binding: &Binding,
    owner_current: impl FnOnce(&str) -> bool,
) -> Result<Option<String>, &'static str> {
    let Some(state) = states
        .into_iter()
        .find(|state| state.nonce == request.nonce)
    else {
        return Ok(None);
    };
    if !same_binding(&state.binding, binding) {
        return Err("phone_login_target_changed");
    }
    if !owner_current(&state.id) {
        return Err("phone_login_owner_changed");
    }
    Ok(Some(state.id.clone()))
}

fn can_start<'a>(
    states: impl Iterator<Item = &'a State>,
    local_busy: bool,
) -> Result<(), &'static str> {
    let states: Vec<_> = states.collect();
    if states.len() >= MAX_FLOWS {
        return Err("phone_login_too_many_attempts");
    }
    if local_busy
        || states
            .iter()
            .any(|state| !state.phase.terminal() || !state.lease_releasable())
    {
        return Err("phone_login_busy");
    }
    Ok(())
}

struct Flow {
    state: State,
    owner: kasa_mcp::device_auth::Stamp,
    cancel: Arc<AtomicBool>,
    input: Option<std::process::ChildStdin>,
    stage: Option<Arc<Stage>>,
    snapshot: Option<Snapshot>,
    staged: Option<Staged>,
}

impl std::ops::Deref for Flow {
    type Target = State;
    fn deref(&self) -> &State {
        &self.state
    }
}

impl std::ops::DerefMut for Flow {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.state
    }
}

impl Flow {
    fn finish(&mut self, phase: Phase, error: Option<&'static str>) {
        self.state.finish(phase, error);
        self.clean_terminal();
    }

    fn clean_terminal(&mut self) {
        if !self.phase.terminal() || self.cancel.swap(true, Ordering::AcqRel) {
            return;
        }
        self.input = None;
        let resources = (self.stage.take(), self.staged.take(), self.snapshot.take());
        let id = self.id.clone();
        // Keychain cleanup belongs off the GUI thread, including cancellation after CLI exit.
        std::thread::spawn(move || {
            drop(resources);
            if let Ok(mut state) = flows().lock() {
                if let Some(flow) = state.by_id.get_mut(&id) {
                    if flow.cleanup_finished() {
                        release_login_lease(&id);
                    }
                }
            }
        });
    }
}

#[derive(Default)]
struct Flows {
    by_id: HashMap<String, Flow>,
}

fn flows() -> &'static Mutex<Flows> {
    static FLOWS: OnceLock<Mutex<Flows>> = OnceLock::new();
    FLOWS.get_or_init(Default::default)
}

fn lease_id(flow: &str) -> String {
    format!("phone-login-{flow}")
}

fn release_login_lease(flow: &str) {
    if let Ok(mut cell) = super::login_cell().lock() {
        release_login_lease_in(&mut cell, flow);
    }
}

fn release_login_lease_in(
    cell: &mut (
        Option<LoginJob>,
        Option<u32>,
        Option<std::process::ChildStdin>,
    ),
    flow: &str,
) {
    if cell.0.as_ref().is_some_and(|job| job.id == lease_id(flow)) {
        *cell = (None, None, None);
    }
}

fn owns_login_lease(flow: &str) -> bool {
    super::login_cell()
        .lock()
        .is_ok_and(|cell| cell.0.as_ref().is_some_and(|job| job.id == lease_id(flow)))
}

fn detach_process(
    cell: &mut (
        Option<LoginJob>,
        Option<u32>,
        Option<std::process::ChildStdin>,
    ),
    flow: &str,
) {
    if cell.0.as_ref().is_some_and(|job| job.id == lease_id(flow)) {
        cell.1 = None;
        cell.2 = None;
    }
}

fn clear_process_lease(flow: &str) {
    if let Ok(mut cell) = super::login_cell().lock() {
        detach_process(&mut cell, flow);
    }
}

struct CliChild {
    inner: std::process::Child,
    flow: String,
    reaped: bool,
    /// Windows 에는 프로세스 묶음 죽이기가 없어, CLI 가 띄운 자식까지 잡을 작업 객체로 묶는다.
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl CliChild {
    fn stop(&mut self) {
        if !self.reaped {
            #[cfg(windows)]
            if let Some(job) = &self.job {
                job.terminate();
            }
            stop_child(&mut self.inner);
            self.reaped = true;
        }
        clear_process_lease(&self.flow);
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = match self.inner.try_wait() {
            Ok(status) => status,
            Err(error) => {
                #[cfg(unix)]
                let gone = error.raw_os_error() == Some(libc::ECHILD);
                #[cfg(not(unix))]
                let gone = false;
                if gone {
                    self.reaped = true;
                    clear_process_lease(&self.flow);
                }
                return Err(error);
            }
        };
        if status.is_some() {
            self.reaped = true;
            clear_process_lease(&self.flow);
        }
        Ok(status)
    }
}

impl Drop for CliChild {
    fn drop(&mut self) {
        self.stop();
    }
}

fn allowed() -> bool {
    !cfg!(test)
        && !crate::version::isolated_environment(|key| std::env::var_os(key).is_some())
        && ![
            "KASATERM_DEVICE_FILE",
            "KASATERM_MACHINES_FILE",
            "KASATERM_WINDOW_SIZE",
            "KASATERM_WINDOW_POS",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "phone_login_busy" => "다른 로그인이 진행 중이에요. 끝내거나 취소한 뒤 다시 시작해 주세요",
        "phone_login_target_changed" => "대상 계정이 바뀌었어요. 계정 목록을 새로 확인해 주세요",
        "phone_login_owner_changed" => "기기 로그인이 바뀌었어요. 기기를 다시 연결해 주세요",
        "phone_login_machine_mismatch" => "다른 기기에 연결됐어요. 기기 목록을 다시 확인해 주세요",
        "phone_login_unsupported" => "이 환경에서는 폰 로그인을 지원하지 않아요",
        _ => "로그인 요청을 확인할 수 없어요. 현재 화면에서 다시 시작해 주세요",
    }
}

fn request_error(code: &'static str) -> &'static str {
    code
}

fn storage_error(code: &str) -> &'static str {
    match code {
        "credential_target_changed" => "phone_login_target_changed",
        "staging_credentials_changed" => "phone_login_stage_changed",
        "credential_identity_unverified" => "phone_login_identity_unverified",
        _ => "phone_login_storage_failed",
    }
}

fn same_binding(before: &Binding, current: &Binding) -> bool {
    before.slot == current.slot
        && before.machine == current.machine
        && before.label == current.label
        && before.email == current.email
        && before.org == current.org
        && before
            .identity
            .as_ref()
            .is_none_or(|old| current.identity.as_ref().is_none_or(|new| old == new))
}

#[derive(Debug, PartialEq, Eq)]
enum IdentityDecision {
    Accept,
    Confirm,
    Reject,
}

fn parse_start(argument: &str) -> Result<StartRequest, &'static str> {
    if argument.len() > 2048 {
        return Err("phone_login_invalid_start");
    }
    let request: StartRequest =
        serde_json::from_str(argument).map_err(|_| "phone_login_invalid_start")?;
    if request.nonce.is_nil() || request.machine_id.is_empty() {
        return Err("phone_login_invalid_start");
    }
    Ok(request)
}

fn identity_decision(
    expected: &Binding,
    actual: &kasa_mcp::VerifiedClaudeIdentity,
) -> IdentityDecision {
    if let Some(identity) = &expected.identity {
        return if identity.matches_verified(actual.clone()) {
            IdentityDecision::Accept
        } else {
            IdentityDecision::Reject
        };
    }
    if (!expected.email.is_empty() && expected.email != actual.email)
        || (!expected.org.is_empty() && expected.org != actual.org)
    {
        return IdentityDecision::Reject;
    }
    IdentityDecision::Confirm
}

impl App {
    fn phone_login_binding(&self, slot: &str) -> Result<Binding, &'static str> {
        let account = self
            .set_claude_accounts
            .iter()
            .find(|account| account.id == slot && !slot.is_empty())
            .ok_or("phone_login_target_changed")?;
        let machine = kasa_mcp::mobile::machine_identity()
            .filter(|id| !id.is_empty())
            .ok_or("phone_login_machine_mismatch")?;
        let (email, org) = super::remembered_identity(slot);
        Ok(Binding {
            slot: slot.into(),
            machine,
            label: account.label.clone(),
            email,
            org,
            identity: super::account_sync_identity(slot),
        })
    }

    pub(crate) fn phone_login_action(
        &mut self,
        action: &str,
        id: &str,
        argument: &str,
    ) -> Result<serde_json::Value, &'static str> {
        if !allowed() {
            return Err(request_error("phone_login_unsupported"));
        }
        if action == "claude-phone-login-start" {
            return self.start_phone_login(id, argument);
        }
        let mut state = flows()
            .lock()
            .map_err(|_| request_error("phone_login_busy"))?;
        let flow = state
            .by_id
            .get_mut(id)
            .ok_or_else(|| request_error("phone_login_unknown"))?;
        let current = self.phone_login_binding(&flow.binding.slot);
        let owner_current = kasa_mcp::device_auth::stamp_is_current(&flow.owner);
        let checked = flow.state.check(
            current.as_ref().map_err(|error| *error),
            owner_current,
            owns_login_lease(id),
            Instant::now(),
        );
        flow.clean_terminal();
        checked?;
        let effect = flow.state.action(
            Action::parse(action, argument)?,
            flow.input.as_mut().map(|input| input as &mut dyn Write),
        );
        flow.clean_terminal();
        if effect? == Effect::VerifyConfirmed {
            let Some(stage) = flow.stage.clone() else {
                flow.workers = flow.workers.saturating_sub(1);
                flow.finish(Phase::Failed, Some("phone_login_storage_failed"));
                return Err("phone_login_storage_failed");
            };
            let id = id.to_string();
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                let worker = WorkerGuard {
                    id: id.clone(),
                    stage: Some(stage),
                };
                if let Some(stage) = worker.stage.as_ref() {
                    verify_stage(&id, stage, &proxy, true);
                }
            });
        }
        Ok(serde_json::json!({"ok":true,"phone_login":flow.view()}))
    }

    fn start_phone_login(
        &mut self,
        slot: &str,
        argument: &str,
    ) -> Result<serde_json::Value, &'static str> {
        let request = parse_start(argument)?;
        let binding = self.phone_login_binding(slot).map_err(request_error)?;
        if request.machine_id != binding.machine {
            return Err(request_error("phone_login_machine_mismatch"));
        }
        let owner = kasa_mcp::device_auth::current_stamp()
            .ok_or_else(|| request_error("phone_login_owner_changed"))?;
        let mut state = flows()
            .lock()
            .map_err(|_| request_error("phone_login_busy"))?;
        let now = Instant::now();
        for flow in state.by_id.values_mut() {
            if !flow.phase.terminal() && now >= flow.deadline {
                flow.finish(Phase::Expired, Some("phone_login_expired"));
            }
        }
        if let Some(id) = replay_id(
            state.by_id.values().map(|flow| &flow.state),
            &request,
            &binding,
            |id| kasa_mcp::device_auth::stamp_is_current(&state.by_id[id].owner),
        )? {
            let flow = &state.by_id[&id];
            return Ok(serde_json::json!({"ok":true,"phone_login":flow.view()}));
        }
        state
            .by_id
            .retain(|_, flow| now < flow.deadline + TTL || !flow.lease_releasable());
        let mut local = super::login_cell()
            .lock()
            .map_err(|_| request_error("phone_login_busy"))?;
        can_start(
            state.by_id.values().map(|flow| &flow.state),
            local
                .0
                .as_ref()
                .is_some_and(|job| matches!(job.state, LoginState::Running | LoginState::NeedCode)),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        local.0 = Some(LoginJob {
            provider: AccountProvider::Claude,
            id: lease_id(&id),
            state: LoginState::Running,
            device: None,
            adopting: None,
        });
        local.1 = None;
        local.2 = None;
        drop(local);
        let flow = Flow {
            state: State::new(
                id.clone(),
                &request,
                binding,
                now,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .saturating_add(TTL.as_millis()) as u64,
            ),
            owner,
            cancel: Arc::new(AtomicBool::new(false)),
            input: None,
            stage: None,
            snapshot: None,
            staged: None,
        };
        let response = serde_json::json!({"ok":true,"phone_login":flow.view()});
        state.by_id.insert(id.clone(), flow);
        let proxy = self.proxy.clone();
        let worker_id = id.clone();
        std::thread::spawn(move || {
            let _worker = WorkerGuard {
                id: worker_id.clone(),
                stage: None,
            };
            run_login(&worker_id, &proxy);
        });
        std::thread::spawn(move || watch_deadline(&id));
        Ok(response)
    }

    pub(crate) fn apply_phone_login_ready(&mut self, ready: &Ready) {
        let Ok(mut state) = flows().lock() else {
            return;
        };
        let Some(flow) = state.by_id.get_mut(&ready.0) else {
            return;
        };
        if flow.phase != Phase::Verifying {
            return;
        }
        let current = self.phone_login_binding(&flow.binding.slot);
        let (Some(snapshot), Some(staged)) = (flow.snapshot.clone(), flow.staged.clone()) else {
            flow.finish(Phase::Failed, Some("phone_login_storage_failed"));
            return;
        };
        let Some(target) = socket::claude_account_dir(&flow.binding.slot) else {
            flow.finish(Phase::Failed, Some("phone_login_target_changed"));
            return;
        };
        let selected = self.set_claude_account == flow.binding.slot;
        let slot = flow.binding.slot.clone();
        let owner_current = kasa_mcp::device_auth::stamp_is_current(&flow.owner);
        let lease_owned = owns_login_lease(&flow.id);
        let committed = flow.state.commit(
            current.as_ref().map_err(|error| *error),
            owner_current,
            lease_owned,
            Instant::now(),
            staged.identity(),
            selected,
            || {
                kasa_mcp::device_auth::with_current(&flow.owner, || {
                    phone_credentials::promote(&target, &snapshot, &staged)
                        .map_err(str::to_string)?;
                    Ok(if selected {
                        phone_credentials::refresh_active(&slot, &staged).unwrap_or(false)
                    } else {
                        false
                    })
                })
                .map_err(|error| {
                    if kasa_mcp::device_auth::stamp_is_current(&flow.owner) {
                        storage_error(&error)
                    } else {
                        "phone_login_owner_changed"
                    }
                })
            },
        );
        if let Some(actual) = committed {
            super::remember_account_identity(&slot, &actual.email, &actual.org);
            super::remember_account_sync_identity(
                &slot,
                crate::session::AccountIdentity::from_verified(actual),
            );
            super::finish_login(AccountProvider::Claude, &slot, LoginState::Ok);
            self.account_flash = Some(Instant::now());
            self.set_toast(if selected && !flow.runtime_updated {
                "폰 로그인을 계정 슬롯에 저장했어요. 현재 작업대는 보존했으니 계정을 다시 선택해 주세요"
            } else { "폰 로그인이 완료됐어요" }.into());
        }
        flow.clean_terminal();
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn fail(id: &str, code: &'static str) {
    if let Ok(mut state) = flows().lock() {
        if let Some(flow) = state
            .by_id
            .get_mut(id)
            .filter(|flow| !flow.phase.terminal())
        {
            flow.finish(Phase::Failed, Some(code));
        }
    }
}

struct WorkerGuard {
    id: String,
    stage: Option<Arc<Stage>>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        // The last Stage drop can run keychain cleanup, including when verification unwinds.
        drop(self.stage.take());
        if let Ok(mut state) = flows().lock() {
            if let Some(flow) = state.by_id.get_mut(&self.id) {
                if flow.worker_finished() {
                    release_login_lease(&self.id);
                }
                return;
            }
            release_login_lease(&self.id);
        }
    }
}

pub(super) fn cancel_from_local() -> bool {
    let Ok(mut state) = flows().lock() else {
        return false;
    };
    let Some(flow) = state
        .by_id
        .values_mut()
        .find(|flow| owns_login_lease(&flow.id))
    else {
        return false;
    };
    if !flow.phase.terminal() {
        flow.finish(Phase::Cancelled, Some("phone_login_cancelled"));
    }
    true
}

fn still_current(id: &str) -> bool {
    flows().lock().is_ok_and(|state| {
        state.by_id.get(id).is_some_and(|flow| {
            !flow.phase.terminal()
                && !flow.cancel.load(Ordering::Acquire)
                && Instant::now() < flow.deadline
                && kasa_mcp::device_auth::stamp_is_current(&flow.owner)
                && owns_login_lease(id)
        })
    })
}

fn watch_deadline(id: &str) {
    loop {
        {
            let Ok(mut state) = flows().lock() else {
                return;
            };
            let Some(flow) = state.by_id.get_mut(id) else {
                return;
            };
            if flow.phase.terminal() {
                return;
            }
            let owner_current = kasa_mcp::device_auth::stamp_is_current(&flow.owner);
            flow.state
                .check_liveness(owner_current, owns_login_lease(id), Instant::now());
            flow.clean_terminal();
            if flow.phase.terminal() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn verify_stage(
    id: &str,
    stage: &Stage,
    proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    confirmed: bool,
) {
    if !still_current(id) {
        return;
    }
    let _ = super::slot_identity_full(Some(stage.storage_dir()));
    let staged = match phone_credentials::read_staged(stage) {
        Ok(staged) => staged,
        Err(_) => {
            fail(id, "phone_login_storage_failed");
            return;
        }
    };
    let Some(actual) = staged.identity() else {
        fail(id, "phone_login_identity_unverified");
        return;
    };
    let Ok(mut state) = flows().lock() else {
        return;
    };
    let Some(flow) = state
        .by_id
        .get_mut(id)
        .filter(|flow| !flow.phase.terminal())
    else {
        return;
    };
    let owner_current = kasa_mcp::device_auth::stamp_is_current(&flow.owner);
    flow.state
        .check_liveness(owner_current, owns_login_lease(id), Instant::now());
    if flow.phase.terminal() {
        flow.clean_terminal();
        return;
    }
    flow.staged = Some(staged);
    if flow.state.verified(actual, confirmed) == Effect::Ready {
        let _ = proxy.send_event(UserEvent::PhoneLoginReady(Ready(id.into())));
    }
    flow.clean_terminal();
}

fn run_login(id: &str, proxy: &winit::event_loop::EventLoopProxy<UserEvent>) {
    let (slot, email) = {
        let Ok(state) = flows().lock() else { return };
        let Some(flow) = state.by_id.get(id) else {
            return;
        };
        (flow.binding.slot.clone(), flow.binding.email.clone())
    };
    let Some(target) = socket::claude_account_dir(&slot) else {
        fail(id, "phone_login_target_changed");
        return;
    };
    let snapshot = match phone_credentials::snapshot(&target) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            fail(id, "phone_login_storage_failed");
            return;
        }
    };
    let stage = match Stage::create() {
        Ok(stage) => Arc::new(stage),
        Err(_) => {
            fail(id, "phone_login_storage_failed");
            return;
        }
    };
    if !still_current(id) {
        return;
    }
    let mut command = crate::proc::command(kasa_mcp::claude_bin());
    command.args(["auth", "login", "--claudeai"]);
    if !email.is_empty() {
        command.args(["--email", &email]);
    }
    isolate_command(&mut command, stage.root(), stage.storage_dir());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            fail(id, "phone_login_cli_failed");
            return;
        }
    };
    let mut child = CliChild {
        #[cfg(windows)]
        job: job::Job::holding(&child),
        inner: child,
        flow: id.into(),
        reaped: false,
    };
    {
        let Ok(mut state) = flows().lock() else {
            return;
        };
        let Some(flow) = state
            .by_id
            .get_mut(id)
            .filter(|flow| !flow.phase.terminal())
        else {
            return;
        };
        flow.input = child.inner.stdin.take();
        flow.stage = Some(stage.clone());
        flow.snapshot = Some(snapshot);
        if let Ok(mut cell) = super::login_cell().lock() {
            if cell.0.as_ref().is_some_and(|job| job.id == lease_id(id)) {
                cell.1 = Some(child.inner.id());
            }
        }
    }
    let (sender, receiver) = std::sync::mpsc::sync_channel::<Output>(8);
    for pipe in [
        child
            .inner
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child
            .inner
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let sender = sender.clone();
        std::thread::spawn(move || read_output(pipe, sender));
    }
    drop(sender);
    loop {
        if !still_current(id) {
            child.stop();
            if let Ok(mut state) = flows().lock() {
                if let Some(flow) = state
                    .by_id
                    .get_mut(id)
                    .filter(|flow| !flow.phase.terminal())
                {
                    if Instant::now() >= flow.deadline {
                        flow.finish(Phase::Expired, Some("phone_login_expired"));
                    } else {
                        flow.finish(Phase::Cancelled, Some("phone_login_cancelled"));
                    }
                }
            }
            return;
        }
        while let Ok(output) = receiver.try_recv() {
            let stopped = if let Ok(mut state) = flows().lock() {
                state.by_id.get_mut(id).is_none_or(|flow| {
                    flow.state.output(output);
                    flow.clean_terminal();
                    flow.phase.terminal()
                })
            } else {
                true
            };
            if stopped {
                child.stop();
                return;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Ok(mut state) = flows().lock() {
                    if let Some(flow) = state.by_id.get_mut(id) {
                        flow.input = None;
                    }
                }
                if !status.success() {
                    fail(id, "phone_login_cli_failed");
                    return;
                }
                verify_stage(id, &stage, proxy, false);
                return;
            }
            Err(_) => {
                child.stop();
                fail(id, "phone_login_cli_failed");
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn isolate_command(
    command: &mut std::process::Command,
    root: &std::path::Path,
    storage: &std::path::Path,
) {
    let home = root.join("home");
    command
        .current_dir(root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CLAUDE_CONFIG_DIR", storage)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", storage)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for key in [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        "CLAUDE_CODE_OAUTH_SCOPES",
        "CLAUDE_CODE_SECURESTORAGE_SUFFIX",
        "CLAUDE_SECURESTORAGE_SUFFIX",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE",
        "ANTHROPIC_VERTEX_PROJECT_ID",
        "AZURE_CLIENT_ID",
        "AZURE_CLIENT_SECRET",
        "AZURE_TENANT_ID",
        "ANTHROPIC_FOUNDRY_API_KEY",
        "ANTHROPIC_FOUNDRY_RESOURCE",
    ] {
        command.env_remove(key);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.env("BROWSER", "/usr/bin/true").process_group(0);
    }
    // Windows 판 CLI 는 BROWSER 가 있으면 그것만 부르고 rundll32(기본 브라우저)로 넘어가지 않는다(2.1.309 실측).
    // 단계 폴더에 없는 경로를 주면 원본 컴퓨터에서 아무 창도 안 뜨고 수동 승인 URL 만 찍는다.
    #[cfg(windows)]
    command.env("BROWSER", root.join("no-browser.exe"));
}

fn stop_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// CLI 와 그 자식을 한 작업 객체에 묶는다 — 손잡이가 닫히면(앱이 먼저 죽어도) 다 같이 끝나, 임시 저장소를
/// 쥔 로그인 프로세스가 남지 않는다.
#[cfg(windows)]
mod job {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub(super) struct Job(HANDLE);

    impl Job {
        pub(super) fn holding(child: &std::process::Child) -> Option<Self> {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                let job = Job(CreateJobObjectW(None, None).ok()?);
                let mut limit = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limit.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &limit as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .ok()?;
                AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
                Some(job)
            }
        }

        pub(super) fn terminate(&self) {
            unsafe {
                let _ = TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

enum Output {
    Url(String),
    Prompt,
    Overflow,
    InvalidUrl,
}

fn authorization_url(raw: &str) -> Option<String> {
    let url = super::login_url_in(&super::strip_ansi(raw))?;
    if url.len() > 8192
        || url
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\'' | '<' | '>'))
    {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    // 2.1.30x 판 CLI 는 claude.com/cai/oauth/authorize 로 보낸다(2026-10-23 Windows 실측) — 옛 주소만 받으면
    // 새 CLI 의 로그인이 전부 「승인 링크가 올바르지 않다」로 끝난다.
    let official = matches!(
        (host, path.split('?').next()?),
        ("claude.ai" | "console.anthropic.com" | "platform.claude.com", "oauth/authorize")
            | ("claude.com", "cai/oauth/authorize")
    );
    if !official || !path.contains('?') || path.contains('#') {
        return None;
    }
    let mut redirect = None;
    for part in path.split_once('?')?.1.split('&') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if decode_query(name)?.as_str() == "redirect_uri" {
            if redirect.is_some() {
                return None;
            }
            redirect = Some(decode_query(value)?);
        }
    }
    if !matches!(
        redirect?.as_str(),
        "https://platform.claude.com/oauth/code/callback"
            | "https://console.anthropic.com/oauth/code/callback"
    ) {
        return None;
    }
    Some(url)
}

fn decode_query(value: &str) -> Option<String> {
    let mut bytes = value.bytes();
    let mut decoded = Vec::with_capacity(value.len());
    while let Some(byte) = bytes.next() {
        decoded.push(match byte {
            b'+' => b' ',
            b'%' => {
                ((bytes.next()? as char).to_digit(16)? * 16
                    + (bytes.next()? as char).to_digit(16)?) as u8
            }
            byte => byte,
        });
    }
    String::from_utf8(decoded).ok()
}

fn emit_url(line: &str, sender: &std::sync::mpsc::SyncSender<Output>) {
    if let Some(url) = authorization_url(line) {
        let _ = sender.try_send(Output::Url(url));
    } else if super::login_url_in(&super::strip_ansi(line))
        .is_some_and(|url| url.contains("/oauth/authorize"))
    {
        let _ = sender.try_send(Output::InvalidUrl);
    }
}

fn read_output(mut pipe: Box<dyn Read + Send>, sender: std::sync::mpsc::SyncSender<Output>) {
    let mut total = 0usize;
    let mut tail = String::new();
    let mut bytes = [0u8; 1024];
    let mut prompt_seen = false;
    loop {
        let count = match pipe.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        total += count;
        if total > 64 * 1024 {
            let _ = sender.try_send(Output::Overflow);
            return;
        }
        tail.push_str(&String::from_utf8_lossy(&bytes[..count]));
        while let Some(end) = tail.find('\n') {
            let line: String = tail.drain(..=end).collect();
            emit_url(&line, &sender);
            if !prompt_seen && line.contains("Paste code") {
                prompt_seen = true;
                let _ = sender.try_send(Output::Prompt);
            }
        }
        if !prompt_seen && tail.contains("Paste code") {
            prompt_seen = true;
            let _ = sender.try_send(Output::Prompt);
        }
        if tail.len() > 16 * 1024 {
            let _ = sender.try_send(Output::Overflow);
            return;
        }
    }
    emit_url(&tail, &sender);
}

#[cfg(test)]
#[path = "phone_login/state_tests.rs"]
mod state_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn actual(
        account: u128,
        organization: u128,
        email: &str,
        org: &str,
    ) -> kasa_mcp::VerifiedClaudeIdentity {
        kasa_mcp::VerifiedClaudeIdentity {
            account_uuid: uuid::Uuid::from_u128(account).to_string(),
            organization_uuid: uuid::Uuid::from_u128(organization).to_string(),
            email: email.into(),
            org: org.into(),
        }
    }

    pub(super) fn binding() -> Binding {
        Binding {
            slot: "acct-fixture".into(),
            machine: "machine-fixture".into(),
            label: "Fixture".into(),
            email: "one@example.test".into(),
            org: "Example".into(),
            identity: None,
        }
    }

    pub(super) fn manual_url() -> &'static str {
        "https://claude.ai/oauth/authorize?response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=fixture-state&code_challenge=fixture-challenge"
    }

    #[test]
    fn start_nonce_and_machine_are_required_and_unknown_fields_are_rejected() {
        let good = r#"{"nonce":"00000000-0000-4000-8000-000000000001","machine_id":"fixture"}"#;
        let first = parse_start(good).unwrap();
        let replay = parse_start(good).unwrap();
        assert_eq!(first.nonce, replay.nonce);
        assert_eq!(first.machine_id, "fixture");
        for bad in [
            r#"{}"#,
            r#"{"nonce":"not-a-uuid","machine_id":"fixture"}"#,
            r#"{"nonce":"00000000-0000-0000-0000-000000000000","machine_id":"fixture"}"#,
            r#"{"nonce":"00000000-0000-4000-8000-000000000001","machine_id":"fixture","slot":"other"}"#,
        ] {
            assert!(parse_start(bad).is_err());
        }
        assert!(parse_start(&"x".repeat(2049)).is_err());
    }

    #[test]
    fn metadata_names_are_not_uuid_proof_even_when_both_names_match() {
        let expected = binding();
        assert_eq!(
            identity_decision(&expected, &actual(1, 2, "one@example.test", "Example")),
            IdentityDecision::Confirm
        );
        assert_eq!(
            identity_decision(&expected, &actual(9, 8, "one@example.test", "Example")),
            IdentityDecision::Confirm
        );
        assert_eq!(
            identity_decision(&expected, &actual(1, 2, "other@example.test", "Example")),
            IdentityDecision::Reject
        );
        assert_eq!(
            identity_decision(&expected, &actual(1, 2, "one@example.test", "Other")),
            IdentityDecision::Reject
        );
        let mut empty = expected.clone();
        empty.email.clear();
        empty.org.clear();
        assert_eq!(
            identity_decision(&empty, &actual(1, 2, "one@example.test", "Example")),
            IdentityDecision::Confirm
        );
    }

    #[test]
    fn known_uuid_cannot_be_overridden_by_alias_names_or_a_confirmation() {
        let mut expected = binding();
        expected.identity = crate::session::AccountIdentity::from_verified(actual(
            1,
            2,
            "one@example.test",
            "Example",
        ));
        assert_eq!(
            identity_decision(&expected, &actual(1, 2, "renamed@example.test", "Renamed")),
            IdentityDecision::Accept
        );
        assert_eq!(
            identity_decision(&expected, &actual(9, 2, "one@example.test", "Example")),
            IdentityDecision::Reject
        );
        assert_eq!(
            identity_decision(&expected, &actual(1, 9, "one@example.test", "Example")),
            IdentityDecision::Reject
        );
    }

    #[test]
    fn a_changed_slot_or_device_cannot_receive_an_old_flow() {
        let expected = binding();
        for mutate in [
            |b: &mut Binding| b.slot.push('2'),
            |b: &mut Binding| b.machine.push('2'),
            |b: &mut Binding| b.label.push('2'),
            |b: &mut Binding| b.email.push('2'),
            |b: &mut Binding| b.org.push('2'),
        ] {
            let mut changed = expected.clone();
            mutate(&mut changed);
            assert!(!same_binding(&expected, &changed));
        }
        assert!(same_binding(&expected, &expected));
    }

    #[test]
    fn authorization_url_is_original_and_requires_the_official_manual_redirect() {
        let expected = manual_url();
        assert_eq!(
            authorization_url(&format!("\u{1b}[36m{expected}\u{1b}[0m\n")).as_deref(),
            Some(expected)
        );
        for bad in [
            expected.replace("https://claude.ai", "http://claude.ai"),
            expected.replace("claude.ai/", "claude.ai.example.test/"),
            expected.replace("claude.ai/", "claude.ai@evil.example/"),
            expected.replace("claude.ai/", "claude.ai:443/"),
            expected.replace(
                "https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback",
                "http%3A%2F%2Flocalhost%3A1234%2Fcallback",
            ),
            format!("{expected}&redirect_uri=%zz"),
            format!("{expected}#fragment"),
            "https://claude.ai/oauth/authorize?state=fixture".into(),
            expected.replace("claude.ai/oauth/", "claude.com/oauth/"),
            expected.replace("claude.ai/oauth/", "claude.ai/cai/oauth/"),
        ] {
            assert!(authorization_url(&bad).is_none());
        }
        let current = expected.replace("claude.ai/oauth/", "claude.com/cai/oauth/");
        assert_eq!(authorization_url(&current).as_deref(), Some(current.as_str()));
    }

    #[test]
    fn fragmented_cli_output_preserves_the_url_and_detects_prompt_without_newline() {
        struct Fragments {
            bytes: Vec<u8>,
            offset: usize,
        }
        impl Read for Fragments {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let n = (self.bytes.len() - self.offset).min(7).min(out.len());
                out[..n].copy_from_slice(&self.bytes[self.offset..self.offset + n]);
                self.offset += n;
                Ok(n)
            }
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        read_output(
            Box::new(Fragments {
                bytes: format!("{}\nPaste code here if prompted >", manual_url()).into_bytes(),
                offset: 0,
            }),
            tx,
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events
            .iter()
            .any(|event| matches!(event,Output::Url(url) if url==manual_url())));
        assert!(events.iter().any(|event| matches!(event, Output::Prompt)));
    }

    #[test]
    fn bad_authorization_links_and_unbounded_output_fail_closed() {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        read_output(
            Box::new(std::io::Cursor::new(
                b"https://evil.example/oauth/authorize?redirect_uri=bad\n",
            )),
            tx,
        );
        assert!(rx
            .try_iter()
            .any(|event| matches!(event, Output::InvalidUrl)));
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        read_output(Box::new(std::io::Cursor::new(vec![b'x'; 17 * 1024])), tx);
        assert!(rx.try_iter().any(|event| matches!(event, Output::Overflow)));
    }

    #[test]
    fn the_login_child_does_not_inherit_provider_credentials_or_a_real_home() {
        let mut command = std::process::Command::new("fixture-cli");
        let root = std::path::Path::new("/fixture-only");
        let storage = root.join("storage");
        isolate_command(&mut command, root, &storage);
        let env: HashMap<_, _> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(command.get_current_dir(), Some(root));
        assert_eq!(env["HOME"], Some(root.join("home").display().to_string()));
        assert_eq!(
            env["CLAUDE_CONFIG_DIR"],
            Some(storage.display().to_string())
        );
        assert_eq!(
            env["CLAUDE_SECURESTORAGE_CONFIG_DIR"],
            Some(storage.display().to_string())
        );
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
            "CLAUDE_SECURESTORAGE_SUFFIX",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "AZURE_CLIENT_SECRET",
        ] {
            assert_eq!(env.get(name), Some(&None));
        }
        #[cfg(unix)]
        assert_eq!(env["BROWSER"], Some("/usr/bin/true".into()));
        #[cfg(windows)]
        assert_eq!(env["BROWSER"], Some(root.join("no-browser.exe").display().to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_ends_the_fake_cli_and_the_children_it_started() {
        let inner = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let shell = inner.id();
        let mut child = CliChild {
            job: job::Job::holding(&inner),
            inner,
            flow: uuid::Uuid::new_v4().to_string(),
            reaped: false,
        };
        assert!(child.job.is_some());
        let started = Instant::now();
        let grandchildren = loop {
            let found: Vec<u32> = kasa_pty::fresh_process_table()
                .into_iter()
                .filter(|(_, parent, _)| *parent == shell)
                .map(|(pid, _, _)| pid)
                .collect();
            if !found.is_empty() || started.elapsed() > Duration::from_secs(5) {
                break found;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(!grandchildren.is_empty(), "cmd never started ping");
        child.stop();
        assert!(child.reaped);
        assert!(child.try_wait().unwrap().is_some());
        std::thread::sleep(Duration::from_millis(200));
        let alive: Vec<u32> = kasa_pty::fresh_process_table().into_iter().map(|(pid, _, _)| pid).collect();
        assert!(grandchildren.iter().all(|pid| !alive.contains(pid)), "ping outlived the cancelled login");
    }

    #[test]
    fn reaping_detaches_only_the_matching_pid_and_preserves_the_busy_login_lease() {
        let job = |flow: &str| LoginJob {
            provider: AccountProvider::Claude,
            id: lease_id(flow),
            state: LoginState::Running,
            device: None,
            adopting: None,
        };
        let mut cell = (Some(job("old")), Some(12345), None);
        detach_process(&mut cell, "old");
        assert!(cell.0.is_some());
        assert!(cell.1.is_none());
        assert!(cell.2.is_none());
        cell = (Some(job("new")), Some(54321), None);
        detach_process(&mut cell, "old");
        assert_eq!(cell.1, Some(54321));
        assert_eq!(cell.0.unwrap().id, lease_id("new"));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_and_reaps_only_the_owned_fake_cli_group() {
        use std::os::unix::process::CommandExt;
        let inner = std::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut child = CliChild {
            inner,
            flow: uuid::Uuid::new_v4().to_string(),
            reaped: false,
        };
        child.stop();
        assert!(child.reaped);
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn ready_payload_and_storage_errors_never_return_authorization_material() {
        let ready = format!("{:?}", Ready("fixture-sensitive-flow".into()));
        assert!(!ready.contains("fixture-sensitive-flow"));
        assert_eq!(
            storage_error("credential_target_changed"),
            "phone_login_target_changed"
        );
        assert_eq!(
            storage_error("credential_identity_unverified"),
            "phone_login_identity_unverified"
        );
        assert_eq!(
            storage_error("fixture-secret-from-subprocess"),
            "phone_login_storage_failed"
        );
        assert!(!error_message("fixture-secret-from-subprocess").contains("fixture-secret"));
    }

    #[test]
    fn terminal_states_and_fixture_runs_cannot_start_or_continue_a_real_login() {
        for phase in [
            Phase::Succeeded,
            Phase::Failed,
            Phase::Cancelled,
            Phase::Expired,
        ] {
            assert!(phase.terminal());
        }
        for phase in [
            Phase::Starting,
            Phase::Authorizing,
            Phase::Code,
            Phase::Verifying,
            Phase::Confirm,
        ] {
            assert!(!phase.terminal());
        }
        assert!(!allowed());
    }
}
