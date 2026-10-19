use super::Reference;
use crate::UserEvent;
use kasa_socket::inline_media::Media;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use winit::event_loop::EventLoopProxy;

pub(crate) struct Poster {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
pub(crate) struct Asset {
    pub media: Media,
    pub poster: Option<Poster>,
}
static CHANGED: AtomicBool = AtomicBool::new(false);
pub(crate) fn changed() -> bool {
    CHANGED.swap(false, Ordering::Relaxed)
}
struct Entry {
    at: Instant,
    result: Option<Result<Arc<Asset>, String>>,
}
fn entries() -> &'static Mutex<HashMap<Reference, Entry>> {
    static VALUE: OnceLock<Mutex<HashMap<Reference, Entry>>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}
pub(crate) fn get(reference: &Reference) -> Result<Option<Arc<Asset>>, String> {
    entries()
        .lock()
        .unwrap()
        .get(reference)
        .and_then(|e| e.result.clone())
        .transpose()
}
pub(crate) fn request(reference: &Reference, proxy: &EventLoopProxy<UserEvent>, retry: bool) {
    let mut map = entries().lock().unwrap();
    if let Some(e) = map.get(reference) {
        if e.result.is_none()
            || e.result.as_ref().is_some_and(Result::is_ok)
            || !retry && e.at.elapsed() < Duration::from_secs(30)
        {
            return;
        }
    }
    if map.values().filter(|e| e.result.is_none()).count() >= 4 {
        return;
    }
    if map.len() >= 64 {
        if let Some(old) = map
            .iter()
            .filter(|(_, e)| e.result.is_some())
            .min_by_key(|(_, e)| e.at)
            .map(|(k, _)| k.clone())
        {
            map.remove(&old);
        }
    }
    map.insert(
        reference.clone(),
        Entry {
            at: Instant::now(),
            result: None,
        },
    );
    let reference = reference.clone();
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let result = load(&reference).map(Arc::new).map_err(|e| e.to_string());
        entries().lock().unwrap().insert(
            reference,
            Entry {
                at: Instant::now(),
                result: Some(result),
            },
        );
        CHANGED.store(true, Ordering::Relaxed);
        let _ = proxy.send_event(UserEvent::Redraw);
    });
}
fn validate(m: &Media, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        m.version == 1 && m.id == id && kasa_socket::inline_media::valid_id(id),
        "미디어 식별자가 달라요"
    );
    anyhow::ensure!(
        matches!(m.kind.as_str(), "audio" | "video") && m.size > 0 && m.size <= 1024 * 1024 * 1024,
        "지원하지 않는 미디어예요"
    );
    anyhow::ensure!(
        m.sha256.len() == 64
            && m.sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "미디어 해시가 올바르지 않아요"
    );
    anyhow::ensure!(m.name.len() <= 1024, "미디어 이름이 너무 길어요");
    Ok(())
}
fn load(reference: &Reference) -> anyhow::Result<Asset> {
    let media: Media = if let Some(base) = &reference.source {
        let bytes = kasa_mcp::remote::remote_get_bytes_bounded(
            base,
            &format!("/term/inline-media?id={}", reference.id),
            16 * 1024,
        )?;
        serde_json::from_slice(&bytes)?
    } else {
        kasa_socket::inline_media::metadata(&reference.id)?
    };
    validate(&media, &reference.id)?;
    let poster = if media.poster {
        let bytes = if let Some(base) = &reference.source {
            kasa_mcp::remote::remote_get_bytes_bounded(
                base,
                &format!("/term/inline-media/poster?id={}", reference.id),
                12 * 1024 * 1024,
            )
        } else {
            kasa_socket::inline_media::read_poster(&reference.id).map_err(Into::into)
        };
        bytes.ok().and_then(|bytes| {
            let mut reader = image::ImageReader::with_format(
                std::io::Cursor::new(bytes),
                image::ImageFormat::Png,
            );
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(1600);
            limits.max_image_height = Some(1600);
            limits.max_alloc = Some(16 * 1024 * 1024);
            reader.limits(limits);
            reader.decode().ok().map(|image| {
                let p = image.to_rgba8();
                Poster {
                    width: p.width(),
                    height: p.height(),
                    rgba: p.into_raw(),
                }
            })
        })
    } else {
        None
    };
    Ok(Asset { media, poster })
}
#[derive(Default)]
pub(crate) struct Download {
    pub cancelled: AtomicBool,
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub result: Mutex<Option<Result<PathBuf, String>>>,
}
pub(crate) fn download(
    reference: Reference,
    asset: Arc<Asset>,
    proxy: EventLoopProxy<UserEvent>,
) -> Arc<Download> {
    let job = Arc::new(Download::default());
    job.total.store(asset.media.size, Ordering::Relaxed);
    let worker = job.clone();
    std::thread::spawn(move || {
        let result = download_file(
            &reference,
            &asset.media,
            &worker,
            &kasa_socket::inline_media::root(),
            || {
                let _ = proxy.send_event(UserEvent::Redraw);
            },
        )
        .map_err(|e| e.to_string());
        *worker.result.lock().unwrap() = Some(result);
        let _ = proxy.send_event(UserEvent::Redraw);
    });
    job
}
fn hash_file(path: &Path, job: &Download) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        anyhow::ensure!(!job.cancelled.load(Ordering::Relaxed), "받기를 취소했어요");
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn download_file(
    reference: &Reference,
    media: &Media,
    job: &Download,
    cache_root: &Path,
    notify: impl Fn(),
) -> anyhow::Result<PathBuf> {
    if reference.source.is_none() {
        let path = kasa_socket::inline_media::media_path(&reference.id)?;
        anyhow::ensure!(
            std::fs::symlink_metadata(&path)?.file_type().is_file(),
            "미디어 파일이 바뀌었어요"
        );
        anyhow::ensure!(
            hash_file(&path, job)? == media.sha256,
            "미디어 파일 내용이 바뀌었어요"
        );
        return Ok(path);
    }
    let base = reference.source.as_deref().unwrap();
    let root = cache_root
        .join("remote")
        .join(format!("{:x}", Sha256::digest(base.as_bytes())));
    std::fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    let extension = Path::new(&media.name)
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| s.len() <= 8 && s.bytes().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("media");
    let target = root.join(format!("{}.{}", media.sha256, extension));
    if std::fs::symlink_metadata(&target)
        .is_ok_and(|m| m.file_type().is_file() && m.len() == media.size)
        && hash_file(&target, job)? == media.sha256
    {
        return Ok(target);
    }
    let temp = root.join(format!("{}.part", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        let mut offset = 0;
        let mut hash = Sha256::new();
        while offset < media.size {
            anyhow::ensure!(!job.cancelled.load(Ordering::Relaxed), "받기를 취소했어요");
            let len = (media.size - offset).min(4 * 1024 * 1024) as usize;
            let bytes = kasa_mcp::remote::remote_get_bytes_bounded(
                base,
                &format!(
                    "/term/inline-media/file?id={}&offset={offset}&len={len}",
                    reference.id
                ),
                len,
            )?;
            anyhow::ensure!(bytes.len() == len, "파일 받기가 중간에 끊겼어요");
            file.write_all(&bytes)?;
            hash.update(&bytes);
            offset += len as u64;
            job.done.store(offset, Ordering::Relaxed);
            notify();
        }
        anyhow::ensure!(
            format!("{:x}", hash.finalize()) == media.sha256,
            "파일 내용이 바뀌었어요. 다시 받아 주세요"
        );
        anyhow::ensure!(!job.cancelled.load(Ordering::Relaxed), "받기를 취소했어요");
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)?;
        Ok(target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn media(bytes: &[u8]) -> Media {
        Media {
            version: 1,
            id: "0123456789abcdef".into(),
            kind: "video".into(),
            name: "movie.mp4".into(),
            mime: "video/mp4".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            duration_ms: Some(1000),
            width: Some(16),
            height: Some(16),
            poster: false,
        }
    }
    #[test]
    fn large_download_is_chunked_verified_cached_and_cancellable() {
        let bytes = Arc::new(
            (0..(33 * 1024 * 1024 + 123))
                .map(|i| (i % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let metadata = media(&bytes);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reference = Reference {
            id: metadata.id.clone(),
            source: Some(base),
        };
        let served = bytes.clone();
        let worker = std::thread::spawn(move || {
            let mut expected = 0;
            while expected < served.len() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                loop {
                    let mut b = [0; 1024];
                    let n = stream.read(&mut b).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&b[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8(request).unwrap();
                // The desktop Info scanner can probe any loopback port while this fixture is serving chunks.
                if text.starts_with("GET / HTTP/") {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let len = (served.len() - expected).min(4 * 1024 * 1024);
                assert!(
                    text.contains(&format!("offset={expected}&len={len}")),
                    "{text}"
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                stream.write_all(&served[expected..expected + len]).unwrap();
                expected += len;
            }
        });
        let root =
            std::env::temp_dir().join(format!("inline-media-download-{}", uuid::Uuid::new_v4()));
        let job = Download::default();
        let mut probe = std::net::TcpStream::connect(reference.source.as_ref().unwrap().trim_start_matches("http://")).unwrap();
        probe.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
        let mut answer = String::new();
        probe.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 404"));
        let path = download_file(&reference, &metadata, &job, &root, || {}).unwrap();
        worker.join().unwrap();
        assert_eq!(job.done.load(Ordering::Relaxed), metadata.size);
        assert_eq!(hash_file(&path, &job).unwrap(), metadata.sha256);
        assert_eq!(
            download_file(&reference, &metadata, &job, &root, || {}).unwrap(),
            path
        );
        job.cancelled.store(true, Ordering::Relaxed);
        assert!(download_file(&reference, &metadata, &job, &root, || {}).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn metadata_cannot_choose_a_path_or_unbounded_allocation() {
        let mut m = media(b"fixture");
        assert!(validate(&m, &m.id).is_ok());
        m.id = "../../etc/passwd".into();
        assert!(validate(&m, &m.id).is_err());
        m.id = "0123456789abcdef".into();
        m.size = 2 << 30;
        assert!(validate(&m, &m.id).is_err());
        m.size = 7;
        m.sha256 = "../../secret".into();
        assert!(validate(&m, &m.id).is_err());
    }
}
