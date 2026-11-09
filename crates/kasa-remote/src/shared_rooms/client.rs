use kasa_device::device_auth::{DeviceCred, Stamp};
use kasa_socket::shared_rooms::{CheckpointSummary, CreateFork, CreateInvite, CreateRoom,
    PublishCheckpoint, PublishSession, SharedAppend, SharedContent, SharedFork, SharedForkInfo, SharedInvite,
    SharedRoom, SharedRoomList, SharedSession, UpdateRoom};
use reqwest::{Method, Url};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

const METADATA_LIMIT: usize = 4 << 20;
const PACKET_LIMIT: usize = 64 << 20;

pub fn run<T>(future: impl std::future::Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map_err(|_| Error::new(0, "shared_unreachable"))?.block_on(future)
}

pub fn loopback_credential(credential: DeviceCred, authorized: Arc<dyn Fn() -> bool + Send + Sync>) -> Result<Client, Error> {
    let url = Url::parse(&credential.relay).map_err(|_| Error::new(400, "bad_origin"))?;
    if url.scheme() != "http" || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1")) {
        return Err(Error::new(400, "bad_origin"));
    }
    Client::with_credential(credential, authorized)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub status: u16,
    pub code: String,
}

impl Error {
    fn new(status: u16, code: &str) -> Self { Self { status, code: code.into() } }
    pub fn message(&self) -> &'static str {
        match (self.status, self.code.as_str()) {
            (_, "account_changed") => "계정이 바뀌었어요. 공유방을 다시 열어 주세요.",
            (401, _) => "KASA 계정에 다시 로그인해 주세요.",
            (_, "fork_disabled") => "이 방은 포크를 허용하지 않아요.",
            (_, "desktop_only") => "대화 게시는 원본 PC에서 할 수 있어요.",
            (403, _) => "이 작업을 할 권한이 없어요.",
            (_, "route_missing") => "중계소의 공유방 기능을 업데이트해야 해요.",
            (404, _) => "공유방이 닫혔거나 참여 권한이 바뀌었어요.",
            (409, _) => "공유 자료가 바뀌었어요. 새로 확인한 뒤 다시 시도해 주세요.",
            (410, _) => "사용할 수 없는 초대 코드예요. 새 코드를 받아 주세요.",
            (413, _) => "공유 자료가 저장 한도를 넘었어요.",
            (429, _) => "중계소가 처리 중이에요. 잠시 뒤 다시 시도해 주세요.",
            (503, _) => "공유방 저장소에 연결하지 못했어요.",
            (_, "shared_unreachable") => "공유방 중계소에 연결하지 못했어요.",
            _ => "공유방 응답을 확인하지 못했어요.",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.message()) }
}
impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    credential: DeviceCred,
    stamp: Option<Stamp>,
    origin: Url,
    http: reqwest::Client,
    authorized: Arc<dyn Fn() -> bool + Send + Sync>,
    scope: String,
    auth_scope: String,
}

impl Client {
    pub fn capture() -> Result<Self, Error> {
        let (credential, stamp) = kasa_device::device_auth::capture()
            .ok_or_else(|| Error::new(401, "unauthorized"))?;
        let guard = stamp.clone();
        let mut client = Self::with_credential(credential,
            Arc::new(move || kasa_device::device_auth::stamp_is_current(&guard)))?;
        client.stamp = Some(stamp);
        Ok(client)
    }

    pub fn with_credential(credential: DeviceCred, authorized: Arc<dyn Fn() -> bool + Send + Sync>) -> Result<Self, Error> {
        let origin = Url::parse(&credential.relay).map_err(|_| Error::new(400, "bad_origin"))?;
        let loopback = matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "::1" | "[::1]"));
        if (origin.scheme() != "https" && !(origin.scheme() == "http" && loopback))
            || origin.host_str().is_none() || !origin.username().is_empty() || origin.password().is_some()
            || !matches!(origin.path(), "" | "/") || origin.query().is_some() || origin.fragment().is_some()
            || credential.account.is_empty() || credential.device_id.is_empty() || credential.token.is_empty()
            || !credential.token.bytes().all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b)) {
            return Err(Error::new(400, "bad_origin"));
        }
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none()).build().map_err(|_| Error::new(0, "shared_unreachable"))?;
        let scope = format!("{:x}", Sha256::digest(format!("{}\0{}", origin, credential.account).as_bytes()));
        let auth_scope = format!("{:x}", Sha256::digest(format!("{}\0{}\0{}", scope, credential.device_id, credential.token).as_bytes()));
        Ok(Self { credential, stamp: None, origin, http, authorized, scope, auth_scope })
    }

    pub fn scope(&self) -> &str { &self.scope }
    pub fn auth_scope(&self) -> &str { &self.auth_scope }
    pub fn account(&self) -> &str { &self.credential.account }
    pub fn is_current(&self) -> bool { (self.authorized)() }

    async fn request(&self, method: Method, path: &[&str], query: &[(&str, String)], body: Option<Value>, limit: usize) -> Result<Value, Error> {
        if !self.is_current() { return Err(Error::new(401, "account_changed")); }
        let mut url = self.origin.clone();
        {
            let mut parts = url.path_segments_mut().map_err(|_| Error::new(400, "bad_origin"))?;
            parts.clear().push("relay");
            for part in path {
                if part.is_empty() || matches!(*part, "." | "..") || part.chars().any(|c| c.is_control() || matches!(c, '/' | '\\')) {
                    return Err(Error::new(400, "shared_invalid"));
                }
                parts.push(part);
            }
        }
        if !query.is_empty() { url.query_pairs_mut().extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str()))); }
        let mut request = self.http.request(method, url).bearer_auth(&self.credential.token);
        if limit == PACKET_LIMIT { request = request.timeout(Duration::from_secs(120)); }
        if let Some(body) = body {
            let bytes = serde_json::to_vec(&body).map_err(|_| Error::new(400, "shared_invalid"))?;
            if bytes.len() > PACKET_LIMIT { return Err(Error::new(413, "shared_limit")); }
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json").body(bytes);
        }
        let mut response = request.send().await.map_err(|_| Error::new(0, "shared_unreachable"))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) { return Err(Error::new(status, "redirect_refused")); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::new(0, "shared_unreachable"))? {
            if !self.is_current() { return Err(Error::new(401, "account_changed")); }
            if bytes.len() + chunk.len() > limit { return Err(Error::new(413, "shared_limit")); }
            bytes.extend_from_slice(&chunk);
        }
        if !self.is_current() { return Err(Error::new(401, "account_changed")); }
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !(200..300).contains(&status) || value["ok"] != true {
            if status == 401 { if let Some(stamp) = &self.stamp { kasa_device::device_auth::reject(stamp); } }
            let code = value["error"].as_str().filter(|s| s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
                .unwrap_or(if status == 404 { "route_missing" } else { "shared_response" });
            return Err(Error::new(status, code));
        }
        Ok(value)
    }

    pub async fn list(&self) -> Result<SharedRoomList, Error> {
        decode(self.request(Method::GET, &["shared-rooms"], &[], None, METADATA_LIMIT).await?)
    }
    pub async fn room(&self, room: &str) -> Result<SharedRoom, Error> {
        field(self.request(Method::GET, &["shared-rooms", room], &[], None, METADATA_LIMIT).await?, "room")
    }
    pub async fn create(&self, request: &CreateRoom) -> Result<SharedRoom, Error> {
        field(self.request(Method::POST, &["shared-rooms"], &[], Some(encode(request)?), METADATA_LIMIT).await?, "room")
    }
    pub async fn update(&self, room: &str, request: &UpdateRoom) -> Result<SharedRoom, Error> {
        field(self.request(Method::PATCH, &["shared-rooms", room], &[], Some(encode(request)?), METADATA_LIMIT).await?, "room")
    }
    pub async fn delete_room(&self, room: &str) -> Result<(), Error> {
        self.request(Method::DELETE, &["shared-rooms", room], &[], None, METADATA_LIMIT).await.map(|_| ())
    }
    pub async fn invite(&self, room: &str, request: &CreateInvite) -> Result<SharedInvite, Error> {
        field(self.request(Method::POST, &["shared-rooms", room, "invites"], &[], Some(encode(request)?), METADATA_LIMIT).await?, "invite")
    }
    pub async fn revoke_invite(&self, room: &str, invite: &str) -> Result<(), Error> {
        self.request(Method::DELETE, &["shared-rooms", room, "invites", invite], &[], None, METADATA_LIMIT).await.map(|_| ())
    }
    pub async fn join(&self, token: &str) -> Result<SharedRoom, Error> {
        field(self.request(Method::POST, &["shared-room-invites", "accept"], &[], Some(json!({"token":token})), METADATA_LIMIT).await?, "room")
    }
    pub async fn remove_member(&self, room: &str, account: &str) -> Result<(), Error> {
        self.request(Method::DELETE, &["shared-rooms", room, "members", account], &[], None, METADATA_LIMIT).await.map(|_| ())
    }
    pub async fn publish(&self, room: &str, request: &PublishSession) -> Result<SharedSession, Error> {
        field(self.request(Method::POST, &["shared-rooms", room, "sessions"], &[], Some(encode(request)?), METADATA_LIMIT).await?, "session")
    }
    pub async fn append(&self, room: &str, session: &str, request: &SharedAppend) -> Result<SharedSession, Error> {
        field(self.request(Method::POST, &["shared-rooms", room, "sessions", session, "append"], &[], Some(encode(request)?), METADATA_LIMIT).await?, "session")
    }
    pub async fn content(&self, room: &str, session: &str, generation: &str, offset: u64, end: u64) -> Result<SharedContent, Error> {
        field(self.request(Method::GET, &["shared-rooms", room, "sessions", session, "content"],
            &[("generation",generation.into()),("offset",offset.to_string()),("end",end.to_string())], None, METADATA_LIMIT).await?, "content")
    }
    pub async fn checkpoint(&self, room: &str, session: &str, request: &PublishCheckpoint) -> Result<CheckpointSummary, Error> {
        field(self.request(Method::POST, &["shared-rooms", room, "sessions", session, "checkpoints"], &[], Some(encode(request)?), PACKET_LIMIT).await?, "checkpoint")
    }
    pub async fn delete_session(&self, room: &str, session: &str) -> Result<(), Error> {
        self.request(Method::DELETE, &["shared-rooms", room, "sessions", session], &[], None, METADATA_LIMIT).await.map(|_| ())
    }
    pub async fn fork(&self, room: &str, session: &str, request: &CreateFork) -> Result<SharedForkInfo, Error> {
        field(self.request(Method::POST, &["shared-rooms", room, "sessions", session, "fork"], &[], Some(encode(request)?), METADATA_LIMIT).await?, "fork")
    }
    pub async fn forks(&self) -> Result<Vec<SharedForkInfo>, Error> {
        field(self.request(Method::GET, &["shared-forks"], &[], None, METADATA_LIMIT).await?, "forks")
    }
    pub async fn read_fork(&self, fork: &str) -> Result<SharedFork, Error> {
        field(self.request(Method::GET, &["shared-forks", fork], &[], None, PACKET_LIMIT).await?, "fork")
    }
    pub async fn delete_fork(&self, fork: &str) -> Result<(), Error> {
        self.request(Method::DELETE, &["shared-forks", fork], &[], None, METADATA_LIMIT).await.map(|_| ())
    }
}

fn encode(value: &impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::new(400, "shared_invalid"))
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::new(502, "shared_response"))
}
fn field<T: DeserializeOwned>(mut value: Value, name: &str) -> Result<T, Error> { decode(value[name].take()) }

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(relay: &str) -> DeviceCred {
        DeviceCred { relay: relay.into(), account: "alice".into(), device_id: "fixture".into(),
            token: "fixture_token".into(), display_name: None }
    }

    #[test]
    fn credentials_are_bound_to_an_explicit_origin() {
        for origin in ["http://example.com", "https://user:password@example.com", "https://example.com/path", "https://example.com/?token=x"] {
            assert!(Client::with_credential(credential(origin), Arc::new(|| true)).is_err());
        }
        assert!(Client::with_credential(credential("http://127.0.0.1:8912"), Arc::new(|| true)).is_ok());
    }

    #[test]
    fn expired_scope_never_starts_a_request() {
        let client = Client::with_credential(credential("http://127.0.0.1:1"), Arc::new(|| false)).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert_eq!(rt.block_on(client.list()).unwrap_err().code, "account_changed");
    }

    #[test]
    fn reauthentication_keeps_storage_scope_but_rejects_old_inflight_scope() {
        let first = credential("https://example.com");
        let mut renewed = first.clone();
        renewed.token = "renewed_token".into();
        let a = Client::with_credential(first, Arc::new(|| true)).unwrap();
        let b = Client::with_credential(renewed.clone(), Arc::new(|| true)).unwrap();
        assert_eq!(a.scope(), b.scope());
        assert_ne!(a.auth_scope(), b.auth_scope());
        renewed.account = "bob".into();
        let other = Client::with_credential(renewed, Arc::new(|| true)).unwrap();
        assert_ne!(a.scope(), other.scope());
    }

    fn server(reply: &'static str, before_reply: impl FnOnce() + Send + 'static) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap(); header.push(byte[0]);
            }
            assert!(header.starts_with(b"GET /relay/shared-rooms "));
            before_reply();
            stream.write_all(reply.as_bytes()).unwrap();
        });
        (origin, worker)
    }

    #[test]
    fn redirects_never_forward_the_account_credential() {
        let (origin, worker) = server("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", || {});
        let client = Client::with_credential(credential(&origin), Arc::new(|| true)).unwrap();
        assert_eq!(run(client.list()).unwrap_err().code, "redirect_refused");
        worker.join().unwrap();
    }

    #[test]
    fn revoked_credentials_discard_an_inflight_response() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let current = Arc::new(AtomicBool::new(true));
        let flip = current.clone();
        let (origin, worker) = server("HTTP/1.1 200 OK\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"ok\":true,\"rooms\":[]}", move || { flip.store(false, Ordering::SeqCst); });
        let client = Client::with_credential(credential(&origin), Arc::new(move || current.load(Ordering::SeqCst))).unwrap();
        assert_eq!(run(client.list()).unwrap_err().code, "account_changed");
        worker.join().unwrap();
    }
}
