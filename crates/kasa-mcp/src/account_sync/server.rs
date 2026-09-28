use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::schema::{validate_patch, validate_snapshot, Patch, Snapshot, MAX_BODY};

#[derive(Debug)]
pub enum Error {
    Conflict(Snapshot),
    Invalid(String),
    Storage,
}

pub struct Store {
    dir: Option<PathBuf>,
    snapshots: Mutex<HashMap<String, Snapshot>>,
}

impl Store {
    pub fn new(dir: Option<PathBuf>) -> Self { Self { dir, snapshots: Mutex::new(HashMap::new()) } }

    fn load(&self, account: &str, cache: &mut HashMap<String, Snapshot>) -> Result<Snapshot, Error> {
        if !crate::relay_auth::valid_account_name(account) { return Err(Error::Storage); }
        if let Some(snapshot) = cache.get(account) { return Ok(snapshot.clone()); }
        let snapshot = if let Some(dir) = &self.dir {
            if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) { return Err(Error::Storage); }
            let path = dir.join(format!("{account}.json"));
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    if !meta.is_file() || meta.len() > MAX_BODY as u64 { return Err(Error::Storage); }
                    #[cfg(unix)] {
                        use std::os::unix::fs::PermissionsExt;
                        if meta.permissions().mode() & 0o077 != 0 { return Err(Error::Storage); }
                    }
                    serde_json::from_slice(&std::fs::read(&path).map_err(|_| Error::Storage)?)
                        .map_err(|_| Error::Storage)?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Snapshot::default(),
                Err(_) => return Err(Error::Storage),
            }
        } else { Snapshot::default() };
        validate_snapshot(&snapshot).map_err(|_| Error::Storage)?;
        cache.insert(account.into(), snapshot.clone());
        Ok(snapshot)
    }

    pub fn get(&self, account: &str) -> Result<Snapshot, Error> {
        let mut cache = self.snapshots.lock().map_err(|_| Error::Storage)?;
        self.load(account, &mut cache)
    }

    pub fn patch(&self, account: &str, patch: &Patch) -> Result<Snapshot, Error> {
        validate_patch(patch).map_err(Error::Invalid)?;
        let mut cache = self.snapshots.lock().map_err(|_| Error::Storage)?;
        let mut current = self.load(account, &mut cache)?;
        if current.revision != patch.expected_revision { return Err(Error::Conflict(current)); }
        let previous = current.clone();
        for (key, value) in &patch.settings {
            if value.is_null() { current.settings.remove(key); } else { current.settings.insert(key.clone(), value.clone()); }
        }
        for (key, value) in &patch.machines {
            if value.is_null() { current.machines.remove(key); } else { current.machines.insert(key.clone(), value.clone()); }
        }
        validate_snapshot(&current).map_err(Error::Invalid)?;
        if current == previous { return Ok(current); }
        current.revision = current.revision.checked_add(1).ok_or(Error::Storage)?;
        if let Some(dir) = &self.dir {
            write_snapshot(dir, account, &current).map_err(|_| Error::Storage)?;
        }
        cache.insert(account.into(), current.clone());
        Ok(current)
    }
}

fn write_snapshot(dir: &Path, account: &str, snapshot: &Snapshot) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other("account sync directory is a symlink"));
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join(format!("{account}.json"));
    let temp = dir.join(format!(".{account}-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(snapshot)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accounts_are_separate_conflicts_preserve_values_and_state_survives_restart() {
        let dir = std::env::temp_dir().join(format!("kasa-profile-{}", uuid::Uuid::new_v4()));
        let store = Store::new(Some(dir.clone()));
        let first = Patch { settings: [("theme".into(), json!("graphite"))].into(), ..Default::default() };
        assert_eq!(store.patch("alice", &first).unwrap().revision, 1);
        assert_eq!(store.get("bob").unwrap(), Snapshot::default());
        let stale = Patch { settings: [("font_size".into(), json!(14))].into(), ..Default::default() };
        assert!(matches!(store.patch("alice", &stale), Err(Error::Conflict(s)) if s.revision == 1));
        let merged = store.patch("alice", &Patch { expected_revision: 1, ..stale }).unwrap();
        assert_eq!(merged.settings.len(), 2);
        assert_eq!(Store::new(Some(dir.clone())).get("alice").unwrap(), merged);
        assert_eq!(store.patch("alice", &Patch { expected_revision: 2, ..Default::default() }).unwrap().revision, 2);
        assert!(store.get("../../alice").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_persisted_profile_is_never_replaced_with_empty_data() {
        let dir = std::env::temp_dir().join(format!("kasa-profile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::relay_auth::write_private(&dir.join("alice.json"), "broken").unwrap();
        let store = Store::new(Some(dir.clone()));
        assert!(matches!(store.patch("alice", &Patch::default()), Err(Error::Storage)));
        assert_eq!(std::fs::read_to_string(dir.join("alice.json")).unwrap(), "broken");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
