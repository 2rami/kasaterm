//! 미리보기 하나가 쓰는 루프백 HTTP Range 창구. 플레이어는 [`Stream::url`] 만 받아 필요한 구간을 묻고,
//! 여기는 그 구간을 원본에서 조각으로 가져와 흘린다 — 이 기기 카드면 등록소를 직접, 다른 기기 카드면
//! 그 기기의 인증된 `/term/inline-media/file?id&offset&len` 을. 파일 전체를 메모리나 디스크에 모으지 않는다.
//!
//! - 주소는 `127.0.0.1` 임의 포트와 추측할 수 없는 경로다. 경로가 틀리면 404 말고는 아무것도 알리지 않는다.
//! - GET/HEAD, 단일 Range(`bytes=a-b`·`a-`·`-n`), 200/206/416. 여러 구간·틀린 형식은 무시하고 전체로 답한다.
//! - 조각은 받는 쪽이 읽는 만큼만 원본에 묻는다. 플레이어는 정보·재생용 구간을 동시에 여럿 열 수 있어 새
//!   요청이 왔다고 옛 요청을 끊지 않는다 — 그 응답의 연결이 끊기면 그 요청만, [`Stream`] 이 사라지면 전부 끊는다.
//! - 한 미리보기는 열 때의 원본 기기·id·sha256·크기에 고정된다. 처음부터 이어 읽은 바이트는 해시를 굴려
//!   끝에서 sha256 과 맞춰 본다 — 원본 쪽 확인(`kasa_socket::inline_media::Store::read_chunk`)과 별개다.

use anyhow::Context;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use kasa_socket::inline_media::{self as registry, Media, Store};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::watch;

/// 원본에 한 번에 묻는 크기. 첫 바이트를 빨리 내면서 원격 왕복 수를 줄이는 사이.
const PIECE: usize = 512 << 10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

type Auth = Arc<dyn Fn() -> Option<String> + Send + Sync>;

enum Source {
    Local(Store),
    Remote { base: String, auth: Auth },
}

/// 열려 있는 동안만 주소가 산다. 버리면 대기·읽기·연결을 끊되 부르는 쪽(GUI)을 기다리게 하지 않는다.
pub struct Stream {
    url: String,
    shared: Arc<Shared>,
}

struct Shared {
    source: Source,
    id: String,
    sha256: String,
    size: u64,
    mime: &'static str,
    path: String,
    host: String,
    closed: watch::Sender<bool>,
    received: AtomicU64,
    error: Mutex<Option<Failure>>,
    check: Mutex<Check>,
}

struct Failure {
    message: String,
    /// 원본이 틀린 경우. 다음 조각이 잘 와도 지우지 않는다.
    sticky: bool,
}

#[derive(Default)]
struct Check {
    hasher: Sha256,
    upto: u64,
    failed: bool,
}

struct Refusal {
    status: StatusCode,
    message: String,
    sticky: bool,
}

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>, sticky: bool) -> Self {
        Self { status, message: message.into(), sticky }
    }
    fn cancelled() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "cancelled", false)
    }
    fn mismatch() -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "미디어 내용이 원본과 달라요", true)
    }
}

impl Stream {
    /// `source` 가 `None` 이면 이 기기 등록소, `Some(base)` 면 그 기기에 이미 맺은 연결의 인증으로 묻는다.
    pub fn open(source: Option<&str>, media: &Media) -> anyhow::Result<Stream> {
        Self::start(Self::source(source)?, media)
    }

    /// 원본 파일 전체를 `dest` 에 받는다 — 사람이 카드의 「저장」을 눌렀을 때만. 재생 창구와 같은 조각 읽기와
    /// sha256 확인을 지나므로 다른 기기 카드도 그 기기의 인증으로 받고, 내용이 틀리면 파일을 남기지 않는다.
    /// 받는 동안 막히므로 GUI 스레드에서 부르지 않는다.
    pub fn save(source: Option<&str>, media: &Media, dest: &std::path::Path) -> anyhow::Result<()> {
        Self::save_from(Self::source(source)?, media, dest)
    }

    fn save_from(source: Source, media: &Media, dest: &std::path::Path) -> anyhow::Result<()> {
        use std::io::Write;
        let rt = runtime()?;
        if matches!(source, Source::Remote { .. }) {
            client()?;
        }
        let shared = Shared {
            source,
            id: media.id.clone(),
            sha256: media.sha256.clone(),
            size: media.size,
            mime: "",
            path: String::new(),
            host: String::new(),
            closed: watch::channel(false).0,
            received: AtomicU64::new(0),
            error: Mutex::new(None),
            check: Mutex::new(Check::default()),
        };
        let mut part = dest.as_os_str().to_owned();
        part.push(".part");
        let part = std::path::PathBuf::from(part);
        let written = rt.block_on(async {
            let mut file = std::fs::File::create(&part)?;
            let mut offset = 0;
            while offset < shared.size {
                let bytes = shared.fetch(offset, piece(offset, shared.size - 1)).await.map_err(|r| anyhow::anyhow!(r.message))?;
                file.write_all(&bytes)?;
                offset += bytes.len() as u64;
            }
            file.sync_all()?;
            anyhow::Ok(())
        });
        match written {
            Ok(()) => std::fs::rename(&part, dest).context("받은 파일을 옮기지 못했어요"),
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                Err(e)
            }
        }
    }

    fn source(source: Option<&str>) -> anyhow::Result<Source> {
        Ok(match source {
            None => Source::Local(registry::store()),
            Some(raw) => {
                let url = reqwest::Url::parse(raw).context("원본 기기 주소가 올바르지 않아요")?;
                anyhow::ensure!(matches!(url.scheme(), "http" | "https"), "원본 기기 주소가 올바르지 않아요");
                let lookup = raw.to_string();
                Source::Remote {
                    base: raw.trim_end_matches('/').to_string(),
                    auth: Arc::new(move || crate::remote::connection_auth_token(&lookup)),
                }
            }
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// 가장 최근의 실패. 잠깐 끊긴 원본은 다음 조각이 오면 지워지고, 내용이 틀린 원본은 남는다.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|f| f.message.clone())
    }

    /// 원본에서 받아 흘려보낸 바이트 합. 같은 구간을 다시 물으면 다시 센다.
    pub fn received_bytes(&self) -> u64 {
        self.shared.received.load(Ordering::Relaxed)
    }

    fn start(source: Source, media: &Media) -> anyhow::Result<Stream> {
        let (mime, ext) = registry::stream_format(media).context("재생할 수 없는 미디어 정보예요")?;
        let rt = runtime()?;
        if matches!(source, Source::Remote { .. }) {
            client()?;
        }
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
        // 확장자를 남겨야 플레이어가 MIME 없이도 컨테이너를 알아본다.
        let path = format!("/{}/media.{ext}", random_token()?);
        let url = format!("http://{host}{path}");
        let shared = Arc::new(Shared {
            source,
            id: media.id.clone(),
            sha256: media.sha256.clone(),
            size: media.size,
            mime,
            path,
            host,
            closed: watch::channel(false).0,
            received: AtomicU64::new(0),
            error: Mutex::new(None),
            check: Mutex::new(Check::default()),
        });
        let listener = {
            let _enter = rt.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        let app = axum::Router::new().fallback(serve).with_state(shared.clone());
        let mut stop = shared.closed.subscribe();
        rt.spawn(async move {
            let shutdown = async move {
                let _ = stop.wait_for(|closed| *closed).await;
            };
            let _ = axum::serve(listener, app).with_graceful_shutdown(shutdown).await;
        });
        Ok(Stream { url, shared })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.closed.send_replace(true);
    }
}

fn runtime() -> anyhow::Result<&'static tokio::runtime::Runtime> {
    static RT: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kasa-media-stream")
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!("미디어 전송 런타임: {e}"))
}

/// 모든 미리보기가 함께 쓴다 — 조각마다 클라이언트를 만들면 원본 기기와의 연결을 매번 새로 맺는다.
fn client() -> anyhow::Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("미디어 전송 클라이언트: {e}"))
}

fn random_token() -> anyhow::Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow::anyhow!("난수를 만들지 못했어요"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn plain(status: StatusCode) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], status.canonical_reason().unwrap_or_default()).into_response()
}

/// 브라우저 페이지가 보낸 요청은 받지 않는다 — 플레이어는 Origin·Sec-Fetch-Site 를 싣지 않는다.
/// Host 를 보는 것은 DNS 리바인딩으로 다른 이름이 이 포트를 가리키는 경우를 막는다.
fn foreign(headers: &HeaderMap, host: &str) -> bool {
    let value = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    value(header::HOST).is_none_or(|h| !h.eq_ignore_ascii_case(host))
        || headers.contains_key(header::ORIGIN)
        || headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|site| site != "none")
}

#[derive(Debug, PartialEq, Eq)]
enum Want {
    Whole,
    Part(u64, u64),
    Unsatisfiable,
}

fn digits(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn parse_range(value: Option<&str>, size: u64) -> Want {
    // 단위 이름은 대소문자를 가리지 않는다(RFC 9110 §14.1).
    let Some(spec) = value
        .map(str::trim)
        .and_then(|v| v.get(..6).filter(|unit| unit.eq_ignore_ascii_case("bytes=")).map(|_| &v[6..]))
    else {
        return Want::Whole;
    };
    if spec.contains(',') {
        return Want::Whole;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return Want::Whole;
    };
    let (a, b) = (a.trim(), b.trim());
    let last = size - 1;
    if a.is_empty() {
        return match digits(b) {
            Some(0) => Want::Unsatisfiable,
            Some(n) => Want::Part(size.saturating_sub(n), last),
            None => Want::Whole,
        };
    }
    let Some(start) = digits(a) else {
        return Want::Whole;
    };
    let end = match (b.is_empty(), digits(b)) {
        (true, _) => last,
        (false, Some(end)) if end >= start => end.min(last),
        _ => return Want::Whole,
    };
    if start > last {
        Want::Unsatisfiable
    } else {
        Want::Part(start, end)
    }
}

async fn serve(State(shared): State<Arc<Shared>>, req: axum::extract::Request) -> Response {
    let (parts, _) = req.into_parts();
    if !same(parts.uri.path(), &shared.path) {
        return plain(StatusCode::NOT_FOUND);
    }
    if *shared.closed.borrow() {
        return plain(StatusCode::GONE);
    }
    if foreign(&parts.headers, &shared.host) {
        return plain(StatusCode::FORBIDDEN);
    }
    if parts.method != Method::GET && parts.method != Method::HEAD {
        let mut resp = plain(StatusCode::METHOD_NOT_ALLOWED);
        resp.headers_mut().insert(header::ALLOW, "GET, HEAD".parse().unwrap());
        return resp;
    }
    let size = shared.size;
    // Range 는 GET 에만 정의돼 있다(RFC 9110 §14.2) — HEAD 는 Range 가 실려 와도 전체로 답한다.
    let range = (parts.method == Method::GET)
        .then(|| parts.headers.get(header::RANGE).and_then(|v| v.to_str().ok()))
        .flatten();
    let (status, start, end) = match parse_range(range, size) {
        Want::Whole => (StatusCode::OK, 0, size - 1),
        Want::Part(start, end) => (StatusCode::PARTIAL_CONTENT, start, end),
        Want::Unsatisfiable => {
            let mut resp = plain(StatusCode::RANGE_NOT_SATISFIABLE);
            resp.headers_mut().insert(header::CONTENT_RANGE, format!("bytes */{size}").parse().unwrap());
            return resp;
        }
    };
    let body = if parts.method == Method::HEAD {
        Body::empty()
    } else {
        // 첫 조각을 먼저 받아 둔다 — 원본이 없거나 꺼졌으면 머리를 보내기 전에 오류 상태로 답할 수 있다.
        let mut cancel = shared.closed.subscribe();
        let first = match shared.fetch_or_cancel(start, piece(start, end), &mut cancel).await {
            Ok(bytes) => bytes,
            Err(refusal) => return plain(refusal.status),
        };
        let state = Piece { shared: shared.clone(), next: start + first.len() as u64, end, cancel, first: Some(first) };
        Body::from_stream(futures_util::stream::unfold(state, |mut st| async move {
            if let Some(bytes) = st.first.take() {
                return Some((Ok(bytes), st));
            }
            if st.next > st.end {
                return None;
            }
            match st.shared.fetch_or_cancel(st.next, piece(st.next, st.end), &mut st.cancel).await {
                Ok(bytes) => {
                    st.next += bytes.len() as u64;
                    Some((Ok(bytes), st))
                }
                Err(refusal) => {
                    st.next = st.end + 1;
                    Some((Err(std::io::Error::other(refusal.message)), st))
                }
            }
        }))
    };
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, shared.mime.parse().unwrap());
    h.insert(header::CONTENT_LENGTH, (end - start + 1).into());
    h.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}").parse().unwrap());
    }
    resp
}

fn piece(start: u64, end: u64) -> usize {
    (end - start + 1).min(PIECE as u64) as usize
}

/// 응답 본문 하나의 진행. 연결이 끊기면 서버가 이것을 버리고, 그때 기다리던 원본 요청도 함께 버려진다.
struct Piece {
    shared: Arc<Shared>,
    next: u64,
    end: u64,
    cancel: watch::Receiver<bool>,
    first: Option<Bytes>,
}

impl Shared {
    /// [`Stream`] 을 버리면 기다리던 원본 요청을 바로 놓는다 — 받는 쪽이 아직 연결을 쥐고 있어도.
    async fn fetch_or_cancel(&self, offset: u64, len: usize, closed: &mut watch::Receiver<bool>) -> Result<Bytes, Refusal> {
        tokio::select! {
            result = self.fetch(offset, len) => result,
            _ = closed.wait_for(|c| *c) => Err(Refusal::cancelled()),
        }
    }

    async fn fetch(&self, offset: u64, len: usize) -> Result<Bytes, Refusal> {
        if self.check.lock().unwrap_or_else(|e| e.into_inner()).failed {
            return Err(Refusal::mismatch());
        }
        let result = match &self.source {
            Source::Local(store) => {
                let (store, id) = (store.clone(), self.id.clone());
                match tokio::task::spawn_blocking(move || store.read_chunk(&id, offset, len)).await {
                    Ok(Ok(bytes)) => Ok(bytes),
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                        Err(Refusal::new(StatusCode::NOT_FOUND, "미디어를 찾을 수 없어요", true))
                    }
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                        Err(Refusal::new(StatusCode::BAD_GATEWAY, "미디어 원본이 등록 때와 달라요", true))
                    }
                    _ => Err(Refusal::new(StatusCode::BAD_GATEWAY, "미디어 원본을 읽지 못했어요", false)),
                }
            }
            Source::Remote { base, auth } => self.fetch_remote(base, auth, offset, len).await,
        }
        .and_then(|bytes| {
            if bytes.len() == len {
                Ok(bytes)
            } else {
                Err(Refusal::new(StatusCode::BAD_GATEWAY, "원본 조각 길이가 달라요", false))
            }
        })
        .and_then(|bytes| self.roll(offset, &bytes).map(|()| bytes));
        let mut error = self.error.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(bytes) => {
                self.received.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if error.as_ref().is_some_and(|f| !f.sticky) {
                    *error = None;
                }
                Ok(Bytes::from(bytes))
            }
            Err(refusal) => {
                if refusal.sticky || error.as_ref().is_none_or(|f| !f.sticky) {
                    *error = Some(Failure { message: refusal.message.clone(), sticky: refusal.sticky });
                }
                Err(refusal)
            }
        }
    }

    async fn fetch_remote(&self, base: &str, auth: &Auth, offset: u64, len: usize) -> Result<Vec<u8>, Refusal> {
        let unreachable = |e: reqwest::Error| {
            Refusal::new(StatusCode::BAD_GATEWAY, format!("원본 기기에 닿지 못했어요: {}", e.without_url()), false)
        };
        let client = client().map_err(|e| Refusal::new(StatusCode::BAD_GATEWAY, e.to_string(), false))?;
        let url = format!("{base}/term/inline-media/file?id={}&offset={offset}&len={len}", self.id);
        let mut req = client.get(url).timeout(REQUEST_TIMEOUT);
        if let Some(token) = auth() {
            req = req.header("x-kasa-token", token);
        }
        let mut resp = req.send().await.map_err(unreachable)?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(Refusal::new(StatusCode::NOT_FOUND, "원본 기기에 미디어가 없어요", true));
        }
        if !status.is_success() {
            return Err(Refusal::new(StatusCode::BAD_GATEWAY, format!("원본 기기가 거절했어요 (HTTP {})", status.as_u16()), false));
        }
        if resp.content_length().is_some_and(|n| n != len as u64) {
            return Err(Refusal::new(StatusCode::BAD_GATEWAY, "원본 조각 길이가 달라요", false));
        }
        let mut bytes = Vec::with_capacity(len);
        while let Some(chunk) = resp.chunk().await.map_err(unreachable)? {
            if bytes.len() + chunk.len() > len {
                return Err(Refusal::new(StatusCode::BAD_GATEWAY, "원본 조각 길이가 달라요", false));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// 처음부터 이어지는 바이트만 해시에 더한다. 끝에 닿았는데 등록 sha256 과 다르면 이 미리보기는 더 내주지 않는다.
    fn roll(&self, offset: u64, bytes: &[u8]) -> Result<(), Refusal> {
        let mut check = self.check.lock().unwrap_or_else(|e| e.into_inner());
        let end = offset + bytes.len() as u64;
        if offset <= check.upto && end > check.upto {
            let from = (check.upto - offset) as usize;
            check.hasher.update(&bytes[from..]);
            check.upto = end;
            if check.upto == self.size {
                let digest = std::mem::take(&mut check.hasher).finalize();
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                if hex != self.sha256 {
                    check.failed = true;
                    return Err(Refusal::mismatch());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_socket::inline_media::Probe;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("media-stream-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saving_copies_every_piece_and_leaves_nothing_when_the_content_differs() {
        let dir = Dir::new("save");
        let (store, media, bytes) = registered(&dir, PIECE * 2 + 1234);
        let dest = dir.0.join("받은 clip.mp4");
        Stream::save_from(Source::Local(store.clone()), &media, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        let wrong = Media { sha256: "0".repeat(64), ..media };
        let dest = dir.0.join("wrong.mp4");
        assert!(Stream::save_from(Source::Local(store), &wrong, &dest).is_err());
        assert!(!dest.exists() && !dir.0.join("wrong.mp4.part").exists(), "틀린 내용은 반쯤 받은 파일도 안 남긴다");
    }

    fn mp4_bytes(len: usize, seed: u32) -> Vec<u8> {
        let mut bytes = b"\0\0\0\x18ftypisom\0\0\0\0isomavc1".to_vec();
        let mut x = seed.wrapping_mul(2654435761).max(1);
        while bytes.len() < len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            bytes.push(x as u8);
        }
        bytes.truncate(len);
        bytes
    }

    fn registered(dir: &Dir, len: usize) -> (Store, Media, Vec<u8>) {
        let store = Store::at(dir.0.join("store"));
        let bytes = mp4_bytes(len, len as u32);
        let src = dir.0.join("clip.mp4");
        std::fs::write(&src, &bytes).unwrap();
        let media = store.register(&src, Probe::default()).unwrap();
        (store, media, bytes)
    }

    fn media_for(bytes: &[u8]) -> Media {
        Media {
            version: registry::VERSION,
            id: "0123456789abcdef".into(),
            kind: "video".into(),
            name: "clip.mp4".into(),
            mime: "video/mp4".into(),
            size: bytes.len() as u64,
            sha256: Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect(),
            duration_ms: None,
            width: None,
            height: None,
            poster: false,
        }
    }

    fn http() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .build()
            .unwrap()
    }

    async fn get(url: &str, range: Option<&str>) -> (StatusCode, reqwest::header::HeaderMap, Vec<u8>) {
        let mut req = http().get(url);
        if let Some(range) = range {
            req = req.header("range", range);
        }
        let resp = req.send().await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        (StatusCode::from_u16(status.as_u16()).unwrap(), headers, resp.bytes().await.unwrap().to_vec())
    }

    /// 머리를 손으로 적는 요청 — reqwest 는 Host 를 제 마음대로 채운다.
    async fn raw(url: &str, head: &str) -> String {
        let rest = url.trim_start_matches("http://");
        let (addr, path) = rest.split_at(rest.find('/').unwrap());
        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(format!("GET {path} HTTP/1.1\r\n{head}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = Vec::new();
        sock.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    async fn until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..150 {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        done()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn local_card_answers_head_ranges_and_refusals() {
        let dir = Dir::new("local");
        let (store, media, bytes) = registered(&dir, 3 * PIECE + 77);
        let size = bytes.len() as u64;
        let stream = Stream::start(Source::Local(store.clone()), &media).unwrap();
        let url = stream.url().to_string();
        let token = url.split('/').nth(3).unwrap();
        assert!(url.starts_with("http://127.0.0.1:") && url.ends_with("/media.mp4"), "{url}");
        assert!(token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()));
        let other = Stream::start(Source::Local(store), &media).unwrap();
        assert_ne!(other.url().split('/').nth(3), Some(token));

        let head = http().head(&url).send().await.unwrap();
        assert_eq!(head.status().as_u16(), 200);
        let h = head.headers().clone();
        assert_eq!(h["content-length"], size.to_string().as_str());
        assert_eq!((h["accept-ranges"].to_str().unwrap(), h["content-type"].to_str().unwrap()), ("bytes", "video/mp4"));
        assert_eq!(h["cache-control"], "no-store");
        assert!(head.bytes().await.unwrap().is_empty());
        let ranged = http().head(&url).header("range", "bytes=0-1").send().await.unwrap();
        assert_eq!(ranged.status().as_u16(), 200, "HEAD 는 Range 를 무시한다");
        assert_eq!(ranged.headers()["content-length"], size.to_string().as_str());
        assert!(!ranged.headers().contains_key("content-range"));
        assert!(ranged.bytes().await.unwrap().is_empty());
        assert_eq!(stream.received_bytes(), 0, "HEAD 는 원본을 읽지 않는다");

        let (status, h, body) = get(&url, Some("bytes=0-1")).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[..2]));
        assert_eq!(h["content-range"], format!("bytes 0-1/{size}").as_str());
        assert_eq!(stream.received_bytes(), 2, "첫 바이트는 요청한 만큼만");

        let seek = size - PIECE as u64 - 10;
        let (status, h, body) = get(&url, Some(&format!("bytes={seek}-"))).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[seek as usize..]));
        assert_eq!(h["content-range"], format!("bytes {seek}-{}/{size}", size - 1).as_str());
        let (status, _, body) = get(&url, Some("bytes=-10")).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[bytes.len() - 10..]));
        let (status, _, body) = get(&url, Some("bytes=100-199")).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[100..200]));
        let (status, h, body) = get(&url, Some("BYTES=100-101")).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[100..102]));
        assert_eq!(h["content-range"], format!("bytes 100-101/{size}").as_str());
        let (status, _, body) = get(&url, None).await;
        assert_eq!((status, body == bytes), (StatusCode::OK, true));
        for ignored in ["bytes=9-3", "bytes=0-1,4-5", "items=0-1", "bytes=+1-2", "bytes=x-"] {
            assert_eq!(get(&url, Some(ignored)).await.0, StatusCode::OK, "{ignored}");
        }
        for unsatisfiable in [format!("bytes={size}-"), "bytes=-0".into(), format!("bytes={}-{}", size + 5, size + 9)] {
            let (status, h, _) = get(&url, Some(&unsatisfiable)).await;
            assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{unsatisfiable}");
            assert_eq!(h["content-range"], format!("bytes */{size}").as_str());
        }

        let base = url.rsplit_once('/').unwrap().0.rsplit_once('/').unwrap().0;
        let mut forged = token.to_string();
        forged.replace_range(63.., if token.ends_with('0') { "1" } else { "0" });
        for wrong in [format!("{base}/{forged}/media.mp4"), format!("{base}/{token}/media.mov"), format!("{base}/"), format!("{base}/{token}")] {
            assert_eq!(get(&wrong, Some("bytes=0-1")).await.0, StatusCode::NOT_FOUND, "{wrong}");
        }
        assert_eq!(http().post(&url).send().await.unwrap().status().as_u16(), 405);
        let host = &base["http://".len()..];
        assert!(raw(&url, &format!("Host: {host}\r\nRange: bytes=0-1\r\n")).await.starts_with("HTTP/1.1 206"));
        assert!(raw(&url, "Host: rebound.example\r\n").await.starts_with("HTTP/1.1 403"));
        assert!(raw(&url, &format!("Host: {host}\r\nOrigin: https://evil.example\r\n")).await.starts_with("HTTP/1.1 403"));
        assert!(raw(&url, &format!("Host: {host}\r\nSec-Fetch-Site: cross-site\r\n")).await.starts_with("HTTP/1.1 403"));
        let wrong_token = raw(&format!("{base}/{forged}/media.mp4"), &format!("Host: {host}\r\n")).await;
        assert!(wrong_token.starts_with("HTTP/1.1 404") && !wrong_token.contains(token));
        assert!(stream.error().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn local_source_rewritten_with_the_same_length_is_refused() {
        use std::io::Write;
        let dir = Dir::new("forged");
        let (store, media, bytes) = registered(&dir, PIECE + 5);
        let stream = Stream::start(Source::Local(store.clone()), &media).unwrap();
        assert_eq!(get(stream.url(), Some("bytes=0-1")).await.0, StatusCode::PARTIAL_CONTENT);

        let blob = store.media_path(&media.id).unwrap();
        let then = std::fs::metadata(&blob).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        // 봉인(0o400)을 풀어야 쓸 수 있다. Windows 는 봉인이 없다.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut forged = bytes.clone();
        forged[PIECE] ^= 0x55;
        let mut f = std::fs::OpenOptions::new().write(true).open(&blob).unwrap();
        f.write_all(&forged).unwrap();
        f.set_modified(then).unwrap();
        drop(f);
        assert_eq!(std::fs::metadata(&blob).unwrap().len(), bytes.len() as u64);

        assert_eq!(get(stream.url(), Some("bytes=0-1")).await.0, StatusCode::BAD_GATEWAY);
        assert_eq!(stream.error().as_deref(), Some("미디어 원본이 등록 때와 달라요"));
    }

    #[derive(Clone)]
    enum Reply {
        Bytes(Vec<u8>),
        Stall,
        Status(u16),
    }

    type Hits = Arc<Mutex<Vec<(u64, usize, Option<String>)>>>;

    struct Origin {
        base: String,
        hits: Hits,
        /// 응답을 기다리던 중 받는 쪽이 끊고 나간 요청 수.
        gone: Arc<AtomicUsize>,
    }

    impl Origin {
        fn hits(&self) -> Vec<(u64, usize, Option<String>)> {
            self.hits.lock().unwrap().clone()
        }
    }

    /// 원본 기기의 `/term/inline-media/file` 을 흉내 낸다. 요청마다 연결을 닫아 끊김을 바로 볼 수 있게 한다.
    async fn origin(reply: impl Fn(u64, usize) -> Reply + Send + Sync + 'static) -> Origin {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Hits = Default::default();
        let gone = Arc::new(AtomicUsize::new(0));
        let reply = Arc::new(reply);
        let (log, left) = (hits.clone(), gone.clone());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let (log, left, reply) = (log.clone(), left.clone(), reply.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let target = text.split(' ').nth(1).unwrap_or_default().to_string();
                    assert!(target.starts_with("/term/inline-media/file?id=0123456789abcdef&"), "{target}");
                    let param = |key: &str| {
                        target.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{key}="))).and_then(|v| v.parse::<u64>().ok())
                    };
                    let (offset, len) = (param("offset").unwrap(), param("len").unwrap() as usize);
                    let token = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("x-kasa-token: ").map(str::to_string));
                    log.lock().unwrap().push((offset, len, token));
                    match reply(offset, len) {
                        Reply::Bytes(body) => {
                            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(&body).await;
                        }
                        Reply::Status(code) => {
                            let head = format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            let _ = sock.write_all(head.as_bytes()).await;
                        }
                        Reply::Stall => {
                            while !matches!(sock.read(&mut buf).await, Ok(0) | Err(_)) {}
                            left.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        Origin { base, hits, gone }
    }

    fn serving(bytes: Arc<Vec<u8>>) -> impl Fn(u64, usize) -> Reply + Send + Sync + 'static {
        move |offset, len| Reply::Bytes(bytes[offset as usize..offset as usize + len].to_vec())
    }

    fn remote(origin: &Origin, media: &Media) -> Stream {
        let source = Source::Remote { base: origin.base.clone(), auth: Arc::new(|| Some("connection-token".into())) };
        Stream::start(source, media).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_card_asks_only_for_requested_pieces_with_the_connection_token() {
        let bytes = Arc::new(mp4_bytes(16 * PIECE + 3, 21));
        let media = media_for(&bytes);
        let size = bytes.len() as u64;
        let origin = origin(serving(bytes.clone())).await;
        let stream = remote(&origin, &media);

        assert_eq!(http().head(stream.url()).send().await.unwrap().status().as_u16(), 200);
        assert!(origin.hits().is_empty(), "HEAD 는 원본 기기에 묻지 않는다");

        let (status, _, body) = get(stream.url(), Some("bytes=0-1")).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[..2]));
        assert_eq!(origin.hits(), vec![(0, 2, Some("connection-token".into()))]);

        // 끝의 moov 로 건너뛴다 — 그 앞을 받지 않는다.
        let seek = size - PIECE as u64 - 10;
        let (status, _, body) = get(stream.url(), Some(&format!("bytes={seek}-"))).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &bytes[seek as usize..]));
        let hits: Vec<_> = origin.hits()[1..].iter().map(|(o, l, _)| (*o, *l)).collect();
        assert_eq!(hits, vec![(seek, PIECE), (seek + PIECE as u64, 10)]);

        // 앞부분만 읽고 끊으면 원본에 더 묻지 않는다.
        let resp = http().get(stream.url()).header("range", "bytes=0-").send().await.unwrap();
        let mut resp = resp;
        let mut got = 0;
        while got < PIECE + 1 {
            got += resp.chunk().await.unwrap().unwrap().len();
        }
        drop(resp);
        let settled = settle(&origin).await;
        let tail: Vec<_> = origin.hits()[3..].iter().map(|(o, l, _)| (*o, *l)).collect();
        let expected: Vec<_> = (0..tail.len()).map(|i| ((i * PIECE) as u64, PIECE)).collect();
        assert_eq!(tail, expected, "앞에서부터 이어지는 조각만 묻는다");
        let asked = (tail.len() * PIECE) as u64;
        assert!(asked <= 5 * PIECE as u64, "읽은 만큼만 물어야 한다: {asked}");
        // 끊는 순간 받던 조각은 원본에 물었어도 끝까지 받지 못해 합에 들지 않을 수 있다 — 같다고 보지 않는다.
        let earlier = 2 + PIECE as u64 + 10;
        let received = stream.received_bytes() - earlier;
        assert!(
            received >= got as u64 && received <= asked,
            "읽은 것은 받았고 물은 것보다 더 받지 않는다: 읽음 {got} 받음 {received} 물음 {asked}"
        );
        assert!(stream.error().is_none());

        drop(stream);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(origin.hits().len(), settled, "끊긴 뒤·닫은 뒤 더 묻지 않는다");
    }

    /// 원본 요청 수가 300ms 동안 그대로일 때까지 기다려 그 수를 준다.
    async fn settle(origin: &Origin) -> usize {
        let mut seen = origin.hits().len();
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let now = origin.hits().len();
            if now == seen {
                return now;
            }
            seen = now;
        }
        panic!("원본 요청이 멎지 않는다: {seen}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_failures_answer_before_headers_and_surface_as_error() {
        let bytes = Arc::new(mp4_bytes(PIECE + 9, 22));
        let media = media_for(&bytes);

        let missing = origin(|_, _| Reply::Status(404)).await;
        let stream = remote(&missing, &media);
        assert_eq!(get(stream.url(), Some("bytes=0-1")).await.0, StatusCode::NOT_FOUND);
        assert_eq!(stream.error().as_deref(), Some("원본 기기에 미디어가 없어요"));

        let refusing = origin(|_, _| Reply::Status(403)).await;
        let stream = remote(&refusing, &media);
        assert_eq!(get(stream.url(), None).await.0, StatusCode::BAD_GATEWAY);
        assert_eq!(stream.error().as_deref(), Some("원본 기기가 거절했어요 (HTTP 403)"));

        let short = bytes.clone();
        let truncating = origin(move |offset, len| Reply::Bytes(short[offset as usize..offset as usize + len - 1].to_vec())).await;
        let stream = remote(&truncating, &media);
        assert_eq!(get(stream.url(), Some("bytes=0-99")).await.0, StatusCode::BAD_GATEWAY);
        assert_eq!(stream.error().as_deref(), Some("원본 조각 길이가 달라요"));

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let stream = Stream::start(Source::Remote { base, auth: Arc::new(|| None) }, &media).unwrap();
        assert_eq!(get(stream.url(), Some("bytes=0-1")).await.0, StatusCode::BAD_GATEWAY);
        assert!(stream.error().unwrap().starts_with("원본 기기에 닿지 못했어요"));
        assert!(!stream.error().unwrap().contains("127.0.0.1"), "주소는 글에 싣지 않는다");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_bytes_forged_with_the_same_length_are_caught_at_the_end() {
        let bytes = Arc::new(mp4_bytes(3 * PIECE + 1, 23));
        let media = media_for(&bytes);
        let mut forged = bytes.as_ref().clone();
        forged[PIECE + 7] ^= 0x01;
        let origin = origin(serving(Arc::new(forged))).await;
        let stream = remote(&origin, &media);

        let mut resp = http().get(stream.url()).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let mut got = 0usize;
        let ended = loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => got += chunk.len(),
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        assert!(ended.is_err() && got < bytes.len(), "틀린 마지막 조각은 내주지 않는다 ({got})");
        assert_eq!(stream.error().as_deref(), Some("미디어 내용이 원본과 달라요"));
        assert_eq!(get(stream.url(), Some("bytes=0-1")).await.0, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reader_leaving_mid_piece_cancels_the_origin_request() {
        let bytes = Arc::new(mp4_bytes(4 * PIECE, 24));
        let media = media_for(&bytes);
        let first = bytes.clone();
        let origin = origin(move |offset, len| {
            if offset == 0 { Reply::Bytes(first[..len].to_vec()) } else { Reply::Stall }
        })
        .await;
        let stream = remote(&origin, &media);
        let mut resp = http().get(stream.url()).header("range", "bytes=0-").send().await.unwrap();
        let mut got = 0;
        while got < PIECE {
            got += resp.chunk().await.unwrap().unwrap().len();
        }
        assert!(until(|| origin.hits().len() == 2).await, "둘째 조각을 묻는 중");
        assert_eq!(origin.gone.load(Ordering::SeqCst), 0);
        drop(resp);
        assert!(until(|| origin.gone.load(Ordering::SeqCst) == 1).await, "끊긴 응답의 원본 요청도 끊는다");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_stream_cancels_waiting_requests_and_closes_the_port() {
        let bytes = Arc::new(mp4_bytes(2 * PIECE, 25));
        let media = media_for(&bytes);
        let origin = origin(|_, _| Reply::Stall).await;
        let stream = remote(&origin, &media);
        let url = stream.url().to_string();
        let waiting = tokio::spawn(async move { http().get(&url).header("range", "bytes=0-").send().await });
        assert!(until(|| origin.hits().len() == 1).await);

        let started = std::time::Instant::now();
        let addr = stream.url()["http://".len()..].split('/').next().unwrap().to_string();
        drop(stream);
        assert!(started.elapsed() < Duration::from_millis(50), "버리기는 기다리지 않는다");
        assert!(until(|| origin.gone.load(Ordering::SeqCst) == 1).await, "원본 요청을 끊는다");
        let answer = waiting.await.unwrap();
        assert!(answer.is_err() || !answer.unwrap().status().is_success());
        let mut refused = false;
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(&addr).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "버린 뒤에는 포트가 닫힌다");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_ranges_live_side_by_side_until_their_reader_leaves() {
        let bytes = Arc::new(mp4_bytes(8 * PIECE, 26));
        let media = media_for(&bytes);
        let origin = origin(|_, _| Reply::Stall).await;
        let stream = remote(&origin, &media);
        let mut waiting = Vec::new();
        for i in 0..6 {
            let url = stream.url().to_string();
            let range = format!("bytes={}-", i * PIECE);
            waiting.push(tokio::spawn(async move { http().get(&url).header("range", range).send().await }));
            assert!(until(|| origin.hits().len() == i + 1).await);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(origin.gone.load(Ordering::SeqCst), 0, "새 구간이 왔다고 옛 구간을 끊지 않는다");
        waiting.remove(2).abort();
        assert!(until(|| origin.gone.load(Ordering::SeqCst) == 1).await, "떠난 요청의 원본 요청만 끊는다");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(origin.gone.load(Ordering::SeqCst), 1);
        drop(stream);
        assert!(until(|| origin.gone.load(Ordering::SeqCst) == 6).await, "버리면 전부 끊는다");
    }

    #[test]
    fn open_refuses_cards_and_sources_it_cannot_pin() {
        let media = media_for(b"\0\0\0\x18ftypisom");
        assert!(Stream::open(Some("file:///etc/passwd"), &media).is_err());
        assert!(Stream::open(Some("not a url"), &media).is_err());
        let html = Media { mime: "text/html".into(), ..media.clone() };
        assert!(Stream::open(Some("http://127.0.0.1:1"), &html).is_err());
        let huge = Media { size: registry::MAX_FILE_BYTES + 1, ..media.clone() };
        assert!(Stream::open(None, &huge).is_err());
        assert!(Stream::open(Some("http://127.0.0.1:1/"), &media).is_ok());
    }

    #[test]
    fn range_forms() {
        assert_eq!(parse_range(None, 10), Want::Whole);
        assert_eq!(parse_range(Some("bytes=0-0"), 10), Want::Part(0, 0));
        assert_eq!(parse_range(Some("bytes=3-"), 10), Want::Part(3, 9));
        assert_eq!(parse_range(Some("bytes=3-99"), 10), Want::Part(3, 9));
        assert_eq!(parse_range(Some("bytes=-3"), 10), Want::Part(7, 9));
        assert_eq!(parse_range(Some("bytes=-30"), 10), Want::Part(0, 9));
        assert_eq!(parse_range(Some("bytes=10-"), 10), Want::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 10), Want::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=18446744073709551616-"), 10), Want::Whole);
        assert_eq!(parse_range(Some("bytes=5-4"), 10), Want::Whole);
        assert_eq!(parse_range(Some("bytes=-"), 10), Want::Whole);
        assert_eq!(parse_range(Some("BYTES=2-3"), 10), Want::Part(2, 3));
        assert_eq!(parse_range(Some("Bytes=-2"), 10), Want::Part(8, 9));
        assert_eq!(parse_range(Some("bytes"), 10), Want::Whole);
        assert_eq!(parse_range(Some("바이트=1-2"), 10), Want::Whole);
    }
}
