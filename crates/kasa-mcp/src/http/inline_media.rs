//! 대화에 박힌 소리·영상 카드의 원본 읽기. 저장소는 `kasa_socket::inline_media` 이고 여기는 id 로만
//! 꺼내 준다 — 경로를 받지도 내주지도 않는다. 주인(이 기기 로컬·원격 토큰·폰 주인)만 읽고 게스트는 막는다.
//!
//! - `GET /term/inline-media?id=` — 카드 정보 JSON
//! - `GET /term/inline-media/poster?id=` — 영상 첫 장면 PNG
//! - `GET /term/inline-media/file?id=&offset=&len=` — 원본 조각. 등록 sha256 과 같다고 확인된 사본에서만 내준다
//!   (`Store::read_chunk`) — 받는 쪽이 파일 전체를 모으지 않고 구간만 재생해도 원본 내용을 믿을 수 있게

use super::*;
use axum::extract::State;
use axum::http::StatusCode;
use kasa_socket::inline_media::{self as registry, Store};
use std::collections::HashMap;

type Stores = Arc<dyn Fn() -> Store + Send + Sync>;

const IMMUTABLE: &str = "private, max-age=31536000, immutable";

pub(super) fn routes() -> axum::Router {
    routes_with(Arc::new(registry::store))
}

fn routes_with(stores: Stores) -> axum::Router {
    axum::Router::new()
        .route("/term/inline-media", get(metadata_handler))
        .route("/term/inline-media/poster", get(poster_handler))
        .route("/term/inline-media/file", get(file_handler))
        .with_state(stores)
}

fn failure(e: &std::io::Error) -> axum::response::Response {
    // 바깥으로는 고정 문구만 — 저장소 오류 글에 경로가 섞여도 새지 않게.
    match e.kind() {
        std::io::ErrorKind::InvalidInput => (StatusCode::BAD_REQUEST, "invalid media request").into_response(),
        _ => (StatusCode::NOT_FOUND, "media unavailable").into_response(),
    }
}

fn checked_id(params: &HashMap<String, String>) -> Option<String> {
    params.get("id").filter(|id| registry::valid_id(id)).cloned()
}

fn bad_id() -> axum::response::Response {
    (StatusCode::BAD_REQUEST, "invalid media id").into_response()
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, axum::response::Response> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(failure(&e)),
        Err(_) => Err((StatusCode::INTERNAL_SERVER_ERROR, "media read failed").into_response()),
    }
}

async fn metadata_handler(
    State(stores): State<Stores>,
    Query(params): Query<HashMap<String, String>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if let Some(denied) = guest_denied(&req) {
        return denied;
    }
    let Some(id) = checked_id(&params) else {
        return bad_id();
    };
    match blocking(move || stores().metadata(&id)).await {
        Ok(media) => ([(header::CACHE_CONTROL, IMMUTABLE)], Json(media)).into_response(),
        Err(resp) => resp,
    }
}

async fn poster_handler(
    State(stores): State<Stores>,
    Query(params): Query<HashMap<String, String>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if let Some(denied) = guest_denied(&req) {
        return denied;
    }
    let Some(id) = checked_id(&params) else {
        return bad_id();
    };
    match blocking(move || stores().read_poster(&id)).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, IMMUTABLE)],
            bytes,
        )
            .into_response(),
        Err(resp) => resp,
    }
}

async fn file_handler(
    State(stores): State<Stores>,
    Query(params): Query<HashMap<String, String>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if let Some(denied) = guest_denied(&req) {
        return denied;
    }
    let Some(id) = checked_id(&params) else {
        return bad_id();
    };
    let offset = params.get("offset").and_then(|v| v.parse::<u64>().ok());
    let len = params.get("len").and_then(|v| v.parse::<usize>().ok());
    let (Some(offset), Some(len)) = (offset, len) else {
        return (StatusCode::BAD_REQUEST, "offset and len required").into_response();
    };
    match blocking(move || stores().read_chunk(&id, offset, len)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CACHE_CONTROL, IMMUTABLE),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Err(resp) => resp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_socket::inline_media::{Media, Probe, CLIENT_CHUNK, MAX_CHUNK};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;

    struct Fixture {
        dir: std::path::PathBuf,
        store: Store,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir = super::super::test_support::temp_dir(&format!("inline-media-{tag}"));
            let store = Store::at(dir.join("store"));
            Self { dir, store }
        }

        fn app(&self) -> axum::Router {
            let store = self.store.clone();
            routes_with(Arc::new(move || store.clone()))
                .layer(axum::middleware::from_fn(origin_guard_mw))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    enum Caller {
        Local,
        Owner,
        Guest,
        RemoteWithoutToken,
    }

    fn request(uri: &str, caller: Caller) -> axum::extract::Request {
        let mut req = axum::http::Request::builder().uri(uri).body(axum::body::Body::empty()).unwrap();
        let user = |owner| {
            MobileAuth(crate::mobile::MobileUser {
                name: "fixture".into(),
                slug: "abcdefghijklmnopqrstuvwxy".into(),
                created: 0,
                owner,
            })
        };
        match caller {
            Caller::Local => {}
            Caller::Owner => {
                req.extensions_mut().insert(user(true));
            }
            Caller::Guest => {
                req.extensions_mut().insert(user(false));
            }
            Caller::RemoteWithoutToken => {
                req.extensions_mut().insert(axum::extract::ConnectInfo(
                    "192.168.0.7:51234".parse::<std::net::SocketAddr>().unwrap(),
                ));
            }
        }
        req
    }

    async fn call(app: &axum::Router, uri: &str, caller: Caller) -> (StatusCode, HeaderMap, Vec<u8>) {
        let resp = app.clone().oneshot(request(uri, caller)).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), MAX_CHUNK + 1).await.unwrap();
        (status, headers, body.to_vec())
    }

    /// 32MiB 를 넘겨 옛 붙여넣기·이미지 상한(32MiB)에 걸리지 않음을 본다. 조각마다 내용이 달라 위치가 어긋나면 해시가 틀린다.
    fn write_large_mp4(path: &std::path::Path) -> Vec<u8> {
        use std::io::Write;
        let len = 33 * 1024 * 1024 + 4097;
        let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let mut hasher = Sha256::new();
        let header = b"\0\0\0\x18ftypisom\0\0\0\0isomavc1";
        file.write_all(header).unwrap();
        hasher.update(header);
        let mut x: u32 = 0x9E37_79B9;
        let mut block = vec![0u8; 64 * 1024];
        let mut written = header.len();
        while written < len {
            let n = block.len().min(len - written);
            for b in &mut block[..n] {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = x as u8;
            }
            file.write_all(&block[..n]).unwrap();
            hasher.update(&block[..n]);
            written += n;
        }
        file.flush().unwrap();
        hasher.finalize().to_vec()
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    #[tokio::test]
    async fn large_media_downloads_in_chunks_and_matches_published_sha() {
        let fx = Fixture::new("large");
        let src = fx.dir.join("big clip.mp4");
        let source_sha = write_large_mp4(&src);
        let probe = Probe { duration_ms: Some(42_000), width: Some(1920), height: Some(1080), poster: Some(png(1600, 900)) };
        let media = fx.store.register(&src, probe).unwrap();
        assert!(media.size > 32 * 1024 * 1024);
        let app = fx.app();

        let (status, headers, body) = call(&app, &format!("/term/inline-media?id={}", media.id), Caller::Owner).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], IMMUTABLE);
        let served: Media = serde_json::from_slice(&body).unwrap();
        assert_eq!(served, media);
        let text = String::from_utf8(body).unwrap();
        assert!(!text.contains(fx.dir.to_str().unwrap()) && !text.contains("blobs"), "{text}");

        let mut hasher = Sha256::new();
        let mut offset = 0u64;
        let mut chunks = 0;
        while offset < served.size {
            let uri = format!("/term/inline-media/file?id={}&offset={offset}&len={CLIENT_CHUNK}", served.id);
            let (status, headers, chunk) = call(&app, &uri, Caller::Local).await;
            assert_eq!(status, StatusCode::OK, "offset {offset}");
            assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
            assert!(!chunk.is_empty() && chunk.len() <= CLIENT_CHUNK);
            hasher.update(&chunk);
            offset += chunk.len() as u64;
            chunks += 1;
        }
        assert_eq!(offset, served.size);
        assert!(chunks > 8);
        let downloaded = hasher.finalize().to_vec();
        assert_eq!(downloaded, source_sha);
        let hex: String = downloaded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, served.sha256);

        let (status, _, chunk) = call(&app, &format!("/term/inline-media/file?id={}&offset=0&len={MAX_CHUNK}", media.id), Caller::Owner).await;
        assert_eq!((status, chunk.len()), (StatusCode::OK, MAX_CHUNK));
        let tail = served.size - 10;
        let (status, _, chunk) = call(&app, &format!("/term/inline-media/file?id={}&offset={tail}&len={MAX_CHUNK}", media.id), Caller::Owner).await;
        assert_eq!((status, chunk.len()), (StatusCode::OK, 10));

        let (status, headers, poster) = call(&app, &format!("/term/inline-media/poster?id={}", media.id), Caller::Owner).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(poster, png(1600, 900));
    }

    #[tokio::test]
    async fn invalid_requests_are_400_and_missing_media_is_404() {
        let fx = Fixture::new("invalid");
        let src = fx.dir.join("note.m4a");
        std::fs::write(&src, b"\0\0\0\x18ftypM4A \0\0\0\0M4A isom audio bytes").unwrap();
        let media = fx.store.register(&src, Probe::default()).unwrap();
        let size = media.size;
        let id = media.id.as_str();
        let app = fx.app();

        let bad = [
            "/term/inline-media".to_string(),
            "/term/inline-media?id=../../meta/x".into(),
            "/term/inline-media?id=0123456789ABCDEF".into(),
            "/term/inline-media/poster?id=%2Fetc%2Fpasswd".into(),
            format!("/term/inline-media/file?id={id}"),
            format!("/term/inline-media/file?id={id}&offset=0"),
            format!("/term/inline-media/file?id={id}&len=1"),
            format!("/term/inline-media/file?id={id}&offset=-1&len=1"),
            format!("/term/inline-media/file?id={id}&offset=abc&len=1"),
            format!("/term/inline-media/file?id={id}&offset=0&len=0"),
            format!("/term/inline-media/file?id={id}&offset=0&len={}", MAX_CHUNK + 1),
            format!("/term/inline-media/file?id={id}&offset={size}&len=1"),
            format!("/term/inline-media/file?id={id}&offset=18446744073709551615&len=1"),
            format!("/term/inline-media/file?id={id}&offset=18446744073709551616&len=1"),
            "/term/inline-media/file?id=../x&offset=0&len=1".into(),
        ];
        for uri in &bad {
            let (status, _, body) = call(&app, uri, Caller::Owner).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert!(!String::from_utf8_lossy(&body).contains(fx.dir.to_str().unwrap()));
        }

        let missing = "0000000000000000";
        for uri in [
            format!("/term/inline-media?id={missing}"),
            format!("/term/inline-media/poster?id={missing}"),
            format!("/term/inline-media/file?id={missing}&offset=0&len=1"),
            format!("/term/inline-media/poster?id={id}"),
        ] {
            assert_eq!(call(&app, &uri, Caller::Owner).await.0, StatusCode::NOT_FOUND, "{uri}");
        }

        // 원본 사본이 사라진 기록은 없는 것으로 본다 — 경로를 알려 주지 않는다.
        let blob = fx.store.media_path(id).unwrap();
        std::fs::remove_file(&blob).unwrap();
        let (status, _, body) = call(&app, &format!("/term/inline-media?id={id}"), Caller::Owner).await;
        assert_eq!((status, body.as_slice()), (StatusCode::NOT_FOUND, &b"media unavailable"[..]));
    }

    /// 원본 사본을 같은 길이로 바꿔 쓰거나 다른 파일로 바꿔 끼우면 크기·기록은 그대로라도 조각을 내주지 않는다.
    #[cfg(unix)]
    #[tokio::test]
    async fn originals_rewritten_or_swapped_with_the_same_length_are_not_served() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new("forged");
        let src = fx.dir.join("clip.mp4");
        let mut bytes = b"\0\0\0\x18ftypisom\0\0\0\0isomavc1".to_vec();
        bytes.extend((0..70_000u32).map(|i| (i * 7 % 251) as u8));
        std::fs::write(&src, &bytes).unwrap();
        let media = fx.store.register(&src, Probe::default()).unwrap();
        let app = fx.app();
        let uri = format!("/term/inline-media/file?id={}&offset=0&len=16", media.id);
        let (status, _, chunk) = call(&app, &uri, Caller::Owner).await;
        assert_eq!((status, chunk.as_slice()), (StatusCode::OK, &bytes[..16]));

        let blob = fx.store.media_path(&media.id).unwrap();
        let then = std::fs::metadata(&blob).unwrap().modified().unwrap();
        let mut forged = bytes.clone();
        forged[60_000] ^= 0x80;
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut f = std::fs::OpenOptions::new().write(true).open(&blob).unwrap();
        f.write_all(&forged).unwrap();
        f.set_modified(then).unwrap();
        drop(f);
        let (status, _, body) = call(&app, &uri, Caller::Owner).await;
        assert_eq!((status, body.as_slice()), (StatusCode::NOT_FOUND, &b"media unavailable"[..]));

        let swap = fx.dir.join("swap.part");
        std::fs::write(&swap, &forged).unwrap();
        std::fs::File::open(&swap).unwrap().set_modified(then).unwrap();
        std::fs::rename(&swap, &blob).unwrap();
        assert_eq!(call(&app, &uri, Caller::Owner).await.0, StatusCode::NOT_FOUND);

        std::fs::write(&swap, &bytes).unwrap();
        std::fs::rename(&swap, &blob).unwrap();
        assert_eq!(call(&app, &uri, Caller::Owner).await.0, StatusCode::OK, "같은 내용으로 돌아오면 다시 확인하고 내준다");
    }

    #[tokio::test]
    async fn guests_and_tokenless_remote_callers_are_refused() {
        let fx = Fixture::new("auth");
        let src = fx.dir.join("clip.mov");
        std::fs::write(&src, b"\0\0\0\x14ftypqt  \0\0\0\0qt  movie bytes").unwrap();
        let media = fx.store.register(&src, Probe { poster: Some(png(4, 4)), ..Default::default() }).unwrap();
        let app = fx.app();
        let uris = [
            format!("/term/inline-media?id={}", media.id),
            format!("/term/inline-media/poster?id={}", media.id),
            format!("/term/inline-media/file?id={}&offset=0&len=4", media.id),
            // 게스트에게는 입력이 틀렸는지조차 알려 주지 않는다.
            "/term/inline-media?id=../x".to_string(),
        ];
        for uri in &uris {
            assert_eq!(call(&app, uri, Caller::Guest).await.0, StatusCode::FORBIDDEN, "guest {uri}");
            assert_eq!(call(&app, uri, Caller::RemoteWithoutToken).await.0, StatusCode::FORBIDDEN, "remote {uri}");
        }
        for uri in &uris[..3] {
            assert_eq!(call(&app, uri, Caller::Owner).await.0, StatusCode::OK, "owner {uri}");
            assert_eq!(call(&app, uri, Caller::Local).await.0, StatusCode::OK, "local {uri}");
        }
    }
}
