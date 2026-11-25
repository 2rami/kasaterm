//! 결과 그림 중 움직이는 것(정지 그림 PNG 에 `kasaterm-anim` 표식이 있는 것)을 칸 안에서 돌린다.
//!
//! 장은 일꾼 스레드에서 한 번 풀어 메모리에 둔다. 그리는 쪽(`Player`)은 이번 프레임에 보이는 칸 그림만 보고, 장이
//! 바뀔 때만 같은 텍스처에 다시 쓴다(`update_image`). 다음 장 시각은 `due` 로 이벤트 루프에 알린다 — 보이는 움직이는
//! 그림이 없으면 만기가 없어 루프가 깨지 않는다.

use super::{load, root, valid_id};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 그림 하나가 풀어 둘 수 있는 RGBA 바이트. 넘으면 장을 줄여 푼다.
const FRAME_BUDGET: Budget = Budget { bytes: 96 * 1024 * 1024, min_edge: 96 };
/// 모든 움직이는 그림을 합친 상한. 넘으면 한동안 안 그린 것부터 놓는다.
const TOTAL_BUDGET: usize = 384 * 1024 * 1024;
const MAX_FRAMES: usize = 1000;

#[derive(Clone, Copy)]
struct Budget {
    bytes: usize,
    /// 줄이더라도 이 긴 변 아래로는 안 내린다 — 그보다는 뒤쪽 장을 버린다.
    min_edge: u32,
}
const RETRY: Duration = Duration::from_secs(30);

pub(crate) struct Frames {
    pub w: u32,
    pub h: u32,
    /// 정지 그림과 같은 크기로 풀었다 — 큰 미리보기는 이때만 돌린다(작게 풀린 장을 원본 크기 미리보기에 그리면
    /// 렌더러의 「원본보다 키우지 않음」 때문에 그림이 줄어든다).
    pub full: bool,
    frames: Vec<Vec<u8>>,
    delays: Vec<u32>,
    total: u64,
}

impl Frames {
    fn bytes(&self) -> usize {
        self.frames.iter().map(Vec::len).sum()
    }

    /// 지금 보일 장과 다음 장까지 남은 시간. 모든 사본이 같은 시계를 써서 같은 그림은 같은 장을 보인다.
    pub(crate) fn at(&self, now: Duration) -> (usize, Duration) {
        let mut at = (now.as_millis() as u64) % self.total.max(1);
        for (i, &delay) in self.delays.iter().enumerate() {
            if at < u64::from(delay) {
                return (i, Duration::from_millis(u64::from(delay) - at));
            }
            at -= u64::from(delay);
        }
        (0, Duration::from_millis(u64::from(self.delays[0])))
    }

    pub(crate) fn rgba(&self, index: usize) -> &[u8] {
        &self.frames[index]
    }
}

enum State {
    Loading,
    Ready(Arc<Frames>),
    Failed(Instant),
}

struct Entry {
    state: State,
    used: Instant,
}

fn store() -> &'static Mutex<HashMap<String, Entry>> {
    static STORE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

fn clock() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

fn due_slot() -> &'static Mutex<Option<Instant>> {
    static DUE: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    DUE.get_or_init(Default::default)
}

/// 다음 장을 그릴 시각 — 이벤트 루프의 대기 만기.
pub(crate) fn due() -> Option<Instant> {
    *due_slot().lock().ok()?
}

/// 만기가 지났으면 비우고 참. 다음 만기는 그 프레임을 그리면서 다시 선다 — 그리지 않는 동안(창이 가려져 그리기를
/// 건너뛰는 등)은 만기가 없어 루프가 헛돌지 않는다.
pub(crate) fn take_due(now: Instant) -> bool {
    let Ok(mut due) = due_slot().lock() else { return false };
    match *due {
        Some(at) if at <= now => {
            *due = None;
            true
        }
        _ => false,
    }
}

fn push_due(at: Instant) {
    if let Ok(mut due) = due_slot().lock() {
        if due.is_none_or(|d| at < d) {
            *due = Some(at);
        }
    }
}

/// 정지 그림 PNG 의 표식(원본 SHA-256). 경로마다 한 번만 읽는다.
fn mark(png: &Path) -> Option<String> {
    static MARKS: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();
    let marks = MARKS.get_or_init(Default::default);
    if let Some(hit) = marks.lock().ok()?.get(png) {
        return hit.clone();
    }
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(png).ok()?.take(4096).read_to_end(&mut head).ok()?;
    let value = load::read_mark(&head);
    let mut marks = marks.lock().ok()?;
    if marks.len() > 512 {
        marks.clear();
    }
    marks.insert(png.to_path_buf(), value.clone());
    value
}

/// 칸 그림이 화면에 잡혔다 — 움직이는 그림이면 장을 준비한다. `source` 는 거울 칸의 원본 기기 주소.
pub(crate) fn want(id: &str, png: &Path, source: Option<&str>, proxy: &winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    if !valid_id(id) {
        return;
    }
    let Some(sha) = mark(png) else { return };
    {
        let Ok(mut entries) = store().lock() else { return };
        match entries.get(id).map(|e| &e.state) {
            Some(State::Loading | State::Ready(_)) => return,
            Some(State::Failed(at)) if at.elapsed() < RETRY => return,
            _ => {}
        }
        entries.insert(id.to_owned(), Entry { state: State::Loading, used: Instant::now() });
    }
    let (id, source, proxy) = (id.to_owned(), source.map(str::to_owned), proxy.clone());
    let (w, h) = image::image_dimensions(png).unwrap_or((1600, 1600));
    // 원본 받기와 장 풀기는 수백 ms 가 걸릴 수 있다 — 프레임을 기다리게 하지 않는다.
    std::thread::spawn(move || {
        let state = match original(&id, &sha, source.as_deref()).and_then(|bytes| decode(&bytes, (w, h), FRAME_BUDGET)) {
            Ok(frames) => State::Ready(Arc::new(frames)),
            Err(_) => State::Failed(Instant::now()),
        };
        if let Ok(mut entries) = store().lock() {
            entries.insert(id, Entry { state, used: Instant::now() });
            evict(&mut entries);
        }
        let _ = proxy.send_event(crate::UserEvent::Redraw);
    });
}

/// 이 기기의 원본, 없으면 원본 기기에서 받아 확인하고 둔다.
fn original(id: &str, sha: &str, source: Option<&str>) -> anyhow::Result<Vec<u8>> {
    original_in(&root().ok_or_else(|| anyhow::anyhow!("image store unavailable"))?, id, sha, source)
}

fn original_in(root: &Path, id: &str, sha: &str, source: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let path = root.join(format!("{id}.anim"));
    let max = kasa_socket::inline_results::ANIM_MAX_BYTES as usize;
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() <= max && format!("{:x}", Sha256::digest(&bytes)) == sha {
            return Ok(bytes);
        }
    }
    let base = source.ok_or_else(|| anyhow::anyhow!("animation original missing"))?;
    let bytes = kasa_remote::remote::remote_get_bytes_bounded(base, &format!("/term/inline-result/anim?id={id}"), max)?;
    store_original(root, id, sha, &bytes)?;
    Ok(bytes)
}

fn store_original(root: &Path, id: &str, sha: &str, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(format!("{:x}", Sha256::digest(bytes)) == sha, "animation hash mismatch");
    anyhow::ensure!(kasa_socket::inline_results::anim_mime(bytes).is_some(), "unsupported animation");
    std::fs::create_dir_all(root)?;
    let temp = root.join(format!("{}.part", uuid::Uuid::new_v4()));
    std::fs::write(&temp, bytes)?;
    if let Err(error) = std::fs::rename(&temp, root.join(format!("{id}.anim"))) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

/// 원본을 장들로 푼다. 장은 정지 그림 크기(`fit`)까지만 키우고, 그림 하나 상한을 넘으면 절반씩 줄여 이어 푼다.
fn decode(bytes: &[u8], fit: (u32, u32), budget: Budget) -> anyhow::Result<Frames> {
    use image::{AnimationDecoder, ImageDecoder};
    let cursor = std::io::Cursor::new(bytes);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10000);
    limits.max_image_height = Some(10000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    let frames = match image::guess_format(bytes)? {
        image::ImageFormat::Gif => {
            let mut d = image::codecs::gif::GifDecoder::new(cursor)?;
            d.set_limits(limits)?;
            d.into_frames()
        }
        image::ImageFormat::WebP => {
            let mut d = image::codecs::webp::WebPDecoder::new(cursor)?;
            d.set_limits(limits)?;
            d.into_frames()
        }
        image::ImageFormat::Png => {
            let mut d = image::codecs::png::PngDecoder::new(cursor)?;
            d.set_limits(limits)?;
            d.apng()?.into_frames()
        }
        _ => anyhow::bail!("unsupported animation"),
    };
    let (mut w, mut h) = fit;
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut delays: Vec<u32> = Vec::new();
    for frame in frames.take(MAX_FRAMES) {
        let frame = frame?;
        let ms = {
            let (n, d) = frame.delay().numer_denom_ms();
            n.checked_div(d).unwrap_or(0)
        };
        // 브라우저와 같이 0~10ms 장은 100ms 로 본다 — 그런 GIF 는 대개 「가능한 빨리」가 아니라 값을 비워 둔 것이다.
        delays.push(if ms <= 10 { 100 } else { ms.min(60_000) });
        let buffer = frame.into_buffer();
        if out.is_empty() {
            let ratio = (f64::from(fit.0) / f64::from(buffer.width())).min(f64::from(fit.1) / f64::from(buffer.height())).min(1.0);
            w = ((f64::from(buffer.width()) * ratio).round() as u32).max(1);
            h = ((f64::from(buffer.height()) * ratio).round() as u32).max(1);
        }
        while (out.len() + 1) * (w as usize * h as usize * 4) > budget.bytes && w.max(h) / 2 >= budget.min_edge {
            (w, h) = (w / 2, h / 2);
            for f in &mut out {
                *f = shrink(f, w * 2, h * 2, w, h);
            }
        }
        if (out.len() + 1) * (w as usize * h as usize * 4) > budget.bytes {
            delays.pop();
            break;
        }
        let pixels = if (buffer.width(), buffer.height()) == (w, h) {
            buffer.into_raw()
        } else {
            image::imageops::resize(&buffer, w, h, image::imageops::FilterType::Triangle).into_raw()
        };
        out.push(pixels);
    }
    anyhow::ensure!(out.len() >= 2, "not animated");
    let total = delays.iter().map(|&d| u64::from(d)).sum();
    let full = {
        let ratio = (f64::from(fit.0) / f64::from(w)).max(f64::from(fit.1) / f64::from(h));
        ratio <= 1.01
    };
    Ok(Frames { w, h, full, frames: out, delays, total })
}

fn shrink(rgba: &[u8], from_w: u32, from_h: u32, w: u32, h: u32) -> Vec<u8> {
    let Some(buffer) = image::RgbaImage::from_raw(from_w, from_h, rgba.to_vec()) else { return rgba.to_vec() };
    image::imageops::resize(&buffer, w, h, image::imageops::FilterType::Triangle).into_raw()
}

/// 상한을 넘으면 오래 안 그린 그림부터 놓는다. 방금 그린 것은 남긴다 — 보이는 그림이 멈추면 안 된다.
fn evict(entries: &mut HashMap<String, Entry>) {
    let mut total: usize = entries.values().filter_map(|e| match &e.state { State::Ready(f) => Some(f.bytes()), _ => None }).sum();
    while total > TOTAL_BUDGET {
        let Some((id, bytes)) = entries
            .iter()
            .filter(|(_, e)| e.used.elapsed() > Duration::from_secs(5))
            .filter_map(|(id, e)| match &e.state { State::Ready(f) => Some((id.clone(), f.bytes(), e.used)), _ => None })
            .min_by_key(|(_, _, used)| *used)
            .map(|(id, bytes, _)| (id, bytes))
        else {
            break;
        };
        entries.remove(&id);
        total -= bytes;
    }
}

fn ready(id: &str) -> Option<Arc<Frames>> {
    let mut entries = store().lock().ok()?;
    let entry = entries.get_mut(id)?;
    entry.used = Instant::now();
    match &entry.state {
        State::Ready(frames) => Some(frames.clone()),
        _ => None,
    }
}

/// 결과 그림 경로(`<보관함>/<id>.png`)의 움직이는 장들 — 큰 미리보기가 같은 그림을 돌릴 때.
pub(crate) fn frames_for_path(path: &str) -> Option<Arc<Frames>> {
    let path = Path::new(path);
    let id = path.file_stem()?.to_str()?;
    (path.extension()? == "png" && path.parent()? == root()?.as_path()).then_some(())?;
    ready(id)
}

/// 다음 장 만기를 세운다 — 미리보기처럼 `Player` 밖에서 장을 그리는 쪽이 쓴다.
pub(crate) fn frame_now(frames: &Frames) -> usize {
    let (index, wait) = frames.at(clock());
    push_due(Instant::now() + wait);
    index
}

/// 창 하나(렌더러 하나)가 지금 텍스처에 올려 둔 장. 창마다 따로 둔다.
#[derive(Default)]
pub(crate) struct Player {
    shown: HashMap<String, usize>,
}

impl Player {
    /// `InlineImages::prepare` 다음에 부른다 — 정지 그림이 올라간 텍스처를 지금 장으로 바꿔 쓴다.
    pub(crate) fn paint(&mut self, g: &mut kasa_gridview::GridRenderer, slots: &[kasa_gridview::images::InlineSlot]) {
        let live: std::collections::HashSet<&str> = slots.iter().map(|s| s.key.as_str()).collect();
        // 칸에서 사라졌다 돌아온 그림은 정지 그림으로 다시 올라간다 — 기억한 장을 지워 다시 쓰게 한다.
        self.shown.retain(|k, _| live.contains(k.as_str()));
        let now = clock();
        let mut next: Option<Duration> = None;
        for slot in slots {
            let Some(id) = slot.key.strip_prefix("result:") else { continue };
            let Some(frames) = ready(id) else { continue };
            if !g.has_image(&slot.key) {
                continue;
            }
            let (index, wait) = frames.at(now);
            if self.shown.get(&slot.key) != Some(&index) {
                g.update_image(&slot.key, frames.rgba(index), frames.w, frames.h);
                self.shown.insert(slot.key.clone(), index);
            }
            next = Some(next.map_or(wait, |n| n.min(wait)));
        }
        if let Some(wait) = next {
            push_due(Instant::now() + wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_follow_their_delays_and_loop() {
        let gif = load::tests::gif(&[[255, 0, 0], [0, 255, 0], [0, 0, 255]], 8, 4, 50);
        let frames = decode(&gif, (8, 4), FRAME_BUDGET).unwrap();
        assert_eq!((frames.w, frames.h, frames.frames.len(), frames.total), (8, 4, 3, 150));
        assert!(frames.full);
        assert_eq!(frames.at(Duration::from_millis(0)), (0, Duration::from_millis(50)));
        assert_eq!(frames.at(Duration::from_millis(60)), (1, Duration::from_millis(40)));
        assert_eq!(frames.at(Duration::from_millis(149)).0, 2);
        assert_eq!(frames.at(Duration::from_millis(150)).0, 0, "끝나면 처음으로");
        assert_eq!(&frames.rgba(1)[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn unset_delays_play_at_browser_speed_and_big_animations_shrink_to_the_budget() {
        let fast = load::tests::gif(&[[0, 0, 0], [255, 255, 255]], 4, 4, 0);
        assert_eq!(decode(&fast, (4, 4), FRAME_BUDGET).unwrap().total, 200);
        let small_fit = decode(&load::tests::gif(&[[0, 0, 0], [9, 9, 9]], 40, 20, 100), (20, 10), FRAME_BUDGET).unwrap();
        assert_eq!((small_fit.w, small_fit.h), (20, 10), "정지 그림보다 크게 풀지 않는다");
        // 40×40 장 12개 = 76.8KB — 상한 64KB 를 넘어 절반 크기로 풀린다.
        let colors: Vec<[u8; 3]> = (0..12).map(|i| [i as u8 * 20, 0, 0]).collect();
        let gif = load::tests::gif(&colors, 40, 40, 40);
        let budget = Budget { bytes: 64 * 1024, min_edge: 10 };
        let big = decode(&gif, (40, 40), budget).unwrap();
        assert_eq!((big.w, big.h, big.frames.len()), (20, 20, 12));
        assert!(!big.full);
        assert!(big.bytes() <= budget.bytes);
        // 더 못 줄이면 뒤쪽 장을 버린다.
        let tight = decode(&gif, (40, 40), Budget { bytes: 40 * 40 * 4 * 5, min_edge: 40 }).unwrap();
        assert_eq!((tight.w, tight.frames.len(), tight.delays.len()), (40, 5, 5));
    }

    #[test]
    fn remote_originals_are_kept_only_when_the_mark_matches() {
        let root = std::env::temp_dir().join(format!("inline-anim-{}", uuid::Uuid::new_v4()));
        let gif = load::tests::gif(&[[1, 2, 3], [4, 5, 6]], 4, 4, 100);
        let sha = format!("{:x}", Sha256::digest(&gif));
        assert!(store_original(&root, "0123456789abcdef", &"0".repeat(64), &gif).is_err());
        assert!(store_original(&root, "0123456789abcdef", &format!("{:x}", Sha256::digest(b"#!")), b"#!").is_err());
        store_original(&root, "0123456789abcdef", &sha, &gif).unwrap();
        assert_eq!(std::fs::read(root.join("0123456789abcdef.anim")).unwrap(), gif);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_mirror_fetches_the_original_once_and_then_reads_its_copy() {
        use std::io::{Read, Write};
        let root = std::env::temp_dir().join(format!("inline-anim-remote-{}", uuid::Uuid::new_v4()));
        let gif = load::tests::gif(&[[9, 9, 9], [200, 0, 0]], 6, 3, 100);
        let sha = format!("{:x}", Sha256::digest(&gif));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = gif.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).contains("/term/inline-result/anim?id=0123456789abcdef"));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len()).unwrap();
            stream.write_all(&served).unwrap();
        });
        assert_eq!(original_in(&root, "0123456789abcdef", &sha, Some(&base)).unwrap(), gif);
        server.join().unwrap();
        // 원본 기기가 사라져도 받아 둔 사본으로 돈다.
        assert_eq!(original_in(&root, "0123456789abcdef", &sha, Some(&base)).unwrap(), gif);
        assert!(original_in(&root, "0123456789abcdef", &"1".repeat(64), None).is_err(), "표식과 다른 사본은 안 쓴다");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn due_is_consumed_once_and_the_earliest_wins() {
        let now = Instant::now();
        *due_slot().lock().unwrap() = None;
        push_due(now + Duration::from_millis(80));
        push_due(now + Duration::from_millis(30));
        assert_eq!(due(), Some(now + Duration::from_millis(30)));
        assert!(!take_due(now));
        assert!(take_due(now + Duration::from_millis(31)));
        assert_eq!(due(), None, "그리지 않으면 다시 깨지 않는다");
    }
}
