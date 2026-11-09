use super::*;
use std::io::{Seek, SeekFrom};

use kasa_socket::shared_rooms::{PublishSession, SharedAppend, SharedContent};

pub(super) const MAX_SOURCE: usize = 512;
pub(super) const MAX_SESSION_TITLE: usize = 512;

/// Both values come from the bearer credential, never from the request body.
#[derive(Clone, Copy)]
pub struct Publisher<'a> { pub account: &'a str, pub machine_id: &'a str }

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRange {
    pub generation: Option<String>,
    #[serde(default)]
    pub offset: u64,
    pub end: Option<u64>,
}

fn session_id(room: &str, machine: &str, source: &str) -> String {
    digest(format!("{room}\0{machine}\0{source}").as_bytes())[..32].into()
}

impl Store {
    fn content_path(&self, room: &str, session: &Session) -> Result<PathBuf, Error> {
        Ok(self.room_dir(room)?.join(format!("{}-{}.log", session.id, session.generation)))
    }

    /// Bytes land before the manifest names them; until then they are an invisible tail or an unnamed generation.
    fn write_content(&self, path: &Path, committed: u64, raw: &[u8], authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600).custom_flags(libc::O_NOFOLLOW); }
        let mut file = options.open(path).map_err(|_| Error::Storage)?;
        file.set_len(committed).map_err(|_| Error::Storage)?;
        file.seek(SeekFrom::Start(committed)).map_err(|_| Error::Storage)?;
        file.write_all(raw).and_then(|()| file.sync_all()).map_err(|_| Error::Storage)?;
        if !authorized() { return Err(Error::Unauthorized); }
        Ok(())
    }

    pub fn publish(&mut self, publisher: Publisher<'_>, room_id: &str, request: PublishSession, authorized: &dyn Fn() -> bool) -> Result<SharedSession, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if !valid_machine(publisher.machine_id) || !valid_source(&request.source_key)
            || !text(&request.title, MAX_SESSION_TITLE) || !valid_harness(&request.harness) {
            return Err(Error::Invalid);
        }
        let mut room = self.owned_room(publisher.account, room_id)?;
        let id = session_id(room_id, publisher.machine_id, &request.source_key);
        let now = self.now();
        let index = match room.sessions.iter().position(|s| s.id == id) {
            Some(index) if room.sessions[index].harness != request.harness => return Err(Error::Conflict),
            Some(index) if room.sessions[index].title == request.title => {
                if !authorized() { return Err(Error::Unauthorized); }
                return Ok(room.sessions[index].view(true));
            }
            Some(index) => {
                room.sessions[index].title = request.title;
                room.sessions[index].updated_at_ms = now;
                index
            }
            None => {
                if room.sessions.len() >= MAX_SESSIONS { return Err(Error::Limit); }
                room.sessions.push(Session {
                    id, source_key: request.source_key, title: request.title, harness: request.harness,
                    publisher_machine_id: publisher.machine_id.into(), revision: 0, generation: new_id(), bytes: 0,
                    cursor: 0, updated_at_ms: now, caught_up: false, checkpoint: None,
                });
                room.sessions.len() - 1
            }
        };
        room.touch(now)?;
        self.save(&room, authorized)?;
        Ok(room.sessions[index].view(true))
    }

    pub fn append(&mut self, publisher: Publisher<'_>, room_id: &str, session_id: &str, update: SharedAppend, authorized: &dyn Fn() -> bool) -> Result<SharedSession, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        if update.raw.len() > MAX_APPEND || update.next < update.from || !hex(&update.generation, 32)
            || (!update.raw.is_empty() && (!update.raw.ends_with('\n') || update.next == update.from))
            || (update.reset && update.from != 0) {
            return Err(Error::Invalid);
        }
        let mut room = self.owned_room(publisher.account, room_id)?;
        let index = room.sessions.iter().position(|s| s.id == session_id).ok_or(Error::Missing)?;
        let previous = room.sessions[index].clone();
        if previous.publisher_machine_id != publisher.machine_id { return Err(Error::Forbidden); }
        if previous.revision != update.expected_revision || previous.generation != update.generation
            || (!update.reset && previous.cursor != update.from) {
            return Err(Error::Conflict);
        }
        let mut session = previous.clone();
        // A checkpoint belongs to the generation it was captured against; a reset retires it with the old content.
        let retired = update.reset && session.checkpoint.is_some();
        if update.reset {
            session.generation = new_id();
            session.bytes = 0;
            session.caught_up = false;
            session.checkpoint = None;
        }
        let total = session.bytes.checked_add(update.raw.len() as u64).ok_or(Error::Limit)?;
        let used = self.owner_usage(publisher.account)?;
        if total > MAX_SESSION_BYTES || used.saturating_sub(previous.bytes).saturating_add(total) > MAX_OWNER_BYTES {
            return Err(Error::Limit);
        }
        let revision = session.revision.checked_add(1).ok_or(Error::Limit)?;
        let path = self.content_path(room_id, &session)?;
        match archive::regular(&path, MAX_SESSION_BYTES + MAX_APPEND as u64) {
            Ok(meta) if meta.len() >= session.bytes => {}
            Err(archive::Error::Missing) if session.bytes == 0 => {}
            _ => return Err(Error::Storage),
        }
        let outcome = match self.write_content(&path, session.bytes, update.raw.as_bytes(), authorized) {
            Err(error) => Err((error, Landed::No)),
            Ok(()) => {
                let now = self.now();
                session.bytes = total;
                session.cursor = update.next;
                session.revision = revision;
                session.caught_up |= update.caught_up;
                session.updated_at_ms = now;
                room.sessions[index] = session;
                room.updated_at_ms = room.updated_at_ms.max(now);
                match if retired { room.touch(now) } else { Ok(()) } {
                    Err(error) => Err((error, Landed::No)),
                    Ok(()) => self.save_landed(&room, authorized),
                }
            }
        };
        self.settle(room_id, outcome)?;
        Ok(room.sessions[index].view(true))
    }

    /// Stops sharing one conversation: members lose it and the publisher's next append gets `Missing`.
    /// Fork copies already taken are the recipients' and stay.
    pub fn unpublish(&mut self, account: &str, room_id: &str, session_id: &str, authorized: &dyn Fn() -> bool) -> Result<(), Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let mut room = self.owned_room(account, room_id)?;
        let index = room.sessions.iter().position(|s| s.id == session_id).ok_or(Error::Missing)?;
        room.sessions.remove(index);
        room.touch(self.now())?;
        let outcome = self.save_landed(&room, authorized);
        self.settle(room_id, outcome)
    }

    pub fn read(&self, account: &str, room_id: &str, session_id: &str, range: &ReadRange, authorized: &dyn Fn() -> bool) -> Result<SharedContent, Error> {
        if !authorized() { return Err(Error::Unauthorized); }
        let room = self.member_room(account, room_id)?;
        let session = room.sessions.iter().find(|s| s.id == session_id).ok_or(Error::Missing)?;
        let end = range.end.unwrap_or(session.bytes);
        if range.generation.as_deref().is_some_and(|g| g != session.generation) || end > session.bytes || range.offset > end {
            return Err(Error::Conflict);
        }
        let mut raw = String::new();
        if range.offset < end {
            let path = self.content_path(room_id, session)?;
            let mut file = open_read(&path, MAX_SESSION_BYTES + MAX_APPEND as u64).map_err(|_| Error::Storage)?;
            if file.metadata().map_err(|_| Error::Storage)?.len() < session.bytes { return Err(Error::Storage); }
            file.seek(SeekFrom::Start(range.offset)).map_err(|_| Error::Storage)?;
            let mut bytes = vec![0; (end - range.offset).min(MAX_READ as u64) as usize];
            file.read_exact(&mut bytes).map_err(|_| Error::Storage)?;
            let valid = match std::str::from_utf8(&bytes) {
                Ok(_) => bytes.len(),
                Err(e) if e.error_len().is_none() && range.offset + (bytes.len() as u64) < end => e.valid_up_to(),
                Err(_) => return Err(Error::Invalid),
            };
            bytes.truncate(valid);
            raw = String::from_utf8(bytes).map_err(|_| Error::Storage)?;
        }
        // Membership changes run on this same worker, so the membership loaded above is still the latest.
        if !authorized() { return Err(Error::Unauthorized); }
        Ok(SharedContent {
            generation: session.generation.clone(), revision: session.revision, offset: range.offset,
            next: range.offset + raw.len() as u64, end, raw,
        })
    }
}
