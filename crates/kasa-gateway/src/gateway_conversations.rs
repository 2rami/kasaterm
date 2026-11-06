use super::*;
use kasa_remote::conversation_archive::{Append, Error, Store, MAX_APPEND};
use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(test)]
#[path = "gateway_conversations/tests.rs"]
mod tests;

type Job = Box<dyn FnOnce(&mut Store) + Send>;

pub(super) struct Service { sender: std::sync::mpsc::SyncSender<Job>, uploads: tokio::sync::Semaphore }

impl Service {
    pub(super) fn open(state: Option<&std::path::Path>) -> Option<Arc<Self>> {
        let mut store = Store::open(state?.with_file_name("conversation-archives")).ok()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Job>(32);
        std::thread::Builder::new().name("conversation-archives".into()).spawn(move || {
            while let Ok(job) = receiver.recv() { job(&mut store); }
        }).ok()?;
        Some(Arc::new(Self { sender, uploads: tokio::sync::Semaphore::new(4) }))
    }

    async fn run<T: Send + 'static>(&self, work: impl FnOnce(&mut Store) -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.sender.try_send(Box::new(move |store| { let _ = send.send(work(store)); })).map_err(|_| Error::Busy)?;
        tokio::time::timeout(Duration::from_secs(20), receive).await.map_err(|_| Error::Busy)?.map_err(|_| Error::Storage)?
    }
}

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/conversations", get(list).post(append))
        .route("/relay/conversations/settings", axum::routing::put(settings))
        .route("/relay/conversations/{id}", get(read).delete(delete))
        .route("/relay/conversations/{id}/restore", axum::routing::post(restore))
}

fn failure(error: Error) -> axum::response::Response {
    let status = match error {
        Error::Invalid => StatusCode::BAD_REQUEST, Error::Unauthorized => StatusCode::UNAUTHORIZED,
        Error::Missing => StatusCode::NOT_FOUND, Error::Conflict => StatusCode::CONFLICT,
        Error::Limit => StatusCode::PAYLOAD_TOO_LARGE, Error::Storage => StatusCode::SERVICE_UNAVAILABLE,
        Error::Busy => StatusCode::TOO_MANY_REQUESTS,
        Error::Disabled => StatusCode::PRECONDITION_REQUIRED,
    };
    json_err(status, error.code())
}

fn respond(value: Result<Value, Error>) -> axum::response::Response {
    match value {
        Ok(value) => ([(header::CACHE_CONTROL, "no-store")], axum::Json(value)).into_response(),
        Err(error) => failure(error),
    }
}

fn open(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<(Arc<Service>, AccountAccess), Error> {
    let access = account_access(gate, headers).filter(AccountAccess::valid).ok_or(Error::Unauthorized)?;
    let service = gate.conversations.clone().ok_or(Error::Storage)?;
    Ok((service, access))
}

async fn list(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(e) => return failure(e) };
    respond(service.run(move |store| {
        if !access.valid() { return Err(Error::Unauthorized); }
        let entries = store.list(&access.account)?;
        let enabled = store.enabled(&access.account)?;
        if !access.valid() { return Err(Error::Unauthorized); }
        Ok(json!({"ok":true,"enabled":enabled,"conversations":entries}))
    }).await)
}

async fn settings(State(gate): State<Gate>, request: axum::extract::Request) -> axum::response::Response {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(e) => return failure(e) };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(request.into_body(), 1024)).await {
        Ok(Ok(bytes)) => bytes, Ok(Err(_)) => return failure(Error::Invalid), Err(_) => return failure(Error::Busy),
    };
    let enabled = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["enabled"].as_bool());
    let Some(enabled) = enabled else { return failure(Error::Invalid) };
    respond(service.run(move |store| {
        store.set_enabled(&access.account, enabled, &|| access.valid())?;
        Ok(json!({"ok":true,"enabled":enabled}))
    }).await)
}

async fn append(State(gate): State<Gate>, request: axum::extract::Request) -> axum::response::Response {
    let (service, access) = match open(&gate, request.headers()) { Ok(v) => v, Err(e) => return failure(e) };
    let _upload = match service.uploads.try_acquire() { Ok(permit) => permit, Err(_) => return failure(Error::Busy) };
    let device = gate.devices.lock().ok().and_then(|devices| devices.get(&access.device_id).cloned());
    let Some(device) = device.filter(|d| d.kind == "desktop") else {
        return json_err(StatusCode::FORBIDDEN, "desktop_only");
    };
    let Some(machine) = device.machine_id.filter(|id| !id.is_empty()) else { return failure(Error::Invalid) };
    // JSON escaping can expand a bounded UTF-8 chunk by up to six bytes per byte.
    let bytes = match tokio::time::timeout(Duration::from_secs(30), axum::body::to_bytes(request.into_body(), MAX_APPEND * 6 + (32 << 10))).await {
        Ok(Ok(bytes)) => bytes, Ok(Err(_)) => return failure(Error::Limit), Err(_) => return failure(Error::Busy),
    };
    let update: Append = match serde_json::from_slice(&bytes) { Ok(update) => update, Err(_) => return failure(Error::Invalid) };
    respond(service.run(move |store| {
        let entry = store.append(&access.account, &machine, &device.label, update, &|| access.valid())?;
        Ok(json!({"ok":true,"conversation":entry}))
    }).await)
}

#[derive(Deserialize)]
struct Read { generation: String, offset: u64, end: u64 }

async fn read(State(gate): State<Gate>, AxPath(id): AxPath<String>, axum::extract::Query(query): axum::extract::Query<Read>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(e) => return failure(e) };
    respond(service.run(move |store| {
        if !access.valid() { return Err(Error::Unauthorized); }
        let chunk = store.read(&access.account, &id, &query.generation, query.offset, query.end)?;
        if !access.valid() { return Err(Error::Unauthorized); }
        Ok(json!({"ok":true,"chunk":chunk}))
    }).await)
}

async fn delete(State(gate): State<Gate>, AxPath(id): AxPath<String>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(e) => return failure(e) };
    respond(service.run(move |store| {
        store.delete(&access.account, &id, &|| access.valid())?;
        Ok(json!({"ok":true}))
    }).await)
}

async fn restore(State(gate): State<Gate>, AxPath(id): AxPath<String>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (service, access) = match open(&gate, &headers) { Ok(v) => v, Err(e) => return failure(e) };
    respond(service.run(move |store| {
        let entry = store.restore(&access.account, &id, &|| access.valid())?;
        Ok(json!({"ok":true,"conversation":entry}))
    }).await)
}
