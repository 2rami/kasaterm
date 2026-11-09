//! 공유방 포크의 프로젝트 자료 — 대화와 별도로 게시 기기의 기준 commit·patch·고른 파일을 묶고,
//! 수신 기기에서는 새 폴더나 별도 worktree 에만 푼다(`docs/shared-rooms.md` 「포크 자료」).
//!
//! 받은 묶음은 다른 사람이 만든 것이라 믿지 않는다. 경로·해시·크기를 먼저 모두 검사하고, 자격·실행 설정
//! 파일(`.env`·키·`.claude/`·`.mcp.json`·`.envrc` 등)과 링크는 받지 않는다. git 은 hooks·fsmonitor·외부 diff
//! 를 끈 채로만 부른다 — 받은 쪽 저장소에서 남의 묶음 때문에 명령이 실행되면 안 된다. clone·fetch 는 하지 않는다.

use std::fmt;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use kasa_socket::shared_rooms::{ProjectSnapshot, SharedFile};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub enum ProjectError {
    /// 기준 commit·patch 가 있는데 그 commit 을 가진 로컬 저장소가 없다. 파일만 부분 복사하지 않는다.
    ProjectRequired(String),
    /// 묶음·경로가 규칙을 어긴다.
    Invalid(String),
    /// 목적지가 이미 있다. 기존 폴더를 덮지 않는다.
    DestinationExists(PathBuf),
    Git(String),
    Io(io::Error),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectRequired(why) => write!(f, "프로젝트 저장소가 필요해요: {why}"),
            Self::Invalid(why) => write!(f, "프로젝트 자료가 올바르지 않아요: {why}"),
            Self::DestinationExists(dest) => write!(f, "이미 있는 폴더예요: {}", dest.display()),
            Self::Git(why) => write!(f, "git 이 실패했어요: {why}"),
            Self::Io(error) => write!(f, "프로젝트 파일을 다루지 못했어요: {error}"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<io::Error> for ProjectError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProjectLimits {
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_files: usize,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        Self { max_file_bytes: 2 * 1024 * 1024, max_total_bytes: 8 * 1024 * 1024, max_files: 200 }
    }
}

const DENIED_DIRS: [&str; 11] = [".git", ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".config", ".claude", ".codex", ".vscode", ".idea"];
const DENIED_FILES: [&str; 12] = [
    ".envrc", ".netrc", ".npmrc", ".pypirc", ".git-credentials", ".gitconfig", ".mcp.json", "credentials", "credentials.json",
    "auth.json", ".htpasswd", ".pgpass",
];
const DENIED_PREFIXES: [&str; 5] = ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa", "secrets."];
const DENIED_SUFFIXES: [&str; 7] = [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks", ".kdbx"];

/// 저장소 안 상대 경로만 받는다. 이유를 돌려주면 거부다.
pub fn check_rel(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') || path.contains('\\') {
        return Err("빈 경로·역슬래시·NUL 은 받지 않아요".into());
    }
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return Err("절대 경로예요".into());
    }
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err("빈 성분·`.`·`..` 이 있어요".into());
        }
        if part.chars().any(char::is_control) {
            return Err("제어 문자가 있어요".into());
        }
    }
    for dir in &parts[..parts.len() - 1] {
        let lower = dir.to_ascii_lowercase();
        if DENIED_DIRS.contains(&lower.as_str()) {
            return Err(format!("{dir} 아래 파일은 자격·실행 설정이라 받지 않아요"));
        }
    }
    let name = parts.last().expect("split 은 비지 않는다").to_ascii_lowercase();
    let denied = DENIED_DIRS.contains(&name.as_str())
        || DENIED_FILES.contains(&name.as_str())
        || name == ".env"
        || name.starts_with(".env.")
        || DENIED_PREFIXES.iter().any(|p| name.starts_with(p))
        || DENIED_SUFFIXES.iter().any(|s| name.ends_with(s));
    if denied {
        return Err(format!("{name} 은 자격·실행 설정 파일이라 받지 않아요"));
    }
    Ok(())
}

/// hooks·fsmonitor·외부 diff 를 끈 git. `hooks` 는 비어 있는 폴더다.
struct Git {
    hooks: PathBuf,
}

impl Git {
    fn new() -> Result<Self, ProjectError> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let hooks = std::env::temp_dir().join(format!("kasaterm-fork-nohooks-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&hooks)?;
        Ok(Self { hooks })
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Result<std::process::Output, ProjectError> {
        let mut cmd = kasa_socket::process::no_window_command("git");
        cmd.arg("-c").arg(format!("core.hooksPath={}", self.hooks.display()));
        cmd.args(["-c", "core.fsmonitor=false", "-c", "diff.external=", "-C"]).arg(dir).args(args);
        cmd.env("PATH", crate::reposync::tool_path()).env("GIT_OPTIONAL_LOCKS", "0").env("GIT_TERMINAL_PROMPT", "0");
        Ok(cmd.output()?)
    }

    fn out(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, ProjectError> {
        let out = self.run(dir, args)?;
        if !out.status.success() {
            return Err(ProjectError::Git(format!(
                "git {}: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }

    fn text(&self, dir: &Path, args: &[&str]) -> Result<String, ProjectError> {
        Ok(String::from_utf8_lossy(&self.out(dir, args)?).trim().to_string())
    }
}

impl Drop for Git {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.hooks);
    }
}

pub struct CapturedProject {
    pub snapshot: ProjectSnapshot,
    pub omissions: Vec<String>,
    /// 대화 경계 시각보다 나중에 고친 파일. 경계 시점의 파일처럼 보이면 안 된다.
    pub newer_than_boundary: Vec<String>,
}

/// 게시 기기에서 기준 commit(HEAD)·추적 파일 변경 patch·고른 파일을 묶는다. `files` 는 저장소 기준 상대 경로다.
pub fn capture_project(
    repo: &Path,
    files: &[String],
    boundary_at_ms: Option<u64>,
    limits: &ProjectLimits,
) -> Result<CapturedProject, ProjectError> {
    let git = Git::new()?;
    let root = PathBuf::from(git.text(repo, &["rev-parse", "--show-toplevel"])?);
    let real_root = root.canonicalize()?;
    let commit = git.text(&root, &["rev-parse", "--verify", "HEAD"]).ok();
    let repo_url = git.text(&root, &["config", "--get", "remote.origin.url"]).ok().and_then(|u| public_url(&u));
    let mut omissions = Vec::new();
    let mut newer = Vec::new();
    let mut total = 0u64;
    let newer_than = |path: &Path| {
        let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
        let ms = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64;
        Some(boundary_at_ms.is_some_and(|b| ms > b))
    };

    let mut patch = String::new();
    if commit.is_some() {
        let changed = git.out(&root, &["diff", "--name-only", "-z", "--no-renames", "HEAD"])?;
        let mut keep = Vec::new();
        for path in changed.split(|&b| b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            let full = root.join(&path);
            let link = std::fs::symlink_metadata(&full).map(|m| m.file_type().is_symlink()).unwrap_or(false)
                || git.text(&root, &["ls-files", "-s", "--", &path]).is_ok_and(|s| s.starts_with("120000"));
            match check_rel(&path) {
                Err(why) => omissions.push(format!("patch 에서 뺀 경로 {path}: {why}")),
                Ok(()) if link => omissions.push(format!("patch 에서 뺀 링크 {path}")),
                Ok(()) => {
                    if newer_than(&full) == Some(true) {
                        newer.push(path.clone());
                    }
                    keep.push(path);
                }
            }
        }
        if !keep.is_empty() {
            let mut args = vec!["diff", "--binary", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames", "HEAD", "--"];
            args.extend(keep.iter().map(String::as_str));
            patch = String::from_utf8(git.out(&root, &args)?).map_err(|_| ProjectError::Invalid("patch 가 UTF-8 이 아니에요".into()))?;
            total += patch.len() as u64;
        }
    }

    let mut shared = Vec::new();
    for rel in files {
        if let Err(why) = check_rel(rel) {
            omissions.push(format!("뺀 파일 {rel}: {why}"));
            continue;
        }
        let full = root.join(rel);
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(error) => {
                omissions.push(format!("뺀 파일 {rel}: {error}"));
                continue;
            }
        };
        let inside = full.parent().and_then(|p| p.canonicalize().ok()).is_some_and(|p| p.starts_with(&real_root));
        if !meta.is_file() || !inside {
            omissions.push(format!("뺀 파일 {rel}: 저장소 안 일반 파일이 아니에요(링크·폴더)"));
            continue;
        }
        if meta.len() > limits.max_file_bytes || shared.len() >= limits.max_files {
            omissions.push(format!("뺀 파일 {rel}: 크기·개수 한도"));
            continue;
        }
        let bytes = std::fs::read(&full)?;
        total += bytes.len() as u64;
        if total > limits.max_total_bytes {
            return Err(ProjectError::Invalid(format!("프로젝트 자료가 한도 {}바이트를 넘어요", limits.max_total_bytes)));
        }
        if newer_than(&full) == Some(true) {
            newer.push(rel.clone());
        }
        shared.push(SharedFile {
            path: rel.clone(),
            sha256: hex(&Sha256::digest(&bytes)),
            bytes_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    if total > limits.max_total_bytes {
        return Err(ProjectError::Invalid(format!("프로젝트 자료가 한도 {}바이트를 넘어요", limits.max_total_bytes)));
    }
    newer.sort();
    newer.dedup();
    let snapshot = ProjectSnapshot { repo_url, commit, captured_at_ms: now_ms(), patch, files: shared };
    validate_project(&snapshot, limits)?;
    Ok(CapturedProject { snapshot, omissions, newer_than_boundary: newer })
}

/// 원격 주소에서 자격(사용자·토큰)과 질의 문자열을 뺀다. 모르는 꼴은 싣지 않는다.
fn public_url(url: &str) -> Option<String> {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return None;
        }
        let rest = rest.split(['?', '#']).next()?;
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = host.rsplit('@').next()?;
        return (!host.is_empty()).then(|| format!("{scheme}://{host}/{path}"));
    }
    // scp 꼴 `git@host:owner/repo.git` 의 사용자는 자격이 아니다. `ext::` 같은 전송 지정은 싣지 않는다.
    let (user_host, path) = url.split_once(':')?;
    let host_ok = user_host.len() > 1 && user_host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'));
    let path_ok = !path.is_empty()
        && !path.starts_with(':')
        && !path.contains("://")
        && !path.chars().any(|c| c == '\\' || c.is_whitespace() || c.is_control());
    (host_ok && path_ok).then(|| url.to_string())
}

/// 받은 묶음을 쓰기 전에 모두 검사한다.
pub fn validate_project(snapshot: &ProjectSnapshot, limits: &ProjectLimits) -> Result<(), ProjectError> {
    let bad = |why: String| Err(ProjectError::Invalid(why));
    if let Some(commit) = &snapshot.commit {
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return bad("commit 표기가 달라요".into());
        }
    }
    if snapshot.files.len() > limits.max_files {
        return bad("파일이 너무 많아요".into());
    }
    let mut total = snapshot.patch.len() as u64;
    let mut seen = std::collections::HashSet::new();
    for file in &snapshot.files {
        check_rel(&file.path).map_err(|why| ProjectError::Invalid(format!("{}: {why}", file.path)))?;
        if !seen.insert(file.path.to_ascii_lowercase()) {
            return bad(format!("{} 가 두 번 있어요", file.path));
        }
        let bytes = decode(file)?;
        if bytes.len() as u64 > limits.max_file_bytes {
            return bad(format!("{} 가 너무 커요", file.path));
        }
        total += bytes.len() as u64;
    }
    if total > limits.max_total_bytes {
        return bad(format!("프로젝트 자료가 한도 {}바이트를 넘어요", limits.max_total_bytes));
    }
    for path in patch_paths(&snapshot.patch)? {
        check_rel(&path).map_err(|why| ProjectError::Invalid(format!("patch 경로 {path}: {why}")))?;
    }
    Ok(())
}

fn decode(file: &SharedFile) -> Result<Vec<u8>, ProjectError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&file.bytes_base64)
        .map_err(|_| ProjectError::Invalid(format!("{} 의 base64 가 깨졌어요", file.path)))?;
    if file.sha256.len() != 64 || hex(&Sha256::digest(&bytes)) != file.sha256 {
        return Err(ProjectError::Invalid(format!("{} 의 sha256 이 맞지 않아요", file.path)));
    }
    Ok(bytes)
}

/// patch 머리의 경로들. 링크 모드·따옴표 경로·이름 바꾸기는 거부한다(검사를 비켜 갈 수 있다).
fn patch_paths(patch: &str) -> Result<Vec<String>, ProjectError> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        let reject = |why: &str| Err(ProjectError::Invalid(format!("patch 의 {why}: {line}")));
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if rest.contains('"') {
                return reject("따옴표 경로");
            }
            let Some((a, b)) = rest.split_once(" b/") else { return reject("머리 꼴") };
            let Some(a) = a.strip_prefix("a/") else { return reject("머리 꼴") };
            paths.push(a.to_string());
            paths.push(b.to_string());
        } else if line.contains(" mode 120000") || (line.starts_with("index ") && line.ends_with(" 120000")) {
            return reject("링크");
        } else if line.starts_with("rename ") || line.starts_with("copy ") {
            return reject("이름 바꾸기");
        } else if let Some(path) = line.strip_prefix("--- a/").or_else(|| line.strip_prefix("+++ b/")) {
            paths.push(path.to_string());
        }
    }
    Ok(paths)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectBase {
    /// 로컬 저장소에서 기준 commit 으로 만든 별도 worktree.
    Worktree { repo: PathBuf, commit: String },
    /// 기준 commit 도 patch 도 없는 묶음 — 새 폴더에 파일만 둔다.
    EmptyDir,
}

#[derive(Debug)]
pub struct AppliedProject {
    pub dest: PathBuf,
    pub base: ProjectBase,
    pub patch_applied: bool,
    pub files_written: Vec<String>,
    /// 묶음을 만든 시각. 화면은 대화 경계 시각과 나란히 보여 준다.
    pub captured_at_ms: u64,
}

/// 받은 묶음을 `dest`(아직 없는 절대 경로)에 푼다. 기준 commit·patch 가 있으면 `local_repo` 에 그 commit 이
/// 있어야 하고 별도 worktree 로 만든다. 실패하면 이 호출이 만든 것만 지운다.
pub fn apply_project(snapshot: &ProjectSnapshot, local_repo: Option<&Path>, dest: &Path) -> Result<AppliedProject, ProjectError> {
    validate_project(snapshot, &ProjectLimits::default())?;
    if !dest.is_absolute() {
        return Err(ProjectError::Invalid("목적지는 절대 경로여야 해요".into()));
    }
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(ProjectError::DestinationExists(dest.to_path_buf()));
    }
    let git = Git::new()?;
    let needs_base = snapshot.commit.is_some() || !snapshot.patch.is_empty();
    let base = if needs_base {
        let Some(commit) = snapshot.commit.clone() else {
            return Err(ProjectError::ProjectRequired("기준 commit 이 없는 patch 는 적용할 수 없어요".into()));
        };
        let Some(repo) = local_repo else {
            return Err(ProjectError::ProjectRequired(format!("commit {commit} 이 있는 로컬 저장소를 골라 주세요")));
        };
        let root = PathBuf::from(git.text(repo, &["rev-parse", "--show-toplevel"]).map_err(|e| ProjectError::ProjectRequired(e.to_string()))?);
        if !git.run(&root, &["cat-file", "-e", &format!("{commit}^{{commit}}")])?.status.success() {
            return Err(ProjectError::ProjectRequired(format!("로컬 저장소에 commit {commit} 이 없어요. 가져온 뒤 다시 해 주세요")));
        }
        if within(dest, &root) {
            return Err(ProjectError::Invalid("목적지가 기존 작업 트리 안이에요".into()));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let dest_text = dest.to_str().ok_or_else(|| ProjectError::Invalid("목적지 경로가 UTF-8 이 아니에요".into()))?;
        git.out(&root, &["worktree", "add", "--detach", dest_text, &commit])?;
        ProjectBase::Worktree { repo: root, commit }
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir(dest)?;
        ProjectBase::EmptyDir
    };
    match fill(&git, snapshot, dest) {
        Ok((patch_applied, files_written)) => Ok(AppliedProject {
            dest: dest.to_path_buf(),
            base,
            patch_applied,
            files_written,
            captured_at_ms: snapshot.captured_at_ms,
        }),
        Err(error) => {
            match &base {
                ProjectBase::Worktree { repo, .. } => {
                    let _ = git.run(repo, &["worktree", "remove", "--force", dest.to_str().unwrap_or_default()]);
                }
                ProjectBase::EmptyDir => {
                    let _ = std::fs::remove_dir_all(dest);
                }
            }
            Err(error)
        }
    }
}

fn fill(git: &Git, snapshot: &ProjectSnapshot, dest: &Path) -> Result<(bool, Vec<String>), ProjectError> {
    let patch_applied = !snapshot.patch.is_empty();
    if patch_applied {
        let file = git.hooks.with_extension("patch");
        std::fs::write(&file, &snapshot.patch)?;
        let path = file.to_str().unwrap_or_default().to_string();
        let checked = git.out(dest, &["apply", "--check", "--whitespace=nowarn", &path]);
        let applied = checked.and_then(|_| git.out(dest, &["apply", "--whitespace=nowarn", &path]));
        let _ = std::fs::remove_file(&file);
        applied?;
    }
    let mut written = Vec::new();
    for file in &snapshot.files {
        let bytes = decode(file)?;
        let target = safe_target(dest, &file.path)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&target)?;
        out.write_all(&bytes)?;
        written.push(file.path.clone());
    }
    Ok((patch_applied, written))
}

/// `dest` 안의 쓸 자리. 이미 있는 성분이 링크면 거부한다 — checkout 된 링크를 따라 밖에 쓰지 않는다.
fn safe_target(dest: &Path, rel: &str) -> Result<PathBuf, ProjectError> {
    let mut cur = dest.to_path_buf();
    for part in Path::new(rel).components() {
        let Component::Normal(part) = part else {
            return Err(ProjectError::Invalid(format!("{rel}: 경로 성분이 달라요")));
        };
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(ProjectError::Invalid(format!("{rel}: 링크를 지나요"))),
            Ok(_) | Err(_) => {}
        }
    }
    Ok(cur)
}

fn within(path: &Path, root: &Path) -> bool {
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut probe = path.to_path_buf();
    let mut tail = Vec::new();
    while !probe.exists() {
        match (probe.file_name().map(|n| n.to_os_string()), probe.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                probe = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let real = tail.iter().rev().fold(probe.canonicalize().unwrap_or(probe), |acc, part| acc.join(part));
    real.starts_with(real_root)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests;
