use super::Reference;
use crate::UserEvent;
use kasa_socket::inline_media::Media;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
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
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
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
