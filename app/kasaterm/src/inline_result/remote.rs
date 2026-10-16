use super::{dimensions_cache, root, valid_id};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Fetch {
    at: Instant,
    pending: bool,
}

fn requests() -> &'static Mutex<HashMap<(String, String), Fetch>> {
    static REQUESTS: OnceLock<Mutex<HashMap<(String, String), Fetch>>> = OnceLock::new();
    REQUESTS.get_or_init(Default::default)
}

pub(super) fn request(base: &str, id: &str, proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    if !valid_id(id) { return; }
    let Some(root) = root() else { return };
    let key = (base.to_owned(), id.to_owned());
    {
        let Ok(mut jobs) = requests().lock() else { return };
        if jobs.get(&key).is_some_and(|f| f.pending || f.at.elapsed() < Duration::from_secs(30)) {
            return;
        }
        if jobs.values().filter(|f| f.pending).count() >= 4 { return; }
        if jobs.len() >= 512 { jobs.retain(|_, f| f.pending || f.at.elapsed() < Duration::from_secs(30)); }
        jobs.insert(key.clone(), Fetch { at: Instant::now(), pending: true });
    }
    // 원본 기기·디스크가 느려도 터미널의 입력과 프레임을 기다리게 하지 않는다.
    std::thread::spawn(move || {
        let _ = fetch(&key.0, &key.1, &root);
        if let Ok(mut jobs) = requests().lock() {
            jobs.insert(key, Fetch { at: Instant::now(), pending: false });
        }
        let _ = proxy.send_event(crate::UserEvent::Redraw);
    });
}

fn fetch(base: &str, id: &str, root: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(valid_id(id), "invalid image id");
    let bytes = kasa_mcp::remote::remote_get_bytes_bounded(
        base, &format!("/term/inline-result?id={id}"), kasa_socket::inline_results::MAX_BYTES as usize,
    )?;
    store(root, id, &bytes)
}

fn store(root: &Path, id: &str, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(valid_id(id) && bytes.len() <= kasa_socket::inline_results::MAX_BYTES as usize, "invalid image asset");
    anyhow::ensure!(format!("{:x}", Sha256::digest(bytes)).starts_with(id), "image id mismatch");
    let dimensions = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png).into_dimensions()?;
    anyhow::ensure!(dimensions.0 > 0 && dimensions.1 > 0 && dimensions.0 <= 1600 && dimensions.1 <= 1600, "invalid image dimensions");
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
    std::fs::create_dir_all(root)?;
    let path = root.join(format!("{id}.png"));
    let temp = root.join(format!("{}.part", uuid::Uuid::new_v4()));
    std::fs::write(&temp, bytes)?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    if let Ok(mut cache) = dimensions_cache().lock() { cache.remove(&path); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn remote_png_becomes_available_after_an_initial_cache_miss() {
        let root = std::env::temp_dir().join(format!("remote-inline-{}", uuid::Uuid::new_v4()));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(80, 40).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let bytes = png.into_inner();
        let id = format!("{:x}", Sha256::digest(&bytes))[..16].to_string();
        assert!(super::super::asset_path_in(&root, &id).is_none());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let expected = format!("/term/inline-result?id={id}");
        let served = bytes.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).contains(&expected));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len()).unwrap();
            stream.write_all(&served).unwrap();
        });
        fetch(&base, &id, &root).unwrap();
        server.join().unwrap();
        let (path, w, h) = super::super::asset_path_in(&root, &id).unwrap();
        assert_eq!((w, h), (80, 40));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(store(&root, "0000000000000000", &bytes).is_err());
        assert!(store(&root, "../../invalid", &bytes).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
