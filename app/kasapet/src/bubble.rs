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
/// 짧은 말이 앉는 상자. 머리 위 빈자리(HEADROOM)에 맞춘 크기다.
const COMPACT: (f32, f32) = (260.0, 110.0);
/// 긴 답(나쵸의 모든 기기 요약)이 앉는 상자. 너비는 설정(`bubble_width`)이고 높이는 창의
/// 절반 조금 못 미치게 — 작은 상자가 넘칠 때만 이리로 커진다(2026-09-17 지시 「답이
/// 채팅창 밖 말풍선으로」).
const ROOMY_HEIGHT_RATIO: f32 = 0.45;

pub fn preview(text: &str, viewport: (f32, f32), requested_pt: f32, roomy_width: f32) -> Option<Preview> {
    if text.trim().is_empty()
        || ![viewport.0, viewport.1, requested_pt, roomy_width]
            .into_iter()
            .all(|v| v.is_finite() && v > 0.0)
    {
        return None;
    }
    let margin = 8.0_f32.min(viewport.0 / 4.0).min(viewport.1 / 4.0);
    let pad = (HALO + 1) as f32 * 2.0 / SCALE;
    let faces = faces();
    if faces.is_empty() {
        return None;
    }
    let mut chars: Vec<char> = text.trim().chars().take(2049).collect();
    let input_cut = chars.len() > 2048;
    chars.truncate(2048);
    let all: String = chars.iter().collect();
    let roomy = (roomy_width.max(COMPACT.0), (viewport.1 * ROOMY_HEIGHT_RATIO).max(COMPACT.1));
    for (last, (box_w, box_h)) in [(false, COMPACT), (true, roomy)] {
        let width = (box_w.min(viewport.0 - 2.0 * margin) - pad).floor();
        let height = box_h.min(viewport.1 - 2.0 * margin);
        if width < 1.0 || height <= pad {
            continue;
        }
        let Some(mut pt) = point_size(&faces, &chars, requested_pt, width, height, pad) else { continue };
        let fits = |body: &str, pt: f32| {
            let layout = layout(&faces, body, pt * SCALE, width * SCALE);
            layout.width.ceil() / SCALE + pad <= width + pad
                && (layout.lines.len() as f32 * layout.line_h).ceil() / SCALE + pad <= height
        };
        if !input_cut && fits(&all, pt) {
            return Some(Preview { text: all, width, pt });
        }
        if !last {
            continue;
        }
        // 큰 상자도 넘치면 글자를 조금 줄여 본다 — 요약을 뒷부분만 잘라 내는 것보다
        // 한 단계 작은 글자로 다 보이는 쪽이 낫다. 그래도 안 들어가면 그때 자른다.
        for shrink in [0.85_f32, 0.72] {
            let smaller = pt * shrink;
            if !input_cut && smaller >= 9.0 && fits(&all, smaller) {
                return Some(Preview { text: all, width, pt: smaller });
            }
        }
        pt = (pt * 0.72).max(9.0_f32.min(pt));
        let fits = |body: &str| fits(body, pt);
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
        return Some(Preview {
            text: format!("{}…", chars[..low].iter().collect::<String>().trim_end()),
            width,
            pt,
        });
    }
    None
}

/// 상자에 맞는 글자 크기 — 요청한 크기에서 시작해, 한 줄 높이나 가장 넓은 글자가 상자를
/// 넘으면 그만큼 줄인다.
fn point_size(faces: &[FontRef], chars: &[char], requested_pt: f32, width: f32, height: f32, pad: f32) -> Option<f32> {
    let mut pt = requested_pt.min((height - pad) / LINE_SPACING);
    let metrics: Vec<_> = faces.iter().map(|face| face.glyph_metrics(&[]).scale(pt * SCALE)).collect();
    let widest = chars
        .iter()
        .copied()
        .chain(['…'])
        .map(|c| { let (face, gid) = glyph_for(faces, c); metrics[face].advance_width(gid) / SCALE })
        .fold(0.0_f32, f32::max);
    if widest > width {
        pt *= width / widest;
    }
    (pt.is_finite() && pt > 0.0).then_some(pt)
}

/// 글자 둘레에 두르는 어두운 테두리의 두께(래스터 픽셀). 말풍선 판을 걷어내고 글자만
/// 띄우므로, 이게 없으면 밝은 바탕화면 위에서 흰 글자가 통째로 사라진다.
const HALO: i32 = 3;

/// 글자도 테두리도 없는 자리의 알파. 0 이면 macOS 가 그 픽셀의 마우스를 아래 창으로
/// 넘겨 버려 말풍선을 누를 수 없다 — 눌러서 그 pane 으로 가는 길이 거기서 끊긴다.
/// 6 이던 것을 2 로 — 말풍선이 커지자(400x300) 그 네모가 바탕화면 위에 옅게 비쳤다
/// (2026-09-17 「텍스트 배경이 은은하게 보인다」). 바닥은 글자 근처(`FLOOR_REACH`)에만 깐다.
const FLOOR_A: u32 = 2;
/// 바닥을 까는 범위 — 글자 획에서 이만큼(래스터 픽셀) 떨어진 곳까지. 줄 사이 틈은 메우고
/// 상자 귀퉁이의 빈 바탕은 안 건드린다.
const FLOOR_REACH: i32 = 14;

/// 시스템 한글 폰트 — 담아 온 메이플스토리체를 못 찾았을 때만.
const FALLBACK_FONT: &str = "/System/Library/Fonts/AppleSDGothicNeo.ttc";

/// 담아 온 서체. 넥슨이 무료로 배포하는 메이플스토리체이고, 소프트웨어에 함께 담아도
/// 되는 조건이라 레포에 넣었다(assets/fonts/LICENSE-Maplestory.txt).
const BUNDLED_FONT: &str = "Maplestory Bold.ttf";

/// 첫 글꼴에 없는 글자를 빌려 오는 순서. 나쵸의 카오모지 「(=^･ω･^=)」「(｡•ᴗ•｡)」는
/// 반각 가나·중점·특수 기호라 메이플스토리체엔 없고, 없는 글자는 네모로 찍힌다
/// (2026-09-17 「카오모지 깨진다」). 히라기노가 반각 가나·중점을, Arial Unicode 가 나머지
/// 기호를 맡는다(실측: 30자 중 ᴗ 하나만 어느 글꼴에도 없다).
const BORROWED_FONTS: [&str; 3] = [
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    FALLBACK_FONT,
];

/// 글꼴 사슬의 바이트 — 첫 칸이 주 글꼴, 나머지가 빌려 오는 순서. 없는 파일은 건너뛴다.
fn faces_data() -> &'static [Vec<u8>] {
    static FONTS: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let primary = bundled_font_path()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_else(|| std::fs::read(FALLBACK_FONT).unwrap_or_default());
        let mut out = vec![primary];
        for path in BORROWED_FONTS {
            if let Ok(bytes) = std::fs::read(path) {
                if !out.iter().any(|have| have.len() == bytes.len() && *have == bytes) {
                    out.push(bytes);
                }
            }
        }
        out
    })
}

/// 사슬의 글꼴들. 파싱이 안 되는 파일은 빠진다.
fn faces() -> Vec<FontRef<'static>> {
    faces_data().iter().filter_map(|data| FontRef::from_index(data, 0)).collect()
}

/// 어느 시스템 글꼴에도 없는 글자의 닮은꼴. 「(｡•ᴗ•｡)」의 ᴗ(작은 대문자 U)가 그렇다 —
/// 아래가 둥근 ◡ 로 찍으면 같은 얼굴이다.
fn lookalike(ch: char) -> Option<char> {
    match ch {
        'ᴗ' | 'ᵕ' => Some('◡'),
        _ => None,
    }
}

/// 이 글자를 가진 첫 글꼴과 그 글리프. 어느 글꼴에도 없으면 닮은꼴을, 그것도 없으면 주
/// 글꼴의 빈 글리프(0).
fn glyph_for(faces: &[FontRef], ch: char) -> (usize, u16) {
    for (i, face) in faces.iter().enumerate() {
        let gid = face.charmap().map(ch as u32);
        if gid != 0 {
            return (i, gid);
        }
    }
    match lookalike(ch) {
        Some(other) => glyph_for(faces, other),
        None => (0, 0),
    }
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
    face: usize,
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
fn layout(faces: &[FontRef], text: &str, px: f32, max_w: f32) -> Layout {
    let m = faces[0].metrics(&[]).scale(px);
    let metrics: Vec<_> = faces.iter().map(|face| face.glyph_metrics(&[]).scale(px)).collect();
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
        let (face, gid) = glyph_for(faces, ch);
        let adv = metrics[face].advance_width(gid);
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
        cur.push(Glyph { face, gid, adv });
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
    let faces = faces();
    if faces.is_empty() {
        return None;
    }
    let px = pt * SCALE;
    let lay = layout(&faces, text, px, max_w * SCALE);
    // 글자가 위아래로 삐져나오지 않게 한 줄 높이 안에 ascent+descent 를 가운데 둔다.
    // 테두리가 잘리지 않게 사방을 그만큼 넓혀 둔다.
    let pad = HALO as u32 + 1;
    let w = lay.width.ceil().max(1.0) as u32 + pad * 2;
    let h = (lay.lines.len() as f32 * lay.line_h).ceil().max(1.0) as u32 + pad * 2;
    let mut buf = vec![0u8; (w * h * 4) as usize];

    let mut ctx = ScaleContext::new();
    let mut render = Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
        Source::Bitmap(StrikeWith::BestFit),
    ]);
    render.format(Format::Alpha);
    // 글꼴마다 한 바퀴 — 스케일러는 한 글꼴에 하나만 살 수 있어, 줄을 따라가며 글꼴을
    // 바꿔 끼우는 대신 같은 자리를 글꼴 수만큼 지나간다(사슬은 넷을 안 넘는다).
    for (face_index, face) in faces.iter().enumerate() {
        if !lay.lines.iter().flatten().any(|g| g.face == face_index) {
            continue;
        }
    let mut scaler = ctx.builder(*face).size(px).hint(true).build();
    let mut baseline = lay.ascent + (lay.line_h - (lay.ascent + lay.descent)) / 2.0;
    for line in &lay.lines {
        let mut pen = 0.0_f32;
        for g in line {
            if g.face != face_index {
                pen += g.adv;
                continue;
            }
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
    }

    // 글자 밑에 어두운 테두리를 깔아 어떤 바탕화면 위에서도 읽힌다. 글자 알파를 조금
    // 부풀린 것이 테두리이고, 결과는 미리곱 알파라 색은 글자 몫만 남긴다.
    let mut out = vec![0u8; buf.len()];
    let near = near_ink(&buf, w, h, FLOOR_REACH);
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
            let floor = if near[(y as u32 * w + x as u32) as usize] { FLOOR_A } else { 0 };
            out[o + 3] = (a + halo * (255 - a) / 255).clamp(floor, 255) as u8;
        }
    }
    Some((out, w, h))
}

/// 글자 획에서 `reach` 픽셀 안에 드는 자리 — 가로·세로 두 번의 최대값 훑기로 상자(마름모가
/// 아니라 네모 반경)를 넓힌다. 픽셀마다 반경을 도는 것보다 반경 배만큼 싸다.
fn near_ink(buf: &[u8], w: u32, h: u32, reach: i32) -> Vec<bool> {
    let (w, h) = (w as usize, h as usize);
    let ink: Vec<bool> = (0..w * h).map(|i| buf[i * 4 + 3] > 0).collect();
    let mut rows = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let lo = x.saturating_sub(reach as usize);
            let hi = (x + reach as usize).min(w - 1);
            rows[y * w + x] = ink[y * w + lo..=y * w + hi].iter().any(|v| *v);
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        for y in 0..h {
            let lo = y.saturating_sub(reach as usize);
            let hi = (y + reach as usize).min(h - 1);
            out[y * w + x] = (lo..=hi).any(|yy| rows[yy * w + x]);
        }
    }
    out
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
    roomy_width: f32,
) -> Option<(wgpu::TextureView, f32, f32)> {
    let plan = preview(text, viewport, pt, roomy_width)?;
    render_text(dev, q, &plan.text, plan.width, plan.pt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &str, max_w: f32) -> (usize, f32) {
        let lay = layout(&faces(), text, FONT_PT * SCALE, max_w * SCALE);
        (lay.lines.len(), lay.width / SCALE)
    }

    /// 나쵸의 카오모지는 주 글꼴에 없어도 사슬에서 빌려 온다 — 네모로 찍히면 말투가
    /// 깨진 것처럼 보인다. 어느 글꼴에도 없는 ᴗ 는 닮은꼴 ◡ 로 찍는다.
    #[test]
    fn kaomoji_glyphs_are_borrowed_from_the_font_chain() {
        let faces = faces();
        assert!(faces.len() >= 2, "빌려 올 글꼴이 없다: {}", faces.len());
        let mut missing = Vec::new();
        for ch in "(=^･ω･^=)(=｀ω´=)(=ↀωↀ=)(｡•ᴗ•｡)(・ω・)(｡>﹏<｡)(´･_･`)(ﾉ´ヮ`)ﾉ*:･ﾟ♪…·■".chars() {
            if glyph_for(&faces, ch).1 == 0 {
                missing.push(ch);
            }
        }
        assert!(missing.is_empty(), "빌려 오지 못한 글자: {missing:?}");
        // 빌려 온 글자도 실제로 찍힌다.
        let (buf, w, h) = raster("･ω･", 260.0, FONT_PT).unwrap();
        assert!(w > 0 && h > 0);
        assert!(buf.chunks(4).filter(|p| p[3] > 0 && p[0] > 128).count() > 30);
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
        // 바닥 알파는 글자 근처에만 — 상자 전체에 깔면 큰 말풍선이 네모로 비친다.
        let (buf2, w2, h2) = raster("가\n\n\n\n나", 260.0, FONT_PT).unwrap();
        let mid = ((h2 / 2) * w2 + w2 / 2) as usize * 4;
        assert_eq!(buf2[mid + 3], 0, "빈 줄 한가운데는 완전히 투명해야 한다");
        assert!(buf2.chunks(4).filter(|p| p[3] == FLOOR_A as u8).count() > 0, "글자 곁에는 바닥이 있다");
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

    /// 작은 상자를 넘치는 답은 잘리지 않고 큰 상자로 통째로 들어간다 — 나쵸의 모든 기기
    /// 요약이 말풍선에서 두 줄 만에 「…」로 끝나면 채팅창에서 꺼낸 뜻이 없다.
    #[test]
    fn an_answer_that_overflows_the_compact_box_grows_instead_of_truncating() {
        let short = "코하루 · 나쵸";
        let plan = preview(short, (634.0, 1072.0), 13.0, 400.0).unwrap();
        assert_eq!(plan.text, short);
        assert!(plan.width <= COMPACT.0);
        let summary = (1..=8).map(|i| format!("{i}번 학생은 파일을 고치는 중이고 사람 손은 아직 필요 없어\n")).collect::<String>();
        let plan = preview(summary.trim(), (634.0, 1072.0), 13.0, 400.0).unwrap();
        assert!(!plan.text.ends_with('…'), "잘림: {}", plan.text);
        assert!(plan.width > COMPACT.0);
        let (_, w, h) = raster(&plan.text, plan.width, plan.pt).unwrap();
        assert!(h as f32 / SCALE > COMPACT.1, "높이 {h}");
        assert!(w as f32 / SCALE <= 400.0);
        // 너비 설정을 줄이면 상자도 준다.
        let narrow = preview(summary.trim(), (634.0, 1072.0), 13.0, 280.0).unwrap();
        assert!(narrow.width <= 280.0 && narrow.width < plan.width);
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
                let Some(plan) = preview(&raw, viewport, pt, 400.0) else {
                    continue;
                };
                assert!(plan.text.ends_with('…') && plan.text.len() < raw.len());
                let (_, w, h) = raster(&plan.text, plan.width, plan.pt).unwrap();
                assert!(w as f32 / SCALE <= viewport.0);
                let roomy = (viewport.1 * ROOMY_HEIGHT_RATIO).max(COMPACT.1);
                assert!(h as f32 / SCALE <= roomy.min(viewport.1));
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
            assert!(preview("안내", viewport, 40.0, 400.0).is_none());
            assert!(geometry(viewport, (260.0, 110.0), (0.0, 0.0)).is_none());
        }
        assert!(geometry((168.0, 284.0), (f32::NAN, 20.0), (0.0, 0.0)).is_none());
        assert!(preview("안내", (168.0, 284.0), f32::NAN, 400.0).is_none());
    }
}
