//! 계정 폰 알림의 학생 얼굴 — 데스크톱이 관문에 맡기는 그림과 그 검사. 관문도 같은 검사로 받는다.
//!
//! 폰 알림 확장은 앱의 열쇠고리를 못 봐 얼굴을 인증 없이 받아야 하는데, 계정 경로(`/relay/account/…`)는 기기 토큰이
//! 있어야 열린다. 그래서 데스크톱이 그림을 관문에 맡기고, 관문이 추측할 수 없는 공개 주소를 payload `avatar` 로
//! 싣는다(`gateway_push_alert.rs`). 폰 앱 번들에는 학생 그림을 넣지 않으니(`mobile/pubspec.yaml`) 확장이 그림을 들고
//! 있을 수도 없다.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 관문이 받는 그림 한 장의 상한. 번들·테마 프사는 96px 에 25KB 를 넘지 않는다.
pub const MAX_BYTES: usize = 64 * 1024;
/// 관문이 받는 가로·세로 상한. 데스크톱은 96px 로 보내지만, 그 값을 바꿔도 깔린 관문이 거절하지 않게 남겨 둔다.
pub const MAX_SIDE: u32 = 256;
/// 데스크톱이 보내는 크기 — 알림 아이콘 자리(40pt, 3배 화면 120px)에 충분하다.
const SIDE: u32 = 96;
/// 옛 관문이 얼굴 필드를 거절한 뒤 다시 실어 보기까지. 관문을 새 판으로 갈면 앱을 재시작하지 않아도 돌아온다.
const PAUSE: Duration = Duration::from_secs(60 * 60);

pub struct Face {
    /// 그림의 sha256(소문자 16진수 64자) — 관문이 같은 그림을 다시 안 받는 열쇠.
    pub hash: String,
    pub png: Vec<u8>,
}

static SOURCE: OnceLock<fn(&str) -> Option<Vec<u8>>> = OnceLock::new();
static PAUSED: Mutex<Option<Instant>> = Mutex::new(None);

/// 슬러그 → 프사 원본. 그림은 HTTP 층(`/term/avatar`, 사용자가 바꾼 얼굴 → 번들 순)이 쥐고 있어 그 층이 서버를 띄울
/// 때 건다 — 이 층에서 같은 찾기 순서를 한 벌 더 두면 둘이 어긋나도 오류가 안 난다.
pub fn set_source(source: fn(&str) -> Option<Vec<u8>>) {
    let _ = SOURCE.set(source);
}

/// 알림에 실을 얼굴. 그림이 없거나 옛 관문이 얼굴을 거절한 직후면 `None` — 알림은 얼굴 없이 간다.
pub(super) fn of(slug: &str) -> Option<Face> {
    if PAUSED.lock().ok()?.is_some_and(|at| at.elapsed() < PAUSE) {
        return None;
    }
    from_raw(&SOURCE.get()?(slug)?)
}

pub(super) fn pause() {
    if let Ok(mut at) = PAUSED.lock() {
        *at = Some(Instant::now());
    }
}

pub fn from_raw(raw: &[u8]) -> Option<Face> {
    let png = shrink(raw)?;
    Some(Face { hash: hash(&png), png })
}

pub fn hash(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// PNG 시그니처와 첫 덩이 IHDR 의 가로·세로.
fn size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let side = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    Some((side(16)?, side(20)?))
}

/// 관문이 받는 그림인가 — PNG 시그니처·가로세로·용량을 보고, 실제로 풀리는지까지 본다(폰에 깨진 그림을 넘기지 않게).
/// 머리에 적힌 크기를 먼저 막으니 푸는 데 드는 메모리도 그 안이다.
pub fn acceptable(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_BYTES
        && size(bytes).is_some_and(|(w, h)| (1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h))
        && image::load_from_memory_with_format(bytes, image::ImageFormat::Png).is_ok()
}

/// 이미 작은 PNG 면 그대로(번들·테마 프사가 그렇다), 아니면 96px 안으로 줄여 PNG 로 — 사람이 큰 그림으로 바꾼 얼굴.
fn shrink(raw: &[u8]) -> Option<Vec<u8>> {
    if size(raw).is_some_and(|(w, h)| w <= SIDE && h <= SIDE) && acceptable(raw) {
        return Some(raw.to_vec());
    }
    let small = image::load_from_memory(raw).ok()?.thumbnail(SIDE, SIDE);
    let mut out = Vec::new();
    small.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    acceptable(&out).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn small_faces_go_as_they_are_and_big_ones_shrink_to_96() {
        let small = png(96, 96);
        let face = from_raw(&small).unwrap();
        assert_eq!(face.png, small);
        assert!(is_hash(&face.hash) && face.hash == hash(&small));

        let big = from_raw(&png(600, 300)).unwrap();
        assert_eq!(size(&big.png), Some((96, 48)));
        assert!(acceptable(&big.png));

        let mut jpeg = Vec::new();
        image::RgbImage::new(200, 200)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(size(&from_raw(&jpeg).unwrap().png), Some((96, 96)));
        assert!(from_raw(b"not an image").is_none());
    }

    #[test]
    fn the_gateway_check_wants_a_real_small_png() {
        assert!(acceptable(&png(1, 1)) && acceptable(&png(MAX_SIDE, 8)));
        assert!(!acceptable(&png(MAX_SIDE + 1, 8)));
        let mut cut = png(96, 96);
        cut.truncate(cut.len() / 2);
        assert!(!acceptable(&cut), "머리만 맞고 몸통이 깨진 그림");
        let mut fake = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x10\0\0\0\x10".to_vec();
        fake.resize(200, 0);
        assert!(!acceptable(&fake));
        assert!(!acceptable(b"GIF89a"));
        let mut huge = png(96, 96);
        huge.resize(MAX_BYTES + 1, 0);
        assert!(!acceptable(&huge));
    }

    #[test]
    fn hashes_are_lowercase_sha256_hex() {
        assert!(is_hash(&"a".repeat(64)));
        for bad in ["A".repeat(64), "a".repeat(63), "g".repeat(64), format!("{}/", "a".repeat(63))] {
            assert!(!is_hash(&bad), "{bad}");
        }
    }
}
