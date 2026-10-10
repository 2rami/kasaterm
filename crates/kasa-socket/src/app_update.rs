//! 앱 업데이트 창구 — 기기가 공식 릴리스 파일을 스스로 받아 확인하고, 곁에 두었다가, 승인된 한 번의 작업으로 갈아 끼운다.
//!
//! 앱 재시작(`app_restart`)의 틀을 그대로 쓴다: 기기 정체(`target_hash`)·나쵸 승인(`Authority`, 조종 기기가 한 번 소비한
//! 것만)·원자적 작업 기록(`create_new`, 같은 작업은 한 번)·앞으로만 가는 상태·강제 종료 없는 도우미·부팅 표식.
//! 여기서 더하는 것은 셋이다.
//! - 받기: 공식 피드(`MAC_FEED`)와 공식 릴리스 주소(`RELEASE_PREFIX`)만. 요청은 URL·경로·명령을 싣지 못한다 — 태그와
//!   파일 이름에서 주소를 여기서 짓고, 피드가 같은 주소·크기·서명을 말할 때만 받는다. 크기 상한, sha256(조종 쪽이 릴리스
//!   단계에서 잰 값), EdDSA(설치본에 박힌 Sparkle 공개키), dmg 안 번들의 서명 팀이 설치본과 같은지, 공증.
//! - 준비: 확인한 번들을 설치본 곁(`.kasaterm.app.next`)에 둔다. 설치본은 아직 안 건드린다.
//! - 갈아 끼우기: 앱이 스스로 끈 뒤 도우미가 설치본을 `.kasaterm.app.previous` 로 옮기고 새 번들을 들인다. 새 앱이
//!   부팅 표식(`mark_booted`)을 남기지 못하고 꺼지면 이전 판을 되돌려 다시 띄운다. 살아 있는데 표식이 없으면 끄지 않는다.
//!
//! 설치 실행은 기본 꺼짐이다(`install_enabled`, `KASATERM_APP_UPDATE=on`). 나쵸에 이 동작의 승인 계약이 아직 없어서다 —
//! 켜지 않으면 받기조차 하지 않는다. 검사는 실제 효과(`Effects`)를 가짜로 바꿔 끼워 처음부터 끝까지 돈다.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::app_restart::{self, ApprovalView, Authority, Facts};

pub const SCHEMA: &str = "kasaterm-update/1";
/// 이 창구가 든 판이 `Facts::update_capability` 로 알린다.
pub const CAPABILITY: u32 = 1;
/// 나쵸 승인의 `action` — 나쵸에 아직 없다. 생기기 전까지 이 창구는 꺼진 채로 둔다.
pub const ACTION: &str = "kasaterm_update";
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_FEED_BYTES: u64 = 256 * 1024;
pub const MAC_FEED: &str = "https://2rami.github.io/kasaterm/appcast.xml";
pub const RELEASE_PREFIX: &str = "https://github.com/2rami/kasaterm/releases/download/";
/// Sparkle·WinSparkle 과 같은 EdDSA 공개키(scripts/build-app.sh `SUPublicEDKey`). 새 키가 아니다.
pub const ED_PUBLIC_KEY: &str = "E4tFAb2UND+0QhgTSv2pFYKIC3ReT/dLia20KHfZxKw=";
const JOB_TTL_MS: u64 = 60 * 60_000;
/// 이 기기가 받은 뒤 적용을 기다리는 상한(자기 시계). 나쵸는 소비한 update 승인을 대상마다 35분씩 살려 둔다 —
/// 이 30분 + 종료 60초 + 부팅 90초 + 여유.
pub const MAX_WAIT_MS: u64 = 30 * 60_000;

/// 설치 실행 스위치 — 기본 꺼짐. 받기·확인·준비·갈아 끼우기가 전부 이 뒤에 있다.
pub fn install_enabled(get: &dyn Fn(&str) -> Option<String>) -> bool {
    get("KASATERM_APP_UPDATE").as_deref() == Some("on")
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub ed_signature: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateJob {
    pub schema: String,
    pub job_id: String,
    pub plan_hash: String,
    pub machine_id: String,
    /// 계획 때 이 기기의 정체(`app_restart::target_hash`). 승인 뒤 바뀌면 안 한다.
    pub target_hash: String,
    pub tag: String,
    pub version: String,
    pub commit: String,
    /// 새 판이 부팅 때 댈 빌드 표식(태그 커밋 앞자리) — 이것으로 「새 판이 떴다」를 판정한다.
    pub build: String,
    pub asset: Asset,
    /// 요구 서명 팀 — 설치본과 같아야 한다.
    pub team: String,
    pub require_notarized: bool,
    pub old_pid: u32,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub job: UpdateJob,
    pub approval_id: String,
    pub authority: String,
}

pub fn asset_name(tag: &str) -> String {
    format!("kasaterm-{tag}.dmg")
}

pub fn asset_url(tag: &str, name: &str) -> String {
    format!("{RELEASE_PREFIX}{tag}/{name}")
}

/// 같은 계획·같은 기기·같은 파일이면 늘 같은 id — 다시 보내도 새 작업이 안 생긴다.
pub fn job_id(plan_hash: &str, machine_id: &str, sha256: &str) -> String {
    format!("up{}", app_restart::fnv(&[plan_hash, machine_id, sha256]))
}

fn hex(s: &str, lo: usize, hi: usize) -> bool {
    (lo..=hi).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn version_parts(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.').map(|p| p.parse::<u64>().ok());
    let parts = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(parts)
}

/// 요청이 댈 수 있는 모양 — 주소는 태그·이름에서 지은 공식 주소와 같아야 하고, 이름은 그 태그의 dmg 뿐이다.
pub fn check_job(job: &UpdateJob) -> std::result::Result<(), String> {
    if job.schema != SCHEMA {
        return Err("작업 모양(schema)이 다르다".into());
    }
    if version_parts(&job.version).is_none() || job.tag != format!("v{}", job.version) {
        return Err("버전·태그가 모양이 아니다".into());
    }
    if !hex(&job.commit, 40, 40) || !hex(&job.build, 7, 40) {
        return Err("커밋·빌드 표식이 소문자 hex 가 아니다".into());
    }
    if !hex(&job.plan_hash, 8, 64) || job.machine_id.is_empty() || job.target_hash.is_empty() {
        return Err("계획·기기 정체가 비었다".into());
    }
    let a = &job.asset;
    if a.name != asset_name(&job.tag) || a.url != asset_url(&job.tag, &a.name) {
        return Err("공식 릴리스 주소가 아니다 — 받는 곳은 태그의 dmg 하나뿐이다".into());
    }
    if a.size == 0 || a.size > MAX_ASSET_BYTES {
        return Err(format!("파일 크기가 상한 밖이다({} 바이트, 상한 {MAX_ASSET_BYTES})", a.size));
    }
    if !a.sha256.strip_prefix("sha256:").is_some_and(|h| hex(h, 64, 64)) {
        return Err("sha256 모양이 아니다".into());
    }
    if base64::engine::general_purpose::STANDARD.decode(&a.ed_signature).map(|s| s.len()) != Ok(64) {
        return Err("EdDSA 서명 모양이 아니다".into());
    }
    if job.team.len() != 10 || !job.team.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
        return Err("요구 서명 팀이 Apple 팀 id 모양이 아니다".into());
    }
    if !job.require_notarized {
        return Err("공증 요구를 끌 수 없다".into());
    }
    if job.job_id != job_id(&job.plan_hash, &job.machine_id, &a.sha256) {
        return Err("작업 id 가 계획·기기·파일과 맞지 않는다".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedItem {
    pub version: String,
    pub url: String,
    pub length: u64,
    pub signature: String,
}

/// appcast 의 최신 항목(CI 는 한 건만 싣는다). 속성 순서와 무관하게 읽는다.
pub fn parse_feed(xml: &str) -> Option<FeedItem> {
    let tag = |open: &str, close: &str| -> Option<String> {
        let i = xml.find(open)? + open.len();
        Some(xml[i..i + xml[i..].find(close)?].trim().to_string())
    };
    let version = tag("<sparkle:version>", "</sparkle:version>")?;
    let i = xml.find("<enclosure")?;
    let enc = &xml[i..i + xml[i..].find('>')?];
    let attr = |name: &str| -> Option<String> {
        let key = format!("{name}=\"");
        let j = enc.find(&key)? + key.len();
        Some(enc[j..j + enc[j..].find('"')?].to_string())
    };
    Some(FeedItem { version, url: attr("url")?, length: attr("length")?.parse().ok()?, signature: attr("sparkle:edSignature")? })
}

/// 피드가 작업과 같은 파일을 말하는가 — 주소·크기·서명·버전 전부.
pub fn check_feed(job: &UpdateJob, item: &FeedItem) -> std::result::Result<(), String> {
    if item.version != job.version {
        return Err(format!("공식 피드가 다른 판을 말한다({} ≠ {})", item.version, job.version));
    }
    if item.url != job.asset.url || item.length != job.asset.size || item.signature != job.asset.ed_signature {
        return Err("공식 피드가 가리키는 파일이 작업의 파일과 다르다".into());
    }
    Ok(())
}

/// 이 기기에 도는 판과 견준 판정·나쵸 승인. 요청이 말하는 것을 믿지 않는다.
pub fn authorize(
    req: &UpdateRequest,
    facts: &Facts,
    current_version: &str,
    authority: &dyn Authority,
    enabled: bool,
    now_ms: u64,
) -> std::result::Result<ApprovalView, String> {
    if !enabled {
        return Err("update_disabled — 이 기기의 설치 창구가 꺼져 있다(KASATERM_APP_UPDATE)".into());
    }
    if facts.os != "macos" {
        return Err(format!("unsupported_os — {} 는 이 창구가 아니라 자기 업데이터(WinSparkle)로 받는다", facts.os));
    }
    let job = &req.job;
    check_job(job)?;
    if let Some(refusal) = blocking(facts, &job.job_id, now_ms, false).first() {
        return Err(refusal.message());
    }
    if job.machine_id != facts.machine_id || job.target_hash != app_restart::target_hash(facts) || job.old_pid != facts.pid {
        return Err("작업이 이 기기의 지금 정체와 맞지 않는다".into());
    }
    match (version_parts(&job.version), version_parts(current_version)) {
        (Some(want), Some(have)) if want > have => {}
        _ => return Err(format!("downgrade — 지금 판({current_version})보다 새 판이 아니다({})", job.version)),
    }
    if !app_restart::valid_approval_id(&req.approval_id) {
        return Err("approval id 모양이 아니다".into());
    }
    let view = usable_approval(authority, &req.approval_id, now_ms)?;
    let s = &view.scope;
    let same = s["plan"].as_str() == Some(&job.plan_hash)
        && s["tag"].as_str() == Some(&job.tag)
        && s["version"].as_str() == Some(&job.version)
        && s["commit"].as_str() == Some(&job.commit)
        && s["asset"]["name"].as_str() == Some(&job.asset.name)
        && s["asset"]["sha256"].as_str() == Some(&job.asset.sha256)
        && s["asset"]["size"].as_u64() == Some(job.asset.size);
    let mine = s["targets"].as_array().into_iter().flatten()
        .any(|t| t["machine_id"].as_str() == Some(&facts.machine_id) && t["hash"].as_str() == Some(&job.target_hash));
    if !same || !mine {
        return Err("승인한 대상·판·파일에 이 작업이 없다".into());
    }
    Ok(view)
}

/// 지금 쓸 수 있는 update 승인인가 — 받을 때마다(다시 보내기 포함), 그리고 갈아 끼우기 직전에 다시 본다. 주인이 거둔
/// 승인(`revoked`)·만료된 승인은 여기서 걸린다. 이미 도우미에 넘긴 뒤에는 못 멈춘다.
pub fn usable_approval(authority: &dyn Authority, approval_id: &str, now_ms: u64) -> std::result::Result<ApprovalView, String> {
    if !app_restart::valid_approval_id(approval_id) {
        return Err("approval id 모양이 아니다".into());
    }
    let view = authority.get(approval_id)?;
    if view.id != approval_id {
        return Err("요청한 승인이 아닌 승인이 돌아왔다".into());
    }
    if view.action != ACTION || view.state != "approved" {
        return Err(format!("승인되지 않았다({} {})", view.action, view.state));
    }
    let controller = view.scope["controller"].as_str();
    if controller.is_none() || view.consumed_at_ms.is_none() || view.consumed_by.as_deref() != controller {
        return Err("조종 기기가 이 승인을 소비하지 않았다".into());
    }
    if now_ms >= view.expires_at_ms {
        return Err("승인이 만료됐다".into());
    }
    Ok(view)
}

/// 재시작 쪽 거부 중 이 작업을 막는 것. 받기·준비(`apply=false`)는 바쁜 학생·미저장 편집기·굽기를 기다리지 않는다 —
/// 설치본을 안 건드리니까. 갈아 끼우기(`apply=true`)는 전부 막는다. 이 작업 자신이 「진행 중인 작업」으로 잡힌 것은 뺀다.
pub fn blocking(facts: &Facts, job_id: &str, now_ms: u64, apply: bool) -> Vec<app_restart::Refusal> {
    use app_restart::Refusal;
    app_restart::refusals(&facts.machine_id, facts, now_ms).into_iter().filter(|r| match r {
        Refusal::JobInFlight { job_id: other } => other != job_id,
        Refusal::BusyStudents { .. } | Refusal::UnsavedEditors { .. } | Refusal::BakeInProgress => apply,
        _ => true,
    }).collect()
}

/// 나쵸가 승인할 범위 — 대상 여럿을 한 번에(키 8개, 나쵸 `validate_update_scope` 와 같은 선). 조종 기기가 만들고 한 번 소비한다.
/// 모든 작업은 같은 rollout·판·파일이어야 한다(`check_rollout`).
pub fn rollout_scope(jobs: &[UpdateJob], controller: &str) -> serde_json::Value {
    let first = jobs.first();
    serde_json::json!({
        "action": ACTION,
        "plan": first.map(|j| j.plan_hash.as_str()).unwrap_or(""),
        "controller": controller,
        "tag": first.map(|j| j.tag.as_str()).unwrap_or(""),
        "version": first.map(|j| j.version.as_str()).unwrap_or(""),
        "commit": first.map(|j| j.commit.as_str()).unwrap_or(""),
        "asset": first.map(|j| serde_json::json!({"name": j.asset.name, "sha256": j.asset.sha256, "size": j.asset.size})).unwrap_or_default(),
        "targets": jobs.iter().enumerate()
            .map(|(n, j)| serde_json::json!({"order": n + 1, "machine_id": j.machine_id, "hash": j.target_hash}))
            .collect::<Vec<_>>(),
    })
}

/// 나쵸 `approvals.scope_hash` 와 같은 값 — 키를 정렬한 빈칸 없는 JSON 의 sha256.
pub fn scope_hash(scope: &serde_json::Value) -> String {
    fn canonical(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) => {
                let sorted: std::collections::BTreeMap<&String, serde_json::Value> = m.iter().map(|(k, v)| (k, canonical(v))).collect();
                serde_json::to_value(sorted).unwrap_or_default()
            }
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    format!("sha256:{:x}", sha2::Sha256::digest(canonical(scope).to_string().as_bytes()))
}

pub const MAX_TARGETS: usize = 16;
/// 키 없는 기기가 update 승인을 읽는 중계 — 명부 파일의 별도 위임 표식이 있어야 한다(재시작 위임으로는 안 나른다).
pub const UPDATE_RELAY: app_restart::Relay = app_restart::Relay { delegation: "update_approvals", route: "/app/update/approvals", action: ACTION };

/// 한 번의 승인으로 여러 기기에 차례로 보내는 묶음(`device-plan --json` 의 `rollout` 한 칸 그대로). `tools/release` 가 짓고, 나쵸가
/// `approval_scope` 를 사람에게 보여 승인을 받는다. 조종 쪽 러너(`run`)는 이 파일을 믿지 않고 작업들로 범위를 다시 지어 대조한다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollout {
    pub id: String,
    pub release_plan: String,
    pub created_at_ms: u64,
    pub approval_scope: serde_json::Value,
    pub approval_scope_hash: String,
    pub jobs: Vec<UpdateJob>,
    /// 이번에 안 보내는 기기와 까닭 — 보이기만 한다.
    #[serde(default)]
    pub excluded: Vec<serde_json::Value>,
}

impl Rollout {
    /// 승인을 소비하는 기기 — 범위에 적힌 조종 기기(러너가 서 있는 곳).
    pub fn controller(&self) -> &str {
        self.approval_scope["controller"].as_str().unwrap_or("")
    }
}

pub fn check_rollout(r: &Rollout) -> std::result::Result<(), String> {
    if r.controller().is_empty() {
        return Err("범위에 조종 기기가 없다".into());
    }
    if r.jobs.is_empty() || r.jobs.len() > MAX_TARGETS {
        return Err(format!("대상이 없거나 {MAX_TARGETS}대를 넘는다"));
    }
    if !hex(&r.id, 16, 16) {
        return Err("rollout id 가 16자리 소문자 hex 가 아니다".into());
    }
    let first = &r.jobs[0];
    let mut seen = std::collections::HashSet::new();
    for job in &r.jobs {
        check_job(job).map_err(|e| format!("{}: {e}", job.machine_id))?;
        if job.plan_hash != r.id || job.tag != first.tag || job.version != first.version || job.commit != first.commit
            || job.asset != first.asset || job.team != first.team || job.build != first.build
        {
            return Err("작업들이 한 rollout·판·파일이 아니다".into());
        }
        if !seen.insert(job.machine_id.as_str()) {
            return Err(format!("같은 기기가 두 번 있다({})", job.machine_id));
        }
    }
    if let Some(n) = r.jobs.iter().position(|j| j.machine_id == r.controller()) {
        if n + 1 != r.jobs.len() {
            return Err("조종 기기는 맨 뒤여야 한다 — 러너가 거기 서 있다".into());
        }
    }
    let scope = rollout_scope(&r.jobs, r.controller());
    if r.approval_scope != scope || r.approval_scope_hash != scope_hash(&scope) {
        return Err("승인 범위가 작업들로 다시 지은 범위와 다르다".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- 작업 기록

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Accepted,
    Fetched,
    Checked,
    Staged,
    Armed,
    HelperStarted,
    Exited,
    Swapped,
    Launched,
    Booted,
    Done,
    Failed,
    RolledBack,
    Cancelled,
}

impl State {
    pub fn parse(word: &str) -> Option<Self> {
        serde_json::from_value(serde_json::json!(word)).ok()
    }

    pub fn word(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    pub fn terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::RolledBack | Self::Cancelled)
    }

    /// 앞으로만 간다. 취소는 도우미에 넘기기 전(Staged 까지), 되돌림은 갈아 끼운 뒤에만.
    fn allows(self, next: Self) -> bool {
        if self.terminal() {
            return false;
        }
        match next {
            Self::Failed => true,
            Self::Cancelled => self <= Self::Staged,
            Self::RolledBack => self >= Self::Swapped,
            _ => next > self,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at_s: u64,
    pub state: State,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub job: UpdateJob,
    pub state: State,
    pub events: Vec<Event>,
}

pub fn jobs_dir() -> Result<PathBuf> {
    if let Some(root) = crate::isolated_collab_root() {
        return Ok(root.join("app-update"));
    }
    Ok(crate::home_dir().context("home directory unavailable")?.join(".config/kasaterm/app-update"))
}

fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn events_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.events"))
}

/// 작업을 적는다. 같은 id 가 있으면 새로 만들지 않고(`false`), 내용이 다르면 충돌로 거부한다.
pub fn create_job(dir: &Path, job: &UpdateJob) -> Result<(UpdateJob, bool)> {
    ensure!(app_restart::valid_job_id(&job.job_id), "invalid job id");
    std::fs::create_dir_all(dir)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(record_path(dir, &job.job_id)) {
        Ok(mut f) => {
            f.write_all(serde_json::to_string_pretty(job)?.as_bytes())?;
            f.sync_all()?;
            append(dir, &job.job_id, State::Accepted, "")?;
            Ok((job.clone(), true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing: UpdateJob = serde_json::from_str(&std::fs::read_to_string(record_path(dir, &job.job_id))?)?;
            ensure!(existing.asset == job.asset && existing.plan_hash == job.plan_hash && existing.machine_id == job.machine_id,
                "job id collision: 같은 id 에 다른 작업이 있다");
            Ok((existing, false))
        }
        Err(e) => Err(e.into()),
    }
}

fn append(dir: &Path, id: &str, state: State, note: &str) -> Result<()> {
    let note: String = note.chars().filter(|c| !c.is_control()).take(300).collect();
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(events_path(dir, id))?;
    writeln!(f, "{at} {} {note}", state.word())?;
    Ok(())
}

/// 상태는 그대로 두고 까닭만 남긴다 — 끊겨서 다시 할 일(받기 실패)·기다리는 일(바쁜 학생).
pub fn note(dir: &Path, id: &str, text: &str) -> Result<()> {
    let now = status(dir, id)?;
    append(dir, id, now.state, text)
}

pub fn advance(dir: &Path, id: &str, next: State, note: &str) -> Result<State> {
    let now = status(dir, id)?;
    ensure!(now.state.allows(next), "{} → {} 는 갈 수 없다", now.state.word(), next.word());
    append(dir, id, next, note)?;
    Ok(next)
}

/// 기록을 접는다. 도우미는 줄만 덧붙이고, 순서를 어긴 줄은 상태를 못 바꾸고 기록에만 남는다.
pub fn status(dir: &Path, id: &str) -> Result<Status> {
    ensure!(app_restart::valid_job_id(id), "invalid job id");
    let job: UpdateJob = serde_json::from_str(&std::fs::read_to_string(record_path(dir, id)).context("no such job")?)?;
    let text = std::fs::read_to_string(events_path(dir, id)).unwrap_or_default();
    let mut state = State::Accepted;
    let mut events = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(at), Some(word)) = (parts.next(), parts.next()) else { continue };
        let (Ok(at_s), Some(next)) = (at.parse::<u64>(), State::parse(word)) else { continue };
        if !(next == State::Accepted && events.is_empty()) && state.allows(next) {
            state = next;
        }
        events.push(Event { at_s, state: next, note: parts.next().unwrap_or("").to_string() });
    }
    Ok(Status { job, state, events })
}

/// 안 끝난 업데이트 작업(한 시간 안) — 두 번째 작업을 막고, 부팅 표식을 붙일 자리를 찾는다.
pub fn active_job(dir: &Path, now_ms: u64) -> Option<Status> {
    std::fs::read_dir(dir).ok()?.flatten().filter_map(|e| {
        let id = e.file_name().to_string_lossy().strip_suffix(".json")?.to_string();
        let s = status(dir, &id).ok()?;
        (!s.state.terminal() && now_ms.saturating_sub(accepted_at_ms(&s)) < JOB_TTL_MS).then_some(s)
    }).next()
}

/// 수락 — 판정을 통과하면 적는다. 같은 작업이 다시 오면 적힌 것을 돌려준다(끊긴 뒤 다시 보낸 경우).
/// 다른 업데이트가 도는 중이면 거부한다.
pub fn accept(
    req: &UpdateRequest,
    facts: &Facts,
    current_version: &str,
    authority: &dyn Authority,
    enabled: bool,
    dir: &Path,
    now_ms: u64,
) -> std::result::Result<(Status, bool), String> {
    authorize(req, facts, current_version, authority, enabled, now_ms)?;
    if let Some(other) = active_job(dir, now_ms).filter(|s| s.job.job_id != req.job.job_id) {
        return Err(format!("job_in_flight — 다른 업데이트가 진행 중이다({})", other.job.job_id));
    }
    let (_, created) = create_job(dir, &req.job).map_err(|e| e.to_string())?;
    Ok((status(dir, &req.job.job_id).map_err(|e| e.to_string())?, created))
}

/// 이 기기가 작업을 처음 받은 때(자기 시계) — 기다림 상한·부팅 표식 창은 조종 쪽이 적은 `created_at_ms` 가 아니라 이것으로 잰다.
pub fn accepted_at_ms(s: &Status) -> u64 {
    s.events.first().map(|e| e.at_s * 1000).unwrap_or(s.job.created_at_ms)
}

// ---------------------------------------------------------------- 받기·확인·준비

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub verified: bool,
    pub team: Option<String>,
    pub notarized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// 닿지 못했거나 도중에 끊겼다 — 다시 하면 된다.
    Cut(String),
    /// 상한을 넘는 파일을 보냈다 — 작업의 파일이 아니다.
    TooLarge,
}

/// 밖과 닿는 일 — 운영은 `SystemEffects`, 검사는 가짜. 주소는 늘 여기서 지은 공식 주소만 넘어온다.
pub trait Effects {
    fn feed(&self, url: &str, max: u64) -> std::result::Result<String, String>;
    /// `dest` 에 받는다. `max` 바이트를 넘으면 `TooLarge` 로 멈춰야 한다.
    fn fetch(&self, url: &str, max: u64, dest: &Path) -> std::result::Result<(), FetchError>;
    fn mount(&self, dmg: &Path, at: &Path) -> std::result::Result<(), String>;
    fn unmount(&self, at: &Path);
    fn identity(&self, app: &Path) -> std::result::Result<Identity, String>;
    fn copy_bundle(&self, from: &Path, to: &Path) -> std::result::Result<(), String>;
}

pub fn sha256_file(path: &Path) -> std::result::Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(format!("sha256:{:x}", sha2::Sha256::digest(&bytes)))
}

/// Sparkle EdDSA — 파일 원문에 대한 Ed25519 서명.
pub fn ed25519_ok(public_b64: &str, signature_b64: &str, data: &[u8]) -> bool {
    let engine = base64::engine::general_purpose::STANDARD;
    let (Ok(key), Ok(sig)) = (engine.decode(public_b64), engine.decode(signature_b64)) else { return false };
    key.len() == 32 && ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &key).verify(data, &sig).is_ok()
}

/// 번들의 판 문자열(Info.plist `CFBundleShortVersionString`).
pub fn bundle_version(app: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    let key = "<key>CFBundleShortVersionString</key>";
    let rest = &text[text.find(key)? + key.len()..];
    let s = rest.find("<string>")? + "<string>".len();
    Some(rest[s..s + rest[s..].find("</string>")?].trim().to_string())
}

pub fn staged_path(installed: &Path) -> PathBuf {
    installed.with_file_name(".kasaterm.app.next")
}

pub fn previous_path(installed: &Path) -> PathBuf {
    installed.with_file_name(".kasaterm.app.previous")
}

pub fn failed_path(installed: &Path) -> PathBuf {
    installed.with_file_name(".kasaterm.app.failed")
}

/// 받은 파일 자리 — 내용(sha256)으로 가른다. 쓰기 전에 늘 다시 잰다.
pub fn work_dir(cache: &Path, job: &UpdateJob) -> PathBuf {
    cache.join(job.asset.sha256.trim_start_matches("sha256:").get(..16).unwrap_or("unknown"))
}

/// 요구 팀은 요청이 아니라 **설치본 자신의 서명**에서 온다(요청의 `team` 은 그것과 같아야 할 뿐). 공증은 늘 요구한다 — 둘 다 승인
/// 범위에 없는 칸이라, 요청이 느슨한 값을 대서 검사를 풀 수 없게.
fn identity_problem(ident: &Identity, team: &str, what: &str) -> Option<String> {
    if !ident.verified {
        return Some(format!("{what} 서명이 깨졌다"));
    }
    if ident.team.as_deref() != Some(team) {
        return Some(format!("{what} 서명 팀이 설치본과 다르다({} ≠ {team})", ident.team.as_deref().unwrap_or("없음")));
    }
    if !ident.notarized {
        return Some(format!("{what} 가 공증되지 않았다"));
    }
    None
}

/// 설치본의 서명 팀 — 설치본 서명이 멀쩡하고 팀이 있어야 하며, 요청이 댄 팀과 같아야 한다.
fn installed_team(effects: &dyn Effects, installed: &Path, job: &UpdateJob) -> std::result::Result<String, String> {
    let own = effects.identity(installed)?;
    let Some(team) = own.team.filter(|_| own.verified) else {
        return Err("설치본의 서명 팀을 잴 수 없다 — 무엇과 견줄지 몰라 받지 않는다".into());
    };
    if team != job.team {
        return Err(format!("요청이 댄 팀({})이 설치본 팀({team})과 다르다", job.team));
    }
    Ok(team)
}

/// 받기 → 확인 → 준비. 단계마다 기록을 남긴다.
/// - 닿지 못함(피드·받기가 끊김)은 끝이 아니다 — 반쯤 받은 것을 걷고 까닭만 적는다. 같은 작업을 다시 보내면 이어 한다.
/// - 내용이 다름(피드가 다른 파일을 말함·해시·서명·팀·공증·판)은 끝이다 — 걷고 실패로 적는다.
/// - 이미 확인해 둔 파일은 다시 받지 않는다.
pub fn prepare(
    dir: &Path,
    job: &UpdateJob,
    cache: &Path,
    installed: &Path,
    effects: &dyn Effects,
    public_key: &str,
) -> std::result::Result<PathBuf, String> {
    let fail = |why: String| -> String {
        let _ = advance(dir, &job.job_id, State::Failed, &why);
        why
    };
    check_job(job).map_err(&fail)?;
    let here = status(dir, &job.job_id).map_err(|e| e.to_string())?.state;
    if here >= State::Staged && !here.terminal() && staged_path(installed).exists() {
        return Ok(staged_path(installed));
    }
    if here.terminal() {
        return Err(format!("이미 끝난 작업이다({})", here.word()));
    }
    let retry = |why: String| -> String {
        let _ = note(dir, &job.job_id, &format!("retry: {why}"));
        why
    };
    let item = effects.feed(MAC_FEED, MAX_FEED_BYTES).map_err(|e| retry(format!("공식 피드를 못 읽었다 — {e}")))?;
    let item = parse_feed(&item).ok_or_else(|| fail("공식 피드를 읽지 못했다".into()))?;
    check_feed(job, &item).map_err(&fail)?;

    let work = work_dir(cache, job);
    std::fs::create_dir_all(&work).map_err(|e| fail(e.to_string()))?;
    let file = work.join(&job.asset.name);
    if !(file.exists() && sha256_file(&file).ok().as_deref() == Some(job.asset.sha256.as_str())) {
        let part = work.join(format!("{}.part", job.asset.name));
        let _ = std::fs::remove_file(&part);
        match effects.fetch(&job.asset.url, job.asset.size.min(MAX_ASSET_BYTES), &part) {
            Ok(()) => {}
            Err(FetchError::Cut(why)) => {
                let _ = std::fs::remove_file(&part);
                return Err(retry(format!("받기가 끊겼다 — {why}")));
            }
            Err(FetchError::TooLarge) => {
                let _ = std::fs::remove_file(&part);
                return Err(fail(format!("받은 파일이 작업의 파일이 아니다 — {} 바이트 상한을 넘었다", job.asset.size)));
            }
        }
        let got = std::fs::metadata(&part).map(|m| m.len()).map_err(|e| e.to_string());
        let checked = got.and_then(|n| {
            if n != job.asset.size {
                return Err(format!("받은 크기가 다르다({n} ≠ {})", job.asset.size));
            }
            let digest = sha256_file(&part)?;
            if digest != job.asset.sha256 {
                return Err("받은 파일의 sha256 이 작업과 다르다".into());
            }
            std::fs::rename(&part, &file).map_err(|e| e.to_string())
        });
        if let Err(why) = checked {
            let _ = std::fs::remove_file(&part);
            return Err(fail(format!("받은 파일이 작업의 파일이 아니다 — {why}")));
        }
    }
    if here < State::Fetched {
        let _ = advance(dir, &job.job_id, State::Fetched, &format!("{} 바이트", job.asset.size));
    }
    let bytes = std::fs::read(&file).map_err(|e| fail(e.to_string()))?;
    if !ed25519_ok(public_key, &job.asset.ed_signature, &bytes) {
        let _ = std::fs::remove_file(&file);
        return Err(fail("EdDSA 서명이 받은 파일과 맞지 않는다".into()));
    }
    if status(dir, &job.job_id).map(|s| s.state < State::Checked).unwrap_or(true) {
        let _ = advance(dir, &job.job_id, State::Checked, "sha256·EdDSA");
    }

    let mnt = work.join("mnt");
    let _ = std::fs::remove_dir_all(&mnt);
    std::fs::create_dir_all(&mnt).map_err(|e| fail(e.to_string()))?;
    let team = installed_team(effects, installed, job).map_err(&fail)?;
    effects.mount(&file, &mnt).map_err(|e| fail(format!("dmg 를 열지 못했다 — {e}")))?;
    let staged = staged_path(installed);
    let result = (|| -> std::result::Result<(), String> {
        let app = mnt.join("kasaterm.app");
        let ident = effects.identity(&app)?;
        if let Some(why) = identity_problem(&ident, &team, "dmg 안 번들") {
            return Err(why);
        }
        if bundle_version(&app).as_deref() != Some(job.version.as_str()) {
            return Err("dmg 안 번들의 판이 작업의 판과 다르다".into());
        }
        let _ = std::fs::remove_dir_all(&staged);
        effects.copy_bundle(&app, &staged)
    })();
    effects.unmount(&mnt);
    if let Err(why) = result.and_then(|_| {
        let ident = effects.identity(&staged)?;
        identity_problem(&ident, &team, "준비한 번들").map_or(Ok(()), Err)
    }) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(fail(why));
    }
    let _ = advance(dir, &job.job_id, State::Staged, &staged.display().to_string());
    Ok(staged)
}

/// 운영 효과 — 모두 시스템 경로의 도구다. 받기는 https 만, 넘겨주기(리다이렉트)도 https 만, 크기 상한을 건다.
pub struct SystemEffects;

fn exec(argv: &[&str]) -> std::result::Result<std::process::Output, String> {
    std::process::Command::new(argv[0]).args(&argv[1..]).output().map_err(|e| format!("{}: {e}", argv[0]))
}

impl Effects for SystemEffects {
    fn feed(&self, url: &str, max: u64) -> std::result::Result<String, String> {
        let out = exec(&["/usr/bin/curl", "-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-time", "20",
                        "--max-filesize", &max.to_string(), url])?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
            .ok_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
    }

    fn fetch(&self, url: &str, max: u64, dest: &Path) -> std::result::Result<(), FetchError> {
        let out = exec(&["/usr/bin/curl", "-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-time", "900",
                        "--max-filesize", &max.to_string(), "-o", &dest.display().to_string(), url]).map_err(FetchError::Cut)?;
        match out.status.code() {
            Some(0) => Ok(()),
            // curl 63 = 받을 크기가 --max-filesize 를 넘었다.
            Some(63) => Err(FetchError::TooLarge),
            _ => Err(FetchError::Cut(String::from_utf8_lossy(&out.stderr).trim().to_string())),
        }
    }

    fn mount(&self, dmg: &Path, at: &Path) -> std::result::Result<(), String> {
        let out = exec(&["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint",
                        &at.display().to_string(), &dmg.display().to_string()])?;
        out.status.success().then_some(()).ok_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
    }

    fn unmount(&self, at: &Path) {
        let _ = exec(&["/usr/bin/hdiutil", "detach", &at.display().to_string()]);
    }

    fn identity(&self, app: &Path) -> std::result::Result<Identity, String> {
        let path = app.display().to_string();
        let verified = exec(&["/usr/bin/codesign", "--verify", "--deep", "--strict", &path])?.status.success();
        let shown = exec(&["/usr/bin/codesign", "-dvv", &path])?;
        let text = String::from_utf8_lossy(&shown.stderr);
        let team = text.lines().find_map(|l| l.strip_prefix("TeamIdentifier=")).map(str::trim)
            .filter(|t| *t != "not set").map(str::to_string);
        let gate = exec(&["/usr/sbin/spctl", "--assess", "--type", "execute", "-vv", &path])?;
        let notarized = gate.status.success() && String::from_utf8_lossy(&gate.stderr).contains("Notarized");
        Ok(Identity { verified, team, notarized })
    }

    fn copy_bundle(&self, from: &Path, to: &Path) -> std::result::Result<(), String> {
        let out = exec(&["/usr/bin/ditto", &from.display().to_string(), &to.display().to_string()])?;
        out.status.success().then_some(()).ok_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ---------------------------------------------------------------- 갈아 끼우기

#[derive(Clone, Debug)]
pub struct SwapSpec {
    pub dir: PathBuf,
    pub job_id: String,
    pub old_pid: u32,
    pub installed: PathBuf,
    /// 다시 띄우는 명령. 운영은 `app_restart::launch_command(installed)` 뿐이다.
    pub launch: Vec<String>,
    pub exit_timeout_s: u32,
    pub boot_timeout_s: u32,
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 도우미 셸. 강제 종료가 없고, 기다림엔 상한이 있고, 상태는 `<id>.events` 에 줄로만 남긴다.
/// 새 앱이 부팅 표식을 남기지 못하고 꺼지면 이전 판을 되돌려 다시 띄운다. 살아 있는데 표식이 없으면 손대지 않는다.
pub fn swap_script(spec: &SwapSpec) -> Result<String> {
    ensure!(app_restart::valid_job_id(&spec.job_id), "invalid job id");
    ensure!(!spec.launch.is_empty(), "launch command is empty");
    let events = sq(&events_path(&spec.dir, &spec.job_id).display().to_string());
    let inst = &spec.installed;
    let exe = sq(&inst.join("Contents/MacOS/kasaterm").display().to_string());
    let (inst_q, next, prev, failed) = (sq(&inst.display().to_string()), sq(&staged_path(inst).display().to_string()),
        sq(&previous_path(inst).display().to_string()), sq(&failed_path(inst).display().to_string()));
    let launch = spec.launch.iter().map(|a| sq(a)).collect::<Vec<_>>().join(" ");
    let (pid, exit_ticks, boot_ticks) = (spec.old_pid, spec.exit_timeout_s * 5, spec.boot_timeout_s * 5);
    Ok(format!(
        r#"ev() {{ printf '%s %s %s\n' "$(/bin/date +%s)" "$1" "$2" >> {events}; }}
running() {{ /bin/ps -Axww -o pid=,command= | /usr/bin/awk -v exe={exe} -v skip="$1" '{{ cmd = $0; sub(/^ *[0-9]+ /, "", cmd); if (index(cmd, exe) == 1 && $1 != skip) {{ print $1; exit }} }}'; }}
booted() {{ /usr/bin/grep -q '^[0-9]* booted ' {events}; }}
restore() {{ /bin/rm -rf {failed}; /bin/mv {inst_q} {failed} && /bin/mv {prev} {inst_q}; }}
ev helper_started "pid $$"
i=0
while /bin/kill -0 {pid} 2>/dev/null; do
  i=$((i+1)); [ "$i" -gt {exit_ticks} ] && {{ ev failed "app did not exit in time; not forcing"; exit 1; }}
  /bin/sleep 0.2
done
ev exited ""
other=$(running {pid})
[ -n "$other" ] && {{ ev failed "another instance is already running (pid $other); not swapping"; exit 1; }}
[ -d {next} ] || {{ ev failed "staged bundle is missing; installed untouched"; exit 1; }}
/bin/rm -rf {prev}
/bin/mv {inst_q} {prev} || {{ ev failed "could not move the installed app aside; installed untouched"; exit 1; }}
if ! /bin/mv {next} {inst_q}; then /bin/mv {prev} {inst_q}; ev failed "swap failed; previous restored"; exit 1; fi
ev swapped ""
if ! {launch}; then
  restore && {{ {launch}; ev rolled_back "launch failed; previous restored"; }} || ev failed "launch failed; restore failed — previous is at {prev}"
  exit 1
fi
j=0
new=""
while [ -z "$new" ]; do
  new=$(running {pid})
  [ -n "$new" ] && break
  booted && break
  j=$((j+1)); [ "$j" -gt {boot_ticks} ] && break
  /bin/sleep 0.2
done
[ -n "$new" ] && ev launched "pid $new"
k=0
until booted; do
  k=$((k+1))
  if [ "$k" -gt {boot_ticks} ] || [ -z "$(running {pid})" ]; then
    booted && exit 0
    if [ -n "$(running {pid})" ]; then ev failed "new app is running without a boot mark; not forcing — previous kept at {prev}"; exit 1; fi
    restore && {{ {launch}; ev rolled_back "new app did not boot; previous restored and relaunched"; }} || ev failed "new app did not boot; restore failed — previous is at {prev}"
    exit 1
  fi
  /bin/sleep 0.2
done
"#
    ))
}

#[cfg(unix)]
pub fn spawn_swap(spec: &SwapSpec) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    let script = swap_script(spec)?;
    std::fs::create_dir_all(&spec.dir)?;
    let log = std::fs::File::create(spec.dir.join(format!("{}.helper.log", spec.job_id)))?;
    let err = log.try_clone()?;
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env_clear()
        .envs(app_restart::helper_env(&|k| std::env::var(k).ok()))
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err)
        .process_group(0)
        .spawn()?;
    Ok(child.id())
}

/// 준비된 작업을 도우미에 넘긴다 — 켜져 있고, 준비됐고, 곁의 번들이 있을 때만. 다음은 앱이 스스로 끄는 일이다.
#[cfg(unix)]
pub fn arm(dir: &Path, spec: &SwapSpec, enabled: bool) -> std::result::Result<u32, String> {
    if !enabled {
        return Err("update_disabled — 설치 창구가 꺼져 있다".into());
    }
    let s = status(dir, &spec.job_id).map_err(|e| e.to_string())?;
    if s.state != State::Staged || !staged_path(&spec.installed).is_dir() {
        return Err(format!("준비되지 않은 작업이다({})", s.state.word()));
    }
    advance(dir, &spec.job_id, State::Armed, "").map_err(|e| e.to_string())?;
    spawn_swap(spec).map_err(|e| {
        let _ = advance(dir, &spec.job_id, State::Failed, &format!("helper did not start: {e}"));
        e.to_string()
    })
}

/// 이 기기의 설치 자리와 도구. 앱은 `SystemEffects`·설치본 경로·`app_restart::launch_command` 로 만든다.
pub struct Device<'a> {
    pub dir: PathBuf,
    pub cache: PathBuf,
    pub installed: PathBuf,
    pub effects: &'a dyn Effects,
    pub public_key: &'a str,
    pub enabled: bool,
    pub launch: Vec<String>,
    pub exit_timeout_s: u32,
    pub boot_timeout_s: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// 준비는 끝났고 적용을 기다린다(바쁜 학생·미저장 편집기·굽기). 같은 작업을 다시 보내면 다시 잰다.
    Waiting(String),
    /// 도우미가 떴다(그 pid) — 다음은 앱이 스스로 끄는 일.
    Armed(u32),
}

/// 수락한 작업을 준비하고, **지금** 사실로 다시 재서 막힘이 없으면 도우미에 넘긴다. 받는 사이에 학생이 일을 시작했으면
/// 준비한 채 기다린다. 승인 뒤 이 기기의 정체(pid·바이너리·자기설치 예정)가 바뀌었으면 실패로 끝낸다.
#[cfg(unix)]
pub fn drive(
    dev: &Device,
    job: &UpdateJob,
    facts_now: &dyn Fn() -> std::result::Result<Facts, String>,
    approval_now: &dyn Fn() -> std::result::Result<(), String>,
    now_ms: &dyn Fn() -> u64,
) -> std::result::Result<Step, String> {
    if !dev.enabled {
        return Err("update_disabled — 설치 창구가 꺼져 있다".into());
    }
    prepare(&dev.dir, job, &dev.cache, &dev.installed, dev.effects, dev.public_key)?;
    let facts = facts_now()?;
    if facts.pid != job.old_pid || app_restart::target_hash(&facts) != job.target_hash || Path::new(&facts.app_path) != dev.installed {
        let why = "이 기기의 정체가 승인 뒤 바뀌었다 — 갈아 끼우지 않는다";
        let _ = advance(&dev.dir, &job.job_id, State::Failed, why);
        return Err(why.into());
    }
    // 부팅 표식은 한 시간 안의 작업에만 붙는다(`active_job`) — 오래 기다린 작업을 지금 갈아 끼우면 새 판의 도착을 못 적는다.
    let accepted = status(&dev.dir, &job.job_id).map(|s| accepted_at_ms(&s)).unwrap_or(job.created_at_ms);
    if now_ms().saturating_sub(accepted) > MAX_WAIT_MS {
        let why = "받은 지 30분 넘게 기다렸다 — 새 계획·승인으로 다시 한다";
        let _ = advance(&dev.dir, &job.job_id, State::Failed, why);
        return Err(why.into());
    }
    if let Some(refusal) = blocking(&facts, &job.job_id, now_ms(), true).first() {
        let why = refusal.message();
        let _ = note(&dev.dir, &job.job_id, &format!("waiting: {why}"));
        return Ok(Step::Waiting(why));
    }
    // 도우미에 넘기기 직전에 승인을 한 번 더 — 기다리는 사이 주인이 거뒀거나 만료됐으면 준비한 채 끝낸다.
    if let Err(why) = approval_now() {
        let why = format!("승인을 더 쓸 수 없다 — {why}");
        let _ = advance(&dev.dir, &job.job_id, State::Failed, &why);
        return Err(why);
    }
    let spec = SwapSpec {
        dir: dev.dir.clone(),
        job_id: job.job_id.clone(),
        old_pid: facts.pid,
        installed: dev.installed.clone(),
        launch: dev.launch.clone(),
        exit_timeout_s: dev.exit_timeout_s,
        boot_timeout_s: dev.boot_timeout_s,
    };
    arm(&dev.dir, &spec, dev.enabled).map(Step::Armed)
}

/// 새로 뜬 앱이 부팅 때 부른다. 갈아 끼운 뒤 기다리던 작업이면 — 빌드 표식이 맞으면 끝, 다르면 실패로 적는다.
pub fn mark_booted(dir: &Path, machine_id: &str, pid: u32, build: &str, now_ms: u64) -> Option<(String, State)> {
    let s = active_job(dir, now_ms)?;
    // 도우미가 `launched` 를 적기 전에 새 앱이 먼저 부팅할 수 있다 — 갈아 끼운 뒤면 받는다.
    let waiting = matches!(s.state, State::Swapped | State::Launched);
    if s.job.machine_id != machine_id || !waiting || pid == s.job.old_pid {
        return None;
    }
    let same = build_matches(build, &s.job.build);
    let build = build.trim_end_matches('+');
    let id = s.job.job_id.clone();
    let _ = advance(dir, &id, State::Booted, &format!("pid {pid} build {build}"));
    let end = if same { State::Done } else { State::Failed };
    let _ = advance(dir, &id, end, &if same { "build matches".to_string() } else { format!("booted build {build} ≠ {}", s.job.build) });
    Some((id, end))
}

// ---------------------------------------------------------------- 조종 쪽 러너

/// 조종 기기가 대상에 닿는 길. 운영은 앱 소켓(다른 기기는 앱이 명부 경유 HTTP 로 넘긴다), 검사는 가짜·실제 HTTP.
pub trait Transport {
    fn facts(&self, machine_id: &str) -> std::result::Result<Facts, String>;
    fn start(&self, machine_id: &str, req: &UpdateRequest) -> std::result::Result<serde_json::Value, Reach>;
    fn status(&self, machine_id: &str, job_id: &str) -> std::result::Result<Status, String>;
}

/// 작업 걸기가 안 된 까닭 — 기기가 판정해 거절한 것과 닿지 못한 것을 가른다. 거절은 다시 보내도 같고, 닿지 못함은 다시 해 본다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    Refused(String),
    Unreachable(String),
}

/// 작업 걸기 오류 글을 가른다 — 원격(`kasa_mcp::remote`)이 닿지 못했거나 앱 소켓이 끊긴 것은 `Unreachable`, 기기가 판정해 답한
/// 것(HTTP 4xx·앱의 거절 글)은 `Refused`.
pub fn reach_of(err: &str) -> Reach {
    const CUT: [&str; 6] = ["원격 POST 요청", "원격 답 해석", "error sending request", "timed out", "Connection refused", "connect to"];
    if CUT.iter().any(|w| err.contains(w)) {
        Reach::Unreachable(err.to_string())
    } else {
        Reach::Refused(err.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct RunPolicy {
    pub poll_every: std::time::Duration,
    /// 기기가 기다림(`waiting:`)·끊김(`retry:`)을 적는 동안 같은 작업을 다시 보내는 간격 — 기기는 그때마다 사실·승인을 다시 잰다.
    pub resend_every_ms: u64,
    /// 도우미에 넘기기 전까지 기다리는 상한. 기기 쪽 `MAX_WAIT_MS` 와 같다.
    pub wait_cap_ms: u64,
    /// 도우미에 넘긴 뒤 새 판 도착까지 — 종료 60초 + 부팅 90초 + 여유.
    pub boot_cap_ms: u64,
    /// 재기동 중 끊기는 것은 정상이다 — 연달아 이만큼 못 물으면 실패로 친다.
    pub max_status_errors: u32,
}

impl Default for RunPolicy {
    fn default() -> Self {
        Self { poll_every: std::time::Duration::from_secs(2), resend_every_ms: 20_000, wait_cap_ms: MAX_WAIT_MS,
               boot_cap_ms: (60 + 90 + 60) * 1000, max_status_errors: 60 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Updated { job_id: String, new_pid: u32, build: String },
    /// 조종 기기 — 도우미에 넘기면 자기를 끈다. 새 판 확인은 다시 뜬 뒤 `status` 로 본다.
    HandedOff { job_id: String },
    Failed { job_id: Option<String>, reason: String },
    Skipped { reason: String },
}

/// 한 번 소비한 기록. 다시 굴릴 때 이것이 있으면 소비하지 않고 나쵸에서 읽기만 한다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub rollout: String,
    pub approval_id: String,
    pub scope_hash: String,
    pub consumed_at_ms: u64,
    pub expires_at_ms: u64,
}

fn last_note(s: &Status) -> &str {
    s.events.last().map(|e| e.note.as_str()).unwrap_or("")
}

/// 대상 하나가 지금 새 판을 받아도 되는가 — 소비 전 한 번씩. 기다리면 풀리는 것(바쁜 학생 등)은 막지 않는다.
fn preflight(job: &UpdateJob, facts: &Facts, now_ms: u64) -> std::result::Result<(), String> {
    if facts.machine_id != job.machine_id {
        return Err("다른 기기가 답했다".into());
    }
    if facts.update_capability < CAPABILITY {
        return Err("이 기기 앱엔 업데이트 창구가 없다(update_endpoint_missing)".into());
    }
    if !facts.update_enabled {
        return Err("update_disabled — 이 기기의 설치 스위치가 꺼져 있다".into());
    }
    if let Some(refusal) = blocking(facts, &job.job_id, now_ms, false).first() {
        return Err(refusal.message());
    }
    if app_restart::target_hash(facts) != job.target_hash {
        return Err("계획 뒤 대상이 바뀌었다(pid·바이너리·자기설치 예정) — 새 rollout 으로".into());
    }
    Ok(())
}

/// rollout 을 한 대씩 굴린다. 모두 준비가 됐을 때만 승인을 한 번 소비하고(기록이 있으면 읽기만), 앞 기기가 새 판으로 끝나야
/// 다음 기기로 간다. 한 대라도 끝나지 않으면 뒤는 건드리지 않는다. 조종 기기는 맨 뒤라 넘기고 끝난다.
#[allow(clippy::too_many_arguments)]
pub fn run(
    rollout: &Rollout,
    approval_id: &str,
    authority: &dyn Authority,
    transport: &dyn Transport,
    policy: &RunPolicy,
    grant: Option<&Grant>,
    save_grant: &dyn Fn(&Grant) -> std::result::Result<(), String>,
    now_ms: &dyn Fn() -> u64,
    sleep: &dyn Fn(std::time::Duration),
) -> Vec<(String, Outcome)> {
    let ids: Vec<String> = rollout.jobs.iter().map(|j| j.machine_id.clone()).collect();
    let stop_all = |failed: Option<(usize, String)>, why: String| -> Vec<(String, Outcome)> {
        ids.iter().enumerate().map(|(n, id)| {
            let outcome = match &failed {
                Some((k, reason)) if *k == n => Outcome::Failed { job_id: None, reason: reason.clone() },
                _ => Outcome::Skipped { reason: why.clone() },
            };
            (id.clone(), outcome)
        }).collect()
    };
    if let Err(why) = check_rollout(rollout) {
        return stop_all(None, format!("rollout 을 믿을 수 없어 시작하지 않았다 · {why}"));
    }
    if !app_restart::valid_approval_id(approval_id) {
        return stop_all(None, "approval id 모양이 아니다(ap_<32 hex>)".into());
    }
    // 앞서 굴려 이미 새 판인 기기는 건너뛴다 — 그 기기의 지금 정체는 rollout 과 다르다(새 pid).
    let done: Vec<bool> = rollout.jobs.iter()
        .map(|j| grant.is_some() && matches!(transport.status(&j.machine_id, &j.job_id), Ok(s) if s.state == State::Done))
        .collect();
    let started: Vec<bool> = rollout.jobs.iter()
        .map(|j| grant.is_some() && transport.status(&j.machine_id, &j.job_id).is_ok())
        .collect();
    for (n, job) in rollout.jobs.iter().enumerate() {
        if done[n] || started[n] {
            continue;
        }
        let checked = transport.facts(&job.machine_id).and_then(|f| preflight(job, &f, now_ms()));
        if let Err(why) = checked {
            return stop_all(Some((n, why)), "앞 기기가 준비되지 않아 승인을 소비하지 않았다".into());
        }
    }
    let scope = &rollout.approval_scope;
    let hash = scope_hash(scope);
    let expires_at_ms = match grant {
        Some(g) if g.rollout != rollout.id || g.approval_id != approval_id || g.scope_hash != hash => {
            return stop_all(None, "이 rollout 은 다른 승인으로 이미 소비됐다 — 기록을 확인".into());
        }
        Some(g) => match usable_approval(authority, approval_id, now_ms()) {
            Ok(view) if view.scope_hash == hash && view.consumed_by.as_deref() == Some(rollout.controller()) => view.expires_at_ms,
            Ok(_) => return stop_all(None, "나쵸의 승인이 소비 기록과 다르다".into()),
            Err(why) => return stop_all(None, format!("이미 소비한 승인을 더 쓸 수 없다({}) · {why}", g.approval_id)),
        },
        None => {
            let view = match authority.consume(approval_id, scope, rollout.controller()) {
                Ok(view) => view,
                Err(why) => return stop_all(None, format!("승인을 쓰지 못해 시작하지 않았다 · {why}")),
            };
            if view.action != ACTION || view.scope != *scope || view.consumed_by.as_deref() != Some(rollout.controller()) {
                return stop_all(None, "나쵸가 돌려준 승인이 이 rollout 과 맞지 않는다".into());
            }
            let record = Grant { rollout: rollout.id.clone(), approval_id: approval_id.into(), scope_hash: hash.clone(),
                                 consumed_at_ms: view.consumed_at_ms.unwrap_or_else(now_ms), expires_at_ms: view.expires_at_ms };
            if let Err(why) = save_grant(&record) {
                return stop_all(None, format!("소비는 됐는데 기록을 못 남겼다 — 다시 굴리기 전에 사람 확인 · {why}"));
            }
            view.expires_at_ms
        }
    };
    let mut out = Vec::new();
    let mut stop: Option<String> = None;
    for (n, job) in rollout.jobs.iter().enumerate() {
        let id = job.machine_id.clone();
        if let Some(reason) = &stop {
            out.push((id, Outcome::Skipped { reason: reason.clone() }));
            continue;
        }
        let controller = id == rollout.controller();
        let outcome = if done[n] {
            match transport.facts(&id) {
                Ok(f) => Outcome::Updated { job_id: job.job_id.clone(), new_pid: f.pid, build: f.binary.build },
                Err(e) => Outcome::Failed { job_id: Some(job.job_id.clone()), reason: format!("새 판 사실을 못 읽었다 · {e}") },
            }
        } else {
            run_one(job, approval_id, rollout.controller(), controller, expires_at_ms, transport, policy, now_ms, sleep)
        };
        if !matches!(outcome, Outcome::Updated { .. } | Outcome::HandedOff { .. }) {
            stop = Some(format!("앞 기기({id})가 새 판으로 끝나지 않아 멈췄다"));
        }
        out.push((id, outcome));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn run_one(
    job: &UpdateJob,
    approval_id: &str,
    authority_machine: &str,
    controller: bool,
    expires_at_ms: u64,
    transport: &dyn Transport,
    policy: &RunPolicy,
    now_ms: &dyn Fn() -> u64,
    sleep: &dyn Fn(std::time::Duration),
) -> Outcome {
    let id = &job.machine_id;
    let job_id = job.job_id.clone();
    let fail = |reason: String| Outcome::Failed { job_id: Some(job_id.clone()), reason };
    if now_ms() >= expires_at_ms {
        return fail("승인이 만료됐다 — 이 기기부터는 새 승인으로".into());
    }
    let req = UpdateRequest { job: job.clone(), approval_id: approval_id.into(), authority: authority_machine.into() };
    let send = || -> std::result::Result<(), Reach> { transport.start(id, &req).map(|_| ()) };
    let mut errors = 0u32;
    let mut sent_at = now_ms();
    loop {
        match send() {
            Ok(()) => break,
            Err(Reach::Refused(why)) => return fail(format!("기기가 작업을 받지 않았다 · {why}")),
            Err(Reach::Unreachable(why)) => {
                errors += 1;
                if errors > policy.max_status_errors || now_ms() >= expires_at_ms {
                    return fail(format!("기기에 닿지 못했다 · {why}"));
                }
            }
        }
        sleep(policy.poll_every);
        sent_at = now_ms();
    }
    let started = now_ms();
    let mut armed_at: Option<u64> = None;
    errors = 0;
    loop {
        match transport.status(id, &job_id) {
            Ok(s) => {
                errors = 0;
                match s.state {
                    State::Done => break,
                    State::Failed | State::RolledBack | State::Cancelled => {
                        return fail(format!("기기가 {} 로 끝냈다 · {}", s.state.word(), last_note(&s)));
                    }
                    st if st >= State::Armed => {
                        if controller {
                            return Outcome::HandedOff { job_id: job_id.clone() };
                        }
                        armed_at.get_or_insert(now_ms());
                    }
                    _ => {
                        let note = last_note(&s);
                        let stalled = note.starts_with("waiting: ") || note.starts_with("retry: ");
                        if stalled && now_ms().saturating_sub(sent_at) >= policy.resend_every_ms {
                            if now_ms() >= expires_at_ms {
                                return fail(format!("승인이 만료될 때까지 적용하지 못했다 · {note}"));
                            }
                            match send() {
                                Ok(()) => {}
                                Err(Reach::Refused(why)) => return fail(format!("다시 보내기를 기기가 거절했다 · {why}")),
                                Err(Reach::Unreachable(_)) => errors += 1,
                            }
                            sent_at = now_ms();
                        }
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if errors > policy.max_status_errors {
                    let when = if armed_at.is_some() { "재기동 뒤" } else { "적용 전" };
                    return fail(format!("{when} 기기에 다시 닿지 못했다 — 사람 확인 필요 · {e}"));
                }
            }
        }
        match armed_at {
            None if now_ms().saturating_sub(started) > policy.wait_cap_ms => {
                return fail("30분 안에 적용하지 못했다(바쁜 학생·미저장 편집기) — 새 rollout 으로".into());
            }
            Some(at) if now_ms().saturating_sub(at) > policy.boot_cap_ms => {
                return fail("제한 시간 안에 새 판이 도착하지 않았다 — 사람 확인 필요".into());
            }
            _ => {}
        }
        sleep(policy.poll_every);
    }
    match transport.facts(id) {
        Ok(after) if after.pid != job.old_pid && build_matches(&after.binary.build, &job.build) => {
            Outcome::Updated { job_id: job_id.clone(), new_pid: after.pid, build: after.binary.build }
        }
        Ok(after) if after.pid == job.old_pid => fail("기기는 끝났다고 했지만 옛 pid 가 그대로다".into()),
        Ok(after) => fail(format!("다시 뜬 앱의 빌드가 태그 커밋이 아니다({})", after.binary.build)),
        Err(e) => fail(format!("새 판 사실을 못 읽었다 · {e}")),
    }
}

/// 앱이 대는 빌드 표식(`git rev-parse --short`, 더러우면 `+`)이 태그 커밋과 같은가.
pub fn build_matches(build: &str, commit: &str) -> bool {
    let build = build.trim_end_matches('+');
    build.len() >= 7 && (commit.starts_with(build) || build.starts_with(commit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_restart::{target_hash, ApprovalView, BinaryId};
    use std::cell::RefCell;
    use std::collections::HashMap;

    const NOW: u64 = 1_790_000_000_000;
    const AP: &str = "ap_0123456789abcdef0123456789abcdef";
    const TEAM: &str = "L366799VND";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kasa-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn facts(installed: &Path) -> Facts {
        Facts {
            schema: app_restart::SCHEMA.into(),
            machine_id: "mac-1".into(),
            label: "미니".into(),
            os: "macos".into(),
            capability: app_restart::CAPABILITY,
            app_path: installed.display().to_string(),
            pid: 4242,
            // 대상 맥이 보고하는 문자열 그대로 — `join` 은 Windows 에서 `\` 를 섞어 `app_exe()` 와 갈린다.
            running_exe: format!("{}/Contents/MacOS/kasaterm", installed.display()),
            binary: BinaryId { inode: 1, mtime_ms: 2, build: "8933a0ca".into() },
            observed_at_ms: NOW,
            ..Facts::default()
        }
    }

    struct Key {
        pair: ring::signature::Ed25519KeyPair,
    }

    impl Key {
        fn new() -> Self {
            let rng = ring::rand::SystemRandom::new();
            let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
            Self { pair: ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap() }
        }
        fn public(&self) -> String {
            use ring::signature::KeyPair;
            base64::engine::general_purpose::STANDARD.encode(self.pair.public_key().as_ref())
        }
        fn sign(&self, data: &[u8]) -> String {
            base64::engine::general_purpose::STANDARD.encode(self.pair.sign(data).as_ref())
        }
    }

    const DMG: &[u8] = b"kasaterm v0.2.1 dmg body";

    fn job_for(f: &Facts, key: &Key) -> UpdateJob {
        let sha = format!("sha256:{:x}", sha2::Sha256::digest(DMG));
        let tag = "v0.2.1";
        UpdateJob {
            schema: SCHEMA.into(),
            job_id: job_id("abcdef0123456789", &f.machine_id, &sha),
            plan_hash: "abcdef0123456789".into(),
            machine_id: f.machine_id.clone(),
            target_hash: target_hash(f),
            tag: tag.into(),
            version: "0.2.1".into(),
            commit: "5e4d138684156c710831055f5b042688b335f617".into(),
            build: "9f1c2b3a".into(),
            asset: Asset { name: asset_name(tag), url: asset_url(tag, &asset_name(tag)), size: DMG.len() as u64, sha256: sha, ed_signature: key.sign(DMG) },
            team: TEAM.into(),
            require_notarized: true,
            old_pid: f.pid,
            created_at_ms: NOW,
        }
    }

    fn feed_for(job: &UpdateJob) -> String {
        format!(r#"<rss><channel><item><sparkle:version>{}</sparkle:version><enclosure url="{}" length="{}" type="application/octet-stream" sparkle:edSignature="{}"/></item></channel></rss>"#,
            job.version, job.asset.url, job.asset.size, job.asset.ed_signature)
    }

    struct Nacho(ApprovalView);

    impl Authority for Nacho {
        fn get(&self, _: &str) -> std::result::Result<ApprovalView, String> {
            Ok(self.0.clone())
        }
        fn consume(&self, _: &str, _: &serde_json::Value, _: &str) -> std::result::Result<ApprovalView, String> {
            Err("대상은 소비하지 않는다".into())
        }
    }

    fn approved(job: &UpdateJob) -> Nacho {
        Nacho(ApprovalView {
            id: AP.into(), action: ACTION.into(), scope: rollout_scope(std::slice::from_ref(job), "ctl"), scope_hash: "sha256:x".into(),
            state: "approved".into(), expires_at_ms: NOW + 600_000, consumed_at_ms: Some(NOW), consumed_by: Some("ctl".into()),
        })
    }

    fn req(job: &UpdateJob) -> UpdateRequest {
        UpdateRequest { job: job.clone(), approval_id: AP.into(), authority: "ctl".into() }
    }

    #[test]
    fn the_public_key_is_the_one_sparkle_already_trusts() {
        let bake = include_str!("../../../scripts/build-app.sh");
        assert!(bake.contains(&format!("<string>{ED_PUBLIC_KEY}</string>")));
    }

    #[test]
    fn only_the_official_release_file_for_the_tag_can_be_named() {
        let key = Key::new();
        let f = facts(Path::new("/Users/x/Applications/kasaterm.app"));
        let job = job_for(&f, &key);
        assert_eq!(check_job(&job), Ok(()));
        let cases: Vec<(UpdateJob, &str)> = vec![
            (UpdateJob { asset: Asset { url: "https://evil.example/kasaterm-v0.2.1.dmg".into(), ..job.asset.clone() }, ..job.clone() }, "공식 릴리스 주소"),
            (UpdateJob { asset: Asset { name: "../../etc/kasaterm.dmg".into(), ..job.asset.clone() }, ..job.clone() }, "공식 릴리스 주소"),
            (UpdateJob { asset: Asset { size: MAX_ASSET_BYTES + 1, ..job.asset.clone() }, ..job.clone() }, "상한"),
            (UpdateJob { asset: Asset { sha256: "md5:x".into(), ..job.asset.clone() }, ..job.clone() }, "sha256"),
            (UpdateJob { tag: "v0.2.2".into(), ..job.clone() }, "태그"),
            (UpdateJob { team: "".into(), ..job.clone() }, "팀"),
            (UpdateJob { require_notarized: false, ..job.clone() }, "공증"),
            (UpdateJob { job_id: "up0000000000000000".into(), ..job.clone() }, "작업 id"),
        ];
        for (bad, word) in cases {
            let why = check_job(&bad).unwrap_err();
            assert!(why.contains(word), "{word}: {why}");
        }
    }

    /// 조종 쪽(tools/release/devices.py `update_job_id`)도 같은 id 를 짓는다 — 양쪽 검사에 같은 값이 박혀 있다.
    #[test]
    fn the_job_id_matches_the_controller_side() {
        assert_eq!(job_id("abcdef0123456789", "mac-1", &format!("sha256:{}", "0".repeat(64))), "upd4e7a732ffa7a1c8");
    }

    #[test]
    fn the_feed_must_name_the_same_file() {
        let key = Key::new();
        let job = job_for(&facts(Path::new("/x/kasaterm.app")), &key);
        let item = parse_feed(&feed_for(&job)).unwrap();
        assert_eq!(check_feed(&job, &item), Ok(()));
        assert!(check_feed(&job, &FeedItem { version: "0.2.2".into(), ..item.clone() }).unwrap_err().contains("다른 판"));
        assert!(check_feed(&job, &FeedItem { length: 1, ..item.clone() }).unwrap_err().contains("다르다"));
        assert!(check_feed(&job, &FeedItem { signature: key.sign(b"other"), ..item }).unwrap_err().contains("다르다"));
        let real = parse_feed(include_str!("../../../docs/appcast.xml")).unwrap();
        assert!(real.url.starts_with(RELEASE_PREFIX) && real.length > 0 && !real.signature.is_empty());
    }

    #[test]
    fn authorize_needs_the_switch_the_machine_a_newer_version_and_a_consumed_approval() {
        let key = Key::new();
        let f = facts(Path::new("/Users/x/Applications/kasaterm.app"));
        let job = job_for(&f, &key);
        let ok = approved(&job);
        assert_eq!(authorize(&req(&job), &f, "0.2.0", &ok, true, NOW).map(|_| ()), Ok(()));
        let err = |f: &Facts, v: &str, n: &Nacho, on: bool, j: &UpdateJob| authorize(&req(j), f, v, n, on, NOW).unwrap_err();
        assert!(err(&f, "0.2.0", &ok, false, &job).starts_with("update_disabled"));
        assert!(err(&Facts { os: "windows".into(), ..f.clone() }, "0.2.0", &ok, true, &job).starts_with("unsupported_os"));
        let busy = Facts { busy: vec![app_restart::BusyPane { surface: "%1".into(), character: "아리스".into(), state: "working".into() }], ..f.clone() };
        // 바쁜 학생·미저장 편집기는 받기·준비를 막지 않는다 — 갈아 끼울 때(`blocking(apply)`) 막는다.
        let dirty = Facts { dirty_editors: 1, ..f.clone() };
        assert_eq!(authorize(&req(&job), &busy, "0.2.0", &ok, true, NOW).map(|_| ()), Ok(()));
        assert!(blocking(&busy, &job.job_id, NOW, true)[0].message().contains("아리스"));
        assert!(blocking(&dirty, &job.job_id, NOW, true)[0].message().contains("저장"));
        assert!(blocking(&dirty, &job.job_id, NOW, false).is_empty());
        assert!(err(&f, "0.2.1", &ok, true, &job).starts_with("downgrade"));
        assert!(err(&f, "0.3.0", &ok, true, &job).starts_with("downgrade"));
        assert!(err(&Facts { pid: 1, ..f.clone() }, "0.2.0", &ok, true, &job).contains("정체"));
        let mut v = ok.0.clone();
        v.consumed_at_ms = None;
        assert!(err(&f, "0.2.0", &Nacho(v), true, &job).contains("소비하지 않았다"));
        let mut v = ok.0.clone();
        v.expires_at_ms = NOW;
        assert!(err(&f, "0.2.0", &Nacho(v), true, &job).contains("만료"));
        let mut v = ok.0.clone();
        v.action = app_restart::ACTION.into();
        assert!(err(&f, "0.2.0", &Nacho(v), true, &job).contains("승인되지"));
        let mut v = ok.0.clone();
        v.scope["asset"]["sha256"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
        assert!(err(&f, "0.2.0", &Nacho(v), true, &job).contains("승인한 대상"));
    }

    #[test]
    fn one_job_per_plan_machine_and_file_and_only_forward() {
        let dir = tmp("records");
        let key = Key::new();
        let f = facts(Path::new("/x/kasaterm.app"));
        let job = job_for(&f, &key);
        assert!(create_job(&dir, &job).unwrap().1);
        assert!(!create_job(&dir, &job).unwrap().1, "다시 보내도 새 작업이 없다");
        let other = UpdateJob { asset: Asset { size: 1, ..job.asset.clone() }, ..job.clone() };
        assert!(create_job(&dir, &other).is_err(), "같은 id 다른 파일은 충돌");
        advance(&dir, &job.job_id, State::Fetched, "").unwrap();
        assert!(advance(&dir, &job.job_id, State::Accepted, "").is_err());
        assert!(advance(&dir, &job.job_id, State::RolledBack, "").is_err(), "갈아 끼우기 전엔 되돌림이 없다");
        advance(&dir, &job.job_id, State::Cancelled, "").unwrap();
        assert!(advance(&dir, &job.job_id, State::Staged, "").is_err(), "끝난 작업은 안 움직인다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_update_is_refused_while_one_is_in_flight_and_a_resend_returns_the_first() {
        let dir = tmp("accept");
        let key = Key::new();
        let f = facts(Path::new("/Users/x/Applications/kasaterm.app"));
        let job = job_for(&f, &key);
        let (s, created) = accept(&req(&job), &f, "0.2.0", &approved(&job), true, &dir, NOW).unwrap();
        assert!(created && s.state == State::Accepted);
        let (_, again) = accept(&req(&job), &f, "0.2.0", &approved(&job), true, &dir, NOW).unwrap();
        assert!(!again);
        let sig = key.sign(b"another file");
        let mut other = job.clone();
        other.asset.ed_signature = sig;
        other.asset.sha256 = format!("sha256:{}", "a".repeat(64));
        other.job_id = job_id(&other.plan_hash, &other.machine_id, &other.asset.sha256);
        let why = accept(&req(&other), &f, "0.2.0", &approved(&other), true, &dir, NOW).unwrap_err();
        assert!(why.starts_with("job_in_flight"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 받기·확인·준비의 가짜 — 공식 주소만 답하고, dmg 는 폴더로 흉내 낸다.
    #[derive(Default)]
    struct FakeWorld {
        feed: RefCell<Option<String>>,
        body: RefCell<Vec<u8>>,
        fetches: RefCell<u32>,
        /// 이만큼은 반쯤 받다가 끊긴다.
        cuts: RefCell<u32>,
        mounted: RefCell<Vec<PathBuf>>,
        identities: RefCell<HashMap<String, Identity>>,
        inner_version: RefCell<String>,
        inner_identity: RefCell<Identity>,
    }

    impl Effects for FakeWorld {
        fn feed(&self, url: &str, _: u64) -> std::result::Result<String, String> {
            assert_eq!(url, MAC_FEED, "공식 피드만 읽는다");
            self.feed.borrow().clone().ok_or_else(|| "끊김".to_string())
        }
        fn fetch(&self, url: &str, max: u64, dest: &Path) -> std::result::Result<(), FetchError> {
            assert!(url.starts_with(RELEASE_PREFIX), "공식 릴리스만 받는다: {url}");
            *self.fetches.borrow_mut() += 1;
            let body = self.body.borrow();
            if *self.cuts.borrow() > 0 {
                *self.cuts.borrow_mut() -= 1;
                std::fs::write(dest, &body[..body.len() / 2]).unwrap();
                return Err(FetchError::Cut("curl: (18) transfer closed with outstanding read data remaining".into()));
            }
            if body.len() as u64 > max {
                return Err(FetchError::TooLarge);
            }
            std::fs::write(dest, &*body).map_err(|e| FetchError::Cut(e.to_string()))
        }
        fn mount(&self, _: &Path, at: &Path) -> std::result::Result<(), String> {
            let app = at.join("kasaterm.app/Contents");
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(app.join("Info.plist"), format!("<key>CFBundleShortVersionString</key>\n<string>{}</string>", self.inner_version.borrow())).unwrap();
            std::fs::write(app.join("marker"), "new").unwrap();
            self.identities.borrow_mut().insert(at.join("kasaterm.app").display().to_string(), self.inner_identity.borrow().clone());
            self.mounted.borrow_mut().push(at.to_path_buf());
            Ok(())
        }
        fn unmount(&self, at: &Path) {
            self.mounted.borrow_mut().retain(|p| p != at);
        }
        fn identity(&self, app: &Path) -> std::result::Result<Identity, String> {
            Ok(self.identities.borrow().get(&app.display().to_string()).cloned().unwrap_or_default())
        }
        fn copy_bundle(&self, from: &Path, to: &Path) -> std::result::Result<(), String> {
            // 실제 번들 복사는 맥 도구가 하지만, 가짜는 어느 OS 에서든 같은 `cp -R` 결과(없는 `to` 를 사본으로)를 낸다.
            fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
                std::fs::create_dir_all(to)?;
                for entry in std::fs::read_dir(from)? {
                    let entry = entry?;
                    let dest = to.join(entry.file_name());
                    if entry.file_type()?.is_dir() { copy_tree(&entry.path(), &dest)? } else { std::fs::copy(entry.path(), dest).map(|_| ())? }
                }
                Ok(())
            }
            let copied = copy_tree(from, to);
            let ident = self.identities.borrow().get(&from.display().to_string()).cloned().unwrap_or_default();
            self.identities.borrow_mut().insert(to.display().to_string(), ident);
            copied.map_err(|e| format!("복사 실패: {e}"))
        }
    }

    struct World {
        dir: PathBuf,
        cache: PathBuf,
        installed: PathBuf,
        key: Key,
        job: UpdateJob,
        fx: FakeWorld,
    }

    fn world(name: &str) -> World {
        let root = tmp(name);
        let installed = root.join("Applications/kasaterm.app");
        std::fs::create_dir_all(installed.join("Contents")).unwrap();
        std::fs::write(installed.join("Contents/marker"), "old").unwrap();
        let key = Key::new();
        let job = job_for(&facts(&installed), &key);
        let fx = FakeWorld::default();
        *fx.feed.borrow_mut() = Some(feed_for(&job));
        *fx.body.borrow_mut() = DMG.to_vec();
        *fx.inner_version.borrow_mut() = "0.2.1".into();
        *fx.inner_identity.borrow_mut() = Identity { verified: true, team: Some(TEAM.into()), notarized: true };
        fx.identities.borrow_mut().insert(installed.display().to_string(), Identity { verified: true, team: Some(TEAM.into()), notarized: true });
        let dir = root.join("jobs");
        create_job(&dir, &job).unwrap();
        World { dir, cache: root.join("cache"), installed, key, job, fx }
    }

    impl World {
        fn prepare(&self) -> std::result::Result<PathBuf, String> {
            prepare(&self.dir, &self.job, &self.cache, &self.installed, &self.fx, &self.key.public())
        }
        fn state(&self) -> State {
            status(&self.dir, &self.job.job_id).unwrap().state
        }
    }

    #[test]
    fn a_verified_official_file_is_staged_beside_the_install_and_nothing_else_moves() {
        let w = world("stage-ok");
        let staged = w.prepare().unwrap();
        assert_eq!(staged, staged_path(&w.installed));
        assert_eq!(std::fs::read_to_string(staged.join("Contents/marker")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(w.installed.join("Contents/marker")).unwrap(), "old", "설치본은 안 건드린다");
        assert_eq!(w.state(), State::Staged);
        let words: Vec<String> = status(&w.dir, &w.job.job_id).unwrap().events.iter().map(|e| e.state.word()).collect();
        assert_eq!(words, ["accepted", "fetched", "checked", "staged"]);
        assert!(w.fx.mounted.borrow().is_empty(), "dmg 는 늘 닫는다");
        assert!(!work_dir(&w.cache, &w.job).join(format!("{}.part", w.job.asset.name)).exists());
        // 끊긴 뒤 다시 와도 다시 받지 않는다.
        assert_eq!(w.prepare().unwrap(), staged);
        assert_eq!(*w.fx.fetches.borrow(), 1);
    }

    #[test]
    fn a_cut_connection_is_not_the_end_the_same_job_picks_up_where_it_stopped() {
        let w = world("cut");
        *w.fx.feed.borrow_mut() = None;
        assert!(w.prepare().unwrap_err().contains("공식 피드를 못 읽었다"));
        assert_eq!(w.state(), State::Accepted, "피드가 안 닿은 것은 실패가 아니다");
        *w.fx.feed.borrow_mut() = Some(feed_for(&w.job));
        *w.fx.cuts.borrow_mut() = 1;
        assert!(w.prepare().unwrap_err().contains("끊겼다"));
        assert_eq!(w.state(), State::Accepted);
        assert!(!work_dir(&w.cache, &w.job).join(format!("{}.part", w.job.asset.name)).exists(), "반쯤 받은 것은 걷는다");
        let notes: Vec<String> = status(&w.dir, &w.job.job_id).unwrap().events.iter().map(|e| e.note.clone()).collect();
        assert_eq!(notes.iter().filter(|n| n.starts_with("retry: ")).count(), 2);
        w.prepare().unwrap();
        assert_eq!(w.state(), State::Staged);
        assert_eq!(*w.fx.fetches.borrow(), 2);
    }

    #[test]
    fn a_verified_file_from_an_interrupted_run_is_not_fetched_again() {
        let w = world("interrupted");
        // 받고 확인까지 한 뒤 앱이 꺼졌다 — 파일과 `fetched` 줄만 남았다.
        std::fs::create_dir_all(work_dir(&w.cache, &w.job)).unwrap();
        std::fs::write(work_dir(&w.cache, &w.job).join(&w.job.asset.name), DMG).unwrap();
        advance(&w.dir, &w.job.job_id, State::Fetched, "").unwrap();
        w.prepare().unwrap();
        assert_eq!(*w.fx.fetches.borrow(), 0);
        assert_eq!(w.state(), State::Staged);
        // 남은 파일이 바꿔치기돼 있으면 해시가 달라 다시 받는다 — 옛 파일을 믿지 않는다.
        let w = world("tampered");
        std::fs::create_dir_all(work_dir(&w.cache, &w.job)).unwrap();
        std::fs::write(work_dir(&w.cache, &w.job).join(&w.job.asset.name), b"kasaterm v0.2.1 dmg bodX").unwrap();
        w.prepare().unwrap();
        assert_eq!(*w.fx.fetches.borrow(), 1);
    }

    #[test]
    fn every_verification_failure_leaves_nothing_staged() {
        type Breaker = fn(&World);
        let cases: Vec<(&str, Breaker, &str)> = vec![
            ("feed-other", |w| *w.fx.feed.borrow_mut() = Some(feed_for(&UpdateJob { version: "0.2.2".into(), ..w.job.clone() })), "다른 판"),
            ("sha", |w| *w.fx.body.borrow_mut() = b"kasaterm v0.2.1 dmg bodX".to_vec(), "sha256"),
            ("oversize", |w| *w.fx.body.borrow_mut() = vec![0; DMG.len() + 5], "상한을 넘었다"),
            ("short", |w| *w.fx.body.borrow_mut() = DMG[..DMG.len() - 3].to_vec(), "받은 크기가 다르다"),
            ("team", |w| *w.fx.inner_identity.borrow_mut() = Identity { verified: true, team: Some("OTHERTEAM1".into()), notarized: true }, "팀"),
            ("unsigned", |w| *w.fx.inner_identity.borrow_mut() = Identity::default(), "서명이 깨졌다"),
            ("notarize", |w| *w.fx.inner_identity.borrow_mut() = Identity { verified: true, team: Some(TEAM.into()), notarized: false }, "공증"),
            ("version", |w| *w.fx.inner_version.borrow_mut() = "0.2.0".into(), "판이 작업의 판과 다르다"),
            // 설치본이 다른 팀으로 서명돼 있으면 요청이 댄 팀(=dmg 팀)과 맞아도 받지 않는다 — 기준은 설치본이다.
            ("installed-team", |w| { w.fx.identities.borrow_mut().insert(w.installed.display().to_string(),
                Identity { verified: true, team: Some("OTHERTEAM1".into()), notarized: true }); }, "설치본 팀"),
            ("installed-unsigned", |w| { w.fx.identities.borrow_mut().remove(&w.installed.display().to_string()); }, "설치본의 서명 팀"),
        ];
        for (name, breaker, word) in cases {
            let w = world(&format!("bad-{name}"));
            breaker(&w);
            let why = w.prepare().unwrap_err();
            assert!(why.contains(word), "{name}: {why}");
            assert_eq!(w.state(), State::Failed, "{name}");
            assert!(!staged_path(&w.installed).exists(), "{name}: 반쯤 준비한 것이 남았다");
            assert!(w.fx.mounted.borrow().is_empty(), "{name}: dmg 가 열린 채 남았다");
            assert!(!work_dir(&w.cache, &w.job).join(format!("{}.part", w.job.asset.name)).exists(), "{name}");
            assert_eq!(std::fs::read_to_string(w.installed.join("Contents/marker")).unwrap(), "old");
        }
        let w = world("bad-eddsa");
        let other = Key::new();
        let why = prepare(&w.dir, &w.job, &w.cache, &w.installed, &w.fx, &other.public()).unwrap_err();
        assert!(why.contains("EdDSA"), "{why}");
        assert!(!work_dir(&w.cache, &w.job).join(&w.job.asset.name).exists(), "서명이 안 맞는 파일은 지운다");
    }

    // ---------------------------------------------------------------- 도우미를 실제 sh 로

    const FAKE_BODY: &str = "exec -a \"$0\" /bin/sleep 30";

    fn start_fake(exe: &Path) -> std::process::Child {
        std::process::Command::new("/bin/bash").args(["-c", FAKE_BODY, &exe.display().to_string()]).spawn().unwrap()
    }

    fn fake_launch(exe: &Path) -> Vec<String> {
        vec!["/bin/bash".into(), "-c".into(), format!("({FAKE_BODY}) >/dev/null 2>&1 &"), exe.display().to_string()]
    }

    /// 새 판으로 떴다가 곧 꺼지는 앱 — 부팅 표식 없이 죽는 판을 흉내 낸다.
    fn crashing_launch(exe: &Path) -> Vec<String> {
        vec!["/bin/bash".into(), "-c".into(), "(exec -a \"$0\" /bin/sleep 0.5) >/dev/null 2>&1 &".into(), exe.display().to_string()]
    }

    fn wait_for(w: &World, want: &[State], secs: u64) -> State {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let s = w.state();
            if want.contains(&s) || s.terminal() || std::time::Instant::now() > until {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn marker(p: &Path) -> String {
        std::fs::read_to_string(p.join("Contents/marker")).unwrap_or_default()
    }

    fn kill_all(exe: &Path) {
        let out = std::process::Command::new("/bin/ps").args(["-Axww", "-o", "pid=,command="]).output().unwrap();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let line = line.trim_start();
            if let Some((pid, cmd)) = line.split_once(' ') {
                if cmd.starts_with(&exe.display().to_string()) {
                    let _ = std::process::Command::new("/bin/kill").arg(pid).status();
                }
            }
        }
    }

    fn armed(name: &str, launch: fn(&Path) -> Vec<String>, boot_s: u32) -> (World, std::process::Child, SwapSpec) {
        let w = world(name);
        w.prepare().unwrap();
        let exe = w.installed.join("Contents/MacOS/kasaterm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let old = start_fake(&exe);
        let spec = SwapSpec { dir: w.dir.clone(), job_id: w.job.job_id.clone(), old_pid: old.id(), installed: w.installed.clone(),
                              launch: launch(&exe), exit_timeout_s: 5, boot_timeout_s: boot_s };
        (w, old, spec)
    }

    #[cfg(unix)]
    #[test]
    fn the_armed_job_swaps_after_exit_and_finishes_on_the_new_boot_mark() {
        let (w, mut old, spec) = armed("swap-ok", fake_launch, 6);
        assert!(arm(&w.dir, &spec, false).unwrap_err().starts_with("update_disabled"));
        arm(&w.dir, &spec, true).unwrap();
        assert_eq!(wait_for(&w, &[State::HelperStarted], 5), State::HelperStarted);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(marker(&w.installed), "old", "앱이 살아 있는 동안은 안 갈아 끼운다");
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_for(&w, &[State::Launched], 8), State::Launched);
        assert_eq!(marker(&w.installed), "new");
        assert_eq!(marker(&previous_path(&w.installed)), "old", "이전 판은 곁에 남는다");
        // 새로 뜬 앱의 부팅 — 빌드 표식이 작업과 같으면 끝.
        let new_pid = status(&w.dir, &w.job.job_id).unwrap().events.iter().rev()
            .find(|e| e.state == State::Launched).unwrap().note.trim_start_matches("pid ").parse::<u32>().unwrap();
        let (_, end) = mark_booted(&w.dir, "mac-1", new_pid, "9f1c2b3a", NOW).unwrap();
        assert_eq!(end, State::Done);
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert_eq!(w.state(), State::Done);
        kill_all(&spec.installed.join("Contents/MacOS/kasaterm"));
    }

    #[cfg(unix)]
    #[test]
    fn a_new_app_that_dies_before_its_boot_mark_is_rolled_back_and_the_old_one_relaunched() {
        let (w, mut old, spec) = armed("swap-crash", crashing_launch, 3);
        arm(&w.dir, &spec, true).unwrap();
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_for(&w, &[State::RolledBack], 15), State::RolledBack);
        assert_eq!(marker(&w.installed), "old", "이전 판이 제자리로 돌아왔다");
        assert_eq!(marker(&failed_path(&w.installed)), "new", "실패한 판은 곁에 남긴다");
        kill_all(&spec.installed.join("Contents/MacOS/kasaterm"));
    }

    #[cfg(unix)]
    #[test]
    fn a_launch_that_fails_restores_the_previous_bundle() {
        let (w, mut old, spec) = armed("swap-launch-fail", |_| vec!["/usr/bin/false".into()], 2);
        arm(&w.dir, &spec, true).unwrap();
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_for(&w, &[State::RolledBack], 8), State::RolledBack);
        assert_eq!(marker(&w.installed), "old");
    }

    #[cfg(unix)]
    #[test]
    fn an_app_that_never_exits_is_never_forced_and_nothing_is_swapped() {
        let (w, mut old, spec) = armed("swap-stuck", fake_launch, 2);
        let spec = SwapSpec { exit_timeout_s: 1, ..spec };
        arm(&w.dir, &spec, true).unwrap();
        assert_eq!(wait_for(&w, &[State::Failed], 8), State::Failed);
        assert!(old.try_wait().unwrap().is_none(), "안 꺼지는 앱을 죽이지 않았다");
        assert_eq!(marker(&w.installed), "old");
        assert!(staged_path(&w.installed).exists(), "준비한 번들은 그대로 남는다");
        old.kill().unwrap();
        let _ = old.wait();
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_staged_bundle_or_a_second_instance_stops_before_touching_the_install() {
        let (w, mut old, spec) = armed("swap-missing", fake_launch, 2);
        arm(&w.dir, &spec, true).unwrap();
        std::fs::remove_dir_all(staged_path(&w.installed)).unwrap();
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_for(&w, &[State::Failed], 8), State::Failed);
        assert_eq!(marker(&w.installed), "old");

        let (w, mut old, spec) = armed("swap-second", fake_launch, 2);
        let exe = spec.installed.join("Contents/MacOS/kasaterm");
        arm(&w.dir, &spec, true).unwrap();
        let mut other = start_fake(&exe);
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_for(&w, &[State::Failed], 8), State::Failed);
        assert_eq!(marker(&w.installed), "old", "다른 인스턴스가 도는 동안은 안 갈아 끼운다");
        other.kill().unwrap();
        let _ = other.wait();
    }

    #[test]
    fn a_boot_with_the_wrong_build_is_a_failure_not_a_success() {
        let w = world("boot-wrong");
        for s in [State::Fetched, State::Checked, State::Staged, State::Armed, State::HelperStarted, State::Exited, State::Swapped] {
            advance(&w.dir, &w.job.job_id, s, "").unwrap();
        }
        assert!(mark_booted(&w.dir, "mac-1", w.job.old_pid, "9f1c2b3a", NOW).is_none(), "옛 pid 는 부팅이 아니다");
        assert!(mark_booted(&w.dir, "mac-2", 7, "9f1c2b3a", NOW).is_none(), "다른 기기");
        let (_, end) = mark_booted(&w.dir, "mac-1", 7, "deadbeef", NOW).unwrap();
        assert_eq!(end, State::Failed);
    }

    #[test]
    fn nothing_is_armed_before_it_is_staged_or_while_the_switch_is_off() {
        let w = world("arm-early");
        let spec = SwapSpec { dir: w.dir.clone(), job_id: w.job.job_id.clone(), old_pid: 1, installed: w.installed.clone(),
                              launch: vec!["/usr/bin/true".into()], exit_timeout_s: 1, boot_timeout_s: 1 };
        #[cfg(unix)]
        {
            assert!(arm(&w.dir, &spec, true).unwrap_err().contains("준비되지 않은"));
            assert!(arm(&w.dir, &spec, false).unwrap_err().starts_with("update_disabled"));
        }
        assert!(!install_enabled(&|_| None));
        assert!(!install_enabled(&|_| Some("1".into())), "켜는 값은 on 하나뿐");
        assert!(install_enabled(&|k| (k == "KASATERM_APP_UPDATE").then(|| "on".to_string())));
    }

    fn device<'a>(w: &'a World, launch: Vec<String>, public: &'a str) -> Device<'a> {
        Device { dir: w.dir.clone(), cache: w.cache.clone(), installed: w.installed.clone(), effects: &w.fx, public_key: public,
                 enabled: true, launch, exit_timeout_s: 5, boot_timeout_s: 6 }
    }

    /// 받기부터 새 판 부팅까지 — 받는 사이 학생이 일을 시작하면 준비한 채 기다리고, 다시 보내면 이어서 갈아 끼운다.
    #[cfg(unix)]
    #[test]
    fn drive_waits_for_busy_students_then_swaps_and_finishes_on_boot() {
        let w = world("drive");
        let exe = w.installed.join("Contents/MacOS/kasaterm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let mut old = start_fake(&exe);
        let mut f = facts(&w.installed);
        f.pid = old.id();
        let job = UpdateJob { old_pid: f.pid, target_hash: target_hash(&f), ..w.job.clone() };
        let job = UpdateJob { job_id: job_id(&job.plan_hash, &job.machine_id, &job.asset.sha256), ..job };
        create_job(&w.dir, &job).unwrap();
        let w = World { job, ..w };
        let public = w.key.public();
        let dev = device(&w, fake_launch(&exe), &public);
        let busy = Facts { busy: vec![app_restart::BusyPane { surface: "%2".into(), character: "모모이".into(), state: "working".into() }],
                           dirty_editors: 1, ..f.clone() };
        let step = drive(&dev, &w.job, &|| Ok(busy.clone()), &|| Ok(()), &|| NOW).unwrap();
        assert!(matches!(&step, Step::Waiting(why) if why.contains("모모이")), "{step:?}");
        assert_eq!(w.state(), State::Staged);
        assert_eq!(marker(&w.installed), "old");
        let step = drive(&dev, &w.job, &|| Ok(f.clone()), &|| Ok(()), &|| NOW).unwrap();
        assert!(matches!(step, Step::Armed(_)), "{step:?}");
        assert_eq!(*w.fx.fetches.borrow(), 1, "기다린 뒤에도 다시 받지 않는다");
        assert!(drive(&dev, &w.job, &|| Ok(f.clone()), &|| Ok(()), &|| NOW).is_err(), "도우미는 한 번만");
        old.kill().unwrap();
        let _ = old.wait();
        assert_eq!(wait_for(&w, &[State::Launched], 8), State::Launched);
        let new_pid = status(&w.dir, &w.job.job_id).unwrap().events.iter().rev()
            .find(|e| e.state == State::Launched).unwrap().note.trim_start_matches("pid ").parse::<u32>().unwrap();
        assert_eq!(mark_booted(&w.dir, "mac-1", new_pid, "9f1c2b3a+", NOW).unwrap().1, State::Done);
        assert_eq!(marker(&w.installed), "new");
        kill_all(&exe);
    }

    #[cfg(unix)]
    #[test]
    fn drive_refuses_when_the_machine_changed_or_the_switch_is_off() {
        let w = world("drive-moved");
        let public = w.key.public();
        let off = Device { enabled: false, ..device(&w, vec!["/usr/bin/true".into()], &public) };
        assert!(drive(&off, &w.job, &|| Ok(facts(&w.installed)), &|| Ok(()), &|| NOW).unwrap_err().starts_with("update_disabled"));
        assert_eq!(*w.fx.fetches.borrow(), 0, "꺼져 있으면 받지도 않는다");
        let dev = device(&w, vec!["/usr/bin/true".into()], &public);
        // 기다림 상한은 조종 쪽이 적은 시각이 아니라 이 기기가 받은 때부터 잰다.
        let accepted = accepted_at_ms(&status(&w.dir, &w.job.job_id).unwrap());
        assert!(accepted > w.job.created_at_ms);
        let stale = drive(&dev, &w.job, &|| Ok(facts(&w.installed)), &|| Ok(()), &|| accepted + MAX_WAIT_MS + 1).unwrap_err();
        assert!(stale.contains("30분"), "{stale}");
        assert_eq!(w.state(), State::Failed);
        let w = world("drive-moved-2");
        let public = w.key.public();
        let dev = device(&w, vec!["/usr/bin/true".into()], &public);
        let moved = Facts { pid: 999, ..facts(&w.installed) };
        assert!(drive(&dev, &w.job, &|| Ok(moved.clone()), &|| Ok(()), &|| NOW).unwrap_err().contains("정체"));
        assert_eq!(w.state(), State::Failed);
        assert_eq!(marker(&w.installed), "old");
        let pending = Facts { install_pending: Some(app_restart::PendingInstall { dist_path: "/x/dist".into(), dist_mtime_ms: 1 }), ..facts(&w.installed) };
        assert!(!blocking(&pending, &w.job.job_id, NOW, false).is_empty(), "자기설치 예정이면 받기부터 막는다");
        let own = Facts { active_job: Some(w.job.job_id.clone()), ..facts(&w.installed) };
        assert!(blocking(&own, &w.job.job_id, NOW, true).is_empty(), "자기 작업은 진행 중인 작업으로 안 친다");
        let other = Facts { active_job: Some("rs0123456789abcdef".into()), ..facts(&w.installed) };
        assert!(!blocking(&other, &w.job.job_id, NOW, false).is_empty(), "재시작이 도는 중이면 막는다");
    }

    #[test]
    fn a_revoked_or_expired_approval_is_not_usable() {
        let key = Key::new();
        let job = job_for(&facts(Path::new("/Users/x/Applications/kasaterm.app")), &key);
        let mut v = approved(&job).0;
        assert!(usable_approval(&Nacho(v.clone()), AP, NOW).is_ok());
        v.state = "revoked".into();
        assert!(usable_approval(&Nacho(v.clone()), AP, NOW).unwrap_err().contains("revoked"));
        v.state = "approved".into();
        assert!(usable_approval(&Nacho(v), AP, NOW + 600_000).unwrap_err().contains("만료"));
    }

    #[cfg(unix)]
    #[test]
    fn an_approval_revoked_while_waiting_stops_the_swap_before_the_helper() {
        let w = world("drive-revoked");
        let public = w.key.public();
        let dev = device(&w, vec!["/usr/bin/true".into()], &public);
        let why = drive(&dev, &w.job, &|| Ok(facts(&w.installed)), &|| Err("승인되지 않았다(kasaterm_update revoked)".into()), &|| NOW).unwrap_err();
        assert!(why.contains("revoked"), "{why}");
        assert_eq!(w.state(), State::Failed, "거둔 승인이면 끝낸다 — 재시작을 오래 막지 않게");
        assert_eq!(marker(&w.installed), "old");
        assert!(!status(&w.dir, &w.job.job_id).unwrap().events.iter().any(|e| e.state == State::Armed), "도우미에 넘기지 않았다");
    }

    // ---------------------------------------------------------------- 러너

    const RID: &str = "abcdef0123456789";
    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn fleet_facts(mid: &str, pid: u32, build: &str) -> Facts {
        let app = "/Users/x/Applications/kasaterm.app";
        Facts {
            schema: app_restart::SCHEMA.into(), machine_id: mid.into(), label: mid.into(), os: "macos".into(),
            capability: app_restart::CAPABILITY, app_path: app.into(), pid, running_exe: format!("{app}/Contents/MacOS/kasaterm"),
            binary: BinaryId { inode: 1, mtime_ms: 2, build: build.into() }, observed_at_ms: NOW,
            update_capability: CAPABILITY, update_enabled: true, ..Facts::default()
        }
    }

    fn fleet_job(f: &Facts) -> UpdateJob {
        let tag = "v0.2.1";
        UpdateJob {
            schema: SCHEMA.into(), job_id: job_id(RID, &f.machine_id, SHA), plan_hash: RID.into(), machine_id: f.machine_id.clone(),
            target_hash: target_hash(f), tag: tag.into(), version: "0.2.1".into(),
            commit: "5e4d138684156c710831055f5b042688b335f617".into(), build: "5e4d138684156c710831055f5b042688b335f617".into(),
            asset: Asset { name: asset_name(tag), url: asset_url(tag, &asset_name(tag)), size: 10, sha256: SHA.into(),
                           ed_signature: base64::engine::general_purpose::STANDARD.encode([7u8; 64]) },
            team: TEAM.into(), require_notarized: true, old_pid: f.pid, created_at_ms: NOW,
        }
    }

    fn rollout_of(jobs: Vec<UpdateJob>, controller: &str) -> Rollout {
        let scope = rollout_scope(&jobs, controller);
        Rollout { id: RID.into(), release_plan: "0011223344556677".into(), created_at_ms: NOW,
                  approval_scope_hash: scope_hash(&scope), approval_scope: scope, jobs, excluded: vec![] }
    }

    #[test]
    fn the_scope_hash_is_the_one_nacho_computes() {
        let scope = serde_json::json!({"action": "kasaterm_update", "plan": RID, "controller": "ctl", "tag": "v0.2.1", "version": "0.2.1",
            "commit": "5e4d138684156c710831055f5b042688b335f617", "asset": {"name": "kasaterm-v0.2.1.dmg", "sha256": SHA, "size": 10},
            "targets": [{"order": 1, "machine_id": "mac-a", "hash": "0123456789abcdef"}, {"order": 2, "machine_id": "ctl", "hash": "fedcba9876543210"}]});
        // tools/release/nacho.py·나쵸 approvals.scope_hash 와 같은 정규화(키 정렬·빈칸 없음)로 파이썬이 잰 값.
        assert_eq!(scope_hash(&scope), "sha256:5352981248969a5cb4a59cf3e9a06ea348894a5d01a3d21dc65af53f03d9f720");
    }

    /// 조종 쪽 파이썬(tools/release/devices.py `target_hash`)도 같은 값을 짓는다 — 격리 왕복이 파이썬이 지은 rollout 을 이 판정에 넣는다.
    #[test]
    fn the_target_hash_matches_the_controller_side() {
        assert_eq!(target_hash(&fleet_facts("mac-a", 11, "8933a0ca")), "d206bd151df55dd2");
    }

    #[test]
    fn a_rollout_must_be_one_file_one_version_controller_last_and_its_own_scope() {
        let (a, c) = (fleet_facts("mac-a", 11, "8933a0ca"), fleet_facts("ctl", 22, "8933a0ca"));
        let ok = rollout_of(vec![fleet_job(&a), fleet_job(&c)], "ctl");
        assert_eq!(check_rollout(&ok), Ok(()));
        assert_eq!(ok.approval_scope.as_object().unwrap().len(), 8, "나쵸 계약 키 8개");
        let last = rollout_of(vec![fleet_job(&c), fleet_job(&a)], "ctl");
        assert!(check_rollout(&last).unwrap_err().contains("맨 뒤"));
        let mut mixed = ok.clone();
        mixed.jobs[1].version = "0.2.2".into();
        mixed.jobs[1].tag = "v0.2.2".into();
        assert!(check_rollout(&mixed).is_err());
        let mut forged = ok.clone();
        forged.approval_scope["targets"][0]["hash"] = serde_json::json!("ffffffffffffffff");
        assert!(check_rollout(&forged).unwrap_err().contains("다시 지은 범위"));
        let twice = rollout_of(vec![fleet_job(&a), fleet_job(&a)], "ctl");
        assert!(check_rollout(&twice).unwrap_err().contains("두 번"));
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mode { Normal, Busy(u32), FailAt, RefuseResend, NeverArm, Gap(u32), WrongBuild }

    struct Dev { facts: Facts, mode: Mode, job: Option<UpdateJob>, polls: u32, sends: Vec<u64>, done: bool }

    struct Fleet { devs: RefCell<HashMap<String, Dev>>, clock: std::rc::Rc<std::cell::Cell<u64>> }

    impl Fleet {
        fn new(devs: Vec<(Facts, Mode)>, clock: std::rc::Rc<std::cell::Cell<u64>>) -> Self {
            Self { devs: RefCell::new(devs.into_iter().map(|(f, m)| (f.machine_id.clone(),
                   Dev { facts: f, mode: m, job: None, polls: 0, sends: vec![], done: false })).collect()), clock }
        }
        fn sends(&self, id: &str) -> Vec<u64> { self.devs.borrow()[id].sends.clone() }
        fn started(&self, id: &str) -> bool { self.devs.borrow()[id].job.is_some() }
    }

    fn st(job: &UpdateJob, state: State, note: &str) -> Status {
        Status { job: job.clone(), state, events: vec![Event { at_s: NOW / 1000, state, note: note.into() }] }
    }

    impl Transport for Fleet {
        fn facts(&self, id: &str) -> std::result::Result<Facts, String> {
            let devs = self.devs.borrow();
            let d = devs.get(id).ok_or("없는 기기")?;
            let mut f = d.facts.clone();
            if d.done {
                f.pid += 1000;
                f.binary.build = if d.mode == Mode::WrongBuild { "deadbeef".into() } else { "5e4d138".into() };
            }
            Ok(f)
        }
        fn start(&self, id: &str, req: &UpdateRequest) -> std::result::Result<serde_json::Value, Reach> {
            let mut devs = self.devs.borrow_mut();
            let d = devs.get_mut(id).unwrap();
            if d.mode == Mode::RefuseResend && !d.sends.is_empty() {
                return Err(Reach::Refused("승인되지 않았다(kasaterm_update revoked)".into()));
            }
            d.sends.push(self.clock.get());
            d.job = Some(req.job.clone());
            Ok(serde_json::json!({"ok": true}))
        }
        fn status(&self, id: &str, job_id: &str) -> std::result::Result<Status, String> {
            let mut devs = self.devs.borrow_mut();
            let d = devs.get_mut(id).unwrap();
            if d.done {
                return Ok(st(d.job.as_ref().unwrap(), State::Done, "build matches"));
            }
            let Some(job) = d.job.clone() else { return Err("no such job".into()) };
            assert_eq!(job.job_id, job_id);
            d.polls += 1;
            let waiting = "waiting: 일하는 중이거나 사람 답을 기다리는 학생 1명";
            let s = match d.mode {
                Mode::FailAt => st(&job, State::Failed, "받은 파일의 sha256 이 작업과 다르다"),
                Mode::NeverArm | Mode::RefuseResend => st(&job, State::Staged, waiting),
                Mode::Busy(n) if (d.sends.len() as u32) <= n => st(&job, State::Staged, waiting),
                Mode::Gap(n) => match d.polls {
                    1 => st(&job, State::Armed, ""),
                    p if p <= 1 + n => return Err("재기동 중 — 닿지 않음".into()),
                    _ => { d.done = true; st(&job, State::Done, "") }
                },
                _ if d.polls > 3 => { d.done = true; st(&job, State::Done, "build matches") }
                _ => st(&job, State::Armed, ""),
            };
            Ok(s)
        }
    }

    struct Counting { view: RefCell<ApprovalView>, consumes: RefCell<u32>, gets: RefCell<u32>, ttl: std::cell::Cell<u64> }

    impl Authority for Counting {
        fn get(&self, _: &str) -> std::result::Result<ApprovalView, String> {
            *self.gets.borrow_mut() += 1;
            Ok(self.view.borrow().clone())
        }
        fn consume(&self, _: &str, scope: &serde_json::Value, consumer: &str) -> std::result::Result<ApprovalView, String> {
            *self.consumes.borrow_mut() += 1;
            let mut v = self.view.borrow_mut();
            if v.consumed_at_ms.is_some() {
                return Err("already_used".into());
            }
            assert_eq!(scope, &v.scope);
            v.consumed_at_ms = Some(NOW);
            v.consumed_by = Some(consumer.into());
            v.expires_at_ms = NOW + self.ttl.get();
            Ok(v.clone())
        }
    }

    fn nacho_for(r: &Rollout) -> Counting {
        Counting { view: RefCell::new(ApprovalView { id: AP.into(), action: ACTION.into(), scope: r.approval_scope.clone(),
                   scope_hash: r.approval_scope_hash.clone(), state: "approved".into(), expires_at_ms: NOW + 600_000,
                   consumed_at_ms: None, consumed_by: None }), consumes: RefCell::new(0), gets: RefCell::new(0),
                   ttl: std::cell::Cell::new(2 * 35 * 60_000) }
    }

    struct Harness { clock: std::rc::Rc<std::cell::Cell<u64>>, saved: RefCell<Option<Grant>> }

    impl Harness {
        fn new() -> Self { Self { clock: std::rc::Rc::new(std::cell::Cell::new(NOW)), saved: RefCell::new(None) } }
        fn go(&self, r: &Rollout, nacho: &Counting, fleet: &Fleet, grant: Option<&Grant>, policy: &RunPolicy) -> Vec<(String, Outcome)> {
            let clock = self.clock.clone();
            run(r, AP, nacho, fleet, policy, grant, &|g| { *self.saved.borrow_mut() = Some(g.clone()); Ok(()) },
                &|| clock.get(), &|d| clock.set(clock.get() + d.as_millis() as u64))
        }
    }

    fn outcome_words(out: &[(String, Outcome)]) -> Vec<String> {
        out.iter().map(|(id, o)| format!("{id}:{}", serde_json::to_value(o).unwrap()["outcome"].as_str().unwrap())).collect()
    }

    fn two(mode_a: Mode) -> (Rollout, Vec<(Facts, Mode)>) {
        let (a, c) = (fleet_facts("mac-a", 11, "8933a0ca"), fleet_facts("ctl", 22, "8933a0ca"));
        (rollout_of(vec![fleet_job(&a), fleet_job(&c)], "ctl"), vec![(a, mode_a), (c, Mode::Normal)])
    }

    #[test]
    fn the_runner_consumes_once_updates_in_order_and_hands_off_on_the_controller() {
        let h = Harness::new();
        let (r, devs) = two(Mode::Normal);
        let fleet = Fleet::new(devs, h.clock.clone());
        let nacho = nacho_for(&r);
        let out = h.go(&r, &nacho, &fleet, None, &RunPolicy::default());
        assert_eq!(outcome_words(&out), ["mac-a:updated", "ctl:handed_off"]);
        assert_eq!(*nacho.consumes.borrow(), 1);
        let g = h.saved.borrow().clone().expect("소비 기록");
        assert_eq!((g.rollout.as_str(), g.approval_id.as_str(), g.scope_hash.as_str()), (RID, AP, r.approval_scope_hash.as_str()));
        assert!(matches!(&out[0].1, Outcome::Updated { new_pid: 1011, build, .. } if build == "5e4d138"));
        assert!(fleet.sends("ctl")[0] > fleet.sends("mac-a")[0], "맥북이 새 판으로 끝난 뒤에 조종 기기");
    }

    #[test]
    fn nothing_is_consumed_unless_every_target_is_ready_now() {
        let h = Harness::new();
        let (r, mut devs) = two(Mode::Normal);
        devs[1].0.update_enabled = false;
        let fleet = Fleet::new(devs, h.clock.clone());
        let nacho = nacho_for(&r);
        let out = h.go(&r, &nacho, &fleet, None, &RunPolicy::default());
        assert_eq!(outcome_words(&out), ["mac-a:skipped", "ctl:failed"]);
        assert_eq!(*nacho.consumes.borrow(), 0);
        assert!(!fleet.started("mac-a"));
        // 계획 뒤 pid 가 바뀐 기기도 같다.
        let (r, mut devs) = two(Mode::Normal);
        devs[0].0.pid = 99;
        let out = h.go(&r, &nacho_for(&r), &Fleet::new(devs, h.clock.clone()), None, &RunPolicy::default());
        assert!(matches!(&out[0].1, Outcome::Failed { reason, .. } if reason.contains("바뀌었다")));
        // 바쁜 학생은 소비를 막지 않는다 — 적용만 기다린다.
        let (r, mut devs) = two(Mode::Normal);
        devs[0].0.busy = vec![app_restart::BusyPane { surface: "%2".into(), character: "모모이".into(), state: "working".into() }];
        let nacho = nacho_for(&r);
        h.go(&r, &nacho, &Fleet::new(devs, h.clock.clone()), None, &RunPolicy::default());
        assert_eq!(*nacho.consumes.borrow(), 1);
    }

    #[test]
    fn a_busy_target_is_resent_on_a_steady_beat_until_it_applies() {
        let h = Harness::new();
        let (r, devs) = two(Mode::Busy(2));
        let fleet = Fleet::new(devs, h.clock.clone());
        let policy = RunPolicy::default();
        let out = h.go(&r, &nacho_for(&r), &fleet, None, &policy);
        assert_eq!(outcome_words(&out), ["mac-a:updated", "ctl:handed_off"]);
        let sends = fleet.sends("mac-a");
        assert_eq!(sends.len(), 3, "처음 한 번 + 기다리는 동안 두 번");
        assert!(sends.windows(2).all(|w| w[1] - w[0] >= policy.resend_every_ms), "{sends:?}");
    }

    #[test]
    fn a_failed_or_refused_or_stuck_target_stops_everything_after_it() {
        for (mode, word) in [(Mode::FailAt, "sha256"), (Mode::RefuseResend, "revoked"), (Mode::NeverArm, "30분"), (Mode::WrongBuild, "빌드")] {
            let h = Harness::new();
            let (r, devs) = two(mode);
            let fleet = Fleet::new(devs, h.clock.clone());
            let out = h.go(&r, &nacho_for(&r), &fleet, None, &RunPolicy::default());
            assert!(matches!(&out[0].1, Outcome::Failed { reason, .. } if reason.contains(word)), "{word}: {:?}", out[0].1);
            assert!(matches!(&out[1].1, Outcome::Skipped { .. }), "{word}");
            assert!(!fleet.started("ctl"), "{word}: 뒤 기기는 건드리지 않는다");
        }
    }

    #[test]
    fn losing_the_target_while_it_restarts_is_normal_up_to_a_limit() {
        let h = Harness::new();
        let (r, devs) = two(Mode::Gap(3));
        let out = h.go(&r, &nacho_for(&r), &Fleet::new(devs, h.clock.clone()), None, &RunPolicy::default());
        assert_eq!(outcome_words(&out), ["mac-a:updated", "ctl:handed_off"]);
        let (r, devs) = two(Mode::Gap(10));
        let tight = RunPolicy { max_status_errors: 2, ..RunPolicy::default() };
        let out = h.go(&r, &nacho_for(&r), &Fleet::new(devs, h.clock.clone()), None, &tight);
        assert!(matches!(&out[0].1, Outcome::Failed { reason, .. } if reason.contains("재기동 뒤")), "{:?}", out[0].1);
    }

    #[test]
    fn a_rerun_reads_the_grant_instead_of_consuming_and_skips_finished_targets() {
        let h = Harness::new();
        let (r, devs) = two(Mode::Normal);
        let fleet = Fleet::new(devs, h.clock.clone());
        let nacho = nacho_for(&r);
        h.go(&r, &nacho, &fleet, None, &RunPolicy::default());
        let grant = h.saved.borrow().clone().unwrap();
        // 조종 기기에서 새 러너가 다시 뜬 상황 — 맥북은 이미 새 판, 조종 기기는 다시 이어 간다.
        fleet.devs.borrow_mut().get_mut("ctl").unwrap().job = None;
        let out = h.go(&r, &nacho, &fleet, Some(&grant), &RunPolicy::default());
        assert_eq!(outcome_words(&out), ["mac-a:updated", "ctl:handed_off"]);
        assert_eq!(*nacho.consumes.borrow(), 1, "두 번 소비하지 않는다");
        assert_eq!(fleet.sends("mac-a").len(), 1, "끝난 기기엔 다시 안 보낸다");
        let other = Grant { approval_id: format!("ap_{}", "1".repeat(32)), ..grant.clone() };
        let out = h.go(&r, &nacho, &fleet, Some(&other), &RunPolicy::default());
        assert!(out.iter().all(|(_, o)| matches!(o, Outcome::Skipped { reason } if reason.contains("다른 승인"))));
        nacho.view.borrow_mut().state = "revoked".into();
        let out = h.go(&r, &nacho, &fleet, Some(&grant), &RunPolicy::default());
        assert!(out.iter().all(|(_, o)| matches!(o, Outcome::Skipped { reason } if reason.contains("revoked"))), "{out:?}");
    }

    #[test]
    fn an_expired_approval_stops_before_the_next_target() {
        let h = Harness::new();
        let (r, devs) = two(Mode::Normal);
        let fleet = Fleet::new(devs, h.clock.clone());
        let nacho = nacho_for(&r);
        // 첫 기기가 끝났을 때 이미 만료 — 조종 기기엔 보내지 않는다.
        nacho.ttl.set(5_000);
        let out = h.go(&r, &nacho, &fleet, None, &RunPolicy::default());
        assert!(matches!(&out[1].1, Outcome::Failed { reason, .. } if reason.contains("만료")), "{out:?}");
        assert!(!fleet.started("ctl"));
    }

    /// 나쵸 고정 자료(approval.kasaterm_update.implemented.json)를 이 러스트 계약으로 읽는다 — rollout 모양·범위 재구성·해시,
    /// 나쵸가 적은 소비 뒤 만료(대상 × 2100초), 거둔 승인. `scripts/nacho-update-interop.sh` 가 `NACHO_DESK_FIXTURES` 로 부른다.
    #[test]
    #[ignore]
    fn nacho_update_fixture_matches_rust() {
        let dir = std::env::var("NACHO_DESK_FIXTURES").expect("NACHO_DESK_FIXTURES");
        let fx: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/approval.kasaterm_update.implemented.json")).unwrap()).unwrap();
        let rollout: Rollout = serde_json::from_value(fx["plan"].clone()).expect("rollout 모양");
        assert_eq!(check_rollout(&rollout), Ok(()));
        assert_eq!(fx["window"]["runner_max_per_target_sec"].as_u64(), Some((RunPolicy::default().wait_cap_ms + RunPolicy::default().boot_cap_ms) / 1000));
        let consumed: ApprovalView = serde_json::from_value(fx["views"]["consumed"].clone()).unwrap();
        assert_eq!(consumed.scope_hash, rollout.approval_scope_hash, "나쵸 정규화 해시 = 러스트");
        assert_eq!(consumed.scope, rollout.approval_scope);
        let per = fx["window"]["update_target_sec"].as_u64().unwrap() * 1000;
        assert_eq!(consumed.expires_at_ms, consumed.consumed_at_ms.unwrap() + per * rollout.jobs.len() as u64);
        assert!(per >= RunPolicy::default().wait_cap_ms + RunPolicy::default().boot_cap_ms, "대상 하나의 러너 최대 시간이 나쵸 창 안");
        let at = consumed.consumed_at_ms.unwrap();
        assert!(usable_approval(&Nacho(consumed.clone()), &consumed.id, at + 1).is_ok());
        let revoked: ApprovalView = serde_json::from_value(fx["views"]["revoked"].clone()).unwrap();
        assert!(usable_approval(&Nacho(revoked), &consumed.id, at + 1).unwrap_err().contains("revoked"));
        let unconsumed: ApprovalView = serde_json::from_value(fx["views"]["approved_unconsumed"].clone()).unwrap();
        assert!(usable_approval(&Nacho(unconsumed), &consumed.id, at + 1).unwrap_err().contains("소비"));
    }
}
