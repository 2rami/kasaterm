//! `imgopen` 이 받는 파일을 결과 그림으로 바꾼다 — 비트맵·SVG·움직이는 그림(GIF·WebP·APNG).
//!
//! 표식(`[[ki:…]]`)과 정지 그림 PNG 는 그대로다. 움직이는 그림은 원본을 `<id>.anim` 으로 곁에 두고, 정지 그림 PNG
//! 의 `tEXt` 조각(`kasaterm-anim`)에 그 원본의 SHA-256 을 적는다. 그래서 ① 옛 앱판·옛 폰은 첫 장을 정지 그림으로
//! 보고 ② 첫 장이 같은 두 GIF 도 PNG 바이트가 달라 다른 id 를 받으며 ③ 받는 쪽은 그 값으로 원본을 확인한다.

use std::io::Read;
use std::path::Path;

const MAX_INPUT: u64 = 64 * 1024 * 1024;
pub(crate) const MARK_KEY: &[u8] = b"kasaterm-anim";
/// SVG 는 크기가 없는 그림이라 결과 그림 상한(긴 변 1600)에 맞춰 굽는다 — 작은 아이콘도 크게 봐서 또렷하다.
const SVG_EDGE: f32 = 1600.0;

pub(crate) struct Loaded {
    pub image: image::DynamicImage,
    /// 두 장 이상 도는 원본 바이트. 상한을 넘으면 `None`(첫 장만 정지 그림).
    pub anim: Option<Vec<u8>>,
    /// 움직이는 그림이었지만 상한을 넘어 첫 장만 남겼다.
    pub anim_dropped: bool,
}

pub(crate) fn load(path: &Path) -> anyhow::Result<Loaded> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(MAX_INPUT + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_INPUT, "이미지가 64MiB를 넘습니다");
    if is_svg(path, &bytes) {
        return Ok(Loaded { image: rasterize_svg(&bytes, path.parent())?, anim: None, anim_dropped: false });
    }
    let format = image::guess_format(&bytes)?;
    let animated = is_animated(&bytes, format);
    let image = decode_still(&bytes, format)?;
    let fits = bytes.len() as u64 <= kasa_socket::inline_results::ANIM_MAX_BYTES;
    Ok(Loaded { image, anim_dropped: animated && !fits, anim: (animated && fits).then_some(bytes) })
}

fn is_svg(path: &Path, bytes: &[u8]) -> bool {
    if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz")) {
        return true;
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    head.trim_start_matches('\u{feff}').trim_start().starts_with('<') && head.contains("<svg")
}

fn decode_still(bytes: &[u8], format: image::ImageFormat) -> anyhow::Result<image::DynamicImage> {
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10000);
    limits.max_image_height = Some(10000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    anyhow::ensure!(
        image.width() > 0 && image.height() > 0 && u64::from(image.width()) * u64::from(image.height()) <= 64_000_000,
        "이미지가 너무 큽니다"
    );
    Ok(image)
}

/// 두 장 이상이면 움직이는 그림이다. 한 장짜리 GIF·APNG 는 그냥 그림으로 둔다.
pub(crate) fn is_animated(bytes: &[u8], format: image::ImageFormat) -> bool {
    use image::AnimationDecoder;
    let cursor = || std::io::Cursor::new(bytes);
    match format {
        image::ImageFormat::Gif => image::codecs::gif::GifDecoder::new(cursor())
            .is_ok_and(|d| d.into_frames().take(2).filter(Result::is_ok).count() == 2),
        image::ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(cursor()).is_ok_and(|d| d.has_animation()),
        image::ImageFormat::Png => image::codecs::png::PngDecoder::new(cursor()).is_ok_and(|d| {
            d.is_apng().unwrap_or(false) && d.apng().is_ok_and(|a| a.into_frames().take(2).filter(Result::is_ok).count() == 2)
        }),
        _ => false,
    }
}

fn rasterize_svg(bytes: &[u8], dir: Option<&Path>) -> anyhow::Result<image::DynamicImage> {
    use resvg::{tiny_skia, usvg};
    let mut options = usvg::Options { resources_dir: dir.map(Path::to_path_buf), ..Default::default() };
    // 에이전트가 그리는 SVG 는 대개 글이 든 도식이다. 글꼴 목록이 비면 글자가 통째로 빠진다.
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_data(bytes, &options)?;
    let size = tree.size();
    anyhow::ensure!(size.width() > 0.0 && size.height() > 0.0, "SVG 크기가 없습니다");
    let scale = SVG_EDGE / size.width().max(size.height());
    let (w, h) = ((size.width() * scale).round().max(1.0) as u32, (size.height() * scale).round().max(1.0) as u32);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow::anyhow!("SVG 를 그릴 수 없습니다"))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut rgba = image::RgbaImage::new(w, h);
    for (out, px) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = px.demultiply();
        *out = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    if needs_light_backing(&rgba) {
        back_with_white(&mut rgba);
    }
    Ok(image::DynamicImage::ImageRgba8(rgba))
}

/// 배경 없는 SVG 의 검은 글·선은 어두운 칸 바탕에 묻힌다. 비친 자리가 있고 그려진 부분이 어두우면 흰 종이에
/// 얹는다 — 브라우저가 SVG 를 흰 페이지에 띄우는 것과 같다. 밝은 선 그림은 칸 바탕에 그대로 둔다.
fn needs_light_backing(rgba: &image::RgbaImage) -> bool {
    let (mut clear, mut luma, mut weight) = (false, 0.0f64, 0.0f64);
    for p in rgba.pixels() {
        let a = f64::from(p[3]) / 255.0;
        clear |= p[3] < 250;
        luma += a * (0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])) / 255.0;
        weight += a;
    }
    clear && weight > 0.0 && luma / weight < 0.5
}

fn back_with_white(rgba: &mut image::RgbaImage) {
    for p in rgba.pixels_mut() {
        let a = u32::from(p[3]);
        for c in 0..3 {
            p[c] = ((u32::from(p[c]) * a + 255 * (255 - a)) / 255) as u8;
        }
        p[3] = 255;
    }
}

/// PNG 의 IHDR 바로 뒤에 `tEXt` 조각을 끼운다. image 크레이트의 PNG 인코더는 글 조각을 못 쓴다.
pub(crate) fn with_mark(png: &[u8], value: &str) -> anyhow::Result<Vec<u8>> {
    const IHDR_END: usize = 8 + 4 + 4 + 13 + 4;
    anyhow::ensure!(png.len() > IHDR_END && png.starts_with(b"\x89PNG\r\n\x1a\n") && &png[12..16] == b"IHDR", "PNG 가 아닙니다");
    let mut data = MARK_KEY.to_vec();
    data.push(0);
    data.extend_from_slice(value.as_bytes());
    let mut chunk = Vec::with_capacity(data.len() + 12);
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(&data);
    chunk.extend_from_slice(&crc32(&chunk[4..]).to_be_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..IHDR_END]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[IHDR_END..]);
    Ok(out)
}

/// PNG 첫 IDAT 전까지의 `kasaterm-anim` 값(소문자 64자리 hex). 표식은 IHDR 바로 뒤에 쓰므로 앞부분만 본다.
pub(crate) fn read_mark(png: &[u8]) -> Option<String> {
    let mut at = 8;
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().ok()?) as usize;
        let kind = &png[at + 4..at + 8];
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        let data = png.get(at + 8..at + 8 + len)?;
        if kind == b"tEXt" {
            if let Some(value) = data.strip_prefix(MARK_KEY).and_then(|rest| rest.strip_prefix(b"\0")) {
                let value = std::str::from_utf8(value).ok()?;
                return (value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    .then(|| value.to_string());
            }
        }
        at += 12 + len;
    }
    None
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 색이 다른 장 `colors.len()` 개짜리 GIF.
    pub(crate) fn gif(colors: &[[u8; 3]], w: u32, h: u32, delay_ms: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
            encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            for c in colors {
                let frame = image::RgbaImage::from_pixel(w, h, image::Rgba([c[0], c[1], c[2], 255]));
                let delay = image::Delay::from_numer_denom_ms(delay_ms, 1);
                encoder.encode_frame(image::Frame::from_parts(frame, 0, 0, delay)).unwrap();
            }
        }
        out
    }

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("inline-load-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn gifs_with_two_frames_keep_their_original_and_single_frames_do_not() {
        let moving = gif(&[[255, 0, 0], [0, 0, 255]], 40, 20, 100);
        let loaded = load(&temp("a.gif", &moving)).unwrap();
        assert_eq!(loaded.anim.as_deref(), Some(moving.as_slice()));
        assert_eq!((loaded.image.width(), loaded.image.height()), (40, 20));
        assert_eq!(loaded.image.to_rgba8().get_pixel(0, 0).0, [255, 0, 0, 255], "정지 그림은 첫 장");
        let still = gif(&[[0, 255, 0]], 40, 20, 100);
        assert!(load(&temp("b.gif", &still)).unwrap().anim.is_none());
    }

    #[test]
    fn svg_rasterizes_at_the_result_edge_and_dark_drawings_get_paper() {
        let dark = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect x="2" y="2" width="4" height="4" fill="#000"/></svg>"##;
        let loaded = load(&temp("d.svg", dark)).unwrap();
        assert_eq!((loaded.image.width(), loaded.image.height()), (1600, 800));
        let rgba = loaded.image.to_rgba8();
        assert_eq!(rgba.get_pixel(1500, 700).0, [255, 255, 255, 255], "검은 그림은 흰 종이 위");
        assert_eq!(rgba.get_pixel(320, 320).0, [0, 0, 0, 255]);
        let light = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><circle cx="5" cy="5" r="2" fill="#fff"/></svg>"##;
        let rgba = load(&temp("no-extension", light)).unwrap().image.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0[3], 0, "밝은 그림은 칸 바탕에 그대로");
    }

    #[test]
    fn the_mark_survives_decoding_and_is_found_again() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(3, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let sha = "ab".repeat(32);
        let marked = with_mark(png.get_ref(), &sha).unwrap();
        assert_eq!(read_mark(&marked).as_deref(), Some(sha.as_str()));
        assert_eq!(image::load_from_memory(&marked).unwrap().width(), 3, "디코더가 조각을 받아들인다");
        assert_eq!(read_mark(png.get_ref()), None);
        assert_eq!(read_mark(&with_mark(png.get_ref(), "not-a-sha").unwrap()), None);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }
}
