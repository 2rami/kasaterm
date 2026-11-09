//! 공유방 포크 — 다른 사람이 게시한 대화를 수신자 계정의 독립 native 세션으로 잇는다
//! (`docs/shared-rooms.md` 「포크 자료」).
//!
//! 게시 기기는 bound transcript 의 완료 경계까지를 [`export_native_snapshot`] 으로 고정한다. 수신 기기는
//! [`stage_snapshot`] 으로 받은 자료를 다시 검증해 비공개 staging 에 두고, [`claude_fork_plan`] 의 인자로
//! 공식 `--fork-session` 을 띄운 뒤 [`confirm_claude_child`] 로 새 ID 가 생겼는지 본다. 원본 파일은 읽기만
//! 하고, 부모 ID 를 수신자 projects 에 설치하지 않는다.
//!
//! Codex 는 같은 흐름을 app-server `thread/fork` 의 실험 `path` 로 한다(`codex` 모듈 머리말).

mod claude;
mod codex;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod codex_tests;

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use kasa_socket::sessions::{claude_project_slug, is_uuid};
use kasa_socket::shared_rooms::NativeForkSnapshot;
use kasa_socket::shell_line::{launch_line, ShellKind};
use sha2::{Digest, Sha256};

pub use claude::{export_claude_snapshot, verify_claude_snapshot};
pub use codex::{
    confirm_codex_child, codex_fork_plan, export_codex_snapshot, reset_codex_launch, run_codex_fork, shadow_thread_id,
    stage_codex_snapshot, verify_codex_snapshot, CodexForkLaunch, CodexForkPlan, StagedCodexFork,
};

#[derive(Debug)]
pub enum ForkError {
    /// 하네스·형식·판이 native 포크를 지원하지 않는다. 포크 가능으로 표시하지 않는다.
    Unsupported(String),
    /// 경계가 아직 완료되지 않았다(진행 중 도구·쓰다 만 행). 다음 완료 뒤 다시 할 수 있다.
    Incomplete(String),
    /// 자료·경로가 규칙을 어긴다. 같은 입력으로 다시 해도 실패한다.
    Invalid(String),
    /// 같은 자리에 다른 내용·다른 세션이 있다. 덮지 않고 멈춘다.
    Conflict(String),
    Io(io::Error),
}

impl fmt::Display for ForkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(why) => write!(f, "포크를 지원하지 않아요: {why}"),
            Self::Incomplete(why) => write!(f, "아직 포크할 수 없는 지점이에요: {why}"),
            Self::Invalid(why) => write!(f, "포크 자료가 올바르지 않아요: {why}"),
            Self::Conflict(why) => write!(f, "포크 자리가 이미 다른 것으로 차 있어요: {why}"),
            Self::Io(error) => write!(f, "포크 파일을 다루지 못했어요: {error}"),
        }
    }
}

impl std::error::Error for ForkError {}

impl From<io::Error> for ForkError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExportLimits {
    /// 원본 transcript 를 읽을 최대 크기. 두 번 훑지만 행 본문은 사슬에 든 것만 메모리에 올린다.
    pub max_source_bytes: u64,
    /// 고정 자료(jsonl) 한도. 관문 업로드와 수신 검증이 같은 값을 쓴다.
    pub max_snapshot_bytes: usize,
}

impl Default for ExportLimits {
    fn default() -> Self {
        Self { max_source_bytes: 512 * 1024 * 1024, max_snapshot_bytes: 16 * 1024 * 1024 }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ForkBoundary<'a> {
    /// 읽은 시점에 완성된 행 가운데 가장 최근의 완료된 턴 끝.
    LatestComplete,
    /// 고른 assistant 행 uuid. 완료된 턴의 마지막 행이어야 한다.
    Turn(&'a str),
}

/// 고정할 때 읽은 원본 범위. 원본은 뒤에 이어 써도 되지만 이 앞부분은 바뀌면 안 된다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRange {
    /// 완성된 행(마지막 개행까지)의 바이트 수.
    pub prefix_len: u64,
    pub prefix_sha256: String,
    /// 열었을 때의 파일 크기. `prefix_len` 과 다르면 쓰다 만 행이 뒤에 있었다.
    pub len_at_read: u64,
}

#[derive(Clone)]
pub struct ExportedFork {
    pub snapshot: NativeForkSnapshot,
    /// 공유 자료에서 뺀 것의 종류와 개수. 체크포인트 `omissions` 로 보여 준다.
    pub omissions: Vec<String>,
    /// 경계 행의 기록 시각. 프로젝트 캡처가 이보다 나중 파일을 경계 시점 것처럼 보이지 않게 쓴다.
    pub boundary_at_ms: Option<u64>,
    /// 원본에서 경계 행이 끝나는 바이트 자리. 체크포인트의 표시용 기록을 여기까지로 자른다.
    pub boundary_end: u64,
    pub source: SourceRange,
}

pub fn export_native_snapshot(
    harness: &str,
    transcript: &Path,
    parent_session_id: &str,
    boundary: ForkBoundary<'_>,
    limits: &ExportLimits,
) -> Result<ExportedFork, ForkError> {
    match harness {
        "claude" => export_claude_snapshot(transcript, parent_session_id, boundary, limits),
        "codex" => export_codex_snapshot(transcript, parent_session_id, boundary, limits),
        other => Err(ForkError::Unsupported(format!("{other} 하네스에는 native 포크 형식이 없어요"))),
    }
}

/// 공유방 화면에 올릴 원본 기록 조각을 줄마다 거른다. 고정 자료와 같은 허용 목록으로 숨긴 추론·주입 문맥·
/// 이미지·권한·경로를 빼고, 공유 형식에 없는 줄은 통째로 뺀다. `raw` 는 완성된 줄들이다.
pub fn display_lines(harness: &str, raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let clean = match harness {
            "claude" => claude::display_row(&row),
            "codex" => codex::display_row(&row),
            _ => None,
        };
        if let Some(clean) = clean.and_then(|row| serde_json::to_string(&row).ok()) {
            out.push_str(&clean);
            out.push('\n');
        }
    }
    out
}

/// 받은 자료를 하네스에 맞게 다시 잰다. 모르는 하네스는 받지 않는다.
pub fn verify_snapshot(snapshot: &NativeForkSnapshot, limits: &ExportLimits) -> Result<(), ForkError> {
    match snapshot.harness.as_str() {
        "claude" => verify_claude_snapshot(snapshot, limits),
        "codex" => verify_codex_snapshot(snapshot, limits),
        other => Err(ForkError::Unsupported(format!("{other} 하네스 자료는 받지 않아요"))),
    }
}

/// 공유 자료에서 뺀 것의 종류. 포크 화면이 문구를 그대로 보여 주므로 종류마다 고정 이름을 쓰고,
/// 원래 형식 이름은 괄호에 붙인다. 순서는 화면에 나올 순서다.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Omit {
    Reasoning,
    Image,
    Attachment,
    Injected,
    Account,
    Settings,
    SourcePath,
    Compaction,
    Request,
    Display,
    Unreadable,
    Other,
}

impl Omit {
    fn label(self) -> &'static str {
        match self {
            Self::Reasoning => "숨긴 추론",
            Self::Image => "이미지",
            Self::Attachment => "첨부 문서·파일 내용",
            Self::Injected => "주입된 지침·문맥",
            Self::Account => "계정·자격 정보",
            Self::Settings => "원본 권한·모델 설정",
            Self::SourcePath => "원본 작업 경로·저장소 정보",
            Self::Compaction => "압축 요약",
            Self::Request => "요청 원문·사용량 기록",
            Self::Display => "화면용 기록",
            Self::Unreadable => "읽을 수 없는 행",
            Self::Other => "기타 기록",
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Omissions(std::collections::BTreeMap<(Omit, String), usize>);

impl Omissions {
    pub(crate) fn add(&mut self, kind: Omit, detail: &str, n: usize) {
        if n > 0 {
            *self.0.entry((kind, detail.to_string())).or_default() += n;
        }
    }

    pub(crate) fn into_vec(self) -> Vec<String> {
        self.0
            .into_iter()
            .map(|((kind, detail), n)| match detail.is_empty() {
                true => format!("{} {n}개", kind.label()),
                false => format!("{}({detail}) {n}개", kind.label()),
            })
            .collect()
    }
}

/// 포크 요청 하나의 신원. 같은 수신자의 재시도는 같은 값을 다시 보낸다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkIdentity {
    pub room_id: String,
    pub session_id: String,
    pub checkpoint_id: String,
    pub request_id: String,
}

impl ForkIdentity {
    /// staging 폴더 이름이자 child ID 의 씨앗. 재시도가 같은 child 로 모이고, 요청·자료가 다르면 갈린다.
    pub fn fork_key(&self, snapshot_sha256: &str) -> Result<String, ForkError> {
        for (name, value) in [
            ("room_id", &self.room_id),
            ("session_id", &self.session_id),
            ("checkpoint_id", &self.checkpoint_id),
            ("request_id", &self.request_id),
        ] {
            if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                return Err(ForkError::Invalid(format!("{name} 이 비었거나 형식이 달라요")));
            }
        }
        let mut hasher = Sha256::new();
        for part in ["kasaterm-shared-fork-v1", &self.room_id, &self.session_id, &self.checkpoint_id, &self.request_id, snapshot_sha256] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        Ok(hex(&hasher.finalize())[..32].to_string())
    }
}

/// fork_key 에서 정해지는 child 세션 ID(UUID v4 꼴). claude `--session-id` 가 UUID 만 받는다.
pub fn child_session_id(fork_key: &str) -> String {
    uuid_v4_from(&Sha256::digest(format!("kasaterm-shared-fork-child-v1\0{fork_key}").as_bytes()))
}

fn uuid_v4_from(digest: &[u8]) -> String {
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn check_child_id(parent: &str, child: &str) -> Result<(), ForkError> {
    if !is_uuid(child) {
        return Err(ForkError::Invalid("child ID 가 UUID 가 아니에요".into()));
    }
    if child.eq_ignore_ascii_case(parent) {
        return Err(ForkError::Conflict("child ID 가 원본 ID 와 같아요".into()));
    }
    Ok(())
}

/// 수신 기기 staging 에 놓인 포크 자료. 이 파일을 `--resume` 경로로 넘겨 새 ID 로 갈라낸다.
#[derive(Clone, Debug)]
pub struct StagedFork {
    pub dir: PathBuf,
    pub transcript: PathBuf,
    pub fork_key: String,
    pub identity: ForkIdentity,
    pub parent_session_id: String,
    pub boundary_id: String,
    pub snapshot_sha256: String,
    /// cwd 를 수신자 작업 폴더로 넣은 뒤의 파일 해시. 실행 직전·확인 때 다시 잰다.
    pub staged_sha256: String,
    pub child_session_id: String,
    pub workdir: PathBuf,
}

/// 받은 고정 자료를 다시 검증해 `staging_root/<fork_key>/<parent>.jsonl` 에 둔다.
///
/// `claude_projects_root` 는 수신자 계정의 `projects` 폴더다. staging 이 그 안이면 부모 ID 가 수신자
/// 세션 목록에 설치되는 것과 같아 거부한다. 같은 요청의 재시도는 같은 파일을 그대로 돌려준다.
pub fn stage_snapshot(
    staging_root: &Path,
    claude_projects_root: &Path,
    identity: &ForkIdentity,
    snapshot: &NativeForkSnapshot,
    workdir: &Path,
) -> Result<StagedFork, ForkError> {
    verify_claude_snapshot(snapshot, &ExportLimits::default())?;
    if !staging_root.is_absolute() || !workdir.is_absolute() {
        return Err(ForkError::Invalid("staging·작업 폴더는 절대 경로여야 해요".into()));
    }
    if !std::fs::metadata(workdir).map(|m| m.is_dir()).unwrap_or(false) {
        return Err(ForkError::Invalid(format!("작업 폴더가 없어요: {}", workdir.display())));
    }
    let workdir_text = workdir
        .to_str()
        .ok_or_else(|| ForkError::Invalid("작업 폴더 경로가 UTF-8 이 아니에요".into()))?;
    if is_within(staging_root, claude_projects_root) {
        return Err(ForkError::Invalid("staging 이 수신자 claude projects 안에 있어요".into()));
    }
    let fork_key = identity.fork_key(&snapshot.sha256)?;
    let child = child_session_id(&fork_key);
    check_child_id(&snapshot.parent_session_id, &child)?;
    let staged = claude::with_cwd(&snapshot.jsonl, workdir_text)?;
    let staged_sha256 = sha256_hex(staged.as_bytes());

    ensure_private_dir(staging_root)?;
    let dir = staging_root.join(&fork_key);
    let transcript = dir.join(format!("{}.jsonl", snapshot.parent_session_id));
    let fork = StagedFork {
        dir: dir.clone(),
        transcript: transcript.clone(),
        fork_key,
        identity: identity.clone(),
        parent_session_id: snapshot.parent_session_id.clone(),
        boundary_id: snapshot.boundary_id.clone(),
        snapshot_sha256: snapshot.sha256.clone(),
        staged_sha256,
        child_session_id: child,
        workdir: workdir.to_path_buf(),
    };
    if dir.exists() {
        if std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
            return Err(ForkError::Conflict(format!("staging 폴더가 링크예요: {}", dir.display())));
        }
        if transcript.exists() {
            return match sha256_file(&transcript)? {
                sha if sha == fork.staged_sha256 => match load_provenance(&dir)? {
                    Some(prov) if !prov.matches(&fork) => {
                        Err(ForkError::Conflict("staging 의 기록이 이 요청과 달라요".into()))
                    }
                    Some(_) => Ok(fork),
                    None => {
                        record_provenance(&dir, &ForkProvenance::staged(&fork))?;
                        Ok(fork)
                    }
                },
                _ => Err(ForkError::Conflict(format!("같은 요청의 staging 내용이 달라요: {}", transcript.display()))),
            };
        }
    } else {
        create_private_dir(&dir)?;
    }
    write_new_atomic(&transcript, staged.as_bytes())?;
    record_provenance(&dir, &ForkProvenance::staged(&fork))?;
    Ok(fork)
}

/// claude 공식 포크 실행 계획. 권한·모델 플래그는 넣지 않는다 — 수신자 설정을 그대로 쓴다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeForkPlan {
    pub cwd: PathBuf,
    /// 프로그램 이름 뒤에 붙일 인자: `--resume <staged> --fork-session --session-id <child>`.
    pub args: Vec<String>,
    pub parent_session_id: String,
    pub child_session_id: String,
    pub staged_transcript: PathBuf,
    pub staged_sha256: String,
    /// claude 가 새 세션을 쓸 자리. 아주 긴 cwd 는 claude 가 이름을 줄여 다른 폴더가 될 수 있어
    /// [`confirm_claude_child`] 는 projects 전체에서도 찾는다.
    pub expected_child_transcript: PathBuf,
    pub fork_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub enum ClaudeForkLaunch {
    Launch(ClaudeForkPlan),
    /// 같은 요청이 이미 child 를 만들었다. 다시 띄우지 않는다.
    AlreadyForked(ForkProvenance),
}

pub fn claude_fork_plan(staged: &StagedFork, claude_projects_root: &Path) -> Result<ClaudeForkLaunch, ForkError> {
    check_child_id(&staged.parent_session_id, &staged.child_session_id)?;
    if sha256_file(&staged.transcript)? != staged.staged_sha256 {
        return Err(ForkError::Conflict("staging 파일이 고정 뒤에 바뀌었어요".into()));
    }
    if !std::fs::metadata(&staged.workdir).map(|m| m.is_dir()).unwrap_or(false) {
        return Err(ForkError::Invalid(format!("작업 폴더가 없어요: {}", staged.workdir.display())));
    }
    let staged_path = staged
        .transcript
        .to_str()
        .ok_or_else(|| ForkError::Invalid("staging 경로가 UTF-8 이 아니에요".into()))?;
    let expected = claude_projects_root
        .join(claude_project_slug(&staged.workdir))
        .join(format!("{}.jsonl", staged.child_session_id));
    if let Some(found) = find_session_file(claude_projects_root, &expected, &staged.child_session_id) {
        let prov = load_provenance(&staged.dir)?;
        return match (has_session_row(&found, &staged.child_session_id), prov) {
            (true, Some(mut prov)) if prov.matches(staged) => {
                if prov.child_transcript.is_none() {
                    prov.child_transcript = Some(found.display().to_string());
                    prov.confirmed_at_ms = Some(now_ms());
                    record_provenance(&staged.dir, &prov)?;
                }
                Ok(ClaudeForkLaunch::AlreadyForked(prov))
            }
            _ => Err(ForkError::Conflict(format!("child ID 를 다른 세션이 쓰고 있어요: {}", found.display()))),
        };
    }
    Ok(ClaudeForkLaunch::Launch(ClaudeForkPlan {
        cwd: staged.workdir.clone(),
        args: vec![
            "--resume".into(),
            staged_path.into(),
            "--fork-session".into(),
            "--session-id".into(),
            staged.child_session_id.clone(),
        ],
        parent_session_id: staged.parent_session_id.clone(),
        child_session_id: staged.child_session_id.clone(),
        staged_transcript: staged.transcript.clone(),
        staged_sha256: staged.staged_sha256.clone(),
        expected_child_transcript: expected,
        fork_dir: staged.dir.clone(),
    }))
}

impl ClaudeForkPlan {
    /// POSIX 셸 꼴 한 줄(cd 없이). `program` 은 부르는 쪽이 고른 claude 실행 이름·경로다.
    pub fn posix_command(&self, program: &str) -> String {
        std::iter::once(program).chain(self.args.iter().map(String::as_str)).map(posix_word).collect::<Vec<_>>().join(" ")
    }

    /// 칸 셸에 칠 한 줄(Enter 포함). cmd·PowerShell 문법 변환과 거부는 `shell_line::launch_line` 이 맡는다.
    pub fn shell_line(&self, shell: ShellKind, program: &str) -> Result<String, ForkError> {
        let cwd = self.cwd.to_str().ok_or_else(|| ForkError::Invalid("작업 폴더 경로가 UTF-8 이 아니에요".into()))?;
        launch_line(shell, Some(cwd), &self.posix_command(program), None, None).map_err(|e| ForkError::Invalid(e.to_string()))
    }
}

fn posix_word(word: &str) -> String {
    let bare = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '+' | '@'));
    if bare {
        word.to_string()
    } else {
        ShellKind::Posix.quote(word)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildState {
    /// 아직 child 기록이 없다. claude 는 첫 기록을 쓸 때 파일을 만든다.
    Pending,
    Confirmed { session_id: String, transcript: PathBuf },
    Mismatch(String),
}

/// 띄운 뒤 child 가 정말 새 ID 로 생겼는지 본다. 확인되면 provenance 에 child 경로를 적는다.
pub fn confirm_claude_child(plan: &ClaudeForkPlan, claude_projects_root: &Path) -> ChildState {
    match sha256_file(&plan.staged_transcript) {
        Ok(sha) if sha == plan.staged_sha256 => {}
        Ok(_) => return ChildState::Mismatch("staging 파일이 실행 중에 바뀌었어요".into()),
        Err(error) => return ChildState::Mismatch(format!("staging 파일을 읽지 못했어요: {error}")),
    }
    let Some(found) = find_session_file(claude_projects_root, &plan.expected_child_transcript, &plan.child_session_id) else {
        return ChildState::Pending;
    };
    if !has_session_row(&found, &plan.child_session_id) {
        return ChildState::Mismatch(format!("child 기록에 새 ID 행이 없어요: {}", found.display()));
    }
    if let Ok(Some(mut prov)) = load_provenance(&plan.fork_dir) {
        if prov.child_session_id.as_deref() == Some(plan.child_session_id.as_str()) && prov.child_transcript.is_none() {
            prov.child_transcript = Some(found.display().to_string());
            prov.confirmed_at_ms = Some(now_ms());
            if let Err(error) = record_provenance(&plan.fork_dir, &prov) {
                return ChildState::Mismatch(format!("provenance 를 적지 못했어요: {error}"));
            }
        }
    }
    ChildState::Confirmed { session_id: plan.child_session_id.clone(), transcript: found }
}

fn find_session_file(root: &Path, expected: &Path, id: &str) -> Option<PathBuf> {
    if expected.is_file() {
        return Some(expected.to_path_buf());
    }
    let name = format!("{id}.jsonl");
    std::fs::read_dir(root).ok()?.flatten().map(|dir| dir.path().join(&name)).find(|path| path.is_file())
}

/// 머리 쪽 행 가운데 sessionId 가 `id` 인 것이 있나. claude 가 복사한 부모 행의 ID 표기와 상관없이 본다.
fn has_session_row(path: &Path, id: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(1024 * 1024).read_to_end(&mut head).is_err() {
        return false;
    }
    head.split(|&b| b == b'\n').filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok()).any(|row| row["sessionId"].as_str() == Some(id))
}

/// 원본 방·대화·체크포인트와 수신자 child 의 관계. staging 폴더의 `provenance.json` 에 둔다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkProvenance {
    pub identity: ForkIdentity,
    pub harness: String,
    pub parent_session_id: String,
    pub boundary_id: String,
    pub snapshot_sha256: String,
    pub fork_key: String,
    /// claude 는 staging 때 정해지고, codex 는 app-server 가 만든 뒤 확인해서 채운다.
    pub child_session_id: Option<String>,
    /// codex 부모를 재키잉한 수신자 소유 ID. child 의 forked_from_id 가 이것이다.
    pub shadow_session_id: Option<String>,
    pub workdir: String,
    pub staged_transcript: String,
    pub created_at_ms: u64,
    /// codex 포크 요청을 보낸 시각. 결과를 확인하기 전에는 다시 띄우지 않는다.
    pub launched_at_ms: Option<u64>,
    pub confirmed_at_ms: Option<u64>,
    pub child_transcript: Option<String>,
}

const PROVENANCE_FILE: &str = "provenance.json";

impl ForkProvenance {
    fn base(identity: &ForkIdentity, parent: &str, boundary: &str, snapshot_sha256: &str, fork_key: &str, workdir: &Path, staged: &Path) -> Self {
        Self {
            identity: identity.clone(),
            harness: String::new(),
            parent_session_id: parent.into(),
            boundary_id: boundary.into(),
            snapshot_sha256: snapshot_sha256.into(),
            fork_key: fork_key.into(),
            child_session_id: None,
            shadow_session_id: None,
            workdir: workdir.display().to_string(),
            staged_transcript: staged.display().to_string(),
            created_at_ms: now_ms(),
            launched_at_ms: None,
            confirmed_at_ms: None,
            child_transcript: None,
        }
    }

    fn staged(fork: &StagedFork) -> Self {
        Self {
            harness: "claude".into(),
            child_session_id: Some(fork.child_session_id.clone()),
            ..Self::base(&fork.identity, &fork.parent_session_id, &fork.boundary_id, &fork.snapshot_sha256, &fork.fork_key, &fork.workdir, &fork.transcript)
        }
    }

    fn same_request(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.harness == other.harness
            && self.parent_session_id == other.parent_session_id
            && self.snapshot_sha256 == other.snapshot_sha256
            && self.fork_key == other.fork_key
            && self.shadow_session_id == other.shadow_session_id
    }

    fn matches(&self, fork: &StagedFork) -> bool {
        self.same_request(&Self::staged(fork)) && self.child_session_id.as_deref() == Some(fork.child_session_id.as_str())
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "format_version": 1,
            "room_id": self.identity.room_id,
            "session_id": self.identity.session_id,
            "checkpoint_id": self.identity.checkpoint_id,
            "request_id": self.identity.request_id,
            "harness": self.harness,
            "parent_session_id": self.parent_session_id,
            "boundary_id": self.boundary_id,
            "snapshot_sha256": self.snapshot_sha256,
            "fork_key": self.fork_key,
            "child_session_id": self.child_session_id,
            "shadow_session_id": self.shadow_session_id,
            "workdir": self.workdir,
            "staged_transcript": self.staged_transcript,
            "created_at_ms": self.created_at_ms,
            "launched_at_ms": self.launched_at_ms,
            "confirmed_at_ms": self.confirmed_at_ms,
            "child_transcript": self.child_transcript,
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        if v["format_version"].as_u64() != Some(1) {
            return None;
        }
        let text = |key: &str| v[key].as_str().map(str::to_string);
        Some(Self {
            identity: ForkIdentity {
                room_id: text("room_id")?,
                session_id: text("session_id")?,
                checkpoint_id: text("checkpoint_id")?,
                request_id: text("request_id")?,
            },
            harness: text("harness")?,
            parent_session_id: text("parent_session_id")?,
            boundary_id: text("boundary_id")?,
            snapshot_sha256: text("snapshot_sha256")?,
            fork_key: text("fork_key")?,
            child_session_id: text("child_session_id"),
            shadow_session_id: text("shadow_session_id"),
            workdir: text("workdir")?,
            staged_transcript: text("staged_transcript")?,
            created_at_ms: v["created_at_ms"].as_u64()?,
            launched_at_ms: v["launched_at_ms"].as_u64(),
            confirmed_at_ms: v["confirmed_at_ms"].as_u64(),
            child_transcript: text("child_transcript"),
        })
    }
}

pub fn record_provenance(fork_dir: &Path, provenance: &ForkProvenance) -> Result<(), ForkError> {
    let body = serde_json::to_vec_pretty(&provenance.to_json()).map_err(|e| ForkError::Invalid(e.to_string()))?;
    let path = fork_dir.join(PROVENANCE_FILE);
    let part = fork_dir.join(format!("{PROVENANCE_FILE}.part"));
    let _ = std::fs::remove_file(&part);
    let mut file = create_private_file(&part)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&part, &path)?;
    sync_dir(fork_dir);
    Ok(())
}

pub fn load_provenance(fork_dir: &Path) -> Result<Option<ForkProvenance>, ForkError> {
    let path = fork_dir.join(PROVENANCE_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| ForkError::Invalid(format!("provenance: {e}")))?;
    ForkProvenance::from_json(&value).map(Some).ok_or_else(|| ForkError::Invalid("provenance 형식이 달라요".into()))
}

/// 설치된 codex 가 낸 app-server JSON Schema(`codex app-server generate-json-schema --out DIR`)로 본
/// `thread/fork` 의 부모 지정 방식.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexForkSupport {
    /// threadId 로만 부모를 찾는다. 부모가 수신자 CODEX_HOME 에 있어야 한다.
    ThreadIdOnly,
    /// rollout 경로를 받는 `path` 가 있다(0.167 은 실험 API 로만 연다). 있으면 threadId 를 보지 않는다.
    PathParam,
    /// 스키마가 없거나 읽히지 않거나 `path` 의 꼴이 분명하지 않다.
    Unknown(String),
}

pub fn codex_fork_support(schema_dir: &Path) -> CodexForkSupport {
    let path = [schema_dir.join("v2/ThreadForkParams.json"), schema_dir.join("ThreadForkParams.json")].into_iter().find(|p| p.is_file());
    let Some(path) = path else {
        return CodexForkSupport::Unknown("ThreadForkParams 스키마가 없어요".into());
    };
    let schema = match std::fs::read(&path).map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes)) {
        Ok(Ok(schema)) => schema,
        Ok(Err(error)) => return CodexForkSupport::Unknown(format!("스키마를 읽지 못했어요: {error}")),
        Err(error) => return CodexForkSupport::Unknown(format!("스키마를 열지 못했어요: {error}")),
    };
    let props = &schema["properties"];
    if !props.is_object() || props["threadId"]["type"].as_str() != Some("string") {
        return CodexForkSupport::Unknown("threadId 꼴이 분명하지 않아요".into());
    }
    let path_prop = &props["path"];
    if path_prop.is_null() {
        return CodexForkSupport::ThreadIdOnly;
    }
    let ty = &path_prop["type"];
    let string_only = ty.as_str() == Some("string")
        || ty.as_array().is_some_and(|types| {
            types.iter().any(|t| t.as_str() == Some("string")) && types.iter().all(|t| matches!(t.as_str(), Some("string" | "null")))
        });
    if string_only {
        CodexForkSupport::PathParam
    } else {
        CodexForkSupport::Unknown("path 인자의 꼴이 분명하지 않아요".into())
    }
}

pub fn codex_unsupported(support: &CodexForkSupport) -> ForkError {
    ForkError::Unsupported(match support {
        CodexForkSupport::ThreadIdOnly => {
            "이 Codex 의 thread/fork 는 로컬 threadId 로만 부모를 찾아요(실험 스키마에도 path 가 없어요). 같은 ID 를 수신자 Codex 홈에 설치하지 않고는 포크할 수 없어요".into()
        }
        CodexForkSupport::PathParam => "경로 포크를 쓸 수 있어요".into(),
        CodexForkSupport::Unknown(why) => format!("Codex thread/fork 지원을 확인하지 못했어요: {why}"),
    })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String, ForkError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn is_within(path: &Path, root: &Path) -> bool {
    let canon = |p: &Path| {
        // 아직 없는 staging 은 있는 조상까지 풀어 붙인다. 링크로 projects 안을 가리켜도 잡힌다.
        let mut tail = Vec::new();
        let mut cur = p.to_path_buf();
        loop {
            if let Ok(real) = cur.canonicalize() {
                return tail.iter().rev().fold(real, |acc: PathBuf, part| acc.join(part));
            }
            match (cur.file_name().map(|n| n.to_os_string()), cur.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    cur = parent.to_path_buf();
                }
                _ => return p.to_path_buf(),
            }
        }
    };
    canon(path).starts_with(canon(root))
}

#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(windows)]
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    kasa_socket::private_store::ensure_dir(dir).map(drop)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(windows)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    kasa_socket::private_store::create_dir(dir).map(drop)
}

/// `create_new`(O_EXCL)는 그 이름이 링크여도 따라가지 않고 실패한다.
#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(windows)]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    kasa_socket::private_store::create_new(path)
}

/// `.part` 에 쓰고 fsync 뒤 rename — 쓰다 만 파일이 정본 이름으로 남지 않는다. 이미 있으면 덮지 않는다.
fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<(), ForkError> {
    if path.exists() {
        return Err(ForkError::Conflict(format!("이미 있는 파일이에요: {}", path.display())));
    }
    let part = path.with_extension("jsonl.part");
    let _ = std::fs::remove_file(&part);
    let mut file = create_private_file(&part)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&part, path)?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}
