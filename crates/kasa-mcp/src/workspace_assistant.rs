//! Account-owned assistance. Authentication and trusted runner provenance are supplied by the gateway.

mod desktop;
mod jev;
pub use desktop::DesktopSession;
mod text;

pub use jev::request_keywords;
pub use jev::JevAdapter;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;
pub use text::{Message, MessageRequest, MessageStatus, TextProvider};

type Result<T> = std::result::Result<T, Error>;
const MAX_TASKS: usize = 200;
const JOB_TTL: u64 = 120;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Disabled,
    Conflict,
    Storage,
    Stale,
    RateLimited,
    Unavailable,
    EvidenceRequired,
}
impl From<crate::sealed::Error> for Error {
    fn from(error: crate::sealed::Error) -> Self {
        match error {
            crate::sealed::Error::Invalid => Self::Invalid,
            crate::sealed::Error::Storage => Self::Storage,
        }
    }
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::Disabled => "key_required",
            Self::Conflict => "revision_conflict",
            Self::Storage => "storage_unavailable",
            Self::Stale => "stale_job",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "decision_unavailable",
            Self::EvidenceRequired => "verified_evidence_required",
        }
    }
}

pub struct AuthContext {
    account: String,
    device: String,
}
impl AuthContext {
    /// Construct only from the gateway's authenticated account and device, never request-body identity.
    pub fn authenticated(account: &str, device: &str) -> Result<Self> {
        if !crate::relay_auth::valid_account_name(account) || !identifier(device, 128) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            account: account.into(),
            device: device.into(),
        })
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}
fn clean_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
}
fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Coding,
    Design,
    Research,
    Operations,
    #[default]
    General,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub kind: ProjectKind,
    pub keywords: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Queued,
    Working,
    Blocked,
    AwaitingVerification,
    Verified,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pass,
    Fail,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub state: CheckState,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub task_id: String,
    pub task_revision: u64,
    pub work_revision: String,
    pub source_device: String,
    pub observed_at: u64,
    pub expires_at: u64,
    pub artifact_digest: String,
    pub checks: Vec<Check>,
    pub complete: bool,
}

/// This is deliberately not deserializable: HTTP clients cannot attest their own reports.
#[derive(Clone, Copy)]
pub enum ProofOrigin {
    Reported,
    TrustedRunner,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct SavedEvidence {
    proof: Evidence,
    trusted: bool,
    digest: String,
    accepted_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub original_prompt: String,
    pub goal: String,
    pub state: TaskState,
    pub project: Option<String>,
    pub step: String,
    pub rev: u64,
    pub work_revision: String,
    pub required_checks: Vec<String>,
    #[serde(default)]
    pub check_results: Vec<Check>,
    pub verify_ok: bool,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(skip_serializing)]
    evidence: Option<SavedEvidence>,
    #[serde(skip_serializing)]
    notification_claimed: Option<u64>,
}

// Public snapshots omit evidence internals; disk serialization must retain them for replay protection.
#[derive(Clone, Deserialize, Serialize)]
struct StoredTask {
    task: Task,
    evidence: Option<SavedEvidence>,
    notification_claimed: Option<u64>,
}
impl From<&Task> for StoredTask {
    fn from(task: &Task) -> Self {
        Self {
            task: task.clone(),
            evidence: task.evidence.clone(),
            notification_claimed: task.notification_claimed,
        }
    }
}
impl StoredTask {
    fn restore(mut self) -> Task {
        self.task.evidence = self.evidence;
        self.task.notification_claimed = self.notification_claimed;
        self.task
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct KeyRecord {
    revision: u64,
    value: Option<String>,
}
impl Default for KeyRecord {
    fn default() -> Self {
        Self {
            revision: 0,
            value: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPurpose {
    ClassifyProject,
    VerifyCompletion,
}

#[derive(Clone, Deserialize, Serialize)]
struct Job {
    id: String,
    device: String,
    task_id: String,
    task_revision: u64,
    key_revision: u64,
    expires_at: u64,
    created_at: u64,
    purpose: DecisionPurpose,
    evidence_digest: Option<String>,
    choices: BTreeMap<String, String>,
    projects: BTreeMap<String, String>,
    request: serde_json::Value,
    cache_key: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Rate {
    hour: u64,
    hour_count: u32,
    day: u64,
    day_count: u32,
}
impl Rate {
    fn take(&mut self, now: u64) -> Result<()> {
        let hour = now / 3600;
        let day = now / 86400;
        if hour < self.hour || day < self.day {
            return Err(Error::RateLimited);
        }
        if hour != self.hour {
            self.hour = hour;
            self.hour_count = 0;
        }
        if day != self.day {
            self.day = day;
            self.day_count = 0;
        }
        if self.hour_count >= 60 || self.day_count >= 240 {
            return Err(Error::RateLimited);
        }
        self.hour_count += 1;
        self.day_count += 1;
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct CachedAdvice {
    at: u64,
    key_revision: u64,
    advice: jev::Advice,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Account {
    revision: u64,
    key: KeyRecord,
    projects: BTreeMap<String, Project>,
    tasks: BTreeMap<String, StoredTask>,
    jobs: BTreeMap<String, Job>,
    cache: BTreeMap<String, CachedAdvice>,
    rate: Rate,
    #[serde(default)]
    messages: BTreeMap<String, text::StoredMessage>,
    #[serde(default)]
    requests: BTreeMap<String, (String, String)>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub key_present: bool,
    pub key_revision: u64,
    pub revision: u64,
    pub model: Option<String>,
    pub model_setup: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub status: Status,
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub conversation: Vec<Message>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Notification {
    pub id: String,
    pub task_id: String,
    pub revision: u64,
    pub goal: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteStudent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub latest: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteInput {
    pub message: String,
    pub students: Vec<RouteStudent>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RouteAdvice {
    pub probabilities: BTreeMap<String, f64>,
    pub latency_ms: u64,
}

const ROUTE_PER_MINUTE: u32 = 90;

pub struct Store {
    vault: crate::sealed::Vault,
    accounts: Mutex<BTreeMap<String, Account>>,
    /// Routing asks while the owner types, so it gets its own per-minute budget in memory
    /// instead of spending the persisted hourly decision rate.
    route_rate: Mutex<BTreeMap<String, (u64, u32)>>,
}
impl Store {
    pub fn open(directory: PathBuf) -> Result<Self> {
        Ok(Self {
            vault: crate::sealed::Vault::open(directory, "kasa.workspace-assistant.v1")?,
            accounts: Mutex::new(BTreeMap::new()),
            route_rate: Mutex::new(BTreeMap::new()),
        })
    }

    fn account(
        &self,
        ctx: &AuthContext,
        accounts: &mut BTreeMap<String, Account>,
    ) -> Result<Account> {
        if let Some(account) = accounts.get(&ctx.account) {
            return Ok(account.clone());
        }
        let account: Account = self.vault.read(&ctx.account)?.unwrap_or_default();
        if account.tasks.len() > MAX_TASKS
            || account.projects.len() > 7
            || account.jobs.len() > 32
            || account.messages.len() > 200
            || account.requests.len() > MAX_TASKS
        {
            return Err(Error::Storage);
        }
        accounts.insert(ctx.account.clone(), account.clone());
        Ok(account)
    }

    fn save(
        &self,
        ctx: &AuthContext,
        account: &mut Account,
        accounts: &mut BTreeMap<String, Account>,
    ) -> Result<()> {
        account.revision = account.revision.checked_add(1).ok_or(Error::Storage)?;
        if let Err(error) = self.vault.write(&ctx.account, account) {
            accounts.remove(&ctx.account);
            return Err(error.into());
        }
        accounts.insert(ctx.account.clone(), account.clone());
        Ok(())
    }

    pub fn snapshot(&self, ctx: &AuthContext) -> Result<Snapshot> {
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let account = self.account(ctx, &mut accounts)?;
        let mut conversation = account
            .messages
            .values()
            .map(|record| record.message.clone())
            .collect::<Vec<_>>();
        conversation.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(Snapshot {
            status: status(&account),
            projects: account.projects.values().cloned().collect(),
            tasks: account
                .tasks
                .values()
                .cloned()
                .map(StoredTask::restore)
                .collect(),
            conversation,
        })
    }

    pub fn set_key(
        &self,
        ctx: &AuthContext,
        expected_revision: u64,
        key: Option<String>,
    ) -> Result<Status> {
        if key.as_ref().is_some_and(|value| {
            value.len() < 8 || value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic())
        }) {
            return Err(Error::Invalid);
        }
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        if account.key.revision != expected_revision {
            return Err(Error::Conflict);
        }
        account.key = KeyRecord {
            revision: expected_revision.checked_add(1).ok_or(Error::Storage)?,
            value: key,
        };
        account.jobs.clear();
        account.cache.clear();
        for record in account.messages.values_mut() {
            if record.message.status == MessageStatus::Pending {
                record.message.status = MessageStatus::Cancelled;
            }
        }
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(status(&account))
    }

    pub fn create_project(
        &self,
        ctx: &AuthContext,
        name: &str,
        kind: ProjectKind,
        keywords: Vec<String>,
    ) -> Result<Project> {
        if !clean_text(name, 160)
            || keywords.len() > 12
            || keywords
                .iter()
                .any(|keyword| !jev::allowed_keyword(keyword))
        {
            return Err(Error::Invalid);
        }
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        enabled(&account)?;
        if account.projects.len() >= 7 {
            return Err(Error::Invalid);
        }
        let project = Project {
            id: id(),
            name: name.into(),
            kind,
            keywords,
        };
        account.projects.insert(project.id.clone(), project.clone());
        account.jobs.clear();
        account.cache.clear();
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(project)
    }

    pub fn create_task(
        &self,
        ctx: &AuthContext,
        prompt: &str,
        work_revision: &str,
        required_checks: Vec<String>,
        now: u64,
    ) -> Result<Task> {
        self.create_task_once(ctx, &id(), prompt, work_revision, required_checks, now)
    }

    pub fn create_task_once(
        &self,
        ctx: &AuthContext,
        request_id: &str,
        prompt: &str,
        work_revision: &str,
        required_checks: Vec<String>,
        now: u64,
    ) -> Result<Task> {
        if !identifier(request_id, 128)
            || !clean_text(prompt, 16384)
            || !identifier(work_revision, 128)
            || !valid_required(&required_checks)
        {
            return Err(Error::Invalid);
        }
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        enabled(&account)?;
        let fingerprint = digest(
            &serde_json::to_vec(&(prompt, work_revision, &required_checks))
                .map_err(|_| Error::Invalid)?,
        );
        if let Some((previous, task_id)) = account.requests.get(request_id) {
            if previous != &fingerprint {
                return Err(Error::Conflict);
            }
            return account
                .tasks
                .get(task_id)
                .cloned()
                .map(StoredTask::restore)
                .ok_or(Error::Storage);
        }
        if account.tasks.len() >= MAX_TASKS {
            return Err(Error::Invalid);
        }
        let goal = prompt
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(prompt)
            .trim()
            .chars()
            .take(160)
            .collect();
        let task = Task {
            id: id(),
            original_prompt: prompt.into(),
            goal,
            state: TaskState::Queued,
            project: None,
            step: "요청 접수".into(),
            rev: 1,
            work_revision: work_revision.into(),
            required_checks,
            check_results: Vec::new(),
            verify_ok: false,
            created_at: now,
            updated_at: now,
            evidence: None,
            notification_claimed: None,
        };
        account
            .tasks
            .insert(task.id.clone(), StoredTask::from(&task));
        account
            .requests
            .insert(request_id.into(), (fingerprint, task.id.clone()));
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(task)
    }

    pub fn update_task(
        &self,
        ctx: &AuthContext,
        task_id: &str,
        expected_revision: u64,
        state: TaskState,
        step: &str,
        work_revision: &str,
        now: u64,
    ) -> Result<Task> {
        if state == TaskState::Verified || !clean_text(step, 512) || !identifier(work_revision, 128)
        {
            return Err(Error::Invalid);
        }
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        enabled(&account)?;
        let mut task = get_task(&account, task_id, expected_revision)?;
        if task.state == TaskState::Cancelled {
            return Err(Error::Stale);
        }
        task.state = state;
        task.step = step.into();
        task.work_revision = work_revision.into();
        task.rev = task.rev.checked_add(1).ok_or(Error::Storage)?;
        task.updated_at = now;
        task.evidence = None;
        task.check_results.clear();
        task.verify_ok = false;
        task.notification_claimed = None;
        account.jobs.retain(|_, job| job.task_id != task_id);
        for record in account.messages.values_mut() {
            if record.message.status == MessageStatus::Pending
                && record.message.task_id.as_deref() == Some(task_id)
            {
                record.message.status = MessageStatus::Cancelled;
            }
        }
        account
            .tasks
            .insert(task.id.clone(), StoredTask::from(&task));
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(task)
    }

    pub fn record_evidence(
        &self,
        ctx: &AuthContext,
        task_id: &str,
        expected_revision: u64,
        evidence: Evidence,
        origin: ProofOrigin,
        now: u64,
    ) -> Result<Task> {
        if evidence.checks.len() > 32
            || evidence.source_device != ctx.device
            || evidence
                .checks
                .iter()
                .any(|check| !identifier(&check.id, 80))
        {
            return Err(Error::Invalid);
        }
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        enabled(&account)?;
        let mut task = get_task(&account, task_id, expected_revision)?;
        if task.state == TaskState::Cancelled || task.state == TaskState::Verified {
            return Err(Error::Stale);
        }
        if evidence.task_id != task.id
            || evidence.task_revision != task.rev
            || evidence.work_revision != task.work_revision
        {
            return Err(Error::Conflict);
        }
        let digest = digest(&serde_json::to_vec(&evidence).map_err(|_| Error::Invalid)?);
        task.rev = task.rev.checked_add(1).ok_or(Error::Storage)?;
        task.check_results = evidence.checks.clone();
        task.evidence = Some(SavedEvidence {
            proof: evidence,
            trusted: matches!(origin, ProofOrigin::TrustedRunner),
            digest,
            accepted_revision: task.rev,
        });
        task.state = if task
            .evidence
            .as_ref()
            .unwrap()
            .proof
            .checks
            .iter()
            .any(|check| check.state == CheckState::Fail)
        {
            TaskState::Blocked
        } else {
            TaskState::AwaitingVerification
        };
        task.step = if task.state == TaskState::Blocked {
            "검사 실패"
        } else {
            "검증 근거 확인 중"
        }
        .into();
        task.updated_at = now;
        task.verify_ok = false;
        account.jobs.retain(|_, job| job.task_id != task_id);
        account
            .tasks
            .insert(task.id.clone(), StoredTask::from(&task));
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(task)
    }

    fn begin(
        &self,
        ctx: &AuthContext,
        task_id: &str,
        revision: u64,
        purpose: DecisionPurpose,
        now: u64,
    ) -> Result<(Job, String, Option<jev::Advice>)> {
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        enabled(&account)?;
        let task = get_task(&account, task_id, revision)?;
        if matches!(task.state, TaskState::Cancelled | TaskState::Verified) {
            return Err(Error::Stale);
        }
        if purpose == DecisionPurpose::VerifyCompletion && !evidence_ok(&task, now) {
            return Err(Error::EvidenceRequired);
        }
        account.jobs.retain(|_, job| job.expires_at >= now);
        account.cache.retain(|_, cache| {
            cache.at <= now
                && now - cache.at <= JOB_TTL
                && cache.key_revision == account.key.revision
        });
        if account.jobs.len() >= 32 || account.jobs.values().any(|job| job.task_id == task_id) {
            return Err(Error::RateLimited);
        }
        let (request, choices, projects) = jev::request(&task, &account.projects, purpose)?;
        let cache_key = digest(&serde_json::to_vec(&request).map_err(|_| Error::Invalid)?);
        let cached = account
            .cache
            .get(&cache_key)
            .map(|cached| cached.advice.clone());
        if cached.is_none() {
            account.rate.take(now)?;
        }
        let job = Job {
            id: id(),
            device: ctx.device.clone(),
            task_id: task.id,
            task_revision: task.rev,
            key_revision: account.key.revision,
            expires_at: now.saturating_add(JOB_TTL),
            created_at: now,
            purpose,
            evidence_digest: task.evidence.map(|proof| proof.digest),
            choices,
            projects,
            request,
            cache_key,
        };
        account.jobs.insert(job.id.clone(), job.clone());
        let key = account.key.value.clone().ok_or(Error::Disabled)?;
        self.save(ctx, &mut account, &mut accounts)?;
        Ok((job, key, cached))
    }

    #[cfg(test)]
    fn apply(
        &self,
        ctx: &AuthContext,
        job_id: &str,
        advice: Option<jev::Advice>,
        now: u64,
    ) -> Result<Task> {
        self.apply_checked(ctx, job_id, advice, now, &|| true)
    }

    fn apply_checked(
        &self,
        ctx: &AuthContext,
        job_id: &str,
        advice: Option<jev::Advice>,
        now: u64,
        authorized: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Task> {
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        let job = account.jobs.remove(job_id).ok_or(Error::Stale)?;
        let mut task = get_task(&account, &job.task_id, job.task_revision)?;
        if !authorized()
            || job.device != ctx.device
            || now < job.created_at
            || account.key.value.is_none()
            || account.key.revision != job.key_revision
            || job.expires_at < now
            || matches!(task.state, TaskState::Cancelled | TaskState::Verified)
            || task.evidence.as_ref().map(|proof| &proof.digest) != job.evidence_digest.as_ref()
        {
            self.save(ctx, &mut account, &mut accounts)?;
            return Err(Error::Stale);
        }
        if let Some(advice) = advice.filter(|advice| advice.valid(&job.choices)) {
            account.cache.insert(
                job.cache_key,
                CachedAdvice {
                    at: now,
                    key_revision: job.key_revision,
                    advice: advice.clone(),
                },
            );
            if advice.confident() {
                if job.purpose == DecisionPurpose::ClassifyProject {
                    if let Some(project) = job.projects.get(advice.choice()) {
                        task.project = Some(project.clone());
                    }
                } else if advice.choice() == "verified" && evidence_ok(&task, now) {
                    task.state = TaskState::Verified;
                    task.verify_ok = true;
                    task.step = "검사와 결과 확인 완료".into();
                }
            }
        }
        task.rev = task.rev.checked_add(1).ok_or(Error::Storage)?;
        if let Some(saved) = &mut task.evidence {
            saved.accepted_revision = task.rev;
        }
        task.updated_at = now;
        account
            .tasks
            .insert(task.id.clone(), StoredTask::from(&task));
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(task)
    }

    /// Advisory routing for a message being typed. Nothing is stored and nothing is sent;
    /// the desktop shows the probabilities and the owner decides.
    pub fn route(
        &self,
        ctx: &AuthContext,
        input: &RouteInput,
        adapter: &JevAdapter,
        now: u64,
        authorized: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<RouteAdvice> {
        let (request, choices) = jev::route_request(input)?;
        {
            let mut rates = self.route_rate.lock().map_err(|_| Error::Storage)?;
            let minute = now / 60;
            let entry = rates.entry(ctx.account.clone()).or_insert((minute, 0));
            if entry.0 != minute {
                *entry = (minute, 0);
            }
            if entry.1 >= ROUTE_PER_MINUTE {
                return Err(Error::RateLimited);
            }
            entry.1 += 1;
        }
        let mut key = {
            let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
            let account = self.account(ctx, &mut accounts)?;
            account.key.value.clone().ok_or(Error::Disabled)?
        };
        if !authorized() {
            key.clear();
            return Err(Error::Stale);
        }
        let started = std::time::Instant::now();
        let advice = adapter.choose(&request, &key);
        key.clear();
        let advice = advice?;
        if !authorized() {
            return Err(Error::Stale);
        }
        if !advice.valid(&choices) {
            return Err(Error::Unavailable);
        }
        Ok(RouteAdvice {
            probabilities: advice.probabilities().clone(),
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Invoke on a bounded blocking worker. The adapter performs no retries or follow-up actions.
    pub fn decide(
        &self,
        ctx: &AuthContext,
        task_id: &str,
        revision: u64,
        purpose: DecisionPurpose,
        adapter: &JevAdapter,
        now: u64,
        authorized: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Task> {
        if !authorized() {
            return Err(Error::Stale);
        }
        let (job, mut key, cached) = self.begin(ctx, task_id, revision, purpose, now)?;
        let started = std::time::Instant::now();
        let advice = if authorized() {
            cached.or_else(|| adapter.choose(&job.request, &key).ok())
        } else {
            None
        };
        key.clear();
        self.apply_checked(
            ctx,
            &job.id,
            advice,
            now.saturating_add(started.elapsed().as_secs()),
            authorized,
        )
    }

    pub fn claim_notification(
        &self,
        ctx: &AuthContext,
        task_id: &str,
        revision: u64,
        now: u64,
    ) -> Result<Option<Notification>> {
        let mut accounts = self.accounts.lock().map_err(|_| Error::Storage)?;
        let mut account = self.account(ctx, &mut accounts)?;
        enabled(&account)?;
        let mut task = get_task(&account, task_id, revision)?;
        if task.state != TaskState::Verified || !task.verify_ok || !evidence_ok(&task, now) {
            return Err(Error::EvidenceRequired);
        }
        if task.notification_claimed == Some(revision) {
            return Ok(None);
        }
        task.notification_claimed = Some(revision);
        account
            .tasks
            .insert(task.id.clone(), StoredTask::from(&task));
        self.save(ctx, &mut account, &mut accounts)?;
        Ok(Some(Notification {
            id: format!("{}-{revision}", task.id),
            task_id: task.id,
            revision,
            goal: task.goal,
        }))
    }
}

fn status(account: &Account) -> Status {
    let last = account
        .messages
        .values()
        .max_by_key(|record| record.message.updated_at);
    Status {
        enabled: account.key.value.is_some(),
        key_present: account.key.value.is_some(),
        key_revision: account.key.revision,
        revision: account.revision,
        model: last.and_then(|record| record.message.model.clone()),
        model_setup: last.is_none_or(|record| record.message.status == MessageStatus::ModelSetup),
    }
}
fn enabled(account: &Account) -> Result<()> {
    if account.key.value.is_some() {
        Ok(())
    } else {
        Err(Error::Disabled)
    }
}
fn get_task(account: &Account, id: &str, revision: u64) -> Result<Task> {
    let task = account
        .tasks
        .get(id)
        .cloned()
        .ok_or(Error::Invalid)?
        .restore();
    if task.rev != revision {
        return Err(Error::Conflict);
    }
    Ok(task)
}
fn valid_required(checks: &[String]) -> bool {
    checks.len() <= 32
        && checks.iter().all(|check| identifier(check, 80))
        && checks.iter().collect::<BTreeSet<_>>().len() == checks.len()
}
fn digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn evidence_ok(task: &Task, now: u64) -> bool {
    let Some(saved) = &task.evidence else {
        return false;
    };
    let proof = &saved.proof;
    saved.trusted
        && proof.complete
        && proof.task_id == task.id
        && saved.accepted_revision == task.rev
        && proof.task_revision > 0
        && proof.task_revision < task.rev
        && proof.work_revision == task.work_revision
        && !task.required_checks.is_empty()
        && proof.observed_at <= now
        && now.saturating_sub(proof.observed_at) <= 900
        && proof.expires_at >= now
        && proof.expires_at > proof.observed_at
        && proof.expires_at - proof.observed_at <= 900
        && proof.artifact_digest.len() == 64
        && proof
            .artifact_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        && proof.checks.len() == task.required_checks.len()
        && proof
            .checks
            .iter()
            .map(|check| &check.id)
            .collect::<BTreeSet<_>>()
            .len()
            == proof.checks.len()
        && task.required_checks.iter().all(|id| {
            proof
                .checks
                .iter()
                .any(|check| &check.id == id && check.state == CheckState::Pass)
        })
        && serde_json::to_vec(proof).is_ok_and(|bytes| digest(&bytes) == saved.digest)
}

#[cfg(all(test, unix))]
mod tests;
