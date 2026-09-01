// Codex rollout transfer primitives.
//
// Codex 0.152.0 writes an interactive thread to one append-only rollout under
// `$CODEX_HOME/sessions/YYYY/MM/DD/`. A kasaterm pane-specific `CODEX_HOME`
// symlinks that directory to the user's real Codex home, so neither the
// temporary home nor `state_5.sqlite` is a durable transfer source.
//
// This module deliberately stops at the verified boundary: it discovers and
// validates the rollout, reads its bytes, and tells the caller where those
// bytes belong under another Codex home. It does not rewrite rollout JSON,
// copy authentication/configuration, mutate Codex's SQLite indexes, or write
// destination files. The caller can therefore use its own atomic-write and
// conflict policy.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, Read};
use std::path::{Component, Path, PathBuf};

/// Transport format understood by this helper. Increment when the required
/// file set or validation contract changes.
pub const CODEX_SESSION_BUNDLE_VERSION: u32 = 1;

/// A rollout is bounded before it is copied into memory. Claude migration has
/// the same 512 MiB safety ceiling; exceeding it is reported rather than
/// silently truncating an append-only conversation.
pub const MAX_CODEX_ROLLOUT_BYTES: u64 = 512 * 1024 * 1024;

/// Validated source location of one resumable Codex thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexSessionLocation {
    /// `session_meta.payload.id`, which is the id accepted by `codex resume`.
    /// `payload.session_id` is intentionally ignored: for a subagent rollout
    /// Codex 0.152.0 puts the parent thread id there.
    pub session_id: String,
    /// Original absolute working directory recorded by Codex.
    pub cwd: PathBuf,
    /// Canonical source rollout path (pane-home symlinks already resolved).
    pub rollout_path: PathBuf,
    /// Portable target below a Codex home, for example
    /// `sessions/2026/09/02/rollout-...jsonl`.
    pub codex_home_relative_path: PathBuf,
}

/// One required session file and its exact bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexSessionFile {
    pub codex_home_relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

/// File-only transfer bundle for a Codex thread.
///
/// The verified required set is exactly the rollout. Account credentials,
/// config, hooks, global history, SQLite indexes and generated artifacts are
/// intentionally not included: they are either destination/account state or
/// are not established as requirements for `codex resume`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexSessionBundle {
    pub version: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    pub files: Vec<CodexSessionFile>,
}

/// A validated destination path paired with borrowed transfer bytes. This is
/// a plan only: applying overwrite/conflict/atomic-rename policy belongs to the
/// migration endpoint, not the format helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRestoreFile<'a> {
    pub path: PathBuf,
    pub bytes: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
struct RolloutMeta {
    id: String,
    cwd: PathBuf,
}

/// Extract the UUID suffix from `rollout-<timestamp>-<uuid>.jsonl`.
///
/// Both the timestamp and UUID contain `-`, so counting fields from the front
/// is unsafe. The last 36 ASCII bytes are accepted only when they are a full
/// UUID and are preceded by a delimiter.
pub fn codex_id_from_rollout_path(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?.strip_prefix("rollout-")?;
    if stem.len() <= 37 {
        return None;
    }
    let split = stem.len().checked_sub(36)?;
    if stem.as_bytes().get(split.checked_sub(1)?) != Some(&b'-') {
        return None;
    }
    let id = stem.get(split..)?;
    super::is_uuid(id).then(|| id.to_string())
}

/// Validate a bound transcript path against an explicit Codex home.
///
/// Passing the pane-specific `CODEX_HOME` is supported: its `sessions` symlink
/// and the rollout path are canonicalized before the portable relative path is
/// derived. A path outside that sessions tree is rejected before any bytes are
/// bundled.
pub fn locate_codex_session_at(
    codex_home: &Path,
    rollout_path: &Path,
) -> Result<CodexSessionLocation> {
    let sessions = codex_home.join("sessions");
    let canonical_sessions = std::fs::canonicalize(&sessions)
        .with_context(|| format!("Codex sessions 폴더를 열 수 없음: {}", sessions.display()))?;
    let canonical_rollout = std::fs::canonicalize(rollout_path)
        .with_context(|| format!("Codex rollout을 열 수 없음: {}", rollout_path.display()))?;
    let inside = canonical_rollout
        .strip_prefix(&canonical_sessions)
        .with_context(|| {
            format!(
                "Codex rollout이 sessions 폴더 밖에 있음: {}",
                canonical_rollout.display()
            )
        })?;
    let relative = Path::new("sessions").join(inside);
    validate_relative_rollout_path(&relative)?;

    let bytes = read_stable_rollout(&canonical_rollout)?;
    let meta = rollout_meta(&bytes)?;
    validate_meta_against_path(&meta, &relative)?;

    Ok(CodexSessionLocation {
        session_id: meta.id,
        cwd: meta.cwd,
        rollout_path: canonical_rollout,
        codex_home_relative_path: relative,
    })
}

/// Find a current (non-archived) rollout by exact UUID below
/// `$CODEX_HOME/sessions` and validate its header.
///
/// Duplicate valid files are an error rather than an arbitrary first hit. A
/// migration must not silently choose between two histories for the same id.
pub fn find_codex_session(
    codex_home: &Path,
    session_id: &str,
) -> Result<Option<CodexSessionLocation>> {
    if !super::is_uuid(session_id) {
        bail!("Codex session id가 UUID가 아님: {session_id}");
    }
    let root = codex_home.join("sessions");
    let mut paths = Vec::new();
    collect_matching_rollouts(&root, session_id, 5, &mut paths);
    paths.sort();
    paths.dedup();

    let mut matches = Vec::new();
    for path in paths {
        matches.push(locate_codex_session_at(codex_home, &path)?);
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        n => bail!("같은 Codex session id의 rollout이 {n}개라 하나를 고를 수 없음"),
    }
}

/// Read one validated rollout into a transfer bundle.
pub fn bundle_codex_session(codex_home: &Path, rollout_path: &Path) -> Result<CodexSessionBundle> {
    let location = locate_codex_session_at(codex_home, rollout_path)?;
    let bytes = read_stable_rollout(&location.rollout_path)?;
    let bundle = CodexSessionBundle {
        version: CODEX_SESSION_BUNDLE_VERSION,
        session_id: location.session_id,
        cwd: location.cwd,
        files: vec![CodexSessionFile {
            codex_home_relative_path: location.codex_home_relative_path,
            bytes,
        }],
    };
    validate_bundle(&bundle)?;
    Ok(bundle)
}

/// Find by UUID and bundle in one call. `None` means no current rollout was
/// found; malformed or ambiguous matching files are errors.
pub fn bundle_codex_session_by_id(
    codex_home: &Path,
    session_id: &str,
) -> Result<Option<CodexSessionBundle>> {
    let Some(location) = find_codex_session(codex_home, session_id)? else {
        return Ok(None);
    };
    bundle_codex_session(codex_home, &location.rollout_path).map(Some)
}

/// Validate a received bundle and return its destination paths below another
/// Codex home. No directory or file is created.
pub fn codex_restore_files<'a>(
    destination_codex_home: &Path,
    bundle: &'a CodexSessionBundle,
) -> Result<Vec<CodexRestoreFile<'a>>> {
    validate_bundle(bundle)?;
    Ok(bundle
        .files
        .iter()
        .map(|file| CodexRestoreFile {
            path: destination_codex_home.join(&file.codex_home_relative_path),
            bytes: &file.bytes,
        })
        .collect())
}

fn validate_bundle(bundle: &CodexSessionBundle) -> Result<()> {
    if bundle.version != CODEX_SESSION_BUNDLE_VERSION {
        bail!(
            "지원하지 않는 Codex session bundle version: {}",
            bundle.version
        );
    }
    if !super::is_uuid(&bundle.session_id) {
        bail!("Codex bundle session id가 UUID가 아님");
    }
    if !bundle.cwd.is_absolute() {
        bail!("Codex bundle cwd가 절대경로가 아님");
    }
    if bundle.files.len() != 1 {
        bail!("Codex bundle v1은 rollout 한 파일만 허용함");
    }
    let file = &bundle.files[0];
    validate_relative_rollout_path(&file.codex_home_relative_path)?;
    if file.bytes.len() as u64 > MAX_CODEX_ROLLOUT_BYTES {
        bail!("Codex rollout이 512 MiB 상한을 넘음");
    }
    let meta = rollout_meta(&file.bytes)?;
    validate_meta_against_path(&meta, &file.codex_home_relative_path)?;
    if meta.id != bundle.session_id || meta.cwd != bundle.cwd {
        bail!("Codex bundle manifest와 rollout metadata가 다름");
    }
    Ok(())
}

fn validate_meta_against_path(meta: &RolloutMeta, relative: &Path) -> Result<()> {
    let filename_id = codex_id_from_rollout_path(relative)
        .context("Codex rollout 파일명에서 UUID를 확인할 수 없음")?;
    if meta.id != filename_id {
        bail!(
            "Codex rollout header id와 파일명 id가 다름: {} != {}",
            meta.id,
            filename_id
        );
    }
    Ok(())
}

fn validate_relative_rollout_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!("Codex rollout 복원 경로가 절대경로임");
    }
    let components: Vec<_> = path.components().collect();
    if components
        .iter()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("Codex rollout 복원 경로에 상위/현재/root 경로가 들어 있음");
    }
    let names: Vec<_> = components
        .iter()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let date_layout = names.len() == 5
        && names[0] == "sessions"
        && valid_digits(names[1], 4)
        && valid_digits(names[2], 2)
        && valid_digits(names[3], 2);
    let legacy_layout = names.len() == 2 && names[0] == "sessions";
    if !(date_layout || legacy_layout) {
        bail!("검증되지 않은 Codex rollout 경로 구조: {}", path.display());
    }
    if codex_id_from_rollout_path(path).is_none() {
        bail!("Codex rollout 파일명이 검증된 형식이 아님");
    }
    Ok(())
}

fn valid_digits(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit())
}

fn rollout_meta(bytes: &[u8]) -> Result<RolloutMeta> {
    // Current Codex puts session_meta first. Scan a small bounded prefix to
    // tolerate harmless prelude records without treating arbitrary later
    // events as metadata.
    const META_SCAN_BYTES: u64 = 2 * 1024 * 1024;
    const META_SCAN_LINES: usize = 32;
    let reader = std::io::BufReader::new(std::io::Cursor::new(bytes));
    for line in reader
        .take(META_SCAN_BYTES)
        .lines()
        .take(META_SCAN_LINES)
        .map_while(std::result::Result::ok)
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
            continue;
        }
        let payload = value
            .get("payload")
            .and_then(|v| v.as_object())
            .context("Codex session_meta.payload가 객체가 아님")?;
        // `session_id` is not a fallback. In a 0.152.0 subagent rollout it is
        // the parent thread id while `id` and the filename name this thread.
        let id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|v| super::is_uuid(v))
            .context("Codex session_meta.payload.id가 UUID가 아님")?;
        let cwd = payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .context("Codex session_meta.payload.cwd가 없음")?;
        if !cwd.is_absolute() {
            bail!("Codex session_meta.payload.cwd가 절대경로가 아님");
        }
        return Ok(RolloutMeta {
            id: id.to_string(),
            cwd,
        });
    }
    bail!("지원하는 Codex session_meta를 rollout 앞부분에서 찾지 못함")
}

fn read_stable_rollout(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Codex rollout 열기 실패: {}", path.display()))?;
    let before = file
        .metadata()
        .context("Codex rollout metadata 읽기 실패")?
        .len();
    if before > MAX_CODEX_ROLLOUT_BYTES {
        bail!("Codex rollout이 512 MiB 상한을 넘음");
    }
    let mut bytes = Vec::with_capacity(before.min(8 * 1024 * 1024) as usize);
    (&mut file)
        .take(MAX_CODEX_ROLLOUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("Codex rollout bytes 읽기 실패")?;
    if bytes.len() as u64 > MAX_CODEX_ROLLOUT_BYTES {
        bail!("Codex rollout이 읽는 동안 512 MiB 상한을 넘음");
    }
    let after = file
        .metadata()
        .context("Codex rollout metadata 재확인 실패")?
        .len();
    if before != after || bytes.len() as u64 != before {
        bail!("Codex rollout이 묶는 동안 바뀜 — 종료/flush 뒤 다시 시도 필요");
    }
    Ok(bytes)
}

fn collect_matching_rollouts(dir: &Path, session_id: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            collect_matching_rollouts(&path, session_id, depth - 1, out);
        } else if kind.is_file() && codex_id_from_rollout_path(&path).as_deref() == Some(session_id)
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const ID: &str = "01a05db2-2814-7db3-bfa0-84e22e2467bc";
    const PARENT_ID: &str = "01a05d83-4ec4-7e73-b66b-8bf5d5309d9f";

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kasaterm-codex-transfer-{}-{name}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rollout(home: &Path, id: &str, header_id: &str, cwd: &str) -> PathBuf {
        let path = home.join(format!(
            "sessions/2026/09/02/rollout-2026-09-02T00-59-11-{id}.jsonl"
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let header = serde_json::json!({
            "timestamp": "2026-09-01T15:59:11.965Z",
            "type": "session_meta",
            "payload": {
                "id": header_id,
                "session_id": PARENT_ID,
                "cwd": cwd,
                "originator": "codex-tui"
            }
        });
        std::fs::write(&path, format!("{header}\n{{\"type\":\"event_msg\"}}\n")).unwrap();
        path
    }

    #[test]
    fn current_thread_id_wins_over_parent_session_id() {
        let root = TempRoot::new("parent-id");
        let path = rollout(&root.0, ID, ID, "/Users/kasa/project");
        let got = locate_codex_session_at(&root.0, &path).unwrap();
        assert_eq!(got.session_id, ID);
        assert_eq!(got.cwd, Path::new("/Users/kasa/project"));
    }

    #[test]
    fn bundle_contains_only_the_rollout_and_plans_the_remote_path() {
        let root = TempRoot::new("bundle");
        let path = rollout(&root.0, ID, ID, "/Users/kasa/project");
        let expected = std::fs::read(&path).unwrap();
        let bundle = bundle_codex_session(&root.0, &path).unwrap();
        assert_eq!(bundle.version, CODEX_SESSION_BUNDLE_VERSION);
        assert_eq!(bundle.session_id, ID);
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].bytes, expected);
        assert_eq!(
            bundle.files[0].codex_home_relative_path,
            Path::new("sessions/2026/09/02").join(path.file_name().unwrap())
        );

        let restore = codex_restore_files(Path::new("/Users/miku/.codex"), &bundle).unwrap();
        assert_eq!(restore.len(), 1);
        assert_eq!(
            restore[0].path,
            Path::new("/Users/miku/.codex/sessions/2026/09/02").join(path.file_name().unwrap())
        );
        assert_eq!(restore[0].bytes, expected);
        assert!(
            !restore[0].path.exists(),
            "restore helper는 파일을 직접 쓰지 않는다"
        );
    }

    #[test]
    fn header_and_filename_must_name_the_same_thread() {
        let root = TempRoot::new("mismatch");
        let path = rollout(&root.0, ID, PARENT_ID, "/Users/kasa/project");
        let err = locate_codex_session_at(&root.0, &path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("header id와 파일명 id가 다름"), "{err}");
    }

    #[test]
    fn legacy_header_without_cwd_is_not_guessed() {
        let root = TempRoot::new("legacy");
        let path = root
            .0
            .join(format!("sessions/rollout-2025-07-15T19-42-06-{ID}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(r#"{{"id":"{ID}","timestamp":"2025-07-15T19:42:06Z"}}"#),
        )
        .unwrap();
        let err = locate_codex_session_at(&root.0, &path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("session_meta"), "{err}");
    }

    #[test]
    fn find_by_id_walks_the_verified_date_layout() {
        let root = TempRoot::new("find");
        let path = rollout(&root.0, ID, ID, "/Users/kasa/project");
        let got = find_codex_session(&root.0, ID).unwrap().unwrap();
        assert_eq!(got.rollout_path, std::fs::canonicalize(path).unwrap());
    }

    #[test]
    fn duplicate_histories_are_ambiguous_not_newest_wins() {
        let root = TempRoot::new("duplicate");
        rollout(&root.0, ID, ID, "/Users/kasa/project");
        let second = root.0.join(format!(
            "sessions/2026/09/01/rollout-2026-09-01T23-00-00-{ID}.jsonl"
        ));
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        let header = serde_json::json!({
            "type": "session_meta",
            "payload": {"id": ID, "cwd": "/Users/kasa/project"}
        });
        std::fs::write(second, format!("{header}\n")).unwrap();
        let err = find_codex_session(&root.0, ID).unwrap_err().to_string();
        assert!(err.contains("2개"), "{err}");
    }

    #[test]
    fn restore_rejects_parent_traversal_even_with_valid_bytes() {
        let root = TempRoot::new("traversal");
        let path = rollout(&root.0, ID, ID, "/Users/kasa/project");
        let mut bundle = bundle_codex_session(&root.0, &path).unwrap();
        bundle.files[0].codex_home_relative_path = PathBuf::from("sessions/../auth.json");
        assert!(codex_restore_files(Path::new("/dest/.codex"), &bundle).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pane_home_sessions_symlink_resolves_to_the_durable_store() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new("symlink");
        let real_home = root.0.join("real-home");
        let pane_home = root.0.join("pane-home");
        std::fs::create_dir_all(real_home.join("sessions")).unwrap();
        std::fs::create_dir_all(&pane_home).unwrap();
        symlink(real_home.join("sessions"), pane_home.join("sessions")).unwrap();
        let path = rollout(&real_home, ID, ID, "/Users/kasa/project");
        let via_pane = pane_home
            .join("sessions/2026/09/02")
            .join(path.file_name().unwrap());

        let got = locate_codex_session_at(&pane_home, &via_pane).unwrap();
        assert_eq!(got.rollout_path, std::fs::canonicalize(path).unwrap());
        assert_eq!(
            got.codex_home_relative_path,
            Path::new("sessions/2026/09/02").join(via_pane.file_name().unwrap())
        );
    }
}
