use super::*;
use std::collections::{HashMap, HashSet};

/// A late duplicate of a creation request is unlikely after this; the tombstone only needs to outlive retries.
pub(super) const TOMBSTONE_RETENTION_MS: u64 = 30 * 24 * 3600 * 1000;

/// Whether a failed write may already have replaced its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Landed { No, Maybe }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone { owner: String, deleted_at_ms: u64 }

fn is_temp(name: &str) -> bool { name.starts_with('.') && name.ends_with(".tmp") }

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect()
}

fn remove_temps(dir: &Path) {
    for name in names(dir).into_iter().filter(|n| is_temp(n)) { let _ = std::fs::remove_file(dir.join(name)); }
}

/// Bytes past the committed length were never visible; they only take space.
fn truncate_tail(path: &Path, committed: u64) {
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > committed) { return; }
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NOFOLLOW); }
    if let Ok(file) = options.open(path) { let _ = file.set_len(committed).and_then(|()| file.sync_all()); }
}

impl Store {
    /// At startup every manifest on disk is durable, so whatever no manifest names can go.
    pub(super) fn recover(&self) -> Result<(), Error> {
        let rooms = self.dir("rooms")?;
        for name in names(&rooms) {
            if name.starts_with(".new-") || name.starts_with(".trash-") {
                remove_tree(&rooms.join(&name));
            } else if hex(&name, 32) {
                self.reclaim(&name);
            }
        }
        remove_temps(&self.dir("accounts")?);
        let forks = self.dir("forks")?;
        for account in names(&forks).into_iter().filter(|n| valid_account(n)) {
            let dir = forks.join(account);
            if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) { remove_temps(&dir); }
        }
        let tombstones = self.dir("tombstones")?;
        remove_temps(&tombstones);
        let now = self.now();
        for name in names(&tombstones).into_iter().filter(|n| n.strip_suffix(".json").is_some_and(|id| hex(id, 32))) {
            let path = tombstones.join(&name);
            if read_json::<Tombstone>(&path, 4 << 10).is_ok_and(|t| t.deleted_at_ms.saturating_add(TOMBSTONE_RETENTION_MS) < now) {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }

    /// Removes replaced or failed generations and checkpoints, temporary files and uncommitted tails. Nothing is
    /// deleted against a manifest that cannot be read.
    pub(super) fn reclaim(&self, room_id: &str) {
        let (Ok(dir), Ok(room)) = (self.room_dir(room_id), self.load(room_id)) else { return };
        let logs: HashMap<String, u64> = room.sessions.iter().map(|s| (format!("{}-{}.log", s.id, s.generation), s.bytes)).collect();
        let checkpoints: HashSet<String> = room.sessions.iter().filter_map(|s| s.checkpoint.as_ref())
            .map(|c| format!("{}.checkpoint", c.summary.id)).collect();
        for name in names(&dir) {
            let path = dir.join(&name);
            if let Some(&committed) = logs.get(&name) {
                truncate_tail(&path, committed);
                continue;
            }
            let log = name.strip_suffix(".log").and_then(|stem| stem.split_once('-')).is_some_and(|(s, g)| hex(s, 32) && hex(g, 32));
            let checkpoint = name.strip_suffix(".checkpoint").is_some_and(|id| hex(id, 32)) && !checkpoints.contains(&name);
            if is_temp(&name) || log || checkpoint { let _ = std::fs::remove_file(path); }
        }
    }

    /// Reclaims right away unless the manifest write may have landed; then the next startup decides.
    pub(super) fn settle(&self, room_id: &str, outcome: Result<(), (Error, Landed)>) -> Result<(), Error> {
        match outcome {
            Ok(()) => { self.reclaim(room_id); Ok(()) }
            Err((error, Landed::No)) => { self.reclaim(room_id); Err(error) }
            Err((error, Landed::Maybe)) => Err(error),
        }
    }

    pub(super) fn buried(&self, room_id: &str) -> Result<bool, Error> {
        match archive::regular(&self.dir("tombstones")?.join(format!("{room_id}.json")), 4 << 10) {
            Ok(_) => Ok(true),
            Err(archive::Error::Missing) => Ok(false),
            Err(_) => Err(Error::Storage),
        }
    }

    pub(super) fn bury(&self, room: &Room, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        let body = serde_json::to_vec(&Tombstone { owner: room.owner.clone(), deleted_at_ms: self.now() }).map_err(|_| Error::Storage)?;
        write_atomic(&self.dir("tombstones")?, &format!("{}.json", room.id), &body, authorized)
    }
}
