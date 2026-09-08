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

/// 기본 글자 크기(pt). 설정에서 바꾸면 `pet/text_pt` 파일이 이 값을 대신한다.
#[cfg(test)]
pub const FONT_PT: f32 = 13.0;
const LINE_SPACING: f32 = 1.4;
/// 레티나 — 래스터는 이 배율, 반환은 논리.
const SCALE: f32 = 2.0;

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    viewport: (f32, f32),
}
impl Geometry {
    pub fn contains(self, point: (f32, f32)) -> bool {
        point.0 >= self.left
            && point.0 <= self.left + self.width
            && point.1 >= self.top
            && point.1 <= self.top + self.height
    }
    pub fn ndc(self) -> (f32, f32, f32, f32) {
        (
            self.left / self.viewport.0 * 2.0 - 1.0,
            1.0 - self.top / self.viewport.1 * 2.0,
            (self.left + self.width) / self.viewport.0 * 2.0 - 1.0,
            1.0 - (self.top + self.height) / self.viewport.1 * 2.0,
        )
    }
}

/// A stale texture can briefly outlive a surface resize. Fit its actual extent
/// before positioning it, and share these exact bounds with mouse hit testing.
pub fn geometry(viewport: (f32, f32), texture: (f32, f32), head: (f32, f32)) -> Option<Geometry> {
    if ![viewport.0, viewport.1, texture.0, texture.1]
        .into_iter()
        .all(|v| v.is_finite() && v > 0.0)
        || !head.0.is_finite()
        || !head.1.is_finite()
    {
        return None;
    }
    let margin = 8.0_f32.min(viewport.0 / 4.0).min(viewport.1 / 4.0);
    let available = (viewport.0 - 2.0 * margin, viewport.1 - 2.0 * margin);
    let scale = 1.0_f32
        .min(available.0 / texture.0)
        .min(available.1 / texture.1);
    let (width, height) = (texture.0 * scale, texture.1 * scale);
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let x = (head.0 + 1.0) * 0.5 * viewport.0 - width * 0.5;
    let y = (1.0 - head.1) * 0.5 * viewport.1 - 10.0 - height;
    Some(Geometry {
        left: x.clamp(margin, (viewport.0 - margin - width).max(margin)),
        top: y.clamp(margin, (viewport.1 - margin - height).max(margin)),
        width,
        height,
        viewport,
    })
}

pub struct Preview {
    pub text: String,
    pub width: f32,
    pub pt: f32,
}

/// Keep notifications as previews. Wrapping respects the current viewport and
/// the chosen font size; an ellipsis marks content available in the full view.
pub fn preview(text: &str, viewport: (f32, f32), requested_pt: f32) -> Option<Preview> {
    if text.trim().is_empty()
        || ![viewport.0, viewport.1, requested_pt]
            .into_iter()
            .all(|v| v.is_finite() && v > 0.0)
    {
        return None;
    }
    let margin = 8.0_f32.min(viewport.0 / 4.0).min(viewport.1 / 4.0);
    let pad = (HALO + 1) as f32 * 2.0 / SCALE;
    let width = (260.0_f32.min(viewport.0 - 2.0 * margin) - pad).floor();
    let height = 110.0_f32.min(viewport.1 - 2.0 * margin);
    if width < 1.0 || height <= pad {
        return None;
    }
    let font = FontRef::from_index(font_data(), 0)?;
    let mut chars: Vec<char> = text.trim().chars().take(2049).collect();
    let input_cut = chars.len() > 2048;
    chars.truncate(2048);
    let mut pt = requested_pt.min((height - pad) / LINE_SPACING);
    let metrics = font.glyph_metrics(&[]).scale(pt * SCALE);
    let map = font.charmap();
    let widest = chars
        .iter()
        .copied()
        .chain(['…'])
        .map(|c| metrics.advance_width(map.map(c as u32)) / SCALE)
        .fold(0.0_f32, f32::max);
    if widest > width {
        pt *= width / widest;
    }
    if !pt.is_finite() || pt <= 0.0 {
        return None;
    }
    let fits = |body: &str| {
        let layout = layout(&font, body, pt * SCALE, width * SCALE);
        layout.width.ceil() / SCALE + pad <= width + pad
            && (layout.lines.len() as f32 * layout.line_h).ceil() / SCALE + pad <= height
    };
    let all: String = chars.iter().collect();
    if !input_cut && fits(&all) {
        return Some(Preview {
            text: all,
            width,
            pt,
        });
    }
    if !fits("…") {
        return None;
    }
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = (low + high + 1) / 2;
        let candidate = format!("{}…", chars[..mid].iter().collect::<String>().trim_end());
        if fits(&candidate) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Some(Preview {
        text: format!("{}…", chars[..low].iter().collect::<String>().trim_end()),
        width,
        pt,
    })
}

/// 글자 둘레에 두르는 어두운 테두리의 두께(래스터 픽셀). 말풍선 판을 걷어내고 글자만
/// 띄우므로, 이게 없으면 밝은 바탕화면 위에서 흰 글자가 통째로 사라진다.
const HALO: i32 = 3;

/// 글자도 테두리도 없는 자리의 알파. 0 이면 macOS 가 그 픽셀의 마우스를 아래 창으로
/// 넘겨 버려 말풍선을 누를 수 없다 — 눌러서 그 pane 으로 가는 길이 거기서 끊긴다.
const FLOOR_A: u32 = 6;

/// 시스템 한글 폰트 — 담아 온 메이플스토리체를 못 찾았을 때만.
const FALLBACK_FONT: &str = "/System/Library/Fonts/AppleSDGothicNeo.ttc";

/// 담아 온 서체. 넥슨이 무료로 배포하는 메이플스토리체이고, 소프트웨어에 함께 담아도
/// 되는 조건이라 레포에 넣었다(assets/fonts/LICENSE-Maplestory.txt).
const BUNDLED_FONT: &str = "Maplestory Bold.ttf";

fn font_data() -> &'static [u8] {
    static FONT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        bundled_font_path()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_else(|| std::fs::read(FALLBACK_FONT).unwrap_or_default())
    })
}

/// 앱 번들 Resources 아니면 개발 트리의 assets/fonts.
fn bundled_font_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let beside = exe.parent().map(|d| d.join(BUNDLED_FONT));
    let dev = exe
        .ancestors()
        .find(|a| a.join("assets/fonts").join(BUNDLED_FONT).is_file())
        .map(|a| a.join("assets/fonts").join(BUNDLED_FONT));
    beside.into_iter().chain(dev).find(|p| p.is_file())
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
fn raster(text: &str, max_w: f32, pt: f32) -> Option<(Vec<u8>, u32, u32)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let font = FontRef::from_index(font_data(), 0)?;
    let px = pt * SCALE;
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
                                (
                                    img.data[i + 3],
                                    [img.data[i], img.data[i + 1], img.data[i + 2]],
                                )
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
            // 바닥 알파를 0 이 아니라 아주 작은 값으로 둔다. macOS 는 투명 창에서 알파가
            // **정확히 0** 인 픽셀의 마우스를 아래 창으로 통과시켜서, 0 으로 두면 글자 획을
            // 정확히 짚지 않는 한 말풍선을 누를 수도 없고 커서도 손모양이 안 된다
            // (2026-09-08 실측: 말풍선 띠 전체에서 화살표였다). 눈에는 안 보인다.
            out[o + 3] = (a + halo * (255 - a) / 255).clamp(FLOOR_A, 255) as u8;
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
    pt: f32,
) -> Option<(wgpu::TextureView, f32, f32)> {
    let (buf, w, h) = raster(text, max_w, pt)?;
    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
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
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * w),
            rows_per_image: Some(h),
        },
        size,
    );
    Some((
        tex.create_view(&Default::default()),
        w as f32 / SCALE,
        h as f32 / SCALE,
    ))
}

pub fn render_preview(
    dev: &wgpu::Device,
    q: &wgpu::Queue,
    text: &str,
    viewport: (f32, f32),
    pt: f32,
) -> Option<(wgpu::TextureView, f32, f32)> {
    let plan = preview(text, viewport, pt)?;
    render_text(dev, q, &plan.text, plan.width, plan.pt)
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
        assert!(raster("", 260.0, FONT_PT).is_none());
        assert!(raster("   \n ", 260.0, FONT_PT).is_none());
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
        let (n, w) = lines_of(
            "띄어쓰기없이아주길게이어지는한국어문장이라도상한을넘으면잘라야한다",
            100.0,
        );
        assert!(n >= 4, "줄 {n}");
        assert!(w <= 100.0, "폭 {w}");
    }

    #[test]
    fn newline_forces_a_break() {
        let (n, _) = lines_of("첫 줄\n둘째 줄", 260.0);
        assert_eq!(n, 2);
    }

    /// 설정에서 키운 크기가 실제로 더 큰 글자로 나온다.
    #[test]
    fn a_bigger_point_size_makes_a_bigger_raster() {
        let (_, w1, h1) = raster("가나다", 400.0, 13.0).unwrap();
        let (_, w2, h2) = raster("가나다", 400.0, 26.0).unwrap();
        assert!(w2 > w1 * 3 / 2, "폭 {w1} → {w2}");
        assert!(h2 > h1 * 3 / 2, "높이 {h1} → {h2}");
    }

    #[test]
    fn raster_paints_white_premultiplied_pixels() {
        let (buf, w, h) = raster("가", 260.0, FONT_PT).unwrap();
        assert!(w > 0 && h > 0);
        let painted = buf.chunks(4).filter(|p| p[3] > 0).count();
        assert!(painted > 20, "찍힌 픽셀 {painted}");
        // 글자 속은 흰색(RGB = 알파), 둘레는 검은 테두리(RGB 0 에 알파만) — 어느 쪽이든
        // RGB 가 알파를 넘지 않아야 미리곱 알파가 성립한다.
        assert!(buf
            .chunks(4)
            .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        let haloed = buf.chunks(4).filter(|p| p[3] > 0 && p[0] == 0).count();
        assert!(haloed > 20, "테두리 픽셀 {haloed}");
    }

    #[test]
    fn recorded_crash_width_fits_before_clamping_and_hit_matches_draw() {
        let viewport = (168.0, 284.0);
        let rect = geometry(viewport, (168.0 * 2.1073446 / 2.0, 70.0), (0.0, 0.9)).unwrap();
        let (left, top, right, bottom) = rect.ndc();
        assert!([left, top, right, bottom]
            .iter()
            .all(|v| v.is_finite() && *v >= -1.0 && *v <= 1.0));
        assert!(rect.width <= viewport.0 - 16.0);
        let center = (
            ((left + right) * 0.25 + 0.5) * viewport.0,
            (0.5 - (top + bottom) * 0.25) * viewport.1,
        );
        assert!(rect.contains(center));
        assert!(!rect.contains((rect.left - 0.1, rect.top)));
        assert!(!rect.contains((rect.left, rect.top + rect.height + 0.1)));
    }

    #[test]
    fn long_korean_large_font_and_resizes_only_shorten_the_preview() {
        let raw = "재시작한뒤곽향말풍선과나쵸대화를확인해주세요 긴 한글 안내입니다. ".repeat(100);
        let original = raw.clone();
        for viewport in [
            (168.0, 284.0),
            (420.0, 710.0),
            (210.0, 355.0),
            (168.0, 284.0),
            (32.0, 24.0),
        ] {
            for pt in [8.0, 13.0, 40.0, 120.0] {
                let Some(plan) = preview(&raw, viewport, pt) else {
                    continue;
                };
                assert!(plan.text.ends_with('…') && plan.text.len() < raw.len());
                let (_, w, h) = raster(&plan.text, plan.width, plan.pt).unwrap();
                assert!(w as f32 / SCALE <= viewport.0);
                assert!(h as f32 / SCALE <= 110.0_f32.min(viewport.1));
                let rect =
                    geometry(viewport, (w as f32 / SCALE, h as f32 / SCALE), (1.5, -1.5)).unwrap();
                assert!(
                    rect.left >= 0.0
                        && rect.top >= 0.0
                        && rect.left + rect.width <= viewport.0
                        && rect.top + rect.height <= viewport.1
                );
            }
        }
        assert_eq!(raw, original);
    }

    #[test]
    fn invalid_or_zero_surfaces_never_create_text_geometry() {
        for viewport in [
            (0.0, 100.0),
            (100.0, 0.0),
            (f32::NAN, 100.0),
            (100.0, f32::INFINITY),
        ] {
            assert!(preview("안내", viewport, 40.0).is_none());
            assert!(geometry(viewport, (260.0, 110.0), (0.0, 0.0)).is_none());
        }
        assert!(geometry((168.0, 284.0), (f32::NAN, 20.0), (0.0, 0.0)).is_none());
        assert!(preview("안내", (168.0, 284.0), f32::NAN).is_none());
    }
}
