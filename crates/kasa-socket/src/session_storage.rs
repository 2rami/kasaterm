//! Persistence paths shared by the app and its offline tools.
use std::path::{Path, PathBuf};

/// An empty/partial process inventory is not evidence that the old app exited.
pub fn sole_app_process(own_pid: u32, processes: &[(u32, u32, String)]) -> bool {
    processes.iter().any(|(pid, _, _)| *pid == own_pid)
        && !processes
            .iter()
            .any(|(pid, _, name)| *pid != own_pid && name.to_ascii_lowercase().contains("kasaterm"))
}

pub fn session_path(root: &Path, override_path: Option<&std::ffi::OsStr>) -> PathBuf {
    override_path
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("sessions/session.json"))
}

/// A remaining legacy file can still have an old writer. Until migration
/// removes it, readers and writers must agree on that same location.
pub fn read_path(root: &Path, name: &str) -> PathBuf {
    let legacy = root.join(name);
    if legacy.try_exists().ok() != Some(false) {
        legacy
    } else {
        root.join("sessions").join(name)
    }
}

fn restored_stamp(path: &Path) -> Option<u64> {
    path.file_name()?
        .to_str()?
        .strip_prefix("session-restored-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

pub fn prune_restored(dir: &Path, keep: usize) -> std::io::Result<()> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| restored_stamp(&e.path()).map(|stamp| (stamp, e.path())))
        .collect();
    paths.sort_by_key(|(stamp, _)| *stamp);
    let remove = paths.len().saturating_sub(keep);
    for (_, path) in paths.into_iter().take(remove) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn preflight_file(old: &Path, new: &Path) -> std::io::Result<()> {
    if !old.try_exists()? {
        return Ok(());
    }
    if !std::fs::symlink_metadata(old)?.file_type().is_file() {
        return Err(std::io::Error::other(
            "legacy state is not a regular file; preserved",
        ));
    }
    let bytes = std::fs::read(old)?;
    if !serde_json::from_slice::<serde_json::Value>(&bytes)
        .is_ok_and(|v| v.is_object() || v.is_array())
    {
        return Err(std::io::Error::other("damaged legacy state preserved"));
    }
    if new.try_exists()? {
        return Err(std::io::Error::other(
            "both state paths exist; legacy state preserved",
        ));
    }
    Ok(())
}

fn migrate_file(old: &Path, new: &Path) -> std::io::Result<()> {
    preflight_file(old, new)?;
    if !old.try_exists()? {
        return Ok(());
    }
    std::fs::create_dir_all(new.parent().unwrap())?;
    // Linking publishes complete bytes without ever replacing a concurrent writer.
    std::fs::hard_link(old, new)?;
    std::fs::remove_file(old)
}

/// Only the app migrates these files. Character mappings have their own lock.
pub fn migrate_legacy(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let dir = root.join("sessions");
    let mut files: Vec<PathBuf> = [
        "session.json",
        "session.documents.json",
        "viewer-documents.json",
    ]
    .iter()
    .map(|name| root.join(name))
    .collect();
    if let Ok(entries) = std::fs::read_dir(root) {
        files.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| restored_stamp(p).is_some()),
        );
    }
    let pending = root.join("server-restore-pending");
    if let Ok(entries) = std::fs::read_dir(&pending) {
        files.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json")),
        );
    }
    for old in &files {
        let new = dir.join(old.strip_prefix(root).unwrap());
        if let Err(e) = preflight_file(old, &new) {
            errors.push(format!("{}: {e}", old.display()));
        }
    }
    if !errors.is_empty() {
        return errors;
    }
    for old in files {
        let new = dir.join(old.strip_prefix(root).unwrap());
        if let Err(e) = migrate_file(&old, &new) {
            errors.push(format!("{}: {e}", old.display()));
        }
    }
    // Nonempty legacy folders (including conflicts) remain recoverable.
    let _ = std::fs::remove_dir(pending);
    if dir.is_dir() {
        if let Err(e) = prune_restored(&dir, 5) {
            errors.push(e.to_string());
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_requires_self_and_no_other_app_including_viewer() {
        assert!(!sole_app_process(1, &[]));
        assert!(!sole_app_process(1, &[(2, 0, "shell".into())]));
        let own = (1, 0, "kasaterm".into());
        assert!(sole_app_process(1, &[own.clone()]));
        assert!(!sole_app_process(
            1,
            &[own.clone(), (2, 0, "kasaterm".into())]
        ));
        assert!(!sole_app_process(
            1,
            &[own, (2, 0, "kasaterm-viewer".into())]
        ));
    }
    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "kasa-storage-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn put(&self, name: &str, bytes: &[u8]) {
            let p = self.0.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn moves_legacy_bundle_and_caps_numeric_backups() {
        let r = Root::new();
        for name in [
            "session.json",
            "session.documents.json",
            "viewer-documents.json",
            "server-restore-pending/a.json",
        ] {
            r.put(name, b"{}");
        }
        r.put("session_characters.json", b"{}");
        for stamp in 8..15 {
            r.put(&format!("session-restored-{stamp}.json"), b"{}");
        }
        assert!(migrate_legacy(&r.0).is_empty());
        for name in [
            "session.json",
            "session.documents.json",
            "viewer-documents.json",
            "server-restore-pending/a.json",
        ] {
            assert!(!r.0.join(name).exists());
            assert!(r.0.join("sessions").join(name).exists());
        }
        assert!(r.0.join("session_characters.json").exists());
        assert!(!r.0.join("sessions/session-restored-9.json").exists());
        assert!(r.0.join("sessions/session-restored-10.json").exists());
        assert!(migrate_legacy(&r.0).is_empty());
    }
    #[test]
    fn conflicts_and_corruption_keep_originals_and_existing_writer_path() {
        let r = Root::new();
        r.put("session.json", b"{\"old\":true}");
        assert_eq!(read_path(&r.0, "session.json"), r.0.join("session.json"));
        r.put("sessions/session.json", b"broken");
        r.put("viewer-documents.json", b"broken");
        assert_eq!(migrate_legacy(&r.0).len(), 2);
        assert_eq!(
            std::fs::read(r.0.join("session.json")).unwrap(),
            b"{\"old\":true}"
        );
        assert_eq!(read_path(&r.0, "session.json"), r.0.join("session.json"));
        assert!(r.0.join("viewer-documents.json").exists());
    }
    #[test]
    fn override_and_new_only_paths_stay_isolated() {
        let r = Root::new();
        r.put("sessions/session.json", b"{}");
        assert!(migrate_legacy(&r.0).is_empty());
        let custom = r.0.join("probe/custom.json");
        assert_eq!(session_path(&r.0, Some(custom.as_os_str())), custom);
        assert_eq!(
            session_path(&r.0, Some(std::ffi::OsStr::new(""))),
            r.0.join("sessions/session.json")
        );
    }
}
