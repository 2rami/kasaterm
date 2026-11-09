use super::*;

use base64::Engine;
use kasa_socket::shared_rooms::{CreateFork, ForkCheckpoint, ProjectSnapshot, PublishCheckpoint, SharedFork, SharedForkInfo, FORMAT_VERSION};

pub const MAX_CHECKPOINT: usize = 40 << 20;
const MAX_NATIVE: usize = 16 << 20;
pub const MAX_DISPLAY: usize = 8 << 20;
const MAX_PATCH: usize = 4 << 20;
const MAX_PROJECT_FILES: usize = 256;
const MAX_PROJECT_BYTES: usize = 8 << 20;
const MAX_OMISSIONS: usize = 64;
const MAX_FORKS: usize = 64;
const MAX_FORK_BYTES: u64 = 512 << 20;
const MAX_FORK_FILE: u64 = MAX_CHECKPOINT as u64 + (64 << 10);
const CLOCK_SKEW_MS: u64 = 10 * 60 * 1000;

const CREDENTIAL_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube", ".docker", ".codex"];
const CREDENTIAL_NAMES: &[&str] = &[
    ".env", ".netrc", ".npmrc", ".pypirc", ".git-credentials", ".credentials.json", "credentials", "credentials.json",
    "auth.json", "settings.local.json", "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519",
];
const CREDENTIAL_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx"];

/// The same rule decides what a capture may include and what a recipient may install.
pub fn installable_project_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default().to_ascii_lowercase();
    !path.is_empty() && path.len() <= 1024 && !path.starts_with('/') && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".."
            && !part.eq_ignore_ascii_case(".git") && !CREDENTIAL_DIRS.iter().any(|dir| part.eq_ignore_ascii_case(dir)))
        && !CREDENTIAL_NAMES.contains(&name.as_str()) && !name.starts_with(".env.")
        && !CREDENTIAL_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

fn ident(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

fn url_has_credentials(url: &str) -> bool {
    url.split_once("://").is_some_and(|(_, rest)| rest.split(['/', '?', '#']).next().is_some_and(|host| host.contains('@')))
}

fn native_lines(jsonl: &str) -> bool {
    jsonl.ends_with('\n') && jsonl.lines().all(|line| line.starts_with('{')
        && serde_json::from_str::<serde::de::IgnoredAny>(line).is_ok())
}

fn check_project(project: &ProjectSnapshot, now: u64) -> Result<(), Error> {
    if project.patch.len() > MAX_PATCH || project.files.len() > MAX_PROJECT_FILES { return Err(Error::Limit); }
    if project.repo_url.as_deref().is_some_and(|url| url.is_empty() || !text(url, 2048) || url_has_credentials(url))
        || project.commit.as_deref().is_some_and(|commit| !hex(commit, 40) && !hex(commit, 64))
        || project.captured_at_ms == 0 || project.captured_at_ms > now.saturating_add(CLOCK_SKEW_MS)
        || project.patch.contains('\0') {
        return Err(Error::Invalid);
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0usize;
    for file in &project.files {
        if !installable_project_path(&file.path) || !seen.insert(file.path.to_ascii_lowercase()) || !hex(&file.sha256, 64) {
            return Err(Error::Invalid);
        }
        if file.bytes_base64.len() > (MAX_PROJECT_BYTES / 3 + 1) * 4 { return Err(Error::Limit); }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&file.bytes_base64).map_err(|_| Error::Invalid)?;
        total += bytes.len();
        if total > MAX_PROJECT_BYTES { return Err(Error::Limit); }
        if digest(&bytes) != file.sha256 { return Err(Error::Invalid); }
    }
    Ok(())
}

/// Format version, provider, hash and size are fixed here; whether the native file resumes is the client's call.
fn check_checkpoint(checkpoint: &ForkCheckpoint, now: u64) -> Result<(), Error> {
    let native = &checkpoint.native;
    if native.jsonl.len() > MAX_NATIVE || checkpoint.display_raw.len() > MAX_DISPLAY { return Err(Error::Limit); }
    if checkpoint.format_version != FORMAT_VERSION || !valid_harness(&native.harness)
        || !ident(&native.parent_session_id, 128) || !ident(&native.boundary_id, 128)
        || !text(&native.boundary_label, 256) || native.native_version.is_empty() || !text(&native.native_version, 64)
        || !hex(&native.sha256, 64) || native.sha256 != digest(native.jsonl.as_bytes()) || !native_lines(&native.jsonl)
        || checkpoint.captured_at_ms == 0 || checkpoint.captured_at_ms > now.saturating_add(CLOCK_SKEW_MS)
        || checkpoint.omissions.len() > MAX_OMISSIONS
        || !checkpoint.omissions.iter().all(|o| !o.is_empty() && text(o, 256)) {
        return Err(Error::Invalid);
    }
    checkpoint.project.as_ref().map_or(Ok(()), |project| check_project(project, now))
}

fn fork_id(account: &str, request_id: &str) -> String {
    digest(format!("shared-fork\0{account}\0{request_id}").as_bytes())[..32].into()
}

impl Store {
    fn forks_dir(&self, account: &str) -> Result<PathBuf, Error> {
        if !valid_account(account) { return Err(Error::Invalid); }
        let dir = self.dir("forks")?.join(account);
        archive::private_dir(&dir)?;
        Ok(dir)
    }

    fn load_fork(&self, account: &str, id: &str) -> Result<SharedFork, Error> {
        if !hex(id, 32) { return Err(Error::Missing); }
        let fork: SharedFork = read_json(&self.forks_dir(account)?.join(format!("{id}.json")), MAX_FORK_FILE)?;
        if fork.info.id != id || check_checkpoint(&fork.checkpoint, u64::MAX).is_err() { return Err(Error::Storage); }
        Ok(fork)
    }

    /// Listing reads these small files so it never parses every stored checkpoint.
    fn write_fork_info(&self, dir: &Path, info: &SharedForkInfo, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        write_atomic(dir, &format!("{}.info", info.id), &serde_json::to_vec(info).map_err(|_| Error::Storage)?, authorized)
    }

    fn fork_usage(&self, dir: &Path) -> Result<(usize, u64), Error> {
        let (mut count, mut bytes) = (0, 0);
        for item in std::fs::read_dir(dir).map_err(|_| Error::Storage)? {
            let path = item.map_err(|_| Error::Storage)?.path();
            if path.extension().is_none_or(|e| e != "json") { continue; }
            count += 1;
            bytes += archive::regular(&path, MAX_FORK_FILE)?.len();
        }
        Ok((count, bytes))
    }

    /// The new checkpoint replaces the session's previous one; forks already made keep their own copies.
    /// Publishers race (an explicit publish and a periodic one), so a capture is taken only for the current
    /// generation, not ahead of the session, and strictly newer by (revision, capture time) than the stored one.
    pub fn checkpoint(&mut self, publisher: Publisher<'_>, room_id: &str, session_id: &str, request: PublishCheckpoint, authorized: &dyn Fn() -> bool) -> Result<CheckpointSummary, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let PublishCheckpoint { expected_generation, expected_revision, checkpoint } = request;
        let now = self.now();
        check_checkpoint(&checkpoint, now)?;
        let mut room = self.owned_room(publisher.account, room_id)?;
        let index = room.sessions.iter().position(|s| s.id == session_id).ok_or(Error::Missing)?;
        let session = &room.sessions[index];
        if session.publisher_machine_id != publisher.machine_id { return Err(Error::Forbidden); }
        if session.harness != checkpoint.native.harness { return Err(Error::Invalid); }
        let body = serde_json::to_vec(&checkpoint).map_err(|_| Error::Storage)?;
        if body.len() > MAX_CHECKPOINT { return Err(Error::Limit); }
        let sha256 = digest(&body);
        if session.generation != expected_generation || expected_revision > session.revision { return Err(Error::Conflict); }
        let old = session.checkpoint.clone();
        if let Some(old) = &old {
            // A resend after a lost response must not move the id recipients are choosing from.
            if old.sha256 == sha256 && old.bytes == body.len() as u64 && old.base_revision == expected_revision {
                if !authorized() { return Err(Error::Unauthorized); }
                return Ok(old.summary.clone());
            }
            if (expected_revision, checkpoint.captured_at_ms) <= (old.base_revision, old.summary.captured_at_ms) {
                return Err(Error::Conflict);
            }
        }
        let used = self.owner_usage(publisher.account)?;
        if used.saturating_sub(old.as_ref().map_or(0, |c| c.bytes)).saturating_add(body.len() as u64) > MAX_OWNER_BYTES {
            return Err(Error::Limit);
        }
        let native = &checkpoint.native;
        let summary = CheckpointSummary {
            id: new_id(), harness: native.harness.clone(), parent_session_id: native.parent_session_id.clone(),
            boundary_id: native.boundary_id.clone(), boundary_label: native.boundary_label.clone(),
            captured_at_ms: checkpoint.captured_at_ms, has_project: checkpoint.project.is_some(),
            omissions: checkpoint.omissions.clone(),
        };
        let dir = self.room_dir(room_id)?;
        // The body is unnamed until the manifest commits; reclaim removes it or the replaced one, never on a maybe.
        let outcome = match write_atomic(&dir, &format!("{}.checkpoint", summary.id), &body, authorized) {
            Err(error) => Err((error, Landed::No)),
            Ok(()) => {
                room.sessions[index].checkpoint = Some(Checkpoint {
                    summary: summary.clone(), bytes: body.len() as u64, sha256, base_generation: expected_generation, base_revision: expected_revision,
                });
                room.sessions[index].updated_at_ms = now;
                match room.touch(now) {
                    Err(error) => Err((error, Landed::No)),
                    Ok(()) => self.save_landed(&room, authorized),
                }
            }
        };
        self.settle(room_id, outcome)?;
        Ok(summary)
    }

    /// The id depends only on the recipient and `request_id`, so a retry finds the copy instead of making another.
    pub fn fork(&mut self, account: &str, room_id: &str, session_id: &str, request: CreateFork, authorized: &dyn Fn() -> bool) -> Result<SharedForkInfo, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !valid_request_id(&request.request_id) || !hex(&request.checkpoint_id, 32) { return Err(Error::Invalid); }
        let id = fork_id(account, &request.request_id);
        let dir = self.forks_dir(account)?;
        match self.load_fork(account, &id) {
            Ok(existing) => {
                let info = existing.info;
                let same = info.room_id == room_id && info.session_id == session_id && info.checkpoint_id == request.checkpoint_id;
                if !same { return Err(Error::Conflict); }
                if !authorized() { return Err(Error::Unauthorized); }
                return Ok(info);
            }
            Err(Error::Missing) => {}
            Err(error) => return Err(error),
        }
        let room = self.member_room(account, room_id)?;
        if !room.allow_fork { return Err(Error::ForkDisabled); }
        let session = room.sessions.iter().find(|s| s.id == session_id).ok_or(Error::Missing)?;
        let stored = session.checkpoint.as_ref().ok_or(Error::Missing)?;
        if stored.summary.id != request.checkpoint_id { return Err(Error::Conflict); }
        let path = self.room_dir(room_id)?.join(format!("{}.checkpoint", stored.summary.id));
        let mut body = Vec::new();
        open_read(&path, MAX_CHECKPOINT as u64).map_err(|_| Error::Storage)?
            .read_to_end(&mut body).map_err(|_| Error::Storage)?;
        if body.len() as u64 != stored.bytes || digest(&body) != stored.sha256 { return Err(Error::Storage); }
        let checkpoint: ForkCheckpoint = serde_json::from_slice(&body).map_err(|_| Error::Storage)?;
        let (count, used) = self.fork_usage(&dir)?;
        if count >= MAX_FORKS || used.saturating_add(body.len() as u64) > MAX_FORK_BYTES { return Err(Error::Limit); }
        let fork = SharedFork {
            info: SharedForkInfo {
                id: id.clone(), room_id: room_id.into(), session_id: session_id.into(),
                checkpoint_id: request.checkpoint_id, created_at_ms: self.now(),
            },
            checkpoint,
        };
        write_atomic(&dir, &format!("{id}.json"), &serde_json::to_vec(&fork).map_err(|_| Error::Storage)?, authorized)?;
        // The copy is committed; a missing listing entry is rebuilt from it by `forks`.
        let _ = self.write_fork_info(&dir, &fork.info, &|| true);
        Ok(fork.info)
    }

    /// Lets a recipient rediscover an interrupted import and reclaim space; never lists other accounts' copies.
    /// `<id>.json` is the copy itself; `<id>.info` is a cache rebuilt from it when missing or unreadable.
    pub fn forks(&self, account: &str, authorized: &dyn Fn() -> bool) -> Result<Vec<SharedForkInfo>, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let dir = self.forks_dir(account)?;
        let mut forks = Vec::new();
        for item in std::fs::read_dir(&dir).map_err(|_| Error::Storage)? {
            let path = item.map_err(|_| Error::Storage)?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if let Some(id) = name.strip_suffix(".info").filter(|id| hex(id, 32)) {
                if !dir.join(format!("{id}.json")).exists() { let _ = std::fs::remove_file(&path); }
                continue;
            }
            let Some(id) = name.strip_suffix(".json").filter(|id| hex(id, 32)) else { continue };
            let cached = read_json::<SharedForkInfo>(&dir.join(format!("{id}.info")), 4 << 10).ok().filter(|info| info.id == id
                && hex(&info.room_id, 32) && hex(&info.session_id, 32) && hex(&info.checkpoint_id, 32));
            let info = match cached {
                Some(info) => info,
                None => {
                    let info = self.load_fork(account, id)?.info;
                    let _ = self.write_fork_info(&dir, &info, &|| true);
                    info
                }
            };
            forks.push(info);
            if forks.len() > MAX_FORKS { return Err(Error::Storage); }
        }
        forks.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms).then_with(|| a.id.cmp(&b.id)));
        if !authorized() { return Err(Error::Unauthorized); }
        Ok(forks)
    }

    pub fn fork_copy(&self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<SharedFork, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let fork = self.load_fork(account, id)?;
        if !authorized() { return Err(Error::Unauthorized); }
        Ok(fork)
    }

    /// Size of the stored copy, so a caller can reserve memory before reading it.
    pub fn fork_len(&self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<u64, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !hex(id, 32) { return Err(Error::Missing); }
        Ok(archive::regular(&self.forks_dir(account)?.join(format!("{id}.json")), MAX_FORK_FILE)?.len())
    }

    /// The validated stored JSON as is, so the download is not serialized a second time.
    pub fn fork_raw(&self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<Vec<u8>, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !hex(id, 32) { return Err(Error::Missing); }
        let mut raw = Vec::new();
        open_read(&self.forks_dir(account)?.join(format!("{id}.json")), MAX_FORK_FILE)?.read_to_end(&mut raw).map_err(|_| Error::Storage)?;
        let fork: SharedFork = serde_json::from_slice(&raw).map_err(|_| Error::Storage)?;
        if fork.info.id != id || check_checkpoint(&fork.checkpoint, u64::MAX).is_err() { return Err(Error::Storage); }
        if !authorized() { return Err(Error::Unauthorized); }
        Ok(raw)
    }

    pub fn delete_fork(&mut self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !hex(id, 32) { return Err(Error::Missing); }
        let dir = self.forks_dir(account)?;
        let path = dir.join(format!("{id}.json"));
        archive::regular(&path, MAX_FORK_FILE)?;
        if !authorized() { return Err(Error::Unauthorized); }
        std::fs::remove_file(&path).map_err(|_| Error::Storage)?;
        let _ = std::fs::remove_file(dir.join(format!("{id}.info")));
        sync_dir(&dir)
    }
}
