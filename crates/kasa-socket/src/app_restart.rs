//! 카사텀 앱 재시작의 순수 핵심 — 계획·거부 사유·계획 해시·승인 검사·작업 기록·도우미·순차 실행.
//!
//! 절차와 명령은 `docs/app-restart.md` 가 정본이다. 여기서 지키는 것:
//! - **대상은 명부의 안정 `machine_id`** 로만 고른다. 호스트·명령을 받지 않는다.
//! - **재시작과 업그레이드를 가른다.** 종료 때 자기설치가 움직일 판(`install_pending`)이면 계획에
//!   드러내고 거부한다 — 재시작 한 번에 모르는 빌드가 깔리면 안 된다.
//! - **확인 못 하면 거부한다.** 바쁜 학생·승인/질문 대기·미저장 편집기·굽는 중·진행 중인 작업이
//!   있거나 사실이 오래됐으면 실행하지 않는다. 강제 종료는 어디에도 없다.
//! - **한 대씩, 조종 기기는 마지막.** 조종 기기 앱이 꺼지면 원격 터널도 함께 내려간다.
//!   앞 기기가 실패하면 뒤 기기는 손대지 않는다.
//! - **같은 작업은 한 번만.** 작업 id 는 계획 해시와 기기 id 로 정해지고, 대상 기기 디스크에
//!   `create_new` 로 적힌다 — 같은 요청이 다시 와도 두 번 돌지 않는다.
//! - **승인은 나쵸 서버가 쥔다.** 조종 쪽은 실행 직전 계획을 다시 재서 나쵸 `consume`(서버에서 원자적 1회)이
//!   성공한 한 번만 진행하고, 대상 쪽은 나쵸에서 읽은 승인(승인됨·소비됨·만료 전·자기 기기와 해시가 scope 에
//!   있음)을 확인한 뒤에만 도우미를 띄운다. 요청이 스스로 「승인됐다」고 말하는 것은 어디서도 안 믿는다.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "kasaterm-restart/1";
/// 이 판이 아는 재시작 계약. 대상 앱이 이보다 낮으면 그 앱엔 재시작 창구가 없다.
pub const CAPABILITY: u32 = 1;
/// 사실이 이보다 오래되면 계획에 쓰지 않는다 — 그 사이 학생이 일을 시작했을 수 있다.
pub const FACTS_TTL_MS: u64 = 30_000;
/// 계획(과 그 승인)의 유효 시간.
pub const PLAN_TTL_MS: u64 = 10 * 60_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BinaryId {
    pub inode: u64,
    pub mtime_ms: u64,
    pub build: String,
}

/// 종료하면 자기설치가 갈아 끼울 번들.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingInstall {
    pub dist_path: String,
    pub dist_mtime_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BusyPane {
    pub surface: String,
    pub character: String,
    pub state: String,
}

/// 대상 앱이 스스로 잰 사실. GUI 스레드가 모은다(바쁜 학생·미저장 편집기는 그쪽만 안다).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    pub schema: String,
    pub machine_id: String,
    pub label: String,
    pub os: String,
    pub capability: u32,
    /// 설치본 번들(`…/kasaterm.app`).
    pub app_path: String,
    pub pid: u32,
    pub running_exe: String,
    pub binary: BinaryId,
    pub install_pending: Option<PendingInstall>,
    pub baking: bool,
    pub busy: Vec<BusyPane>,
    pub dirty_editors: u32,
    /// 다시 켜면 이어지는 학생 대화(세션 id 가 있는 claude·codex 창).
    pub restorable_sessions: u32,
    /// 다시 켜면 새 셸로만 돌아오는 창 — 돌던 명령은 이어지지 않는다.
    pub plain_shells: u32,
    pub registered_servers: u32,
    pub pet_alive: Option<bool>,
    pub active_job: Option<String>,
    pub observed_at_ms: u64,
}

impl Facts {
    /// 설치본 번들 안의 실행 파일 — 도우미가 생존·재기동을 이 경로로 잰다.
    pub fn app_exe(&self) -> String {
        format!("{}/Contents/MacOS/kasaterm", self.app_path.trim_end_matches('/'))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Refusal {
    Unreachable { reason: String },
    IdentityMismatch { asked: String, got: String },
    UnsupportedOs { os: String },
    CapabilityMissing { have: u32, need: u32 },
    NotInstalledApp { running: String },
    BusyStudents { panes: Vec<BusyPane> },
    UnsavedEditors { count: u32 },
    SelfInstallPending { dist: String },
    BakeInProgress,
    JobInFlight { job_id: String },
    StaleFacts { age_ms: u64 },
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::Unreachable { reason } => format!("기기에 닿지 못했다 · {reason}"),
            Self::IdentityMismatch { asked, got } => format!("물은 기기({asked})가 아닌 기기({got})가 답했다"),
            Self::UnsupportedOs { os } => format!("지원하지 않는 OS({os}) — 지금은 macOS 만"),
            Self::CapabilityMissing { have, need } => format!("이 기기 앱엔 재시작 창구가 없다(판 {have} < {need}) — 새 판 설치 뒤 한 번은 사람이 재시작"),
            Self::NotInstalledApp { running } => format!("설치본으로 도는 앱이 아니다({running}) — 개발 실행은 재시작하지 않는다"),
            Self::BusyStudents { panes } => format!(
                "일하는 중이거나 사람 답을 기다리는 학생 {}명: {}",
                panes.len(),
                panes.iter().map(|p| format!("{} {} {}", p.surface, p.character, p.state)).collect::<Vec<_>>().join(", ")
            ),
            Self::UnsavedEditors { count } => format!("저장 안 한 편집기 {count}개"),
            Self::SelfInstallPending { dist } => format!("종료하면 새 빌드({dist})가 자기설치된다 — 재시작이 업그레이드가 되므로 거부"),
            Self::BakeInProgress => "앱을 굽는 중이다".into(),
            Self::JobInFlight { job_id } => format!("이미 진행 중인 재시작 작업이 있다({job_id})"),
            Self::StaleFacts { age_ms } => format!("사실이 {}초 전 것이다 — 다시 계획해야 한다", age_ms / 1000),
        }
    }
}

/// 이 기기를 지금 재시작해도 되는가. 빈 목록이면 된다.
pub fn refusals(asked: &str, facts: &Facts, now_ms: u64) -> Vec<Refusal> {
    let mut out = Vec::new();
    if facts.machine_id != asked {
        out.push(Refusal::IdentityMismatch { asked: asked.into(), got: facts.machine_id.clone() });
    }
    if facts.os != "macos" {
        out.push(Refusal::UnsupportedOs { os: facts.os.clone() });
    }
    if facts.capability < CAPABILITY {
        out.push(Refusal::CapabilityMissing { have: facts.capability, need: CAPABILITY });
    }
    if facts.running_exe != facts.app_exe() || !valid_app_path(&facts.app_path) {
        out.push(Refusal::NotInstalledApp { running: facts.running_exe.clone() });
    }
    if !facts.busy.is_empty() {
        out.push(Refusal::BusyStudents { panes: facts.busy.clone() });
    }
    if facts.dirty_editors > 0 {
        out.push(Refusal::UnsavedEditors { count: facts.dirty_editors });
    }
    if let Some(pending) = &facts.install_pending {
        out.push(Refusal::SelfInstallPending { dist: pending.dist_path.clone() });
    }
    if facts.baking {
        out.push(Refusal::BakeInProgress);
    }
    if let Some(job_id) = &facts.active_job {
        out.push(Refusal::JobInFlight { job_id: job_id.clone() });
    }
    let age = now_ms.saturating_sub(facts.observed_at_ms);
    if age > FACTS_TTL_MS {
        out.push(Refusal::StaleFacts { age_ms: age });
    }
    out
}

/// 재기동에 쓸 번들 경로로 받아들이는 모양 — 사용자·시스템 Applications 의 `kasaterm.app` 만.
/// 임의 경로를 `open -a` 에 넘기지 않는다.
pub fn valid_app_path(path: &str) -> bool {
    let p = Path::new(path);
    p.is_absolute()
        && p.file_name().is_some_and(|n| n == "kasaterm.app")
        && p.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "Applications")
        && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

fn fnv(parts: &[&str]) -> String {
    let mut h = 0xcbf29ce484222325u64;
    for part in parts {
        for b in part.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        h = (h ^ 0x1f).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 대상 하나의 정체 — 승인 뒤 이것이 바뀌면(다른 pid·다른 바이너리·자기설치 예정 변화) 실행을 멈춘다.
/// 보안용 서명이 아니라 「계획을 세운 그 대상인가」를 재는 표다.
pub fn target_hash(f: &Facts) -> String {
    let pending = f.install_pending.as_ref().map(|p| format!("{}@{}", p.dist_path, p.dist_mtime_ms)).unwrap_or_default();
    fnv(&[
        &f.machine_id, &f.app_path, &f.pid.to_string(), &f.binary.inode.to_string(),
        &f.binary.mtime_ms.to_string(), &f.binary.build, &pending, &f.capability.to_string(),
    ])
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTarget {
    pub machine_id: String,
    pub label: String,
    pub controller: bool,
    pub facts: Option<Facts>,
    pub refusals: Vec<Refusal>,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub schema: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub controller: String,
    pub targets: Vec<PlanTarget>,
    pub hash: String,
    /// 승인에 묶이는 정확한 대상 — 나쵸가 이 객체로 `scope_hash` 를 계산하고, 실행 직전 같은 객체로 소비한다.
    pub scope: serde_json::Value,
}

impl Plan {
    pub fn runnable(&self) -> bool {
        !self.targets.is_empty() && self.targets.iter().all(|t| t.refusals.is_empty())
    }
}

/// 계획을 세운다. 조종 기기는 요청 순서와 상관없이 맨 뒤다.
pub fn build_plan(
    controller: &str,
    requested: &[String],
    lookup: &dyn Fn(&str) -> std::result::Result<Facts, String>,
    now_ms: u64,
) -> Plan {
    let mut ids: Vec<String> = Vec::new();
    for id in requested.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }
    ids.sort_by_key(|id| id == controller);
    let targets: Vec<PlanTarget> = ids
        .into_iter()
        .map(|id| match lookup(&id) {
            Ok(facts) => PlanTarget {
                controller: id == controller,
                label: facts.label.clone(),
                refusals: refusals(&id, &facts, now_ms),
                hash: target_hash(&facts),
                facts: Some(facts),
                machine_id: id,
            },
            Err(reason) => PlanTarget {
                controller: id == controller,
                label: String::new(),
                refusals: vec![Refusal::Unreachable { reason }],
                hash: String::new(),
                facts: None,
                machine_id: id,
            },
        })
        .collect();
    let parts: Vec<String> = targets.iter().map(|t| format!("{}={}", t.machine_id, t.hash)).collect();
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    let hash = fnv(&refs);
    let scope = json!({
        "action": ACTION,
        "plan": hash,
        "controller": controller,
        "targets": targets.iter().enumerate()
            .map(|(n, t)| json!({"order": n + 1, "machine_id": t.machine_id, "hash": t.hash}))
            .collect::<Vec<_>>(),
    });
    Plan {
        schema: SCHEMA.into(),
        created_at_ms: now_ms,
        expires_at_ms: now_ms + PLAN_TTL_MS,
        controller: controller.into(),
        hash,
        scope,
        targets,
    }
}

/// 나쵸 승인의 `action` — 나쵸 `approvals.ACTIONS` 에 같은 낱말이 있어야 요청이 만들어진다.
pub const ACTION: &str = "kasaterm_restart";

/// 나쵸가 돌려주는 승인(`approvals.public`). 카사텀은 이것을 **나쵸에게서** 읽을 뿐, 만들거나 고치지 않는다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovalView {
    pub id: String,
    pub action: String,
    pub scope: serde_json::Value,
    pub scope_hash: String,
    pub state: String,
    pub expires_at_ms: u64,
    pub consumed_at_ms: Option<u64>,
    /// 한 번을 쓴 기기 — 나쵸가 `scope.controller` 와 같을 때만 소비를 받는다.
    pub consumed_by: Option<String>,
}

/// 승인을 쥔 곳(나쵸). 운영은 나쵸 앱 창구, 검사는 가짜.
pub trait Authority {
    fn get(&self, approval_id: &str) -> std::result::Result<ApprovalView, String>;
    /// 서버가 `scope` 로 해시를 다시 계산해 승인한 대상과 같고 `consumer` 가 scope 의 조종 기기일 때만
    /// **한 번** 성공한다(나쵸 `POST /api/app/approvals/{id}/consume`).
    fn consume(&self, approval_id: &str, scope: &serde_json::Value, consumer: &str) -> std::result::Result<ApprovalView, String>;
}

pub fn valid_approval_id(id: &str) -> bool {
    id.len() == 35 && id.starts_with("ap_") && id[3..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// 조종 쪽 — 실행 직전의 계획으로 승인을 소비한다. 성공한 한 번만 진행한다.
pub fn authorize_run(plan: &Plan, approval_id: &str, authority: &dyn Authority) -> std::result::Result<ApprovalView, String> {
    if !valid_approval_id(approval_id) {
        return Err("approval id 모양이 아니다(ap_<32 hex>)".into());
    }
    if !plan.runnable() {
        return Err("거부 사유가 남은 계획은 승인을 소비하지 않는다".into());
    }
    let view = authority.consume(approval_id, &plan.scope, &plan.controller)?;
    if view.action != ACTION || view.scope != plan.scope || view.consumed_at_ms.is_none()
        || view.consumed_by.as_deref() != Some(plan.controller.as_str())
    {
        return Err("나쵸가 돌려준 승인이 이 계획과 맞지 않는다".into());
    }
    Ok(view)
}

/// 대상 기기에 가는 작업 요청. `authority` 는 조종 기기가 믿는 승인 기기라는 **주장**일 뿐이다 —
/// 대상은 그것으로 신뢰원을 고르지 않는다(`trust_source`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRequest {
    pub job: Job,
    pub approval_id: String,
    pub authority: String,
}

/// 대상이 승인을 읽을 곳.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustSource {
    /// 이 기기의 나쵸 앱 키로 나쵸에 직접.
    Local,
    /// 사람이 명부 파일에 위임을 적은 기기의 카사텀이 읽기만 중계.
    Relay(String),
}

/// 대상 쪽 신뢰원 — 키가 있으면 `None`(자기 나쵸), 없으면 위임된 기기의 중계. `connect` 는 위임된 machine_id 로만
/// 불리고, 요청이 댄 기기로는 연결조차 하지 않는다.
pub fn target_authority(
    local_key: bool,
    entries: &[serde_json::Value],
    requested: &str,
    connect: impl FnOnce(&str) -> std::result::Result<Fetch, String>,
) -> std::result::Result<Option<RelayAuthority>, String> {
    match trust_source(local_key, delegated_authority(entries), requested)? {
        TrustSource::Local => Ok(None),
        TrustSource::Relay(machine_id) => Ok(Some(RelayAuthority { fetch: connect(&machine_id)?, machine_id })),
    }
}

/// 신뢰원은 이 기기 안에서만 정해진다. 요청이 대는 기기는 고르는 데 쓰지 않고, 위임된 기기와 다르면
/// 거부해 잘못 걸린 요청을 드러낸다 — 요청자가 명부의 아무 기기나 대서 그 기기의 답을 믿게 만들 수 없다.
pub fn trust_source(local_key: bool, delegated: std::result::Result<String, String>, requested: &str) -> std::result::Result<TrustSource, String> {
    if local_key {
        return Ok(TrustSource::Local);
    }
    let delegated = delegated?;
    if requested != delegated {
        return Err(format!("요청이 댄 승인 기기({requested})는 이 기기가 위임한 기기({delegated})가 아니다"));
    }
    Ok(TrustSource::Relay(delegated))
}

/// 명부에 있다는 것은 위임이 아니다. 명부 **파일**에 사람이 `"restart_approvals": true` 와 `machine_id` 를 함께
/// 적은 항목 하나만 승인 중계원이다. 손님 항목은 파일에 없고, 폴링으로 배운 id 는 상대가 스스로 댄 값이라 안 본다.
pub fn delegated_authority(entries: &[serde_json::Value]) -> std::result::Result<String, String> {
    let marked: Vec<&serde_json::Value> = entries.iter().filter(|e| e["restart_approvals"] == serde_json::Value::Bool(true)).collect();
    let [entry] = marked.as_slice() else {
        return Err(if marked.is_empty() {
            "재시작 승인을 중계할 기기가 명부 파일에 위임돼 있지 않다(restart_approvals)".into()
        } else {
            "재시작 승인 위임이 명부 파일에 여럿이다 — 하나만 둔다".into()
        });
    };
    entry["machine_id"].as_str()
        .filter(|id| (8..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
        .map(str::to_string)
        .ok_or_else(|| "위임 항목에 machine_id 가 적혀 있지 않다".into())
}

/// 위임된 기기의 카사텀이 중계한 승인(`GET /app/restart/approvals/{id}`). 답이 스스로 밝힌 기기가 위임된
/// 기기여야 한다 — 끊긴 터널 자리를 다른 경로가 대신 받은 답을 거른다. 소비는 여기서 하지 않는다.
pub struct RelayAuthority {
    pub machine_id: String,
    pub fetch: Fetch,
}

/// 중계 기기에 경로 하나를 GET 해 JSON 을 받는다.
pub type Fetch = Box<dyn Fn(&str) -> std::result::Result<serde_json::Value, String> + Send + Sync>;

pub fn relayed_view(reply: &serde_json::Value, machine_id: &str) -> std::result::Result<ApprovalView, String> {
    if reply["machine_id"].as_str() != Some(machine_id) {
        return Err("위임한 기기가 아닌 곳이 승인을 답했다".into());
    }
    serde_json::from_value(reply["approval"].clone()).map_err(|_| "중계된 승인을 읽지 못했다".into())
}

impl Authority for RelayAuthority {
    fn get(&self, approval_id: &str) -> std::result::Result<ApprovalView, String> {
        if !valid_approval_id(approval_id) {
            return Err("approval id 모양이 아니다".into());
        }
        relayed_view(&(self.fetch)(&format!("/app/restart/approvals/{approval_id}"))?, &self.machine_id)
    }

    fn consume(&self, _: &str, _: &serde_json::Value, _: &str) -> std::result::Result<ApprovalView, String> {
        Err("승인 소비는 나쵸 앱 키가 있는 조종 기기에서만 한다".into())
    }
}

/// 대상 쪽 — 요청이 말하는 것을 믿지 않고, 나쵸에서 읽은 승인과 지금 이 기기의 사실로만 판정한다.
pub fn authorize_target(req: &JobRequest, facts: &Facts, authority: &dyn Authority, now_ms: u64) -> std::result::Result<(), String> {
    if let Some(refusal) = refusals(&facts.machine_id, facts, now_ms).first() {
        return Err(refusal.message());
    }
    if !valid_approval_id(&req.approval_id) {
        return Err("approval id 모양이 아니다".into());
    }
    let hash = target_hash(facts);
    let job = &req.job;
    if job.machine_id != facts.machine_id || job.target_hash != hash || job.job_id != job_id(&job.plan_hash, &facts.machine_id) {
        return Err("작업이 이 기기의 지금 정체와 맞지 않는다".into());
    }
    let view = authority.get(&req.approval_id)?;
    if view.id != req.approval_id {
        return Err("요청한 승인이 아닌 승인이 돌아왔다".into());
    }
    if view.action != ACTION || view.state != "approved" {
        return Err(format!("승인되지 않았다({} {})", view.action, view.state));
    }
    if view.consumed_at_ms.is_none() || view.consumed_by.as_deref() != view.scope["controller"].as_str() {
        return Err("조종 기기가 이 승인을 소비하지 않았다".into());
    }
    if now_ms >= view.expires_at_ms {
        return Err("승인이 만료됐다".into());
    }
    let mine = view.scope["targets"].as_array().into_iter().flatten().any(|t| {
        t["machine_id"].as_str() == Some(facts.machine_id.as_str()) && t["hash"].as_str() == Some(hash.as_str())
    });
    if view.scope["plan"].as_str() != Some(job.plan_hash.as_str()) || !mine {
        return Err("승인한 대상에 이 기기의 지금 정체가 없다".into());
    }
    Ok(())
}

/// 대상 쪽 수락 — 판정을 통과하면 작업을 적는다. 같은 작업이 다시 오면 새로 만들지 않는다(`false`).
pub fn accept_job(req: &JobRequest, facts: &Facts, authority: &dyn Authority, dir: &Path, now_ms: u64) -> std::result::Result<bool, String> {
    authorize_target(req, facts, authority, now_ms)?;
    create_job(dir, &req.job).map(|(_, created)| created).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- 작업 기록

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Accepted,
    HelperStarted,
    Exited,
    Relaunching,
    Launched,
    Booted,
    Verified,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn parse(word: &str) -> Option<Self> {
        serde_json::from_value(json!(word)).ok()
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::HelperStarted => "helper_started",
            Self::Exited => "exited",
            Self::Relaunching => "relaunching",
            Self::Launched => "launched",
            Self::Booted => "booted",
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn terminal(self) -> bool {
        matches!(self, Self::Verified | Self::Failed | Self::Cancelled)
    }

    /// 앞으로만 간다. 끝난 작업은 다시 안 움직이고, 취소는 앱을 끄기 전(도우미 시작 전)까지만 된다.
    fn allows(self, next: Self) -> bool {
        if self.terminal() {
            return false;
        }
        match next {
            Self::Failed => true,
            Self::Cancelled => self == Self::Accepted,
            _ => next > self,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub schema: String,
    pub job_id: String,
    pub plan_hash: String,
    pub machine_id: String,
    pub target_hash: String,
    pub old_pid: u32,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub at_s: u64,
    pub state: JobState,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub job: Job,
    pub state: JobState,
    pub events: Vec<JobEvent>,
}

/// 작업 id — 같은 계획의 같은 기기는 늘 같은 id 라, 다시 보내도 새 작업이 생기지 않는다.
pub fn job_id(plan_hash: &str, machine_id: &str) -> String {
    format!("rs{}", fnv(&[plan_hash, machine_id]))
}

pub fn valid_job_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 작업 기록 자리. 격리 리그는 그 collab 루트 아래로 간다.
pub fn jobs_dir() -> Result<PathBuf> {
    if let Some(root) = crate::isolated_collab_root() {
        return Ok(root.join("app-restart"));
    }
    Ok(crate::home_dir().context("home directory unavailable")?.join(".config/kasaterm/app-restart"))
}

fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn events_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.events"))
}

/// 작업을 적는다. 같은 id 가 이미 있으면 **새로 만들지 않고** 그 기록을 돌려준다(`false`).
/// 같은 id 인데 다른 계획이면 거부한다 — 재전송이 아니라 충돌이다.
pub fn create_job(dir: &Path, job: &Job) -> Result<(Job, bool)> {
    ensure!(valid_job_id(&job.job_id), "invalid job id");
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = record_path(dir, &job.job_id);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(serde_json::to_string_pretty(job)?.as_bytes())?;
            file.sync_all()?;
            append_line(dir, &job.job_id, JobState::Accepted, "")?;
            Ok((job.clone(), true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing: Job = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            ensure!(
                existing.plan_hash == job.plan_hash && existing.machine_id == job.machine_id,
                "job id collision: 같은 id 에 다른 계획이 있다"
            );
            Ok((existing, false))
        }
        Err(e) => Err(e.into()),
    }
}

fn append_line(dir: &Path, id: &str, state: JobState, note: &str) -> Result<()> {
    let note: String = note.chars().filter(|c| !c.is_control()).take(300).collect();
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(events_path(dir, id))?;
    writeln!(f, "{at} {} {note}", state.word())?;
    Ok(())
}

/// 상태를 한 칸 옮긴다. 뒤로 가거나 끝난 작업을 움직이려 하면 거부한다.
pub fn advance(dir: &Path, id: &str, next: JobState, note: &str) -> Result<JobState> {
    let status = job_status(dir, id)?;
    ensure!(status.state.allows(next), "{} → {} 는 갈 수 없다", status.state.word(), next.word());
    append_line(dir, id, next, note)?;
    Ok(next)
}

/// 기록을 읽어 지금 상태로 접는다. 도우미(셸)는 `<id>.events` 에 줄만 덧붙이고, 접기는 여기서 한다 —
/// 순서를 어긴 줄은 상태를 못 바꾸고 기록에만 남는다.
pub fn job_status(dir: &Path, id: &str) -> Result<JobStatus> {
    ensure!(valid_job_id(id), "invalid job id");
    let job: Job = serde_json::from_str(&std::fs::read_to_string(record_path(dir, id)).context("no such job")?)?;
    let text = std::fs::read_to_string(events_path(dir, id)).unwrap_or_default();
    let mut state = JobState::Accepted;
    let mut events = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(at), Some(word)) = (parts.next(), parts.next()) else { continue };
        let (Ok(at_s), Some(next)) = (at.parse::<u64>(), JobState::parse(word)) else { continue };
        let note = parts.next().unwrap_or("").to_string();
        if next == JobState::Accepted && events.is_empty() {
            events.push(JobEvent { at_s, state: next, note });
            continue;
        }
        if state.allows(next) {
            state = next;
        }
        events.push(JobEvent { at_s, state: next, note });
    }
    Ok(JobStatus { job, state, events })
}

/// 이 기기에서 아직 안 끝난 작업(한 시간 안에 생긴 것). 둘째 재시작을 막는 데 쓴다.
pub fn active_job(dir: &Path, now_ms: u64) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries.flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().strip_suffix(".json")?.to_string();
        let status = job_status(dir, &name).ok()?;
        (!status.state.terminal() && now_ms.saturating_sub(status.job.created_at_ms) < 60 * 60_000).then_some(name)
    }).next()
}

/// 새로 뜬 앱이 부팅 때 부른다 — 이 기기의 작업이 재기동을 기다리고 있으면 도착을 적는다.
pub fn mark_booted(dir: &Path, machine_id: &str, pid: u32, build: &str, now_ms: u64) -> Option<String> {
    let id = active_job(dir, now_ms)?;
    let status = job_status(dir, &id).ok()?;
    let waiting = matches!(status.state, JobState::HelperStarted | JobState::Exited | JobState::Relaunching | JobState::Launched);
    (status.job.machine_id == machine_id && waiting && pid != status.job.old_pid).then(|| {
        let _ = advance(dir, &id, JobState::Booted, &format!("pid {pid} build {build}"));
        id
    })
}

// ---------------------------------------------------------------- 도우미

/// 앱 밖에서 도는 재기동 도우미. 앱이 스스로 종료하기 직전에 띄우고, 앱이 사라진 뒤에도 산다.
#[derive(Clone, Debug)]
pub struct HelperSpec {
    pub dir: PathBuf,
    pub job_id: String,
    pub old_pid: u32,
    /// 생존·재기동을 재는 실행 파일 경로(`Facts::app_exe`).
    pub app_exe: String,
    /// 다시 띄우는 명령. 운영은 `launch_command(app_path)` 뿐이다.
    pub launch: Vec<String>,
    pub exit_timeout_s: u32,
    pub boot_timeout_s: u32,
}

/// 운영에서 쓰는 유일한 재기동 명령 — LaunchServices 가 띄우므로 새 앱은 도우미의 env 를 안 물려받는다.
pub fn launch_command(app_path: &str) -> Result<Vec<String>> {
    ensure!(valid_app_path(app_path), "launch path is not an installed kasaterm.app: {app_path}");
    Ok(vec!["/usr/bin/open".into(), "-a".into(), app_path.into()])
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 도우미 셸 스크립트. 강제 종료가 없고, 기다림엔 상한이 있고, 상태는 `<id>.events` 에 줄로만 남긴다.
/// 앱은 명령줄이 **실행 파일 경로로 시작하는** 프로세스로만 센다 — 도우미 자신의 명령줄에도 그
/// 경로 글자가 들어 있어, 부분일치로 세면 자기를 「이미 떠 있는 다른 앱」으로 읽는다(검사로 잡았다).
pub fn helper_script(spec: &HelperSpec) -> Result<String> {
    ensure!(valid_job_id(&spec.job_id), "invalid job id");
    ensure!(!spec.launch.is_empty(), "launch command is empty");
    let events = sq(&events_path(&spec.dir, &spec.job_id).display().to_string());
    let exe = sq(&spec.app_exe);
    let launch = spec.launch.iter().map(|a| sq(a)).collect::<Vec<_>>().join(" ");
    let (pid, exit_ticks, boot_ticks) = (spec.old_pid, spec.exit_timeout_s * 5, spec.boot_timeout_s * 5);
    Ok(format!(
        r#"ev() {{ printf '%s %s %s\n' "$(/bin/date +%s)" "$1" "$2" >> {events}; }}
running() {{ /bin/ps -Axww -o pid=,command= | /usr/bin/awk -v exe={exe} -v skip="$1" '{{ cmd = $0; sub(/^ *[0-9]+ /, "", cmd); if (index(cmd, exe) == 1 && $1 != skip) {{ print $1; exit }} }}'; }}
ev helper_started "pid $$"
i=0
while /bin/kill -0 {pid} 2>/dev/null; do
  i=$((i+1)); [ "$i" -gt {exit_ticks} ] && {{ ev failed "app did not exit in time; not forcing"; exit 1; }}
  /bin/sleep 0.2
done
ev exited ""
other=$(running {pid})
[ -n "$other" ] && {{ ev failed "another instance is already running (pid $other); not launching a second"; exit 1; }}
ev relaunching ""
{launch} || {{ ev failed "launch command failed"; exit 1; }}
j=0
while :; do
  new=$(running {pid})
  [ -n "$new" ] && break
  j=$((j+1)); [ "$j" -gt {boot_ticks} ] && {{ ev failed "no new app process in time"; exit 1; }}
  /bin/sleep 0.2
done
ev launched "pid $new"
"#
    ))
}

/// 도우미에게 넘기는 env — 허용목록뿐이다. claude 마커·토큰·카사텀 소켓 경로가 새지 않게.
pub fn helper_env(get: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut env = vec![("PATH".to_string(), "/usr/bin:/bin:/usr/sbin:/sbin".to_string())];
    for key in ["HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE"] {
        if let Some(value) = get(key).filter(|v| !v.is_empty()) {
            env.push((key.to_string(), value));
        }
    }
    env
}

/// 도우미를 띄운다 — 새 프로세스 그룹, stdin 없음, 출력은 작업 옆 로그.
#[cfg(unix)]
pub fn spawn_helper(spec: &HelperSpec) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    let script = helper_script(spec)?;
    std::fs::create_dir_all(&spec.dir)?;
    let log = std::fs::File::create(spec.dir.join(format!("{}.helper.log", spec.job_id)))?;
    let err = log.try_clone()?;
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env_clear()
        .envs(helper_env(&|k| std::env::var(k).ok()))
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err)
        .process_group(0)
        .spawn()?;
    Ok(child.id())
}

// ---------------------------------------------------------------- 순차 실행

/// 기기 하나에 닿는 길. 운영은 앱 소켓·명부 경유 HTTP, 검사는 가짜.
pub trait Transport {
    fn facts(&self, machine_id: &str) -> std::result::Result<Facts, String>;
    /// 대상 앱에 작업을 건다. 대상이 나쵸 승인과 자기 사실로 다시 판정하고 거부할 수 있다.
    fn start(&self, machine_id: &str, req: &JobRequest) -> std::result::Result<(), String>;
    fn status(&self, machine_id: &str, job_id: &str) -> std::result::Result<JobState, String>;
}

#[derive(Clone, Debug)]
pub struct RunPolicy {
    pub poll_every: std::time::Duration,
    pub boot_timeout: std::time::Duration,
    /// 재기동 중 연결이 끊기는 것은 정상이다 — 연달아 이만큼 못 물으면 실패로 친다.
    pub max_status_errors: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TargetOutcome {
    Verified { job_id: String, new_pid: u32 },
    /// 조종 기기 — 자기를 끄고 나면 여기서 확인할 수 없다. 재기동 뒤 `status` 로 본다.
    HandedOff { job_id: String },
    Failed { job_id: Option<String>, reason: String },
    Skipped { reason: String },
}

/// 계획대로 한 대씩 재시작한다. 먼저 나쵸에서 승인을 소비하고, 실패하면 아무 기기도 건드리지 않는다.
pub fn run(
    plan: &Plan,
    approval_id: &str,
    authority_machine: &str,
    authority: &dyn Authority,
    transport: &dyn Transport,
    policy: &RunPolicy,
    now_ms: &dyn Fn() -> u64,
) -> Vec<(String, TargetOutcome)> {
    let mut out = Vec::new();
    let mut stop: Option<String> = None;
    if let Err(reason) = authorize_run(plan, approval_id, authority) {
        stop = Some(format!("승인을 쓰지 못해 시작하지 않았다 · {reason}"));
    }
    let request = |job: Job| JobRequest { job, approval_id: approval_id.into(), authority: authority_machine.into() };
    for target in &plan.targets {
        let id = target.machine_id.clone();
        if let Some(reason) = &stop {
            out.push((id, TargetOutcome::Skipped { reason: reason.clone() }));
            continue;
        }
        let outcome = run_one(plan, target, transport, policy, now_ms, &request);
        if !matches!(outcome, TargetOutcome::Verified { .. } | TargetOutcome::HandedOff { .. }) {
            stop = Some(format!("앞 기기({id})가 끝나지 않아 멈췄다"));
        }
        out.push((id, outcome));
    }
    out
}

fn run_one(
    plan: &Plan,
    target: &PlanTarget,
    transport: &dyn Transport,
    policy: &RunPolicy,
    now_ms: &dyn Fn() -> u64,
    request: &dyn Fn(Job) -> JobRequest,
) -> TargetOutcome {
    let id = &target.machine_id;
    let fail = |job_id: Option<String>, reason: String| TargetOutcome::Failed { job_id, reason };
    let facts = match transport.facts(id) {
        Ok(facts) => facts,
        Err(e) => return fail(None, format!("실행 직전 사실을 못 읽었다 · {e}")),
    };
    if let Some(refusal) = refusals(id, &facts, now_ms()).first() {
        return fail(None, refusal.message());
    }
    if target_hash(&facts) != target.hash {
        return fail(None, "계획 뒤 대상이 바뀌었다(pid·바이너리·자기설치 예정 중 하나) — 다시 계획해야 한다".into());
    }
    let job = Job {
        schema: SCHEMA.into(),
        job_id: job_id(&plan.hash, id),
        plan_hash: plan.hash.clone(),
        machine_id: id.clone(),
        target_hash: target.hash.clone(),
        old_pid: facts.pid,
        created_at_ms: now_ms(),
    };
    let job_id = job.job_id.clone();
    let old_pid = job.old_pid;
    if let Err(e) = transport.start(id, &request(job)) {
        return fail(Some(job_id), format!("대상이 작업을 받지 않았다 · {e}"));
    }
    if target.controller {
        return TargetOutcome::HandedOff { job_id: job_id.clone() };
    }
    let started = std::time::Instant::now();
    let mut errors = 0;
    loop {
        match transport.status(id, &job_id.clone()) {
            Ok(JobState::Booted) | Ok(JobState::Verified) => break,
            Ok(JobState::Failed) => return fail(Some(job_id.clone()), "대상 도우미가 실패를 적었다 — 기록을 확인".into()),
            Ok(JobState::Cancelled) => return fail(Some(job_id.clone()), "작업이 취소됐다".into()),
            Ok(_) => errors = 0,
            Err(_) => {
                errors += 1;
                if errors > policy.max_status_errors {
                    return fail(Some(job_id.clone()), "재기동 뒤 다시 닿지 못했다 — 사람 확인 필요".into());
                }
            }
        }
        if started.elapsed() > policy.boot_timeout {
            return fail(Some(job_id.clone()), "제한 시간 안에 새 앱이 도착하지 않았다 — 사람 확인 필요".into());
        }
        std::thread::sleep(policy.poll_every);
    }
    match transport.facts(id) {
        Ok(after) if after.pid != old_pid && after.binary == facts.binary && after.machine_id == *id => {
            TargetOutcome::Verified { job_id: job_id.clone(), new_pid: after.pid }
        }
        Ok(after) if after.binary != facts.binary => fail(Some(job_id.clone()), "다시 뜬 앱의 바이너리가 다르다 — 재시작이 업그레이드가 됐다".into()),
        Ok(_) => fail(Some(job_id.clone()), "새 pid 가 옛 pid 와 같다 — 재기동되지 않았다".into()),
        Err(e) => fail(Some(job_id.clone()), format!("재기동 뒤 사실을 못 읽었다 · {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const NOW: u64 = 10_000_000;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("app-restart-{tag}-{}-{}", std::process::id(), NOW));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn facts(id: &str, pid: u32) -> Facts {
        Facts {
            schema: SCHEMA.into(),
            machine_id: id.into(),
            label: format!("기기 {id}"),
            os: "macos".into(),
            capability: CAPABILITY,
            app_path: "/Users/x/Applications/kasaterm.app".into(),
            pid,
            running_exe: "/Users/x/Applications/kasaterm.app/Contents/MacOS/kasaterm".into(),
            binary: BinaryId { inode: 7, mtime_ms: 1, build: "abc".into() },
            observed_at_ms: NOW,
            ..Default::default()
        }
    }

    #[test]
    fn every_unsafe_condition_is_a_named_refusal() {
        assert!(refusals("m", &facts("m", 1), NOW).is_empty());
        let mut f = facts("m", 1);
        f.os = "windows".into();
        f.capability = 0;
        f.running_exe = "/repo/target/debug/kasaterm".into();
        f.busy = vec![BusyPane { surface: "%1".into(), character: "치나츠".into(), state: "working".into() }];
        f.dirty_editors = 2;
        f.install_pending = Some(PendingInstall { dist_path: "/repo/dist/kasaterm.app".into(), dist_mtime_ms: 5 });
        f.baking = true;
        f.active_job = Some("rs1234567890".into());
        f.observed_at_ms = NOW - FACTS_TTL_MS - 1;
        let codes: Vec<String> = refusals("other", &f, NOW).iter()
            .map(|r| serde_json::to_value(r).unwrap()["code"].as_str().unwrap().to_string()).collect();
        for code in ["identity_mismatch", "unsupported_os", "capability_missing", "not_installed_app", "busy_students",
            "unsaved_editors", "self_install_pending", "bake_in_progress", "job_in_flight", "stale_facts"] {
            assert!(codes.iter().any(|c| c == code), "{code} 누락: {codes:?}");
        }
    }

    #[test]
    fn only_installed_bundles_can_be_relaunched() {
        assert!(valid_app_path("/Users/x/Applications/kasaterm.app"));
        assert!(valid_app_path("/Applications/kasaterm.app"));
        for bad in ["/tmp/kasaterm.app", "/Users/x/Applications/Other.app", "Applications/kasaterm.app", "/Users/x/Applications/../kasaterm.app"] {
            assert!(!valid_app_path(bad), "{bad}");
            assert!(launch_command(bad).is_err());
        }
        assert_eq!(launch_command("/Applications/kasaterm.app").unwrap(), vec!["/usr/bin/open", "-a", "/Applications/kasaterm.app"]);
    }

    #[test]
    fn controller_goes_last_duplicates_collapse_and_unreachable_is_refused() {
        let lookup = |id: &str| if id == "off" { Err("응답 없음".to_string()) } else { Ok(facts(id, 1)) };
        let plan = build_plan("ctl", &["ctl".into(), "a".into(), "a".into(), "off".into()], &lookup, NOW);
        let order: Vec<&str> = plan.targets.iter().map(|t| t.machine_id.as_str()).collect();
        assert_eq!(order, ["a", "off", "ctl"]);
        assert!(plan.targets[2].controller);
        assert!(matches!(plan.targets[1].refusals[0], Refusal::Unreachable { .. }));
        assert!(!plan.runnable());
    }

    #[test]
    fn plan_hash_moves_with_pid_binary_and_pending_install() {
        let base = target_hash(&facts("m", 1));
        let mut f = facts("m", 2);
        assert_ne!(target_hash(&f), base, "pid");
        f = facts("m", 1);
        f.binary.mtime_ms = 2;
        assert_ne!(target_hash(&f), base, "바이너리");
        f = facts("m", 1);
        f.install_pending = Some(PendingInstall { dist_path: "d".into(), dist_mtime_ms: 1 });
        assert_ne!(target_hash(&f), base, "자기설치 예정");
        f = facts("m", 1);
        f.busy.clear();
        f.observed_at_ms = NOW + 5;
        assert_eq!(target_hash(&f), base, "관측 시각·바쁨은 정체가 아니다(거부는 따로)");
    }

    const AP: &str = "ap_0123456789abcdef0123456789abcdef";

    /// 나쵸 서버 대역 — 상태·만료·범위·소비 기기·한 번을 서버처럼 판정한다.
    struct FakeNacho {
        items: RefCell<HashMap<String, (ApprovalView, serde_json::Value)>>,
    }

    impl FakeNacho {
        fn approving(scope: &serde_json::Value, state: &str, expires_at_ms: u64) -> Self {
            let view = ApprovalView { id: AP.into(), action: ACTION.into(), scope: scope.clone(), state: state.into(), expires_at_ms, ..Default::default() };
            FakeNacho { items: RefCell::new(HashMap::from([(AP.to_string(), (view, scope.clone()))])) }
        }
    }

    impl Authority for FakeNacho {
        fn get(&self, id: &str) -> std::result::Result<ApprovalView, String> {
            self.items.borrow().get(id).map(|(v, _)| v.clone()).ok_or_else(|| "no_approval".into())
        }
        fn consume(&self, id: &str, scope: &serde_json::Value, consumer: &str) -> std::result::Result<ApprovalView, String> {
            let mut items = self.items.borrow_mut();
            let (view, approved) = items.get_mut(id).ok_or("no_approval")?;
            if view.state != "approved" { return Err(format!("not_approved:{}", view.state)); }
            if view.consumed_at_ms.is_some() { return Err("already_used".into()); }
            if NOW >= view.expires_at_ms { return Err("expired".into()); }
            if scope != approved { return Err("scope_changed".into()); }
            if approved["controller"].as_str() != Some(consumer) { return Err("wrong_consumer".into()); }
            view.consumed_at_ms = Some(NOW);
            view.consumed_by = Some(consumer.into());
            Ok(view.clone())
        }
    }

    #[test]
    fn approval_is_consumed_once_on_the_server_for_exactly_this_plan() {
        let lookup = |id: &str| Ok(facts(id, 1));
        let plan = build_plan("ctl", &["a".into(), "ctl".into()], &lookup, NOW);
        assert_eq!(plan.scope["targets"][1]["order"], 2, "대상은 순서대로, 숫자는 정수");
        let nacho = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        assert!(authorize_run(&plan, AP, &nacho).is_ok());
        assert_eq!(authorize_run(&plan, AP, &nacho).unwrap_err(), "already_used", "한 번만");
        for state in ["pending", "denied", "expired"] {
            let n = FakeNacho::approving(&plan.scope, state, NOW + 1000);
            assert!(authorize_run(&plan, AP, &n).unwrap_err().starts_with("not_approved"), "{state}");
        }
        assert_eq!(authorize_run(&plan, AP, &FakeNacho::approving(&plan.scope, "approved", NOW)).unwrap_err(), "expired");
        let changed = build_plan("ctl", &["a".into(), "ctl".into()], &|id: &str| Ok(facts(id, if id == "a" { 2 } else { 1 })), NOW);
        let n = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        assert_eq!(authorize_run(&changed, AP, &n).unwrap_err(), "scope_changed", "승인 뒤 pid 가 바뀌었다");
        let other = build_plan("a", &["a".into(), "ctl".into()], &lookup, NOW);
        let n = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        assert!(authorize_run(&other, AP, &n).is_err(), "다른 조종 기기");
        assert!(authorize_run(&plan, "approved:true", &nacho).is_err(), "자기주장은 id 모양부터 거부");
    }

    fn request_for(plan: &Plan, id: &str, pid: u32) -> JobRequest {
        let f = facts(id, pid);
        JobRequest {
            job: Job { schema: SCHEMA.into(), job_id: job_id(&plan.hash, id), plan_hash: plan.hash.clone(), machine_id: id.into(), target_hash: target_hash(&f), old_pid: pid, created_at_ms: NOW },
            approval_id: AP.into(),
            authority: "ctl".into(),
        }
    }

    #[test]
    fn a_target_believes_only_what_nacho_says_and_its_own_facts() {
        let lookup = |id: &str| Ok(facts(id, 1));
        let plan = build_plan("ctl", &["a".into(), "ctl".into()], &lookup, NOW);
        let req = request_for(&plan, "a", 1);
        let pending = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        assert!(authorize_target(&req, &facts("a", 1), &pending, NOW).unwrap_err().contains("소비"), "조종 쪽 소비 전");
        let nacho = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        authorize_run(&plan, AP, &nacho).unwrap();
        assert!(authorize_target(&req, &facts("a", 1), &nacho, NOW).is_ok());
        assert!(authorize_target(&req, &facts("a", 2), &nacho, NOW).is_err(), "그 사이 pid 가 바뀐 대상");
        assert!(authorize_target(&req, &facts("b", 1), &nacho, NOW).is_err(), "다른 기기에 온 작업");
        assert!(authorize_target(&req, &facts("a", 1), &nacho, NOW + 1000).is_err(), "만료");
        let mut busy = facts("a", 1);
        busy.busy = vec![BusyPane { surface: "%1".into(), character: "치나츠".into(), state: "working".into() }];
        assert!(authorize_target(&req, &busy, &nacho, NOW).is_err(), "그 사이 학생이 일을 시작했다");
        let nobody = FakeNacho { items: RefCell::default() };
        assert_eq!(authorize_target(&req, &facts("a", 1), &nobody, NOW).unwrap_err(), "no_approval", "나쵸에 없는 승인");
        let mut lie = req.clone();
        lie.job.plan_hash = "0000".into();
        assert!(authorize_target(&lie, &facts("a", 1), &nacho, NOW).is_err(), "요청이 다른 계획을 말한다");
    }

    fn entries(value: serde_json::Value) -> Vec<serde_json::Value> {
        value.as_array().cloned().unwrap()
    }

    #[test]
    fn registration_alone_is_not_a_delegation_to_vouch_for_approvals() {
        let id = "4af3d95d64374ea3bdf951942caf19b2";
        assert!(delegated_authority(&entries(serde_json::json!([{"label": "mini", "machine_id": id}]))).is_err(), "등록만으로는 위임이 아니다");
        assert_eq!(delegated_authority(&entries(serde_json::json!([{"label": "mini", "machine_id": id, "restart_approvals": true}]))), Ok(id.to_string()));
        assert!(delegated_authority(&entries(serde_json::json!([{"label": "mini", "machine_id": id, "restart_approvals": "true"}]))).is_err(), "참 값만");
        assert!(delegated_authority(&entries(serde_json::json!([{"label": "mini", "restart_approvals": true}]))).is_err(), "폴링으로 배운 id 는 위임이 아니다");
        assert!(delegated_authority(&entries(serde_json::json!([
            {"label": "mini", "machine_id": id, "restart_approvals": true},
            {"label": "evil", "machine_id": "evil-machine", "restart_approvals": true},
        ]))).is_err(), "위임이 둘이면 아무것도 안 믿는다");
    }

    #[test]
    fn the_request_cannot_pick_who_vouches_for_the_approval() {
        assert_eq!(trust_source(true, Err("없음".into()), "evil-machine"), Ok(TrustSource::Local), "키가 있으면 요청이 무엇을 대든 자기 나쵸");
        assert_eq!(trust_source(false, Ok("ctl-machine".into()), "ctl-machine"), Ok(TrustSource::Relay("ctl-machine".into())));
        assert!(trust_source(false, Ok("ctl-machine".into()), "evil-machine").is_err(), "명부에 있는 다른 기기");
        assert!(trust_source(false, Err("위임 없음".into()), "ctl-machine").is_err(), "위임이 없으면 중계도 없다");
    }

    /// 승인 쪽이 무엇을 묻든 같은 view 를 돌려주는 거짓 창구.
    struct Says(ApprovalView);

    impl Authority for Says {
        fn get(&self, _: &str) -> std::result::Result<ApprovalView, String> {
            Ok(self.0.clone())
        }
        fn consume(&self, _: &str, _: &serde_json::Value, _: &str) -> std::result::Result<ApprovalView, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn forged_or_misrouted_approvals_never_start_a_restart() {
        let lookup = |id: &str| Ok(facts(id, 1));
        let plan = build_plan("ctl", &["a".into(), "ctl".into()], &lookup, NOW);
        let mut req = request_for(&plan, "a", 1);
        let forged = ApprovalView {
            id: AP.into(), action: ACTION.into(), scope: plan.scope.clone(), state: "approved".into(),
            expires_at_ms: NOW + 1000, consumed_at_ms: Some(NOW), consumed_by: Some("ctl".into()), ..Default::default()
        };
        let registry = entries(serde_json::json!([
            {"label": "ctl", "machine_id": "ctl-machine", "restart_approvals": true},
            {"label": "evil", "machine_id": "evil-machine"},
        ]));
        let connected = RefCell::new(Vec::new());
        let answering = |who: &'static str, view: &ApprovalView| {
            let reply = serde_json::json!({"machine_id": who, "approval": view});
            let connected = &connected;
            move |id: &str| -> std::result::Result<Fetch, String> {
                connected.borrow_mut().push(id.to_string());
                Ok(Box::new(move |_: &str| Ok(reply.clone())))
            }
        };

        req.authority = "evil-machine".into();
        assert!(target_authority(false, &registry, &req.authority, answering("evil-machine", &forged)).is_err(), "명부에 있는 다른 기기를 대도");
        let unmarked = entries(serde_json::json!([{"label": "ctl", "machine_id": "ctl-machine"}]));
        assert!(target_authority(false, &unmarked, "ctl-machine", answering("ctl-machine", &forged)).is_err(), "위임이 없으면");
        assert!(connected.borrow().is_empty(), "고르지 못한 기기에는 연결조차 안 한다");

        req.authority = "ctl-machine".into();
        let run = |who: &'static str, view: &ApprovalView| {
            let relay = target_authority(false, &registry, &req.authority, answering(who, view)).unwrap().unwrap();
            authorize_target(&req, &facts("a", 1), &relay, NOW)
        };
        assert!(run("evil-machine", &forged).unwrap_err().contains("위임한 기기가 아닌"), "끊긴 터널 자리를 다른 기기가 받아 답했다");
        let raw = RelayAuthority { machine_id: "ctl-machine".into(), fetch: Box::new(|_| Ok(serde_json::json!({"approved": true, "id": AP}))) };
        assert!(authorize_target(&req, &facts("a", 1), &raw, NOW).is_err(), "기기를 안 밝힌 자기주장");
        let mut other_consumer = forged.clone();
        other_consumer.consumed_by = Some("a".into());
        assert!(run("ctl-machine", &other_consumer).is_err(), "조종 기기가 아닌 기기가 썼다");
        let mut other_id = forged.clone();
        other_id.id = "ap_ffffffffffffffffffffffffffffffff".into();
        assert!(run("ctl-machine", &other_id).is_err(), "다른 승인");
        assert!(run("ctl-machine", &forged).is_ok(), "위임한 기기가 밝힌 소비된 승인만 통과한다");
        assert_eq!(connected.borrow().iter().filter(|id| id.as_str() != "ctl-machine").count(), 0);

        assert!(authorize_run(&plan, AP, &Says(other_consumer.clone())).is_err(), "조종 쪽도 남이 쓴 소비를 안 받는다");
        let nacho = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        assert_eq!(nacho.consume(AP, &plan.scope, "a").unwrap_err(), "wrong_consumer", "나쵸 계약: 소비는 scope 의 조종 기기만");
        assert!(RelayAuthority { machine_id: "ctl-machine".into(), fetch: Box::new(|_| unreachable!()) }.consume(AP, &plan.scope, "ctl").is_err(), "중계는 소비하지 않는다");
    }

    #[test]
    fn one_approval_creates_at_most_one_job_per_machine() {
        let dir = tmp("once");
        let lookup = |id: &str| Ok(facts(id, 1));
        let plan = build_plan("ctl", &["a".into(), "ctl".into()], &lookup, NOW);
        let nacho = FakeNacho::approving(&plan.scope, "approved", NOW + 1000);
        authorize_run(&plan, AP, &nacho).unwrap();
        let req = request_for(&plan, "a", 1);
        assert_eq!(accept_job(&req, &facts("a", 1), &nacho, &dir, NOW), Ok(true));
        assert_eq!(accept_job(&req, &facts("a", 1), &nacho, &dir, NOW), Ok(false), "같은 승인을 다시 보내도 새 작업은 없다");
        assert!(accept_job(&request_for(&plan, "a", 2), &facts("a", 2), &nacho, &dir, NOW).is_err(), "재기동 뒤(새 pid) 같은 승인은 대상이 아니다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn job(id: &str) -> Job {
        Job { schema: SCHEMA.into(), job_id: job_id("plan", id), plan_hash: "plan".into(), machine_id: id.into(), target_hash: "t".into(), old_pid: 41, created_at_ms: NOW }
    }

    #[test]
    fn resending_a_job_does_not_create_or_rerun_it() {
        let dir = tmp("dup");
        let j = job("m");
        let (_, created) = create_job(&dir, &j).unwrap();
        assert!(created);
        advance(&dir, &j.job_id, JobState::HelperStarted, "").unwrap();
        let (again, created) = create_job(&dir, &j).unwrap();
        assert!(!created, "같은 id 는 새 작업이 아니다");
        assert_eq!(again, j);
        assert_eq!(job_status(&dir, &j.job_id).unwrap().state, JobState::HelperStarted, "재전송이 상태를 되돌리지 않는다");
        let clash = Job { plan_hash: "other".into(), ..j.clone() };
        assert!(create_job(&dir, &clash).is_err(), "같은 id 다른 계획은 충돌");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn states_only_move_forward_and_cancel_only_before_the_helper() {
        let dir = tmp("fsm");
        let a = job("a");
        create_job(&dir, &a).unwrap();
        advance(&dir, &a.job_id, JobState::Exited, "").unwrap();
        assert!(advance(&dir, &a.job_id, JobState::HelperStarted, "").is_err(), "뒤로 못 간다");
        assert!(advance(&dir, &a.job_id, JobState::Cancelled, "").is_err(), "앱을 끈 뒤엔 취소 없음");
        advance(&dir, &a.job_id, JobState::Failed, "x").unwrap();
        assert!(advance(&dir, &a.job_id, JobState::Booted, "").is_err(), "끝난 작업은 안 움직인다");
        let b = job("b");
        create_job(&dir, &b).unwrap();
        advance(&dir, &b.job_id, JobState::Cancelled, "사람이 취소").unwrap();
        assert!(active_job(&dir, NOW).is_none(), "끝난 작업만 남았다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_app_marks_arrival_only_for_its_own_waiting_job() {
        let dir = tmp("boot");
        let j = job("m");
        create_job(&dir, &j).unwrap();
        assert!(mark_booted(&dir, "m", 99, "b", NOW).is_none(), "도우미가 아직 안 떴으면 도착이 아니다");
        advance(&dir, &j.job_id, JobState::Launched, "pid 99").unwrap();
        assert!(mark_booted(&dir, "other", 99, "b", NOW).is_none(), "다른 기기의 작업");
        assert!(mark_booted(&dir, "m", 41, "b", NOW).is_none(), "옛 pid 는 도착이 아니다");
        assert_eq!(mark_booted(&dir, "m", 99, "b", NOW), Some(j.job_id.clone()));
        assert_eq!(job_status(&dir, &j.job_id).unwrap().state, JobState::Booted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn helper_env_is_an_allowlist() {
        let env = helper_env(&|k| match k {
            "HOME" => Some("/Users/x".into()),
            "CLAUDECODE" | "CLAUDE_CODE_SESSION_ID" | "KASATERM_SOCKET_PATH" | "OG_API_KEY" => Some("leak".into()),
            _ => None,
        });
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["PATH", "HOME"]);
        assert!(env.iter().all(|(_, v)| v != "leak"));
    }

    fn fake_app(dir: &Path) -> String {
        // 실제 앱처럼 argv[0] 이 번들 안 실행 파일 경로인 프로세스를 흉내 낸다. 시스템 바이너리를
        // 다른 자리로 복사해 돌리면 macOS 가 곧바로 죽이므로(실측), 진짜 sleep 에 argv[0] 만 입힌다.
        let exe = dir.join("kasaterm.app/Contents/MacOS/kasaterm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        exe.display().to_string()
    }

    const FAKE_BODY: &str = "exec -a \"$0\" /bin/sleep 30";

    fn start_fake(exe: &str) -> std::process::Child {
        std::process::Command::new("/bin/bash").args(["-c", FAKE_BODY, exe]).spawn().unwrap()
    }

    fn fake_launch(exe: &str) -> Vec<String> {
        vec!["/bin/bash".into(), "-c".into(), format!("({FAKE_BODY}) >/dev/null 2>&1 &"), exe.into()]
    }

    fn wait_state(dir: &Path, id: &str, want: JobState, secs: u64) -> JobState {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let state = job_status(dir, id).map(|s| s.state).unwrap_or(JobState::Accepted);
            if state == want || state.terminal() || std::time::Instant::now() > until {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn spec(dir: &Path, exe: &str, old_pid: u32, launch: Vec<String>, exit_s: u32) -> HelperSpec {
        HelperSpec { dir: dir.into(), job_id: job_id("plan", "m"), old_pid, app_exe: exe.into(), launch, exit_timeout_s: exit_s, boot_timeout_s: 5 }
    }

    #[cfg(unix)]
    #[test]
    fn helper_waits_for_exit_relaunches_and_records_the_new_pid() {
        let dir = tmp("helper-ok");
        let exe = fake_app(&dir);
        let mut old = start_fake(&exe);
        let j = Job { old_pid: old.id(), ..job("m") };
        create_job(&dir, &j).unwrap();
        let launch = fake_launch(&exe);
        spawn_helper(&spec(&dir, &exe, old.id(), launch, 5)).unwrap();
        assert_eq!(wait_state(&dir, &j.job_id, JobState::HelperStarted, 5), JobState::HelperStarted);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(job_status(&dir, &j.job_id).unwrap().state, JobState::HelperStarted, "앱이 살아 있는 동안은 기다린다");
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_state(&dir, &j.job_id, JobState::Launched, 8), JobState::Launched);
        let note = job_status(&dir, &j.job_id).unwrap().events.last().unwrap().note.clone();
        let new_pid: u32 = note.trim_start_matches("pid ").parse().unwrap();
        assert_ne!(new_pid, j.old_pid);
        let _ = std::process::Command::new("/bin/kill").arg(new_pid.to_string()).status();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn helper_never_forces_an_app_that_does_not_exit() {
        let dir = tmp("helper-stuck");
        let exe = fake_app(&dir);
        let mut app = start_fake(&exe);
        let j = Job { old_pid: app.id(), ..job("m") };
        create_job(&dir, &j).unwrap();
        spawn_helper(&spec(&dir, &exe, app.id(), vec!["/usr/bin/true".into()], 1)).unwrap();
        assert_eq!(wait_state(&dir, &j.job_id, JobState::Failed, 8), JobState::Failed);
        assert!(app.try_wait().unwrap().is_none(), "안 꺼지는 앱을 죽이지 않았다");
        app.kill().unwrap();
        let _ = app.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn helper_refuses_to_launch_a_second_instance() {
        let dir = tmp("helper-second");
        let exe = fake_app(&dir);
        let mut other = start_fake(&exe);
        let j = Job { old_pid: 999_999, ..job("m") };
        create_job(&dir, &j).unwrap();
        spawn_helper(&spec(&dir, &exe, 999_999, vec!["/usr/bin/true".into()], 2)).unwrap();
        assert_eq!(wait_state(&dir, &j.job_id, JobState::Failed, 8), JobState::Failed);
        assert!(job_status(&dir, &j.job_id).unwrap().events.iter().any(|e| e.note.contains("another instance")));
        other.kill().unwrap();
        let _ = other.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn helper_outlives_the_process_that_spawned_it() {
        let dir = tmp("helper-orphan");
        let exe = fake_app(&dir);
        // 앱 대역: 도우미를 띄우고 곧바로 끝나는 부모. 도우미가 부모와 함께 죽으면 재기동이 없다.
        let mut parent = std::process::Command::new("/bin/sh").arg("-c").arg("exec /bin/sleep 1").spawn().unwrap();
        let j = Job { old_pid: parent.id(), ..job("m") };
        create_job(&dir, &j).unwrap();
        let launch = fake_launch(&exe);
        spawn_helper(&spec(&dir, &exe, parent.id(), launch, 5)).unwrap();
        let _ = parent.wait();
        assert_eq!(wait_state(&dir, &j.job_id, JobState::Launched, 10), JobState::Launched);
        let note = job_status(&dir, &j.job_id).unwrap().events.last().unwrap().note.clone();
        let _ = std::process::Command::new("/bin/kill").arg(note.trim_start_matches("pid ")).status();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 격리 E2E 의 기기 한 대 — 번들 경로를 흉내 낸 가짜 앱 프로세스와 그 기기의 작업 기록.
    struct FakeMachine {
        exe: String,
        pid: RefCell<u32>,
        /// 처음 띄운 앱 — 검사 프로세스의 자식이라 끈 뒤 거둬야 좀비로 「살아 있음」이 되지 않는다.
        first: RefCell<Option<std::process::Child>>,
        jobs: PathBuf,
        exits: bool,
    }

    /// 대상 앱이 하는 일을 그대로 한다: 나쵸 승인·자기 사실로 수락 → 도우미 → 스스로 종료. 새 앱은 부팅 때 도착을 적는다.
    struct FakeFleet {
        machines: HashMap<String, FakeMachine>,
        nacho: FakeNacho,
    }

    impl FakeFleet {
        fn machine(root: &Path, id: &str, exits: bool) -> (String, FakeMachine) {
            let dir = root.join(id);
            let exe = fake_app(&dir);
            let child = start_fake(&exe);
            (id.into(), FakeMachine { exe, pid: RefCell::new(child.id()), first: RefCell::new(Some(child)), jobs: dir.join("jobs"), exits })
        }
    }

    impl Transport for FakeFleet {
        fn facts(&self, id: &str) -> std::result::Result<Facts, String> {
            let m = self.machines.get(id).ok_or("모르는 기기")?;
            Ok(Facts { active_job: active_job(&m.jobs, NOW), ..facts(id, *m.pid.borrow()) })
        }
        fn start(&self, id: &str, req: &JobRequest) -> std::result::Result<(), String> {
            let m = self.machines.get(id).ok_or("모르는 기기")?;
            let facts = self.facts(id)?;
            if !accept_job(req, &facts, &self.nacho, &m.jobs, NOW)? {
                return Ok(());
            }
            let old = *m.pid.borrow();
            spawn_helper(&HelperSpec { dir: m.jobs.clone(), job_id: req.job.job_id.clone(), old_pid: old, app_exe: m.exe.clone(), launch: fake_launch(&m.exe), exit_timeout_s: 3, boot_timeout_s: 5 }).map_err(|e| e.to_string())?;
            if m.exits {
                if let Some(mut child) = m.first.borrow_mut().take().filter(|c| c.id() == old) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            Ok(())
        }
        fn status(&self, id: &str, job_id: &str) -> std::result::Result<JobState, String> {
            let m = self.machines.get(id).ok_or("모르는 기기")?;
            let status = job_status(&m.jobs, job_id).map_err(|e| e.to_string())?;
            if status.state == JobState::Launched {
                let new: u32 = status.events.last().unwrap().note.trim_start_matches("pid ").parse().unwrap();
                *m.pid.borrow_mut() = new;
                mark_booted(&m.jobs, id, new, "abc", NOW);
                return Ok(job_status(&m.jobs, job_id).map_err(|e| e.to_string())?.state);
            }
            Ok(status.state)
        }
    }

    fn e2e_policy() -> RunPolicy {
        RunPolicy { poll_every: std::time::Duration::from_millis(50), boot_timeout: std::time::Duration::from_secs(10), max_status_errors: 3 }
    }

    fn wait_booted(fleet: &FakeFleet, id: &str, job: &str) -> JobState {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let state = fleet.status(id, job).unwrap_or(JobState::Accepted);
            if state == JobState::Booted || state.terminal() || std::time::Instant::now() > until {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn reap(fleet: &FakeFleet) {
        for m in fleet.machines.values() {
            if let Some(mut child) = m.first.borrow_mut().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = std::process::Command::new("/bin/kill").arg(m.pid.borrow().to_string()).status();
        }
    }

    #[cfg(unix)]
    #[test]
    fn e2e_remote_first_then_controller_with_one_consumed_approval() {
        let root = tmp("e2e-ok");
        let machines = HashMap::from([FakeFleet::machine(&root, "a", true), FakeFleet::machine(&root, "ctl", true)]);
        let mut fleet = FakeFleet { machines, nacho: FakeNacho { items: RefCell::default() } };
        let plan = build_plan("ctl", &["ctl".into(), "a".into()], &|id: &str| fleet.facts(id), NOW);
        assert!(plan.runnable(), "{:?}", plan.targets.iter().map(|t| &t.refusals).collect::<Vec<_>>());
        fleet.nacho = FakeNacho::approving(&plan.scope, "approved", NOW + 60_000);
        let (old_a, old_ctl) = (*fleet.machines["a"].pid.borrow(), *fleet.machines["ctl"].pid.borrow());
        let out = run(&plan, AP, "ctl", &fleet.nacho, &fleet, &e2e_policy(), &|| NOW);
        assert!(matches!(out[0], (ref id, TargetOutcome::Verified { new_pid, .. }) if id == "a" && new_pid != old_a), "{out:?}");
        let TargetOutcome::HandedOff { job_id: ctl_job } = &out[1].1 else { panic!("{out:?}") };
        assert_eq!(wait_booted(&fleet, "ctl", ctl_job), JobState::Booted, "조종 기기는 넘긴 뒤 새 앱이 도착을 적는다");
        assert_ne!(*fleet.machines["ctl"].pid.borrow(), old_ctl);
        // 같은 계획·승인을 다시 보내도 새로 돌지 않는다 — 승인은 이미 쓰였다.
        let again = run(&plan, AP, "ctl", &fleet.nacho, &fleet, &e2e_policy(), &|| NOW);
        assert!(again.iter().all(|(_, o)| matches!(o, TargetOutcome::Skipped { .. })), "{again:?}");
        reap(&fleet);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn e2e_remote_failure_leaves_the_controller_untouched() {
        let root = tmp("e2e-fail");
        let machines = HashMap::from([FakeFleet::machine(&root, "a", false), FakeFleet::machine(&root, "ctl", true)]);
        let mut fleet = FakeFleet { machines, nacho: FakeNacho { items: RefCell::default() } };
        let plan = build_plan("ctl", &["a".into(), "ctl".into()], &|id: &str| fleet.facts(id), NOW);
        fleet.nacho = FakeNacho::approving(&plan.scope, "approved", NOW + 60_000);
        let old_ctl = *fleet.machines["ctl"].pid.borrow();
        let out = run(&plan, AP, "ctl", &fleet.nacho, &fleet, &e2e_policy(), &|| NOW);
        assert!(matches!(out[0].1, TargetOutcome::Failed { .. }), "안 꺼지는 원격 앱은 실패로: {out:?}");
        assert!(matches!(out[1].1, TargetOutcome::Skipped { .. }));
        assert_eq!(*fleet.machines["ctl"].pid.borrow(), old_ctl, "조종 기기 앱은 그대로");
        assert!(active_job(&fleet.machines["ctl"].jobs, NOW).is_none() && !fleet.machines["ctl"].jobs.exists(), "조종 기기엔 작업조차 없다");
        reap(&fleet);
        let _ = std::fs::remove_dir_all(&root);
    }

    struct Fake {
        facts: RefCell<HashMap<String, Vec<std::result::Result<Facts, String>>>>,
        states: RefCell<HashMap<String, Vec<std::result::Result<JobState, String>>>>,
        started: RefCell<Vec<String>>,
        refuse_start: Vec<String>,
    }

    impl Fake {
        fn new() -> Self {
            Fake { facts: RefCell::default(), states: RefCell::default(), started: RefCell::default(), refuse_start: Vec::new() }
        }
        fn facts_seq(self, id: &str, seq: Vec<std::result::Result<Facts, String>>) -> Self {
            self.facts.borrow_mut().insert(id.into(), seq);
            self
        }
        fn states_seq(self, id: &str, seq: Vec<std::result::Result<JobState, String>>) -> Self {
            self.states.borrow_mut().insert(id.into(), seq);
            self
        }
    }

    fn pop<T: Clone>(map: &RefCell<HashMap<String, Vec<T>>>, id: &str) -> Option<T> {
        let mut map = map.borrow_mut();
        let seq = map.get_mut(id)?;
        if seq.len() > 1 { Some(seq.remove(0)) } else { seq.first().cloned() }
    }

    impl Transport for Fake {
        fn facts(&self, id: &str) -> std::result::Result<Facts, String> {
            pop(&self.facts, id).unwrap_or_else(|| Err("모름".into()))
        }
        fn start(&self, id: &str, _req: &JobRequest) -> std::result::Result<(), String> {
            if self.refuse_start.iter().any(|x| x == id) {
                return Err("대상이 거부".into());
            }
            self.started.borrow_mut().push(id.into());
            Ok(())
        }
        fn status(&self, id: &str, _job_id: &str) -> std::result::Result<JobState, String> {
            pop(&self.states, id).unwrap_or(Ok(JobState::Launched))
        }
    }

    fn policy() -> RunPolicy {
        RunPolicy { poll_every: std::time::Duration::from_millis(1), boot_timeout: std::time::Duration::from_millis(200), max_status_errors: 3 }
    }

    fn approved(plan: &Plan) -> FakeNacho {
        FakeNacho::approving(&plan.scope, "approved", NOW + 60_000)
    }

    #[test]
    fn without_a_consumable_approval_no_machine_is_touched() {
        let plan = plan_for(&["a", "ctl"], "ctl");
        let fake = Fake::new().facts_seq("a", vec![Ok(facts("a", 1))]).facts_seq("ctl", vec![Ok(facts("ctl", 1))]);
        let pending = FakeNacho::approving(&plan.scope, "pending", NOW + 60_000);
        let out = run(&plan, AP, "ctl", &pending, &fake, &policy(), &|| NOW);
        assert!(out.iter().all(|(_, o)| matches!(o, TargetOutcome::Skipped { .. })), "{out:?}");
        assert!(fake.started.borrow().is_empty());
    }

    fn plan_for(ids: &[&str], controller: &str) -> Plan {
        let lookup = |id: &str| Ok(facts(id, 1));
        build_plan(controller, &ids.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &lookup, NOW)
    }

    #[test]
    fn one_at_a_time_controller_last_and_reconnect_errors_are_tolerated() {
        let plan = plan_for(&["ctl", "a"], "ctl");
        let fake = Fake::new()
            .facts_seq("a", vec![Ok(facts("a", 1)), Ok(facts("a", 2))])
            .facts_seq("ctl", vec![Ok(facts("ctl", 1))])
            .states_seq("a", vec![Err("끊김".into()), Err("끊김".into()), Ok(JobState::Launched), Ok(JobState::Booted)]);
        let out = run(&plan, AP, "ctl", &approved(&plan), &fake, &policy(), &|| NOW);
        assert_eq!(*fake.started.borrow(), ["a", "ctl"], "원격 먼저, 조종 기기 마지막");
        assert!(matches!(out[0].1, TargetOutcome::Verified { new_pid: 2, .. }));
        assert!(matches!(out[1].1, TargetOutcome::HandedOff { .. }));
    }

    #[test]
    fn a_failed_machine_stops_the_rest() {
        let plan = plan_for(&["a", "b", "ctl"], "ctl");
        let fake = Fake::new()
            .facts_seq("a", vec![Ok(facts("a", 1))])
            .facts_seq("b", vec![Ok(facts("b", 1))])
            .states_seq("a", vec![Ok(JobState::Failed)]);
        let out = run(&plan, AP, "ctl", &approved(&plan), &fake, &policy(), &|| NOW);
        assert!(matches!(out[0].1, TargetOutcome::Failed { .. }));
        assert!(matches!(out[1].1, TargetOutcome::Skipped { .. }));
        assert!(matches!(out[2].1, TargetOutcome::Skipped { .. }), "조종 기기도 손대지 않는다");
        assert_eq!(*fake.started.borrow(), ["a"]);
    }

    #[test]
    fn changed_target_after_planning_is_not_touched() {
        let plan = plan_for(&["a"], "ctl");
        let mut changed = facts("a", 1);
        changed.binary.build = "newer".into();
        let fake = Fake::new().facts_seq("a", vec![Ok(changed)]);
        let out = run(&plan, AP, "ctl", &approved(&plan), &fake, &policy(), &|| NOW);
        assert!(matches!(&out[0].1, TargetOutcome::Failed { reason, .. } if reason.contains("바뀌었다")));
        assert!(fake.started.borrow().is_empty());
    }

    #[test]
    fn offline_after_restart_and_same_pid_are_failures_not_success() {
        let plan = plan_for(&["a"], "ctl");
        let lost = Fake::new().facts_seq("a", vec![Ok(facts("a", 1))]).states_seq("a", vec![Err("끊김".into())]);
        assert!(matches!(&run(&plan, AP, "ctl", &approved(&plan), &lost, &policy(), &|| NOW)[0].1, TargetOutcome::Failed { reason, .. } if reason.contains("닿지")));
        let same = Fake::new().facts_seq("a", vec![Ok(facts("a", 1)), Ok(facts("a", 1))]).states_seq("a", vec![Ok(JobState::Booted)]);
        assert!(matches!(&run(&plan, AP, "ctl", &approved(&plan), &same, &policy(), &|| NOW)[0].1, TargetOutcome::Failed { reason, .. } if reason.contains("pid")));
        let mut upgraded = facts("a", 2);
        upgraded.binary.inode = 8;
        let up = Fake::new().facts_seq("a", vec![Ok(facts("a", 1)), Ok(upgraded)]).states_seq("a", vec![Ok(JobState::Booted)]);
        assert!(matches!(&run(&plan, AP, "ctl", &approved(&plan), &up, &policy(), &|| NOW)[0].1, TargetOutcome::Failed { reason, .. } if reason.contains("업그레이드")));
    }

    #[test]
    fn target_refusal_at_start_is_reported_and_stops() {
        let plan = plan_for(&["a", "b"], "ctl");
        let mut fake = Fake::new().facts_seq("a", vec![Ok(facts("a", 1))]).facts_seq("b", vec![Ok(facts("b", 1))]);
        fake.refuse_start = vec!["a".into()];
        let out = run(&plan, AP, "ctl", &approved(&plan), &fake, &policy(), &|| NOW);
        assert!(matches!(&out[0].1, TargetOutcome::Failed { reason, .. } if reason.contains("거부")));
        assert!(matches!(out[1].1, TargetOutcome::Skipped { .. }));
    }
}
