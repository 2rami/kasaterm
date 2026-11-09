//! Account archives outlive the desktop connection; byte commits never expose a partial append.
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod sync;
pub use sync::spawn;
#[cfg(test)]
mod tests;

pub const MAX_APPEND: usize = 4 << 20;
pub const MAX_READ: usize = 256 << 10;
pub const MAX_RECORD: u64 = 32 << 20;
const MAX_ACCOUNT: u64 = 512 << 20;
const MAX_RECORDS: usize = 2048;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub machine_id: String,
    pub machine_name: String,
    pub source: String,
    pub surface_key: String,
    pub title: String,
    pub name: String,
    pub harness: String,
    pub cwd: String,
    pub generation: String,
    pub revision: u64,
    pub cursor: u64,
    pub bytes: u64,
    pub updated_at: u64,
    pub ready: bool,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Append {
    pub source: String,
    pub surface_key: String,
    pub title: String,
    pub name: String,
    pub harness: String,
    pub cwd: String,
    pub expected_revision: u64,
    pub from: u64,
    pub next: u64,
    pub reset: bool,
    pub caught_up: bool,
    pub raw: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub raw: String,
    pub next: u64,
    pub end: u64,
    pub generation: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error { Invalid, Unauthorized, Missing, Conflict, Limit, Storage, Busy, Disabled }

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_archive", Self::Unauthorized => "unauthorized", Self::Missing => "archive_missing",
            Self::Conflict => "archive_changed", Self::Limit => "archive_limit", Self::Storage => "archive_storage",
            Self::Busy => "archive_busy",
            Self::Disabled => "archive_disabled",
        }
    }
}

pub(crate) fn hex(value: &str, size: usize) -> bool {
    value.len() == size && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn text(value: &str, max: usize) -> bool { value.len() <= max && !value.chars().any(char::is_control) }

pub fn id_for(machine: &str, source: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{machine}\0{source}").as_bytes()))
}

pub(crate) fn private_dir(path: &Path) -> Result<(), Error> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err(Error::Storage); }
    }
    std::fs::create_dir_all(path).map_err(|_| Error::Storage)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|_| Error::Storage)?;
    }
    Ok(())
}

pub(crate) fn regular(path: &Path, max: u64) -> Result<std::fs::Metadata, Error> {
    let meta = std::fs::symlink_metadata(path).map_err(|e|
        if e.kind() == std::io::ErrorKind::NotFound { Error::Missing } else { Error::Storage })?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max { return Err(Error::Storage); }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 { return Err(Error::Storage); }
    }
    Ok(meta)
}

/// Owned by the gateway's single archive worker, not shared under an I/O mutex.
pub struct Store { root: PathBuf }

impl Store {
    pub fn open(root: PathBuf) -> Result<Self, Error> { private_dir(&root)?; Ok(Self { root }) }

    pub fn enabled(&self, account: &str) -> Result<bool, Error> {
        let path = self.account_dir(account)?.join("archive.prefs");
        match regular(&path, 1024) {
            Ok(_) => {
                let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|_| Error::Storage)?).map_err(|_| Error::Storage)?;
                value["enabled"].as_bool().ok_or(Error::Storage)
            }
            Err(Error::Missing) => Ok(false), Err(error) => Err(error),
        }
    }

    pub fn set_enabled(&mut self, account: &str, enabled: bool, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        let path = self.account_dir(account)?.join("archive.prefs");
        if path.exists() { regular(&path, 1024)?; }
        if !authorized() { return Err(Error::Unauthorized); }
        self.write_json(account, "archive.prefs", &serde_json::json!({"enabled":enabled}), authorized)
    }

    fn account_dir(&self, account: &str) -> Result<PathBuf, Error> {
        if !kasa_identity::relay_auth::valid_account_name(account) { return Err(Error::Invalid); }
        private_dir(&self.root)?;
        let path = self.root.join(account);
        private_dir(&path)?;
        Ok(path)
    }

    fn load(&self, account: &str, id: &str) -> Result<Entry, Error> {
        if !hex(id, 64) { return Err(Error::Invalid); }
        let path = self.account_dir(account)?.join(format!("{id}.json"));
        regular(&path, 32 << 10)?;
        let entry: Entry = serde_json::from_slice(&std::fs::read(path).map_err(|_| Error::Storage)?).map_err(|_| Error::Storage)?;
        if entry.id != id || !hex(&entry.generation, 32) || entry.bytes > MAX_RECORD
            || id_for(&entry.machine_id, &entry.source) != id { return Err(Error::Storage); }
        Ok(entry)
    }

    pub fn list(&self, account: &str) -> Result<Vec<Entry>, Error> {
        let mut entries = Vec::new();
        for path in std::fs::read_dir(self.account_dir(account)?).map_err(|_| Error::Storage)? {
            let path = path.map_err(|_| Error::Storage)?.path();
            if path.extension().is_none_or(|e| e != "json") { continue; }
            let id = path.file_stem().and_then(|s| s.to_str()).ok_or(Error::Storage)?;
            entries.push(self.load(account, id)?);
            if entries.len() > MAX_RECORDS { return Err(Error::Limit); }
        }
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        for entry in entries.iter().filter(|e| !e.deleted) {
            let path = self.data(account, entry)?;
            if path.exists() {
                let meta = regular(&path, MAX_RECORD + MAX_APPEND as u64)?;
                if meta.len() < entry.bytes { return Err(Error::Storage); }
                if meta.len() > entry.bytes {
                    let mut options = std::fs::OpenOptions::new(); options.write(true);
                    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NOFOLLOW); }
                    let file = options.open(&path).map_err(|_| Error::Storage)?;
                    file.set_len(entry.bytes).and_then(|()| file.sync_all()).map_err(|_| Error::Storage)?;
                }
            } else if entry.bytes != 0 { return Err(Error::Storage); }
        }
        let live: std::collections::HashSet<_> = entries.iter().filter(|e| !e.deleted)
            .map(|e| format!("{}-{}.jsonl", e.id, e.generation)).collect();
        // Only committed generations survive recovery; failed resets must not consume unaccounted space.
        for item in std::fs::read_dir(self.account_dir(account)?).map_err(|_| Error::Storage)? {
            let path = item.map_err(|_| Error::Storage)?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            let Some(stem) = name.strip_suffix(".jsonl") else { continue };
            let Some((id, generation)) = stem.split_once('-') else { continue };
            if hex(id, 64) && hex(generation, 32) && !live.contains(name) {
                regular(&path, MAX_RECORD + MAX_APPEND as u64)?;
                std::fs::remove_file(path).map_err(|_| Error::Storage)?;
            }
        }
        Ok(entries)
    }

    fn data(&self, account: &str, entry: &Entry) -> Result<PathBuf, Error> {
        if !hex(&entry.id, 64) || !hex(&entry.generation, 32) { return Err(Error::Storage); }
        Ok(self.account_dir(account)?.join(format!("{}-{}.jsonl", entry.id, entry.generation)))
    }

    fn write_json(&self, account: &str, name: &str, value: &impl Serialize, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        let dir = self.account_dir(account)?;
        let path = dir.join(name);
        if path.exists() { regular(&path, 32 << 10)?; }
        let temp = dir.join(format!(".{}-{}.tmp", name, uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            let mut file = options.open(&temp).map_err(|_| Error::Storage)?;
            file.write_all(&serde_json::to_vec(value).map_err(|_| Error::Storage)?).map_err(|_| Error::Storage)?;
            file.sync_all().map_err(|_| Error::Storage)?;
            if !authorized() { return Err(Error::Unauthorized); }
            std::fs::rename(&temp, &path).map_err(|_| Error::Storage)?;
            #[cfg(unix)] std::fs::File::open(&dir).and_then(|d| d.sync_all()).map_err(|_| Error::Storage)?;
            Ok(())
        })();
        if result.is_err() { let _ = std::fs::remove_file(temp); }
        result
    }

    fn commit(&self, account: &str, entry: &Entry, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        self.write_json(account, &format!("{}.json", entry.id), entry, authorized)
    }

    pub fn append(&mut self, account: &str, machine: &str, machine_name: &str, update: Append, authorized: &dyn Fn() -> bool) -> Result<Entry, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !self.enabled(account)? { return Err(Error::Disabled); }
        if machine.is_empty() || !text(machine, 128) || !text(machine_name, 256) || update.source.is_empty()
            || !text(&update.source, 512) || !text(&update.surface_key, 128) || !text(&update.title, 512)
            || !text(&update.name, 128) || !text(&update.cwd, 4096) || !matches!(update.harness.as_str(), "claude" | "codex")
            || update.raw.len() > MAX_APPEND || update.next < update.from || (!update.raw.is_empty() && !update.raw.ends_with('\n'))
            || (update.reset && update.from != 0) || (!update.raw.is_empty() && update.next == update.from) {
            return Err(Error::Invalid);
        }
        let id = id_for(machine, &update.source);
        let previous = match self.load(account, &id) { Ok(e) => Some(e), Err(Error::Missing) => None, Err(e) => return Err(e) };
        if previous.as_ref().is_some_and(|e| e.deleted) { return Err(Error::Missing); }
        if previous.as_ref().map_or(0, |e| e.revision) != update.expected_revision
            || (!update.reset && previous.as_ref().map_or(0, |e| e.cursor) != update.from) { return Err(Error::Conflict); }
        if previous.is_none() && !update.reset { return Err(Error::Invalid); }
        let mut entry = previous.clone().unwrap_or_else(|| Entry {
            id, machine_id: machine.into(), machine_name: machine_name.into(), source: update.source.clone(),
            surface_key: String::new(), title: String::new(), name: String::new(), harness: update.harness.clone(), cwd: String::new(),
            generation: uuid::Uuid::new_v4().simple().to_string(), revision: 0, cursor: 0, bytes: 0, updated_at: 0, ready: false, deleted: false,
        });
        let old_data = previous.as_ref().map(|e| self.data(account, e)).transpose()?;
        if update.reset { entry.generation = uuid::Uuid::new_v4().simple().to_string(); entry.bytes = 0; entry.ready = false; }
        let total = entry.bytes.checked_add(update.raw.len() as u64).ok_or(Error::Limit)?;
        let listed = self.list(account)?;
        let used: u64 = listed.iter().filter(|e| e.id != entry.id && !e.deleted).map(|e| e.bytes).sum();
        if total > MAX_RECORD || used.saturating_add(total) > MAX_ACCOUNT || (previous.is_none() && listed.len() >= MAX_RECORDS) {
            return Err(Error::Limit);
        }
        let data = self.data(account, &entry)?;
        if data.exists() {
            let meta = regular(&data, MAX_RECORD + MAX_APPEND as u64)?;
            if meta.len() < entry.bytes { return Err(Error::Storage); }
        } else if entry.bytes != 0 { return Err(Error::Storage); }
        if !authorized() { return Err(Error::Unauthorized); }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600).custom_flags(libc::O_NOFOLLOW); }
        let mut file = options.open(&data).map_err(|_| Error::Storage)?;
        // A crash after writing bytes but before committing metadata leaves only an invisible tail.
        file.set_len(entry.bytes).map_err(|_| Error::Storage)?;
        file.seek(SeekFrom::Start(entry.bytes)).map_err(|_| Error::Storage)?;
        file.write_all(update.raw.as_bytes()).map_err(|_| Error::Storage)?;
        file.sync_all().map_err(|_| Error::Storage)?;
        if !authorized() { return Err(Error::Unauthorized); }
        entry.bytes = total;
        entry.cursor = update.next;
        entry.revision = entry.revision.checked_add(1).ok_or(Error::Limit)?;
        entry.updated_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        entry.ready |= update.caught_up;
        entry.title = update.title;
        entry.name = update.name;
        entry.surface_key = update.surface_key;
        entry.cwd = update.cwd;
        entry.machine_name = machine_name.into();
        self.commit(account, &entry, authorized)?;
        if let Some(old) = old_data.filter(|old| old != &data) { let _ = std::fs::remove_file(old); }
        Ok(entry)
    }

    pub fn read(&self, account: &str, id: &str, generation: &str, offset: u64, end: u64) -> Result<Chunk, Error> {
        let entry = self.load(account, id)?;
        if entry.deleted { return Err(Error::Missing); }
        if entry.generation != generation || end > entry.bytes || offset > end { return Err(Error::Conflict); }
        let path = self.data(account, &entry)?;
        if regular(&path, MAX_RECORD + MAX_APPEND as u64)?.len() < entry.bytes { return Err(Error::Storage); }
        let mut file = std::fs::File::open(path).map_err(|_| Error::Storage)?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| Error::Storage)?;
        let count = (end - offset).min(MAX_READ as u64) as usize;
        let mut bytes = vec![0; count];
        file.read_exact(&mut bytes).map_err(|_| Error::Storage)?;
        let valid = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(e) if e.error_len().is_none() && offset + (bytes.len() as u64) < end => e.valid_up_to(),
            Err(_) => return Err(Error::Invalid),
        };
        bytes.truncate(valid);
        let raw = String::from_utf8(bytes).map_err(|_| Error::Storage)?;
        Ok(Chunk { next: offset + raw.len() as u64, raw, end, generation: entry.generation })
    }

    pub fn delete(&mut self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        let mut entry = self.load(account, id)?;
        if !authorized() { return Err(Error::Unauthorized); }
        let path = self.data(account, &entry)?;
        entry.deleted = true;
        entry.bytes = 0;
        entry.ready = false;
        entry.revision = entry.revision.checked_add(1).ok_or(Error::Limit)?;
        self.commit(account, &entry, authorized)?;
        if path.exists() { regular(&path, MAX_RECORD + MAX_APPEND as u64)?; std::fs::remove_file(path).map_err(|_| Error::Storage)?; }
        Ok(())
    }

    pub fn restore(&mut self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<Entry, Error> {
        let mut entry = self.load(account, id)?;
        if !authorized() { return Err(Error::Unauthorized); }
        if entry.deleted {
            entry.deleted = false;
            entry.cursor = 0;
            entry.bytes = 0;
            entry.ready = false;
            entry.generation = uuid::Uuid::new_v4().simple().to_string();
            entry.revision = entry.revision.checked_add(1).ok_or(Error::Limit)?;
            self.commit(account, &entry, authorized)?;
        }
        Ok(entry)
    }
}
