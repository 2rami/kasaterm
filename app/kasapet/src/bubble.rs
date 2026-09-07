//! 말풍선 글자 — **투명 위에 흰 글자만** 그린 텍스처. 말풍선 모양(배경·테두리·꼬리)은
//! PNG 한 장이 맡는다(2026-09-07 지시 「말풍선도 png 로 글만 뜨게」). 글자는 시스템
//! 한글 폰트(AppleSDGothicNeo 레귤러)를 swash 로 찍는다 — 워크스페이스(kasa-cells)가
//! 이미 쓰는 크레이트라 의존 트리가 안 는다.
//!
//! 레티나에서 흐리지 않게 2배로 래스터하고 **논리 크기**를 돌려준다. 픽셀은
//! 프리멀티플라이드 RGBA — 그리는 파이프라인(p_plain, One/OneMinusSrcAlpha)이 그렇게
//! 섞는다. 흰 글자라 RGB 도 커버리지와 같다.

use swash::scale::{image::Content, Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

const FONT_PATH: &str = "/System/Library/Fonts/AppleSDGothicNeo.ttc";
const FONT_PT: f32 = 13.0;
const LINE_SPACING: f32 = 1.4;
/// 레티나 — 래스터는 이 배율, 반환은 논리.
const SCALE: f32 = 2.0;

/// 글자 둘레에 두르는 어두운 테두리의 두께(래스터 픽셀). 말풍선 판을 걷어내고 글자만
/// 띄우므로, 이게 없으면 밝은 바탕화면 위에서 흰 글자가 통째로 사라진다.
const HALO: i32 = 3;

fn font_data() -> &'static [u8] {
    static FONT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| std::fs::read(FONT_PATH).unwrap_or_default())
}

struct Glyph {
    gid: u16,
    adv: f32,
}

/// 줄바꿈까지 끝난 판 — 픽셀 단위(2배).
struct Layout {
    lines: Vec<Vec<Glyph>>,
    width: f32,
    line_h: f32,
    ascent: f32,
    descent: f32,
}

/// 낱말이 있으면 낱말에서, 없으면 글자에서 끊는다 — 한국어는 띄어쓰기 없이 오래
/// 이어지는 문장이 많아 글자 단위가 없으면 한 줄이 상한을 뚫는다. `\n` 은 그대로 줄.
fn layout(font: &FontRef, text: &str, px: f32, max_w: f32) -> Layout {
    let m = font.metrics(&[]).scale(px);
    let metrics = font.glyph_metrics(&[]).scale(px);
    let charmap = font.charmap();
    let mut lines: Vec<Vec<Glyph>> = Vec::new();
    let mut cur: Vec<Glyph> = Vec::new();
    let mut cur_w = 0.0_f32;
    let mut last_space: Option<usize> = None;
    let sum = |v: &[Glyph]| v.iter().map(|g| g.adv).sum::<f32>();
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0.0;
            last_space = None;
            continue;
        }
        let gid = charmap.map(ch as u32);
        let adv = metrics.advance_width(gid);
        if cur_w + adv > max_w && !cur.is_empty() {
            match last_space {
                Some(i) => {
                    let tail = cur.split_off(i + 1);
                    cur.truncate(i); // 줄 끝 빈칸은 버린다
                    lines.push(std::mem::replace(&mut cur, tail));
                }
                None => lines.push(std::mem::take(&mut cur)),
            }
            cur_w = sum(&cur);
            last_space = None;
            if ch == ' ' {
                continue; // 줄머리 빈칸은 버린다
            }
        }
        if ch == ' ' {
            last_space = Some(cur.len());
        }
        cur_w += adv;
        cur.push(Glyph { gid, adv });
    }
    lines.push(cur);
    let width = lines.iter().map(|l| sum(l)).fold(0.0_f32, f32::max);
    Layout {
        lines,
        width,
        line_h: px * LINE_SPACING,
        ascent: m.ascent,
        descent: m.descent,
    }
}

/// 글자만 찍은 프리멀티플라이드 RGBA(2배 픽셀) — (버퍼, 폭, 높이). 빈 글이면 None.
/// `max_w` 는 논리 pt.
fn raster(text: &str, max_w: f32) -> Option<(Vec<u8>, u32, u32)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let font = FontRef::from_index(font_data(), 0)?;
    let px = FONT_PT * SCALE;
    let lay = layout(&font, text, px, max_w * SCALE);
    // 글자가 위아래로 삐져나오지 않게 한 줄 높이 안에 ascent+descent 를 가운데 둔다.
    // 테두리가 잘리지 않게 사방을 그만큼 넓혀 둔다.
    let pad = HALO as u32 + 1;
    let w = lay.width.ceil().max(1.0) as u32 + pad * 2;
    let h = (lay.lines.len() as f32 * lay.line_h).ceil().max(1.0) as u32 + pad * 2;
    let mut buf = vec![0u8; (w * h * 4) as usize];

    let mut ctx = ScaleContext::new();
    let mut scaler = ctx.builder(font).size(px).hint(true).build();
    let mut render = Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
        Source::Bitmap(StrikeWith::BestFit),
    ]);
    render.format(Format::Alpha);
    let mut baseline = lay.ascent + (lay.line_h - (lay.ascent + lay.descent)) / 2.0;
    for line in &lay.lines {
        let mut pen = 0.0_f32;
        for g in line {
            if let Some(img) = render.render(&mut scaler, g.gid) {
                let (pw, ph) = (img.placement.width as i32, img.placement.height as i32);
                let x0 = pen.round() as i32 + img.placement.left + pad as i32;
                let y0 = baseline.round() as i32 - img.placement.top + pad as i32;
                for ry in 0..ph {
                    for rx in 0..pw {
                        let (x, y) = (x0 + rx, y0 + ry);
                        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                            continue;
                        }
                        // 흰 글자: RGB = 커버리지. 컬러 비트맵(이모지)은 제 색 그대로.
                        let (cov, rgb) = match img.content {
                            Content::Color => {
                                let i = ((ry * pw + rx) * 4) as usize;
                                (img.data[i + 3], [img.data[i], img.data[i + 1], img.data[i + 2]])
                            }
                            _ => {
                                let c = img.data[(ry * pw + rx) as usize];
                                (c, [c, c, c])
                            }
                        };
                        if cov == 0 {
                            continue;
                        }
                        let o = ((y as u32 * w + x as u32) * 4) as usize;
                        // 겹치는 자리는 밝은 쪽 — 글자끼리 살짝 물려도 어두워지지 않게.
                        for i in 0..3 {
                            buf[o + i] = buf[o + i].max(rgb[i]);
                        }
                        buf[o + 3] = buf[o + 3].max(cov);
                    }
                }
            }
            pen += g.adv;
        }
        baseline += lay.line_h;
    }

    // 글자 밑에 어두운 테두리를 깔아 어떤 바탕화면 위에서도 읽힌다. 글자 알파를 조금
    // 부풀린 것이 테두리이고, 결과는 미리곱 알파라 색은 글자 몫만 남긴다.
    let mut out = vec![0u8; buf.len()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let at = |xx: i32, yy: i32| -> u32 {
                if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                    0
                } else {
                    buf[((yy as u32 * w + xx as u32) * 4 + 3) as usize] as u32
                }
            };
            let mut halo = 0u32;
            for dy in -HALO..=HALO {
                for dx in -HALO..=HALO {
                    if dx * dx + dy * dy <= HALO * HALO {
                        halo = halo.max(at(x + dx, y + dy));
                    }
                }
            }
            let o = ((y as u32 * w + x as u32) * 4) as usize;
            let a = buf[o + 3] as u32;
            // 테두리는 검정이라 RGB 기여가 0 이다 — 글자 색만 그대로 옮긴다.
            out[o] = buf[o];
            out[o + 1] = buf[o + 1];
            out[o + 2] = buf[o + 2];
            let halo = halo * 200 / 255;
            out[o + 3] = (a + halo * (255 - a) / 255).min(255) as u8;
        }
    }
    Some((out, w, h))
}

/// 글자만 그린 투명 텍스처. 반환은 (view, 논리폭, 논리높이).
pub fn render_text(
    dev: &wgpu::Device,
    q: &wgpu::Queue,
    text: &str,
    max_w: f32,
) -> Option<(wgpu::TextureView, f32, f32)> {
    let (buf, w, h) = raster(text, max_w)?;
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let tex = dev.create_texture(&wgpu::TextureDescriptor {
        label: Some("bubble-text"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    q.write_texture(
        tex.as_image_copy(),
        &buf,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
        size,
    );
    Some((tex.create_view(&Default::default()), w as f32 / SCALE, h as f32 / SCALE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &str, max_w: f32) -> (usize, f32) {
        let font = FontRef::from_index(font_data(), 0).expect("시스템 한글 폰트");
        let lay = layout(&font, text, FONT_PT * SCALE, max_w * SCALE);
        (lay.lines.len(), lay.width / SCALE)
    }

    #[test]
    fn empty_text_is_none() {
        assert!(raster("", 260.0).is_none());
        assert!(raster("   \n ", 260.0).is_none());
    }

    #[test]
    fn short_text_stays_on_one_line() {
        let (n, w) = lines_of("안녕", 260.0);
        assert_eq!(n, 1);
        assert!(w > 0.0 && w < 60.0, "폭 {w}");
    }

    #[test]
    fn long_text_wraps_within_max_width() {
        let long = "선생님 오늘도 수고 많으셨어요 이제 좀 쉬셔도 돼요 코하루가 지켜보고 있을게요";
        let (n, w) = lines_of(long, 120.0);
        assert!(n >= 3, "줄 {n}");
        assert!(w <= 120.0, "폭 {w}");
        // 상한이 넓으면 줄이 준다.
        let (n2, _) = lines_of(long, 400.0);
        assert!(n2 < n);
    }

    #[test]
    fn text_without_spaces_wraps_by_character() {
        let (n, w) = lines_of("띄어쓰기없이아주길게이어지는한국어문장이라도상한을넘으면잘라야한다", 100.0);
        assert!(n >= 4, "줄 {n}");
        assert!(w <= 100.0, "폭 {w}");
    }

    #[test]
    fn newline_forces_a_break() {
        let (n, _) = lines_of("첫 줄\n둘째 줄", 260.0);
        assert_eq!(n, 2);
    }

    #[test]
    fn raster_paints_white_premultiplied_pixels() {
        let (buf, w, h) = raster("가", 260.0).unwrap();
        assert!(w > 0 && h > 0);
        let painted = buf.chunks(4).filter(|p| p[3] > 0).count();
        assert!(painted > 20, "찍힌 픽셀 {painted}");
        // 글자 속은 흰색(RGB = 알파), 둘레는 검은 테두리(RGB 0 에 알파만) — 어느 쪽이든
        // RGB 가 알파를 넘지 않아야 미리곱 알파가 성립한다.
        assert!(buf.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        let haloed = buf.chunks(4).filter(|p| p[3] > 0 && p[0] == 0).count();
        assert!(haloed > 20, "테두리 픽셀 {haloed}");
    }
}
