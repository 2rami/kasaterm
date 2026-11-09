//! Shared rooms (`docs/shared-rooms.md`). Members read published conversations and take fork copies; nothing here
//! reaches `/relay/account` or an uplink, so joining a room never grants control of the owner's machines.
use super::*;
use axum::routing::{delete, post};
use kasa_remote::shared_rooms::store::{Error, Publisher, ReadRange, Room, Store, MAX_APPEND, MAX_CHECKPOINT};
use kasa_socket::shared_rooms::{CreateFork, CreateInvite, CreateRoom, PublishCheckpoint, PublishSession, SharedAppend, UpdateRoom};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
#[cfg(test)]
#[path = "gateway_shared_rooms/tests.rs"]
mod tests;

type Job = Box<dyn FnOnce(&mut Store) + Send>;

/// Fork copies in memory for downloads at once; slow readers cannot pile up copies beyond this.
const DOWNLOAD_BUDGET: usize = 96 << 20;
const DOWNLOAD_UNIT: usize = 64 << 10;
/// A reader that has not taken the whole copy by then loses it and the reservation.
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(120);
const FORK_PREFIX: &[u8] = b"{\"ok\":true,\"fork\":";

pub(super) struct Service {
    sender: std::sync::mpsc::SyncSender<Job>,
    uploads: tokio::sync::Semaphore,
    /// Checkpoint bodies are the largest uploads; one at a time keeps their parse within memory.
    checkpoints: tokio::sync::Semaphore,
    downloads: Arc<tokio::sync::Semaphore>,
}

impl Service {
    pub(super) fn open(state: Option<&std::path::Path>) -> Option<Arc<Self>> {
        let mut store = Store::open(state?.with_file_name("shared-rooms")).ok()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Job>(32);
        std::thread::Builder::new().name("shared-rooms".into()).spawn(move || {
            while let Ok(job) = receiver.recv() { job(&mut store); }
        }).ok()?;
        Some(Arc::new(Self {
            sender, uploads: tokio::sync::Semaphore::new(4), checkpoints: tokio::sync::Semaphore::new(1),
            downloads: Arc::new(tokio::sync::Semaphore::new(DOWNLOAD_BUDGET / DOWNLOAD_UNIT)),
        }))
    }

    async fn run<T: Send + 'static>(&self, work: impl FnOnce(&mut Store) -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.sender.try_send(Box::new(move |store| { let _ = send.send(work(store)); })).map_err(|_| Error::Busy)?;
        tokio::time::timeout(Duration::from_secs(30), receive).await.map_err(|_| Error::Busy)?.map_err(|_| Error::Storage)?
    }
}

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/shared-rooms", get(list).post(create))
        .route("/relay/shared-rooms/{room}", get(show).patch(update).delete(remove))
        .route("/relay/shared-rooms/{room}/invites", post(invite))
        .route("/relay/shared-rooms/{room}/invites/{invite}", delete(revoke))
        .route("/relay/shared-room-invites/accept", post(accept))
        .route("/relay/shared-rooms/{room}/members/{account}", delete(leave))
        .route("/relay/shared-rooms/{room}/sessions", post(publish))
        .route("/relay/shared-rooms/{room}/sessions/{session}", delete(unpublish))
        .route("/relay/shared-rooms/{room}/sessions/{session}/append", post(append))
        .route("/relay/shared-rooms/{room}/sessions/{session}/content", get(content))
        .route("/relay/shared-rooms/{room}/sessions/{session}/checkpoints", post(checkpoint))
        .route("/relay/shared-rooms/{room}/sessions/{session}/fork", post(fork))
        .route("/relay/shared-forks", get(forks))
        .route("/relay/shared-forks/{fork}", get(fork_get).delete(fork_delete))
}

type Reply = axum::response::Response;

fn failure(error: Error) -> Reply {
    let status = match error {
        Error::Invalid => StatusCode::BAD_REQUEST, Error::Unauthorized => StatusCode::UNAUTHORIZED,
        Error::Forbidden | Error::ForkDisabled => StatusCode::FORBIDDEN, Error::Missing => StatusCode::NOT_FOUND,
        Error::Conflict => StatusCode::CONFLICT, Error::Gone => StatusCode::GONE, Error::Limit => StatusCode::PAYLOAD_TOO_LARGE,
        Error::Storage => StatusCode::SERVICE_UNAVAILABLE, Error::Busy => StatusCode::TOO_MANY_REQUESTS,
    };
    json_err(status, error.code())
}

fn respond(value: Result<Value, Error>) -> Reply {
    match value {
        Ok(value) => ([(header::CACHE_CONTROL, "no-store")], axum::Json(value)).into_response(),
        Err(error) => failure(error),
    }
}

fn open(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<(Arc<Service>, AccountAccess), Reply> {
    let access = account_access(gate, headers).filter(AccountAccess::valid).ok_or_else(|| failure(Error::Unauthorized))?;
    let service = gate.shared_rooms.clone().ok_or_else(|| failure(Error::Storage))?;
    Ok((service, access))
}

/// Publishing comes from the desktop that owns the conversation; the machine id is the device record's, never the body's.
fn publisher_machine(gate: &Gate, access: &AccountAccess) -> Result<String, Reply> {
    let device = gate.devices.lock().ok().and_then(|devices| devices.get(&access.device_id).cloned());
    let Some(device) = device.filter(|d| d.kind == "desktop") else { return Err(json_err(StatusCode::FORBIDDEN, "desktop_only")) };
    device.machine_id.filter(|id| !id.is_empty()).ok_or_else(|| failure(Error::Invalid))
}

async fn body<T: DeserializeOwned>(request: axum::extract::Request, limit: usize, secs: u64) -> Result<T, Reply> {
    let bytes = match tokio::time::timeout(Duration::from_secs(secs), axum::body::to_bytes(request.into_body(), limit)).await {
        Ok(Ok(bytes)) => bytes, Ok(Err(_)) => return Err(failure(Error::Limit)), Err(_) => return Err(failure(Error::Busy)),
    };
    serde_json::from_slice(&bytes).map_err(|_| failure(Error::Invalid))
}

/// Other members see a nickname or the account key, never the sign-in email behind `display_name`.
fn room_json(gate: &Gate, room: &Room, viewer: &str) -> Value {
    let name = |account: &str| {
        let shown = if account == viewer { gate.display_name(account) } else { gate.profiles.nickname(account) };
        shown.unwrap_or_else(|| account.to_string())
    };
    serde_json::to_value(room.view(viewer, &name)).unwrap_or(Value::Null)
}

async fn list(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let rooms = service.run(move |store| {
        if !access.valid() { return Err(Error::Unauthorized); }
        let rooms = store.list(&access.account)?;
        if !access.valid() { return Err(Error::Unauthorized); }
        Ok(rooms)
    }).await;
    respond(rooms.map(|rooms| json!({"ok":true,"rooms":rooms.iter().map(|r| room_json(&gate, r, &account)).collect::<Vec<_>>()})))
}

async fn create(State(gate): State<Gate>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let create: CreateRoom = match body(request, 4 << 10, 10).await { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let room = service.run(move |store| store.create(&access.account, create, &|| access.valid())).await;
    respond(room.map(|room| json!({"ok":true,"room":room_json(&gate, &room, &account)})))
}

async fn show(State(gate): State<Gate>, AxPath(room): AxPath<String>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let room = service.run(move |store| store.room(&access.account, &room, &|| access.valid())).await;
    respond(room.map(|room| json!({"ok":true,"room":room_json(&gate, &room, &account)})))
}

async fn update(State(gate): State<Gate>, AxPath(room): AxPath<String>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let update: UpdateRoom = match body(request, 4 << 10, 10).await { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let room = service.run(move |store| store.update(&access.account, &room, update, &|| access.valid())).await;
    respond(room.map(|room| json!({"ok":true,"room":room_json(&gate, &room, &account)})))
}

async fn remove(State(gate): State<Gate>, AxPath(room): AxPath<String>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| store.delete(&access.account, &room, &|| access.valid())).await.map(|()| json!({"ok":true})))
}

async fn invite(State(gate): State<Gate>, AxPath(room): AxPath<String>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let create: CreateInvite = match body(request, 1 << 10, 10).await { Ok(v) => v, Err(r) => return r };
    let invite = service.run(move |store| store.invite(&access.account, &room, create, &|| access.valid())).await;
    respond(invite.map(|invite| json!({"ok":true,"invite":invite})))
}

async fn revoke(State(gate): State<Gate>, AxPath((room, invite)): AxPath<(String, String)>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let room = service.run(move |store| store.revoke_invite(&access.account, &room, &invite, &|| access.valid())).await;
    respond(room.map(|room| json!({"ok":true,"room":room_json(&gate, &room, &account)})))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Accept { token: String }

async fn accept(State(gate): State<Gate>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let accept: Accept = match body(request, 1 << 10, 10).await { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let room = service.run(move |store| store.accept(&access.account, &accept.token, &|| access.valid())).await;
    respond(room.map(|room| json!({"ok":true,"room":room_json(&gate, &room, &account)})))
}

async fn leave(State(gate): State<Gate>, AxPath((room, member)): AxPath<(String, String)>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    let account = access.account.clone();
    let room = service.run(move |store| store.remove_member(&access.account, &room, &member, &|| access.valid())).await;
    respond(room.map(|room| json!({"ok":true,"room":room.map(|room| room_json(&gate, &room, &account))})))
}

async fn publish(State(gate): State<Gate>, AxPath(room): AxPath<String>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let machine = match publisher_machine(&gate, &access) { Ok(v) => v, Err(r) => return r };
    let publish: PublishSession = match body(request, 8 << 10, 10).await { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| {
        let publisher = Publisher { account: &access.account, machine_id: &machine };
        let session = store.publish(publisher, &room, publish, &|| access.valid())?;
        Ok(json!({"ok":true,"session":session}))
    }).await)
}

async fn unpublish(State(gate): State<Gate>, AxPath((room, session)): AxPath<(String, String)>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| store.unpublish(&access.account, &room, &session, &|| access.valid())).await.map(|()| json!({"ok":true})))
}

async fn append(State(gate): State<Gate>, AxPath((room, session)): AxPath<(String, String)>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let Ok(_upload) = service.uploads.try_acquire() else { return failure(Error::Busy) };
    let machine = match publisher_machine(&gate, &access) { Ok(v) => v, Err(r) => return r };
    // JSON escaping can expand a bounded UTF-8 chunk by up to six bytes per byte.
    let update: SharedAppend = match body(request, MAX_APPEND * 6 + (32 << 10), 30).await { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| {
        let publisher = Publisher { account: &access.account, machine_id: &machine };
        let session = store.append(publisher, &room, &session, update, &|| access.valid())?;
        Ok(json!({"ok":true,"session":session}))
    }).await)
}

async fn content(State(gate): State<Gate>, AxPath((room, session)): AxPath<(String, String)>, range: Result<axum::extract::Query<ReadRange>, axum::extract::rejection::QueryRejection>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    let Ok(axum::extract::Query(range)) = range else { return failure(Error::Invalid) };
    respond(service.run(move |store| {
        let content = store.read(&access.account, &room, &session, &range, &|| access.valid())?;
        Ok(json!({"ok":true,"content":content}))
    }).await)
}

async fn checkpoint(State(gate): State<Gate>, AxPath((room, session)): AxPath<(String, String)>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let Ok(_upload) = service.checkpoints.try_acquire() else { return failure(Error::Busy) };
    let machine = match publisher_machine(&gate, &access) { Ok(v) => v, Err(r) => return r };
    // The whole packet is held to the checkpoint limit; the store checks its compact form again.
    let checkpoint: PublishCheckpoint = match body(request, MAX_CHECKPOINT, 60).await { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| {
        let publisher = Publisher { account: &access.account, machine_id: &machine };
        let summary = store.checkpoint(publisher, &room, &session, checkpoint, &|| access.valid())?;
        Ok(json!({"ok":true,"checkpoint":summary}))
    }).await)
}

async fn fork(State(gate): State<Gate>, AxPath((room, session)): AxPath<(String, String)>, request: axum::extract::Request) -> Reply {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(r) => return r };
    let create: CreateFork = match body(request, 1 << 10, 10).await { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| {
        let info = store.fork(&access.account, &room, &session, create, &|| access.valid())?;
        Ok(json!({"ok":true,"fork":info}))
    }).await)
}

async fn forks(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| {
        let forks = store.forks(&access.account, &|| access.valid())?;
        Ok(json!({"ok":true,"forks":forks}))
    }).await)
}

/// The reservation is taken before the copy is read and is held by the sending task until the body is written,
/// abandoned or past its deadline.
async fn fork_get(State(gate): State<Gate>, AxPath(fork): AxPath<String>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    let downloads = service.downloads.clone();
    let copy = service.run(move |store| {
        let len = store.fork_len(&access.account, &fork, &|| access.valid())? as usize + FORK_PREFIX.len() + 1;
        let units = u32::try_from(len.div_ceil(DOWNLOAD_UNIT)).map_err(|_| Error::Limit)?;
        let permit = downloads.try_acquire_many_owned(units).map_err(|_| Error::Busy)?;
        Ok((store.fork_raw(&access.account, &fork, &|| access.valid())?, permit))
    }).await;
    match copy {
        Ok((raw, permit)) => download(raw, permit),
        Err(error) => failure(error),
    }
}

fn download(raw: Vec<u8>, permit: tokio::sync::OwnedSemaphorePermit) -> Reply {
    let parts = [axum::body::Bytes::from_static(FORK_PREFIX), axum::body::Bytes::from(raw), axum::body::Bytes::from_static(b"}")];
    let length: usize = parts.iter().map(axum::body::Bytes::len).sum();
    let (send, receive) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(2);
    tokio::spawn(async move {
        let _permit = permit;
        let deadline = tokio::time::Instant::now() + DOWNLOAD_DEADLINE;
        for part in parts {
            for start in (0..part.len()).step_by(DOWNLOAD_UNIT) {
                let chunk = part.slice(start..part.len().min(start + DOWNLOAD_UNIT));
                if !matches!(tokio::time::timeout_at(deadline, send.send(Ok(chunk))).await, Ok(Ok(()))) { return; }
            }
        }
    });
    // With the length declared, a body cut short by the deadline is an error to the client, not a short JSON.
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, length)
        .body(axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receive)))
        .unwrap_or_else(|_| failure(Error::Storage))
}

async fn fork_delete(State(gate): State<Gate>, AxPath(fork): AxPath<String>, headers: axum::http::HeaderMap) -> Reply {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(r) => return r };
    respond(service.run(move |store| store.delete_fork(&access.account, &fork, &|| access.valid())).await.map(|()| json!({"ok":true})))
}
