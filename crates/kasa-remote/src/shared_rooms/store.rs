//! One manifest (`room.json`) per room is the source of truth. Content and checkpoint files are synced before
//! the manifest names them, so a crash leaves only unreferenced bytes, never a torn view. The gateway owns the
//! store from a single worker thread, which also serializes membership changes against reads and writes.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use kasa_identity::relay_auth::valid_account_name as valid_account;
use kasa_socket::shared_rooms::{
    CheckpointSummary, CreateInvite, CreateRoom, InviteSummary, SharedInvite, SharedMember, SharedRoom, SharedSession,
    UpdateRoom,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::conversation_archive::{self as archive, hex, text};

mod content;
mod fork;
mod recovery;
#[cfg(test)]
mod tests;

use recovery::Landed;

pub use content::{Publisher, ReadRange};
pub use fork::{installable_project_path, MAX_CHECKPOINT, MAX_DISPLAY};

pub const MAX_APPEND: usize = archive::MAX_APPEND;
pub const MAX_READ: usize = archive::MAX_READ;
pub const MAX_SESSION_BYTES: u64 = 32 << 20;
const MAX_OWNER_BYTES: u64 = 512 << 20;
const MAX_TITLE: usize = 200;
const MAX_MEMBERS: usize = 32;
const MAX_INVITES: usize = 16;
const MAX_SESSIONS: usize = 32;
const MAX_OWNED: usize = 32;
const MAX_JOINED: usize = 128;
/// Index entries are hints that may outlive a membership until the next listing prunes them.
const MAX_INDEX: usize = MAX_JOINED * 2;
const MAX_MANIFEST: u64 = 256 << 10;
/// Removals older than the longest invite lifetime cannot be matched by any live invite.
const MAX_REMOVED: usize = 64;
const ROOM_VERSION: u32 = 1;
const INVITE_DEFAULT_SECS: u64 = 24 * 3600;
const INVITE_MIN_SECS: u64 = 60;
const INVITE_MAX_SECS: u64 = 7 * 24 * 3600;
const INVITE_MAX_USES: u32 = 32;
const TOKEN_PREFIX: &str = "ksr1.";

#[derive(Debug, PartialEq, Eq)]
pub enum Error { Invalid, Unauthorized, Forbidden, ForkDisabled, Missing, Conflict, Gone, Limit, Storage, Busy }

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "shared_invalid", Self::Unauthorized => "unauthorized", Self::Forbidden => "shared_forbidden",
            Self::ForkDisabled => "fork_disabled", Self::Missing => "shared_missing", Self::Conflict => "shared_conflict",
            Self::Gone => "invite_unavailable", Self::Limit => "shared_limit", Self::Storage => "shared_storage",
            Self::Busy => "shared_busy",
        }
    }
}

impl From<archive::Error> for Error {
    fn from(error: archive::Error) -> Self { if error == archive::Error::Missing { Self::Missing } else { Self::Storage } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Role { Owner, Member }

impl Role {
    fn name(self) -> &'static str { match self { Self::Owner => "owner", Self::Member => "member" } }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member { account: String, role: Role, joined_at_ms: u64 }

/// Only the token digest is kept; the token itself exists in the creation response alone.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    id: String,
    token_sha256: String,
    created_at_ms: u64,
    expires_at_ms: u64,
    max_uses: u32,
    uses: u32,
    /// Room revision when issued; revisions only grow, unlike wall-clock time.
    #[serde(default)]
    created_revision: u64,
}

/// An owner's removal, so invites issued before it cannot bring the account back.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Removal { account: String, revision: u64, at_ms: u64 }

/// `base_*` is the content state the publisher captured against; a later capture must be strictly newer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint { summary: CheckpointSummary, bytes: u64, sha256: String, base_generation: String, base_revision: u64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    id: String,
    source_key: String,
    title: String,
    harness: String,
    publisher_machine_id: String,
    revision: u64,
    generation: String,
    bytes: u64,
    /// The publisher's source offset, separate from stored bytes like the conversation archive cursor.
    cursor: u64,
    updated_at_ms: u64,
    caught_up: bool,
    checkpoint: Option<Checkpoint>,
}

/// What the creating request asked for, so a retry after a lost response finds the same room even after a rename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin { request_id: String, title: String, allow_fork: bool }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Room {
    version: u32,
    id: String,
    title: String,
    owner: String,
    allow_fork: bool,
    /// Room-level changes only; content appends move `updated_at_ms` so owner edits are not starved by uploads.
    revision: u64,
    created_at_ms: u64,
    updated_at_ms: u64,
    members: Vec<Member>,
    invites: Vec<Invite>,
    sessions: Vec<Session>,
    #[serde(default)]
    origin: Option<Origin>,
    #[serde(default)]
    removed: Vec<Removal>,
}

impl Room {
    pub fn id(&self) -> &str { &self.id }

    pub fn owner(&self) -> &str { &self.owner }

    fn role(&self, account: &str) -> Option<Role> {
        self.members.iter().find(|m| m.account == account).map(|m| m.role)
    }

    /// Invites and publisher cursors are the owner's alone.
    pub fn view(&self, viewer: &str, name: &dyn Fn(&str) -> String) -> SharedRoom {
        let full = viewer == self.owner;
        SharedRoom {
            id: self.id.clone(), title: self.title.clone(), owner_account: self.owner.clone(), owner_name: name(&self.owner),
            my_role: self.role(viewer).map_or("", Role::name).into(), allow_fork: self.allow_fork,
            revision: self.revision, updated_at_ms: self.updated_at_ms,
            members: self.members.iter().map(|m| SharedMember {
                account: m.account.clone(), display_name: name(&m.account), role: m.role.name().into(),
            }).collect(),
            sessions: self.sessions.iter().map(|s| s.view(full)).collect(),
            invites: if full {
                self.invites.iter().map(|i| InviteSummary {
                    id: i.id.clone(), expires_at_ms: i.expires_at_ms, max_uses: i.max_uses, uses: i.uses,
                }).collect()
            } else { Vec::new() },
        }
    }

    fn touch(&mut self, now: u64) -> Result<(), Error> {
        self.revision = self.revision.checked_add(1).ok_or(Error::Limit)?;
        self.updated_at_ms = self.updated_at_ms.max(now);
        Ok(())
    }

    fn prune_invites(&mut self, now: u64) { self.invites.retain(|i| i.expires_at_ms > now); }

    fn record_removal(&mut self, account: &str, now: u64) {
        let horizon = now.saturating_sub(INVITE_MAX_SECS * 1000);
        self.removed.retain(|r| r.at_ms > horizon && r.account != account);
        self.removed.push(Removal { account: account.into(), revision: self.revision, at_ms: now });
        while self.removed.len() > MAX_REMOVED {
            // Forgetting a removal is only safe once no invite issued before it remains.
            let forgotten = self.removed.remove(0);
            self.invites.retain(|i| i.created_revision > forgotten.revision);
        }
    }

    fn removed_since(&self, account: &str, invite: &Invite) -> bool {
        self.removed.iter().any(|r| r.account == account && invite.created_revision <= r.revision)
    }

    fn valid(&self, id: &str) -> bool {
        let mut accounts = std::collections::HashSet::new();
        let mut invites = std::collections::HashSet::new();
        let mut sessions = std::collections::HashSet::new();
        self.version == ROOM_VERSION && self.id == id && hex(&self.id, 32) && valid_title(&self.title)
            && (1..=MAX_MEMBERS).contains(&self.members.len())
            && self.members.iter().all(|m| valid_account(&m.account) && accounts.insert(m.account.as_str())
                && (m.role == Role::Owner) == (m.account == self.owner))
            && accounts.contains(self.owner.as_str())
            && self.invites.len() <= MAX_INVITES
            && self.invites.iter().all(|i| hex(&i.id, 32) && hex(&i.token_sha256, 64) && invites.insert(i.id.as_str())
                && (1..=INVITE_MAX_USES).contains(&i.max_uses) && i.uses < i.max_uses)
            && self.sessions.len() <= MAX_SESSIONS
            && self.sessions.iter().all(|s| s.valid() && sessions.insert(s.id.as_str()))
            && self.origin.as_ref().is_none_or(|o| valid_request_id(&o.request_id) && valid_title(&o.title))
            && self.removed.len() <= MAX_REMOVED && self.removed.iter().all(|r| valid_account(&r.account))
    }
}

impl Session {
    /// Members never see the publisher's local source key, machine id or source cursor.
    fn view(&self, full: bool) -> SharedSession {
        SharedSession {
            id: self.id.clone(),
            source_key: if full { self.source_key.clone() } else { String::new() },
            title: self.title.clone(), harness: self.harness.clone(),
            publisher_machine_id: if full { self.publisher_machine_id.clone() } else { String::new() },
            revision: self.revision, generation: self.generation.clone(), bytes: self.bytes,
            cursor: if full { self.cursor } else { 0 }, updated_at_ms: self.updated_at_ms, caught_up: self.caught_up,
            checkpoint: self.checkpoint.as_ref().map(|c| c.summary.clone()),
        }
    }

    fn valid(&self) -> bool {
        hex(&self.id, 32) && hex(&self.generation, 32) && self.bytes <= MAX_SESSION_BYTES && valid_harness(&self.harness)
            && valid_source(&self.source_key) && text(&self.title, content::MAX_SESSION_TITLE)
            && valid_machine(&self.publisher_machine_id)
            && self.checkpoint.as_ref().is_none_or(|c| hex(&c.summary.id, 32) && hex(&c.sha256, 64)
                && c.bytes <= MAX_CHECKPOINT as u64 && c.summary.harness == self.harness
                && c.base_generation == self.generation && c.base_revision <= self.revision)
    }
}

fn valid_title(title: &str) -> bool { !title.is_empty() && title.trim() == title && text(title, MAX_TITLE) }

fn clean_title(title: &str) -> Result<String, Error> {
    let title = title.trim();
    if valid_title(title) { Ok(title.into()) } else { Err(Error::Invalid) }
}

fn valid_request_id(id: &str) -> bool {
    (8..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_harness(harness: &str) -> bool { matches!(harness, "claude" | "codex") }

fn valid_machine(machine: &str) -> bool { !machine.is_empty() && text(machine, 128) }

fn valid_source(source: &str) -> bool { !source.is_empty() && text(source, content::MAX_SOURCE) }

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn new_id() -> String { uuid::Uuid::new_v4().simple().to_string() }

fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

fn random_hex(len: usize) -> Result<String, Error> {
    use ring::rand::SecureRandom;
    let mut bytes = vec![0u8; len];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| Error::Storage)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn parse_token(token: &str) -> Option<(&str, &str)> {
    let mut parts = token.strip_prefix(TOKEN_PREFIX)?.split('.');
    let (room, invite, secret) = (parts.next()?, parts.next()?, parts.next()?);
    (parts.next().is_none() && hex(room, 32) && hex(invite, 32) && hex(secret, 32)).then_some((room, invite))
}

fn open_read(path: &Path, max: u64) -> Result<std::fs::File, Error> {
    archive::regular(path, max)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NOFOLLOW); }
    options.open(path).map_err(|_| Error::Storage)
}

fn read_json<T: DeserializeOwned>(path: &Path, max: u64) -> Result<T, Error> {
    let mut bytes = Vec::new();
    open_read(path, max)?.take(max + 1).read_to_end(&mut bytes).map_err(|_| Error::Storage)?;
    if bytes.len() as u64 > max { return Err(Error::Storage); }
    serde_json::from_slice(&bytes).map_err(|_| Error::Storage)
}

fn sync_dir(dir: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    std::fs::File::open(dir).and_then(|d| d.sync_all()).map_err(|_| Error::Storage)?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Readers see either the old or the new file; the credential is checked again right before the rename.
/// A failure after the rename is `Landed::Maybe`: a crash may keep either file, so nothing may be deleted on it.
fn write_atomic_landed(dir: &Path, name: &str, body: &[u8], authorized: &dyn Fn() -> bool) -> Result<(), (Error, Landed)> {
    let path = dir.join(name);
    if std::fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_file()) { return Err((Error::Storage, Landed::No)); }
    let temp = dir.join(format!(".{name}-{}.tmp", new_id()));
    let renamed = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&temp).map_err(|_| Error::Storage)?;
        file.write_all(body).and_then(|()| file.sync_all()).map_err(|_| Error::Storage)?;
        if !authorized() { return Err(Error::Unauthorized); }
        std::fs::rename(&temp, &path).map_err(|_| Error::Storage)
    })();
    if let Err(error) = renamed {
        let _ = std::fs::remove_file(&temp);
        return Err((error, Landed::No));
    }
    sync_dir(dir).map_err(|error| (error, Landed::Maybe))
}

fn write_atomic(dir: &Path, name: &str, body: &[u8], authorized: &dyn Fn() -> bool) -> Result<(), Error> {
    write_atomic_landed(dir, name, body, authorized).map_err(|(error, _)| error)
}

fn remove_tree(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => { let _ = std::fs::remove_dir_all(path); }
        Ok(_) => { let _ = std::fs::remove_file(path); }
        Err(_) => {}
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index { rooms: Vec<String> }

pub struct Store { root: PathBuf, clock: Box<dyn Fn() -> u64 + Send> }

impl Store {
    pub fn open(root: PathBuf) -> Result<Self, Error> { Self::with_clock(root, Box::new(now_ms)) }

    pub fn with_clock(root: PathBuf, clock: Box<dyn Fn() -> u64 + Send>) -> Result<Self, Error> {
        archive::private_dir(&root)?;
        let store = Self { root, clock };
        store.recover()?;
        Ok(store)
    }

    fn now(&self) -> u64 { (self.clock)() }

    fn dir(&self, name: &str) -> Result<PathBuf, Error> {
        let path = self.root.join(name);
        archive::private_dir(&path)?;
        Ok(path)
    }

    fn room_dir(&self, id: &str) -> Result<PathBuf, Error> {
        if !hex(id, 32) { return Err(Error::Missing); }
        let path = self.dir("rooms")?.join(id);
        let meta = std::fs::symlink_metadata(&path).map_err(|e|
            if e.kind() == std::io::ErrorKind::NotFound { Error::Missing } else { Error::Storage })?;
        if !meta.is_dir() { return Err(Error::Storage); }
        Ok(path)
    }

    fn load(&self, id: &str) -> Result<Room, Error> {
        let room: Room = read_json(&self.room_dir(id)?.join("room.json"), MAX_MANIFEST)?;
        if !room.valid(id) { return Err(Error::Storage); }
        Ok(room)
    }

    /// Non-members get the same answer as for a room that does not exist.
    fn member_room(&self, account: &str, id: &str) -> Result<Room, Error> {
        let room = self.load(id)?;
        if room.role(account).is_none() { return Err(Error::Missing); }
        Ok(room)
    }

    fn owned_room(&self, account: &str, id: &str) -> Result<Room, Error> {
        let room = self.member_room(account, id)?;
        if room.owner != account { return Err(Error::Forbidden); }
        Ok(room)
    }

    fn save_landed(&self, room: &Room, authorized: &dyn Fn() -> bool) -> Result<(), (Error, Landed)> {
        let early = |error| (error, Landed::No);
        if !room.valid(&room.id) { return Err(early(Error::Invalid)); }
        let body = serde_json::to_vec(room).map_err(|_| early(Error::Storage))?;
        if body.len() as u64 > MAX_MANIFEST { return Err(early(Error::Limit)); }
        write_atomic_landed(&self.room_dir(&room.id).map_err(early)?, "room.json", &body, authorized)
    }

    fn save(&self, room: &Room, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        self.save_landed(room, authorized).map_err(|(error, _)| error)
    }

    fn index_path(&self, account: &str) -> Result<PathBuf, Error> {
        if !valid_account(account) { return Err(Error::Invalid); }
        Ok(self.dir("accounts")?.join(format!("{account}.json")))
    }

    fn index(&self, account: &str) -> Result<Index, Error> {
        match read_json::<Index>(&self.index_path(account)?, 64 << 10) {
            Ok(index) if index.rooms.len() <= MAX_INDEX && index.rooms.iter().all(|id| hex(id, 32)) => Ok(index),
            Ok(_) => Err(Error::Storage),
            Err(Error::Missing) => Ok(Index::default()),
            Err(error) => Err(error),
        }
    }

    fn write_index(&self, account: &str, index: &Index, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        let body = serde_json::to_vec(index).map_err(|_| Error::Storage)?;
        write_atomic(&self.dir("accounts")?, &format!("{account}.json"), &body, authorized)
    }

    /// Written before the membership commit so the index is always a superset of the truth in `room.json`.
    fn add_index(&self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        let mut index = self.index(account)?;
        if index.rooms.iter().any(|room| room == id) { return Ok(()); }
        if index.rooms.len() >= MAX_INDEX { return Err(Error::Limit); }
        index.rooms.push(id.into());
        self.write_index(account, &index, authorized)
    }

    /// Best effort: a stale hint is pruned by the next listing.
    fn drop_index(&self, account: &str, ids: &[&str]) {
        let Ok(mut index) = self.index(account) else { return };
        let before = index.rooms.len();
        index.rooms.retain(|room| !ids.contains(&room.as_str()));
        if index.rooms.len() != before { let _ = self.write_index(account, &index, &|| true); }
    }

    pub fn list(&self, account: &str) -> Result<Vec<Room>, Error> {
        let index = self.index(account)?;
        let mut rooms = Vec::new();
        let mut stale = Vec::new();
        for id in &index.rooms {
            match self.load(id) {
                Ok(room) if room.role(account).is_some() => rooms.push(room),
                Ok(_) | Err(Error::Missing) => stale.push(id.as_str()),
                Err(error) => return Err(error),
            }
        }
        if !stale.is_empty() { self.drop_index(account, &stale); }
        rooms.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms).then_with(|| a.id.cmp(&b.id)));
        Ok(rooms)
    }

    pub fn room(&self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<Room, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let room = self.member_room(account, id)?;
        if !authorized() { return Err(Error::Unauthorized); }
        Ok(room)
    }

    /// With a `request_id` the room id is derived from the account and request, so retries return the same room
    /// and a reused id with different initial settings is a conflict.
    pub fn create(&mut self, account: &str, request: CreateRoom, authorized: &dyn Fn() -> bool) -> Result<Room, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let title = clean_title(&request.title)?;
        if !valid_account(account) || request.request_id.as_deref().is_some_and(|id| !valid_request_id(id)) {
            return Err(Error::Invalid);
        }
        let origin = request.request_id.map(|request_id| Origin { request_id, title: title.clone(), allow_fork: request.allow_fork });
        let id = match &origin {
            Some(origin) => digest(format!("shared-room\0{account}\0{}", origin.request_id).as_bytes())[..32].to_string(),
            None => new_id(),
        };
        if origin.is_some() {
            match self.load(&id) {
                Ok(room) if room.owner == account && room.origin == origin => {
                    if !authorized() { return Err(Error::Unauthorized); }
                    return Ok(room);
                }
                Ok(_) => return Err(Error::Conflict),
                Err(Error::Missing) if self.buried(&id)? => return Err(Error::Conflict),
                Err(Error::Missing) => {}
                Err(error) => return Err(error),
            }
        }
        let rooms = self.list(account)?;
        if rooms.len() >= MAX_JOINED || rooms.iter().filter(|r| r.owner == account).count() >= MAX_OWNED {
            return Err(Error::Limit);
        }
        let now = self.now();
        let room = Room {
            version: ROOM_VERSION, id, title, owner: account.into(), allow_fork: request.allow_fork, revision: 1,
            created_at_ms: now, updated_at_ms: now,
            members: vec![Member { account: account.into(), role: Role::Owner, joined_at_ms: now }],
            invites: Vec::new(), sessions: Vec::new(), origin, removed: Vec::new(),
        };
        self.add_index(account, &room.id, authorized)?;
        let rooms_dir = self.dir("rooms")?;
        let staged = rooms_dir.join(format!(".new-{}", new_id()));
        let result = (|| {
            archive::private_dir(&staged)?;
            write_atomic(&staged, "room.json", &serde_json::to_vec(&room).map_err(|_| Error::Storage)?, authorized)?;
            if !authorized() { return Err(Error::Unauthorized); }
            std::fs::rename(&staged, rooms_dir.join(&room.id)).map_err(|_| Error::Storage)?;
            sync_dir(&rooms_dir)
        })();
        if result.is_err() { remove_tree(&staged); }
        result.map(|()| room)
    }

    pub fn update(&mut self, account: &str, id: &str, update: UpdateRoom, authorized: &dyn Fn() -> bool) -> Result<Room, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if update.title.is_none() && update.allow_fork.is_none() { return Err(Error::Invalid); }
        let title = update.title.as_deref().map(clean_title).transpose()?;
        let mut room = self.owned_room(account, id)?;
        if room.revision != update.expected_revision { return Err(Error::Conflict); }
        if let Some(title) = title { room.title = title; }
        if let Some(allow) = update.allow_fork { room.allow_fork = allow; }
        room.touch(self.now())?;
        self.save(&room, authorized)?;
        Ok(room)
    }

    /// Recipient fork copies live outside the room and survive this. A room made with a `request_id` leaves a
    /// tombstone so a late retry of its creation cannot bring it back.
    pub fn delete(&mut self, account: &str, id: &str, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let room = self.owned_room(account, id)?;
        if room.origin.is_some() { self.bury(&room, authorized)?; }
        let rooms_dir = self.dir("rooms")?;
        let trash = rooms_dir.join(format!(".trash-{}", new_id()));
        if !authorized() { return Err(Error::Unauthorized); }
        std::fs::rename(self.room_dir(id)?, &trash).map_err(|_| Error::Storage)?;
        sync_dir(&rooms_dir)?;
        remove_tree(&trash);
        for member in &room.members { self.drop_index(&member.account, &[id]); }
        Ok(())
    }

    pub fn invite(&mut self, account: &str, id: &str, request: CreateInvite, authorized: &dyn Fn() -> bool) -> Result<SharedInvite, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let secs = request.expires_in_secs.unwrap_or(INVITE_DEFAULT_SECS);
        let max_uses = request.max_uses.unwrap_or(1);
        if !(INVITE_MIN_SECS..=INVITE_MAX_SECS).contains(&secs) || !(1..=INVITE_MAX_USES).contains(&max_uses) {
            return Err(Error::Invalid);
        }
        let mut room = self.owned_room(account, id)?;
        let now = self.now();
        room.prune_invites(now);
        if room.invites.len() >= MAX_INVITES || room.members.len() >= MAX_MEMBERS { return Err(Error::Limit); }
        let invite_id = new_id();
        let token = format!("{TOKEN_PREFIX}{id}.{invite_id}.{}", random_hex(16)?);
        let expires_at_ms = now.saturating_add(secs * 1000);
        room.touch(now)?;
        room.invites.push(Invite {
            id: invite_id.clone(), token_sha256: digest(token.as_bytes()), created_at_ms: now, expires_at_ms, max_uses, uses: 0,
            created_revision: room.revision,
        });
        self.save(&room, authorized)?;
        Ok(SharedInvite { id: invite_id, room_id: id.into(), token, expires_at_ms, max_uses })
    }

    pub fn revoke_invite(&mut self, account: &str, id: &str, invite: &str, authorized: &dyn Fn() -> bool) -> Result<Room, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let mut room = self.owned_room(account, id)?;
        let index = room.invites.iter().position(|i| i.id == invite).ok_or(Error::Missing)?;
        room.invites.remove(index);
        let now = self.now();
        room.prune_invites(now);
        room.touch(now)?;
        self.save(&room, authorized)?;
        Ok(room)
    }

    /// Unknown, expired, used-up and revoked invites look the same to the caller.
    pub fn accept(&mut self, account: &str, token: &str, authorized: &dyn Fn() -> bool) -> Result<Room, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !valid_account(account) { return Err(Error::Invalid); }
        let token = token.trim();
        let (room_id, invite_id) = parse_token(token).ok_or(Error::Invalid)?;
        let mut room = match self.load(room_id) { Err(Error::Missing) => return Err(Error::Gone), room => room? };
        if room.role(account).is_some() {
            if !authorized() { return Err(Error::Unauthorized); }
            return Ok(room);
        }
        let now = self.now();
        let hash = digest(token.as_bytes());
        let index = room.invites.iter().position(|i| i.id == invite_id && i.token_sha256 == hash && i.expires_at_ms > now)
            .ok_or(Error::Gone)?;
        if room.removed_since(account, &room.invites[index]) { return Err(Error::Gone); }
        if room.members.len() >= MAX_MEMBERS || self.list(account)?.len() >= MAX_JOINED { return Err(Error::Limit); }
        room.invites[index].uses += 1;
        if room.invites[index].uses >= room.invites[index].max_uses { room.invites.remove(index); }
        room.prune_invites(now);
        room.members.push(Member { account: account.into(), role: Role::Member, joined_at_ms: now });
        room.touch(now)?;
        self.add_index(account, room_id, authorized)?;
        self.save(&room, authorized)?;
        Ok(room)
    }

    /// The owner removes a member, or a member leaves. The owner ends the room with `delete` instead.
    pub fn remove_member(&mut self, account: &str, id: &str, member: &str, authorized: &dyn Fn() -> bool) -> Result<Option<Room>, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let mut room = self.member_room(account, id)?;
        if member == room.owner || (account != room.owner && account != member) { return Err(Error::Forbidden); }
        let index = room.members.iter().position(|m| m.account == member).ok_or(Error::Missing)?;
        room.members.remove(index);
        let now = self.now();
        room.touch(now)?;
        if account == room.owner { room.record_removal(member, now); }
        self.save(&room, authorized)?;
        self.drop_index(member, &[id]);
        Ok((account == room.owner).then_some(room))
    }

    fn owner_usage(&self, account: &str) -> Result<u64, Error> {
        Ok(self.list(account)?.iter().filter(|r| r.owner == account).flat_map(|r| &r.sessions)
            .map(|s| s.bytes + s.checkpoint.as_ref().map_or(0, |c| c.bytes)).sum())
    }
}
