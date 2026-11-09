use super::*;
use kasa_remote::shared_rooms::client::Client;
use kasa_socket::shared_rooms as api;
use std::sync::Arc;

pub(crate) mod scene;
mod input;
mod lifecycle;
pub(crate) mod service;
mod publisher;
mod fork;
mod fork_storage;
mod sources;
mod verification;
#[cfg(test)]
mod tests;

pub(crate) const PANE: &str = "\0kasaterm-shared-rooms";
pub(crate) const FIELD: &str = "shared-room-field";
type Rect = (f32, f32, f32, f32);

#[derive(Clone, Default)]
pub(crate) struct Nav {
    pub(crate) rows: Vec<(String, String, bool, usize)>,
    pub(crate) collapsed: bool,
    pub(crate) active: Option<String>,
}

impl Nav {
    pub(crate) fn height(&self) -> f32 {
        44.0 + if self.collapsed { 0.0 } else { self.rows.len() as f32 * 44.0 }
    }
}

#[derive(Clone)]
pub(crate) enum Action {
    Open(String), New, Join, Back, CloseView, Refresh, SignIn, Select(String), Invite,
    CopyInvite, CopyConversation, Manage, Rename, ToggleFork,
    RevokeInvite(String), RemoveMember(String), Leave, Delete, Confirm,
    Cancel, Submit, FocusField, ToggleProject, Publish, Fork,
    SelectSource(String), ToggleTool(usize),
}

#[derive(Clone)]
pub(crate) enum FormKind {
    Create { request_id: String }, Join, Rename { room: String, revision: u64 },
    Publish, Fork { room: String, session: String, checkpoint: String, request_id: String },
    Confirm { action: Box<Action>, label: String },
    Manage,
}

#[derive(Clone)]
pub(crate) struct Form {
    kind: FormKind,
    text: String,
    cursor: usize,
    allow_fork: bool,
    include_project: bool,
    selected: std::collections::HashSet<String>,
    scroll: f32,
}

impl Form {
    fn new(kind: FormKind, text: String) -> Self {
        let cursor = text.chars().count();
        Self { kind, text, cursor, allow_fork: true, include_project: false, selected: Default::default(), scroll: 0.0 }
    }
}

#[derive(Default)]
pub(crate) struct Reader {
    room: String,
    session: String,
    generation: String,
    next: u64,
    end: u64,
    tail: String,
    conversation: crate::chat_view::parse::Conversation,
    version: u64,
    cache: crate::chat_view::classic::Cache,
    open: std::collections::HashSet<usize>,
    scroll: f32,
    follow: bool,
    rect: Option<Rect>,
    cell: (f32, f32),
    selection: Option<((usize, usize), (usize, usize))>,
    dragging: bool,
    retry_at: Option<Instant>,
}

pub(crate) struct State {
    client: Option<Client>,
    rooms: Arc<Vec<api::SharedRoom>>,
    active: Option<String>,
    reader: Reader,
    form: Option<Form>,
    invites: HashMap<String, api::SharedInvite>,
    return_pane: Option<String>,
    pending: bool,
    fetching: bool,
    reading: bool,
    sequence: u64,
    view_sequence: u64,
    catalog_sequence: u64,
    write_sequence: u64,
    cancelled_writes: std::collections::HashSet<u64>,
    reader_sequence: u64,
    auth_epoch: u64,
    refreshed: Option<Instant>,
    error: Option<String>,
    note: Option<String>,
    hits: Vec<(Action, Rect)>,
    area: Option<Rect>,
    form_scroll_max: f32,
    list_scroll: f32,
    list_max: f32,
    list_rect: Option<Rect>,
    sources: Vec<sources::Source>,
    runtime: service::Runtime,
}

impl Default for State {
    fn default() -> Self {
        Self { client: None, rooms: Arc::new(Vec::new()), active: None, reader: Reader::default(),
            form: None, invites: HashMap::new(), return_pane: None, pending: false, fetching: false,
            reading: false, sequence: 0, view_sequence: 0, catalog_sequence: 0, write_sequence: 0,
            cancelled_writes: Default::default(), reader_sequence: 0, auth_epoch: kasa_device::device_auth::change_epoch(),
            refreshed: None, error: None, note: None, hits: Vec::new(), area: None, form_scroll_max: 0.0,
            sources: Vec::new(), runtime: service::Runtime::default(), list_scroll: 0.0, list_max: 0.0, list_rect: None }
    }
}

#[derive(Clone)]
enum Job {
    List, Create(api::CreateRoom), Join(String), Update(String, api::UpdateRoom),
    Invite(String), RevokeInvite(String, String), RemoveMember(String, String), Delete(String),
    Read { room: String, session: String, generation: String, offset: u64, end: u64, sequence: u64 },
    Fork { room: String, session: String, request: api::CreateFork, cwd: String, include_project: bool },
}

#[derive(Clone)]
enum Reply {
    Rooms(Vec<api::SharedRoom>), Room(api::SharedRoom), Invite(api::SharedInvite), Changed,
    Content { room: String, session: String, sequence: u64, content: api::SharedContent },
    Fork { info: api::SharedForkInfo, cwd: String, include_project: bool },
}

#[derive(Clone)]
pub(crate) struct Event {
    sequence: u64,
    view_sequence: u64,
    catalog_sequence: u64,
    write_sequence: Option<u64>,
    reader_sequence: Option<u64>,
    auth_epoch: u64,
    client: Option<Client>,
    reading: bool,
    polling: bool,
    result: Result<Reply, kasa_remote::shared_rooms::client::Error>,
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedRoomEvent").field("sequence", &self.sequence).finish_non_exhaustive()
    }
}

pub(crate) fn capture_client() -> Result<Client, kasa_remote::shared_rooms::client::Error> {
    if crate::verification_run() {
        let result = (|| {
            let path = std::path::PathBuf::from(std::env::var_os("KASATERM_SHARED_FIXTURE_CREDENTIAL")?);
            let bytes = std::fs::read(&path).ok()?;
            let credential: kasa_device::device_auth::DeviceCred = serde_json::from_slice(&bytes).ok()?;
            kasa_remote::shared_rooms::client::loopback_credential(credential,
                Arc::new(move || std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()))).ok()
        })();
        return result.ok_or_else(|| kasa_remote::shared_rooms::client::Error { status: 401, code: "unauthorized".into() });
    }
    Client::capture()
}

impl State {
    fn room(&self) -> Option<&api::SharedRoom> {
        self.active.as_deref().and_then(|id| self.rooms.iter().find(|r| r.id == id))
    }
    fn owner(&self) -> bool { self.room().is_some_and(|r| r.my_role == "owner") }
    fn session(&self) -> Option<&api::SharedSession> {
        self.room().and_then(|r| r.sessions.iter().find(|s| s.id == self.reader.session))
    }
    fn current_invite(&self) -> Option<&api::SharedInvite> {
        self.active.as_deref().and_then(|id| self.invites.get(id))
    }
    fn reset_reader(&mut self) {
        self.reader_sequence = self.reader_sequence.wrapping_add(1);
        self.reader = Reader::default();
        self.reading = false;
    }
    fn accepts(&self, event: &Event) -> bool {
        event.sequence == self.sequence
            && event.reader_sequence.is_none_or(|sequence| sequence == self.reader_sequence)
            && (!event.polling || event.catalog_sequence == self.catalog_sequence)
    }
    fn cancel_view(&mut self) {
        self.view_sequence = self.view_sequence.wrapping_add(1);
        if self.pending { self.cancelled_writes.insert(self.write_sequence); }
        self.form = None;
    }
    fn clear_account(&mut self) {
        self.client = None;
        self.rooms = Arc::new(Vec::new());
        self.active = None;
        self.reader = Reader::default();
        self.form = None;
        self.invites.clear();
        self.sources.clear();
        self.error = None;
        self.note = None;
        self.hits.clear();
        self.sequence = self.sequence.wrapping_add(1);
        self.view_sequence = self.view_sequence.wrapping_add(1);
        self.catalog_sequence = self.catalog_sequence.wrapping_add(1);
        self.cancelled_writes.clear();
        self.reader_sequence = self.reader_sequence.wrapping_add(1);
        self.fetching = false; self.reading = false; self.pending = false;
    }
}

impl App {
    pub(crate) fn shared_room_active(&self) -> bool {
        self.internal_room_active(crate::internal_room::InternalRoomKind::SharedRooms)
    }

    fn shared_job(&mut self, job: Job) {
        let reading = matches!(job, Job::Read { .. });
        let polling = matches!(job, Job::List);
        if reading { self.shared_rooms.reading = true; }
        else if polling { self.shared_rooms.fetching = true; self.shared_rooms.refreshed = Some(Instant::now()); }
        else {
            self.shared_rooms.pending = true; self.shared_rooms.error = None;
            self.shared_rooms.write_sequence = self.shared_rooms.write_sequence.wrapping_add(1);
            self.shared_rooms.catalog_sequence = self.shared_rooms.catalog_sequence.wrapping_add(1);
            self.shared_rooms.fetching = false;
        }
        let sequence = self.shared_rooms.sequence;
        let view_sequence = self.shared_rooms.view_sequence;
        let catalog_sequence = self.shared_rooms.catalog_sequence;
        let write_sequence = (!reading && !polling).then_some(self.shared_rooms.write_sequence);
        let reader_sequence = match &job { Job::Read { sequence, .. } => Some(*sequence), _ => None };
        let auth_epoch = kasa_device::device_auth::change_epoch();
        let expected_scope = self.shared_rooms.client.as_ref().map(|c| c.auth_scope().to_string());
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut selected = None;
            let result = (|| -> Result<Reply, kasa_remote::shared_rooms::client::Error> {
                let client = capture_client()?;
                if !polling && expected_scope.as_deref() != Some(client.auth_scope()) {
                    return Err(kasa_remote::shared_rooms::client::Error { status: 401, code: "account_changed".into() });
                }
                selected = Some(client.clone());
                kasa_remote::shared_rooms::client::run(async {
                    Ok(match job {
                        Job::List => Reply::Rooms(client.list().await?.rooms),
                        Job::Create(request) => Reply::Room(client.create(&request).await?),
                        Job::Join(token) => Reply::Room(client.join(token.trim()).await?),
                        Job::Update(room, request) => Reply::Room(client.update(&room, &request).await?),
                        Job::Invite(room) => Reply::Invite(client.invite(&room, &api::CreateInvite::default()).await?),
                        Job::RevokeInvite(room, id) => { client.revoke_invite(&room, &id).await?; Reply::Changed }
                        Job::RemoveMember(room, account) => { client.remove_member(&room, &account).await?; Reply::Changed }
                        Job::Delete(room) => { client.delete_room(&room).await?; Reply::Changed }
                        Job::Read { room, session, generation, offset, end, sequence } => {
                            let content = client.content(&room, &session, &generation, offset, end).await?;
                            Reply::Content { room, session, sequence, content }
                        }
                        Job::Fork { room, session, request, cwd, include_project } => Reply::Fork {
                            info: client.fork(&room, &session, &request).await?, cwd, include_project,
                        },
                    })
                })
            })();
            let result = if selected.as_ref().is_some_and(|c| !c.is_current()) {
                Err(kasa_remote::shared_rooms::client::Error { status: 401, code: "account_changed".into() })
            } else { result };
            let _ = proxy.send_event(UserEvent::SharedRooms(Event { sequence, view_sequence, catalog_sequence, write_sequence,
                reader_sequence, auth_epoch, client: selected,
                reading, polling, result }));
        });
    }

    pub(crate) fn poll_shared_rooms(&mut self) {
        verification::tick(self);
        let epoch = kasa_device::device_auth::change_epoch();
        if self.shared_rooms.auth_epoch != epoch {
            if matches!(self.ime_focus, Some(ImeFocus::SharedRooms)) {
                let _ = self.hangul.flush(); self.preedit.clear(); self.in_preedit = false;
            }
            crate::lineedit::forget(FIELD);
            self.shared_rooms.clear_account();
            self.shared_rooms.auth_epoch = epoch;
            self.shared_rooms.refreshed = None;
            self.sync_shared_navigation();
        }
        let interval = if self.shared_room_active() { 3 } else { 15 };
        if !self.shared_rooms.fetching && !self.shared_rooms.pending
            && self.shared_rooms.refreshed.is_none_or(|at| at.elapsed().as_secs() >= interval) {
            self.shared_job(Job::List);
        }
        if self.shared_room_active() && !self.shared_rooms.reading
            && self.shared_rooms.reader.retry_at.is_none_or(|at| Instant::now() >= at) {
            if let Some(session) = self.shared_rooms.session().cloned() {
                if self.shared_rooms.reader.generation != session.generation {
                    self.shared_rooms.reader_sequence = self.shared_rooms.reader_sequence.wrapping_add(1);
                    self.shared_rooms.reader = Reader { room: self.shared_rooms.active.clone().unwrap_or_default(),
                        session: session.id.clone(), generation: session.generation.clone(), follow: true, ..Reader::default() };
                }
                if self.shared_rooms.reader.next < session.bytes {
                    self.shared_job(Job::Read { room: self.shared_rooms.active.clone().unwrap_or_default(),
                        session: session.id, generation: session.generation, offset: self.shared_rooms.reader.next,
                        end: session.bytes, sequence: self.shared_rooms.reader_sequence });
                }
            }
        }
    }

    pub(crate) fn shared_ui_tick(&self) -> Option<Instant> {
        let active = self.shared_room_active();
        let room_poll = (!self.shared_rooms.fetching && !self.shared_rooms.pending
            && (active || self.shared_rooms.client.is_some() || self.shared_rooms.error.is_none())).then(|| {
                self.shared_rooms.refreshed.unwrap_or_else(Instant::now)
                    + std::time::Duration::from_secs(if active { 3 } else { 15 })
            });
        let reading = (active && !self.shared_rooms.reading
            && self.shared_rooms.session().is_some_and(|s| self.shared_rooms.reader.next < s.bytes))
            .then(|| self.shared_rooms.reader.retry_at.unwrap_or_else(|| Instant::now() + std::time::Duration::from_millis(10)));
        room_poll.into_iter().chain(reading).min()
    }

    pub(crate) fn shared_rooms_event(&mut self, event: Event) {
        if event.auth_epoch != kasa_device::device_auth::change_epoch() || !self.shared_rooms.accepts(&event) { return; }
        if event.client.as_ref().is_some_and(|client| !client.is_current()) {
            self.shared_rooms.clear_account();
            self.shared_rooms.refreshed = None;
            self.sync_shared_navigation();
            self.poll_shared_rooms();
            self.chrome_dirty = true;
            if let Some(w) = &self.window { w.request_redraw(); }
            return;
        }
        let current_view = event.view_sequence == self.shared_rooms.view_sequence;
        let current_write = event.write_sequence.is_none_or(|sequence| sequence == self.shared_rooms.write_sequence);
        let cancelled = event.write_sequence.is_some_and(|sequence| self.shared_rooms.cancelled_writes.remove(&sequence));
        if event.reading { self.shared_rooms.reading = false; }
        else if event.polling { self.shared_rooms.fetching = false; }
        else if current_write { self.shared_rooms.pending = false; self.shared_rooms.refreshed = None; }
        if let Some(client) = event.client {
            if self.shared_rooms.client.as_ref().is_some_and(|old| old.auth_scope() != client.auth_scope()) {
                self.shared_rooms.clear_account();
            }
            self.shared_rooms.client = Some(client);
        }
        match event.result {
            Ok(Reply::Rooms(rooms)) => {
                self.shared_rooms.rooms = Arc::new(rooms);
                self.shared_rooms.invites.retain(|id, invite| self.shared_rooms.rooms.iter()
                    .any(|r| r.id == *id && r.invites.iter().any(|i| i.id == invite.id)));
                if self.shared_rooms.active.as_ref().is_some_and(|id| !self.shared_rooms.rooms.iter().any(|r| &r.id == id)) {
                    self.shared_rooms.active = None; self.shared_rooms.reset_reader();
                    self.shared_rooms.form = None;
                    self.shared_rooms.view_sequence = self.shared_rooms.view_sequence.wrapping_add(1);
                }
                if let Some(id) = self.shared_rooms.active.clone() { self.shared_select_room(&id); }
                self.shared_rooms.error = None;
            }
            Ok(Reply::Room(room)) => {
                let id = room.id.clone();
                let rooms = Arc::make_mut(&mut self.shared_rooms.rooms);
                if let Some(existing) = rooms.iter_mut().find(|r| r.id == id) {
                    if room.revision >= existing.revision { *existing = room; }
                }
                else { rooms.push(room); }
                if current_view && current_write && !cancelled {
                    self.shared_rooms.form = None;
                    self.shared_select_room(&id);
                }
            }
            Ok(Reply::Invite(invite)) => { self.shared_rooms.invites.insert(invite.room_id.clone(), invite); self.shared_rooms.refreshed = None; }
            Ok(Reply::Changed) => {
                if current_view && current_write { self.shared_rooms.form = None; }
                self.shared_rooms.refreshed = None;
            }
            Ok(Reply::Content { room, session, sequence, content }) => {
                let reader = &mut self.shared_rooms.reader;
                if sequence != self.shared_rooms.reader_sequence || room != reader.room || session != reader.session
                    || content.generation != reader.generation || content.offset != reader.next { return; }
                if !reader.append(&content) {
                    self.shared_rooms.error = Some("공유 대화의 읽기 위치가 바뀌었어요. 다시 확인해 주세요.".into());
                    self.shared_rooms.reader = Reader::default();
                    self.shared_rooms.refreshed = None;
                }
            }
            Ok(Reply::Fork { info, cwd, include_project }) => {
                if !cancelled && current_write {
                    if current_view { self.shared_rooms.form = None; }
                    self.start_shared_fork(info.id, cwd, include_project, current_view.then_some(event.view_sequence));
                }
            }
            Err(error) => {
                if error.status == 401 { self.shared_rooms.clear_account(); }
                else if event.reading && matches!(error.status, 403 | 404 | 409) {
                    self.shared_rooms.reset_reader();
                    self.shared_rooms.refreshed = None;
                } else if event.reading {
                    self.shared_rooms.reader.retry_at = Some(Instant::now() + std::time::Duration::from_secs(3));
                }
                if self.shared_rooms.client.is_none() { self.shared_rooms.rooms = Arc::new(Vec::new()); }
                if event.polling || event.reading || (current_view && current_write) {
                    self.shared_rooms.error = Some(error.to_string());
                }
            }
        }
        self.sync_shared_navigation();
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }

    fn shared_select_room(&mut self, id: &str) {
        if self.shared_rooms.active.as_deref() != Some(id) {
            self.shared_rooms.reset_reader();
            self.shared_rooms.view_sequence = self.shared_rooms.view_sequence.wrapping_add(1);
            self.shared_rooms.form = None;
            self.shared_rooms.list_scroll = 0.0;
        }
        self.shared_rooms.active = Some(id.into());
        if !self.shared_rooms.reader.session.is_empty()
            && self.shared_rooms.room().is_none_or(|room| !room.sessions.iter().any(|s| s.id == self.shared_rooms.reader.session)) {
            self.shared_rooms.reset_reader();
        }
        if self.shared_rooms.reader.session.is_empty() {
            if let Some(session) = self.shared_rooms.room().and_then(|r| r.sessions.first()).cloned() {
                self.shared_rooms.reader = Reader { room: id.into(), session: session.id,
                    generation: session.generation, follow: true, ..Reader::default() };
            }
        }
        self.sync_shared_navigation();
    }

    fn sync_shared_navigation(&mut self) {
        self.info.navigation.shared.rows = self.shared_rooms.rooms.iter()
            .map(|r| (r.id.clone(), r.title.clone(), r.my_role == "owner", r.sessions.len())).collect();
        self.info.navigation.shared.active = self.shared_room_active().then(|| self.shared_rooms.active.clone()).flatten();
    }
}

impl Reader {
    fn append(&mut self, content: &api::SharedContent) -> bool {
        if content.generation != self.generation || content.offset != self.next
            || content.offset.checked_add(content.raw.len() as u64) != Some(content.next)
            || content.next > content.end || (content.raw.is_empty() && content.next < content.end)
            || self.tail.len() + content.raw.len() > 8 << 20 { return false; }
        self.next = content.next; self.end = content.end; self.retry_at = None;
        self.tail.push_str(&content.raw);
        if let Some(end) = self.tail.rfind('\n') {
            let complete: String = self.tail.drain(..=end).collect();
            if !self.conversation.apply(&complete) { return false; }
            self.version = self.version.wrapping_add(1);
        }
        true
    }
}
