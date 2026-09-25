//! swash-driven glyph rasterizer. Loads one font from raw bytes,
//! returns alpha bitmaps the atlas can paste into its R8 texture.
//!
//! Phase 1 keeps the surface intentionally small — one font, no
//! fallback chain, no shaping for clusters (CJK / emoji / Nerd icons
//! arrive in later phases). The atlas is the side that caches; this
//! module is stateless per glyph so the atlas can decide which keys
//! to keep around.

use anyhow::{Context, Result};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

/// Backing storage for one font face. `Mapped` is the common case — the
/// font file is mmap'd, so only the table/glyph pages swash actually touches
/// become resident (a 180MB color-emoji font costs ~0 RSS until an emoji is
/// drawn). `Owned` covers the primary font handed in as bytes and the small
/// `include_bytes!` Nerd-icon face. Replacing `std::fs::read` (whole file →
/// heap) with mmap is what keeps the fallback chain off the resident set.
enum FontData {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl FontData {
    fn as_slice(&self) -> &[u8] {
        match self {
            FontData::Mapped(m) => &m[..],
            FontData::Owned(v) => v,
        }
    }
    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
    /// Empty placeholder for an unfilled bold/italic slot.
    fn empty() -> Self {
        FontData::Owned(Vec::new())
    }
}

/// mmap a font file. Returns None if the path is missing (optional fallbacks)
/// or the bytes aren't a valid TTF/OTF/TTC entry.
fn map_font(path: &str, index: u32) -> Option<FontData> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: fonts are stable system files; we never mutate the mapping.
    // Same assumption every terminal (incl. Ghostty) makes for font I/O.
    let mmap = unsafe { memmap2::Mmap::map(&file).ok()? };
    FontRef::from_index(&mmap[..], index as usize)?;
    Some(FontData::Mapped(mmap))
}

pub struct Shaper {
    /// Owned font bytes for each face in the fallback chain. The
    /// primary face sits at index 0; subsequent faces are tried in
    /// order whenever the previous one's `charmap.map(ch)` returns
    /// glyph 0 (a.k.a. "this font doesn't cover this codepoint").
    /// Matches the cosmic-text fallback chain we configured under
    /// sugarloaf: D2Coding → JetBrainsMono → Apple SD → Apple
    /// Color Emoji on macOS.
    faces: Vec<(FontData, u32)>,
    /// Optional bold-weight variant for each slot in `faces`. Parallel
    /// indexing — `bold_faces[i]` mirrors `faces[i]` when present (an empty
    /// face means "no bold installed for this slot"). Filled via
    /// `set_bold_face_path`; consumed by `rasterize` when bold=true.
    bold_faces: Vec<(FontData, u32)>,
    /// Optional italic variant for each slot. When set, `rasterize` uses
    /// it instead of synthesising italic via a swash skew transform.
    /// Fonts that ship a designed italic (JetBrains Mono) read much
    /// cleaner than skew-synth which can clip ascenders at the cell edge.
    italic_faces: Vec<(FontData, u32)>,
    scale_ctx: ScaleContext,
    /// `cell_advance` 결과 캐시(size_px 비트 → advance). 이게 없으면 **공백을
    /// 그릴 때마다 'M' 을 통째로 래스터라이즈**한다 — 비트맵 Vec 할당까지 도는
    /// 일이라 글자 하나가 마이크로초 단위로 비싸졌고, 텍스트가 많은 크롬
    /// (Info 패널 40행 = 9.0ms)이 먼저 무너졌다. 폭은 크기당 상수라 한 번만
    /// 재면 된다.
    cell_adv: std::collections::HashMap<u32, f32>,
    variation_weight: Option<f32>,
    /// (폴백 슬롯, 크기 비트) → 한글 굵게에 더 얹을 윤곽 팽창(px). 처음 한 번 잰다.
    cjk_bold_embolden: std::collections::HashMap<(usize, u32), f32>,
}

/// One baked glyph's raster + metric. Coordinates follow the swash /
/// freetype convention: `bearing_x` is the offset from the pen
/// position to the left edge of the bitmap; `bearing_y` is the offset
/// to the *top* of the bitmap (positive = above baseline). The atlas
/// uses these to position the glyph inside a cell quad.
pub struct Rasterized {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: f32,
    /// `true` when `data` is a 4-byte/texel RGBA color bitmap (Apple
    /// Color Emoji sbix, CBDT, COLR/CPAL). `false` = 1-byte/texel
    /// coverage mask. The atlas uses this to choose how to upload, and
    /// the shader to choose mask-multiply vs. verbatim color.
    pub is_color: bool,
}

/// East Asian Wide / Fullwidth — full-width by design, so a fallback
/// face serving these must NOT get the symbol/icon size boost. Boosting
/// makes the raster wider than its (un-boosted) advance, so the syllable
/// overruns its cell and bleeds into the neighbour. cosmic-text (the
/// sugarloaf path) never boosts, which is why it rendered Hangul right.
fn is_cjk_wide(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp,
        0x1100..=0x115F        // Hangul Jamo
        | 0x2E80..=0x303E      // CJK Radicals, Kangxi, CJK Symbols
        | 0x3041..=0x33FF      // Kana, CJK enclosed/compat
        | 0x3400..=0x4DBF      // CJK Ext A
        | 0x4E00..=0x9FFF      // CJK Unified
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul Syllables
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F      // CJK Compatibility Forms
        | 0xFF00..=0xFF60      // Fullwidth Forms
        | 0xFFE0..=0xFFE6      // Fullwidth signs
    ) || cp >= 0x20000          // CJK Ext B and beyond
}

/// CJK 폴백 배율을 "설계 크기 일치"(0.0) ↔ "두 칸 꽉 채움"(1.0) 사이 어디에
/// 둘지. `KASATERM_CJK_FIT_BIAS` 로 덮을 수 있다.
///
/// 0.0 은 한글 크기가 라틴과 자연스럽게 맞는 대신 두 칸이 글자보다 넓어
/// 자간이 벌어져 보이고, 1.0 은 자간이 붙는 대신 한글이 라틴 대문자보다
/// 14% 커진다. 어느 쪽도 공짜가 아니라 기본값은 눈으로 골랐다.
fn cjk_fit_bias() -> f32 {
    use std::sync::OnceLock;
    static BIAS: OnceLock<f32> = OnceLock::new();
    *BIAS.get_or_init(|| {
        std::env::var("KASATERM_CJK_FIT_BIAS")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_CJK_FIT_BIAS)
            .clamp(0.0, 1.0)
    })
}

const DEFAULT_CJK_FIT_BIAS: f32 = 0.0;

/// Synthesised bold via horizontal alpha dilation. Walks each row twice:
/// once left→right, once right→left, taking the max against the original
/// neighbour at each step. The result thickens vertical stems by ~2px
/// while leaving horizontal strokes intact. Done from an immutable copy
/// of the row so neither pass cascades — left+right + original give a
/// symmetric weight gain without smear or "drift" toward one side.
fn widen_alpha_horizontal(data: &mut [u8], w: usize, h: usize) {
    if w == 0 || h == 0 || data.len() < w * h {
        return;
    }
    let mut orig = vec![0u8; w];
    for y in 0..h {
        let row_start = y * w;
        orig.copy_from_slice(&data[row_start..row_start + w]);
        for x in 0..w {
            let mut v = orig[x];
            if x > 0 {
                v = v.max(orig[x - 1]);
            }
            if x + 1 < w {
                v = v.max(orig[x + 1]);
            }
            data[row_start + x] = v;
        }
    }
}

/// 한글 굵게가 보통보다 이만큼은 진해야 라틴 굵게와 나란히 읽힌다. JetBrains Mono
/// Bold 가 Regular 대비 잉크 ×1.35 쯤이라 거기에 맞췄다.
const CJK_BOLD_INK_TARGET: f32 = 1.35;
const CJK_BOLD_PROBE: [char; 3] = ['한', '글', '국'];
/// 팽창 상한(em 비). 이보다 두꺼우면 한글 획 사이 빈틈이 메워진다.
const CJK_BOLD_EMBOLDEN_MAX: f32 = 0.04;

/// 윤곽 하나를 알파로 래스터해 잉크 총량(커버리지 합)을 잰다.
fn outline_ink(ctx: &mut ScaleContext, data: &[u8], index: u32, ch: char, size: f32, embolden: f32) -> f32 {
    let Some(font) = FontRef::from_index(data, index as usize) else { return 0.0 };
    let gid = font.charmap().map(ch as u32);
    if gid == 0 {
        return 0.0;
    }
    let mut scaler = ctx.builder(font).size(size).hint(true).build();
    let mut render = Render::new(&[Source::Outline]);
    render.format(Format::Alpha).embolden(embolden);
    render
        .render(&mut scaler, gid)
        .map(|img| img.data.iter().map(|&a| a as f32 / 255.0).sum())
        .unwrap_or(0.0)
}

impl Shaper {
    pub fn from_bytes(font_data: Vec<u8>, font_index: u32) -> Result<Self> {
        FontRef::from_index(&font_data, font_index as usize)
            .context("font bytes not a TTF/OTF/TTC entry")?;
        Ok(Self {
            faces: vec![(FontData::Owned(font_data), font_index)],
            bold_faces: Vec::new(),
            italic_faces: Vec::new(),
            scale_ctx: ScaleContext::new(),
            cell_adv: std::collections::HashMap::new(),
            variation_weight: None,
            cjk_bold_embolden: std::collections::HashMap::new(),
        })
    }

    pub fn set_variation_weight(&mut self, weight: f32) {
        self.variation_weight = Some(weight);
        self.cell_adv.clear();
    }

    /// Register an OS-installed bold face that mirrors the regular face at
    /// the same slot. When `rasterize` sees `bold=true` it'll route to
    /// `bold_faces[i]` if non-empty, else fall back to synthesised
    /// emboldening (rendering twice with a 1-px x-offset at draw time —
    /// handled in the renderer caller, not here). Italic synthesises via
    /// swash's shear transform inside `rasterize`.
    pub fn set_bold_face_path(&mut self, idx: usize, path: &str, index: u32) {
        let Some(data) = map_font(path, index) else { return };
        while self.bold_faces.len() <= idx {
            self.bold_faces.push((FontData::empty(), 0));
        }
        self.bold_faces[idx] = (data, index);
    }

    /// Register a real italic face for slot `idx`. Mirrors `set_bold_face_path`.
    /// When present, italic cells render from this face instead of via swash
    /// skew — JetBrains Mono / Cascadia Italic look much better than synthesis.
    pub fn set_italic_face_path(&mut self, idx: usize, path: &str, index: u32) {
        let Some(data) = map_font(path, index) else { return };
        while self.italic_faces.len() <= idx {
            self.italic_faces.push((FontData::empty(), 0));
        }
        self.italic_faces[idx] = (data, index);
    }

    pub fn from_path(path: &str, index: u32) -> Result<Self> {
        let data = map_font(path, index).with_context(|| format!("read font {path}"))?;
        Ok(Self {
            faces: vec![(data, index)],
            bold_faces: Vec::new(),
            italic_faces: Vec::new(),
            scale_ctx: ScaleContext::new(),
            cell_adv: std::collections::HashMap::new(),
            variation_weight: None,
            cjk_bold_embolden: std::collections::HashMap::new(),
        })
    }

    /// Append a fallback face. Tried after the primary + every face
    /// already added, in insertion order. Silently ignores paths
    /// that don't exist (so a caller can list optional fallbacks
    /// like Apple Color Emoji without erroring on Linux/Windows).
    pub fn add_fallback_path(&mut self, path: &str, index: u32) {
        if let Some(data) = map_font(path, index) {
            self.faces.push((data, index));
            // 체인이 바뀌면 'M' 이 다른 페이스에서 잡힐 수 있다.
            self.cell_adv.clear();
        }
    }

    /// 폴백 페이스를 추가하면서 같은 슬롯의 designed bold 도 함께 건다.
    ///
    /// 슬롯 번호를 호출부가 세지 않아도 되는 게 핵심이다 — `add_fallback_path` 는
    /// 로드에 실패하면 조용히 건너뛰므로, 바깥에서 순번을 세면 볼드가 엉뚱한
    /// 페이스에 붙을 수 있다. 폴백에 볼드가 없으면 그 페이스가 담당하는 문자는
    /// 볼드로 요청해도 regular 로 그려진다(CJK 는 합성 팽창 대상이 아니라서
    /// 특히 그렇다 — 한글 볼드가 통째로 밋밋해진다).
    pub fn add_fallback_with_bold(
        &mut self,
        path: &str,
        index: u32,
        bold: Option<(String, u32)>,
    ) {
        let Some(data) = map_font(path, index) else { return };
        self.faces.push((data, index));
        self.cell_adv.clear();
        let slot = self.faces.len() - 1;
        if let Some((bold_path, bold_idx)) = bold {
            self.set_bold_face_path(slot, &bold_path, bold_idx);
        }
    }

    /// Append a fallback face from in-memory bytes. Used for fonts
    /// we ship inside the binary via `include_bytes!` — guarantees
    /// the chain has Misc-Technical / Nerd icon coverage regardless
    /// of what's installed on the user's system.
    pub fn add_fallback_bytes(&mut self, bytes: &'static [u8], index: u32) {
        if FontRef::from_index(bytes, index as usize).is_some() {
            self.faces.push((FontData::Owned(bytes.to_vec()), index));
            self.cell_adv.clear();
        }
    }

    fn face(&self, idx: usize) -> FontRef<'_> {
        let (bytes, fi) = &self.faces[idx];
        FontRef::from_index(bytes.as_slice(), *fi as usize).unwrap()
    }

    /// Walk the fallback chain and return the first face that covers
    /// `ch` together with its glyph id. Returns None when no face
    /// has a glyph for the codepoint (caller skips the cell).
    #[allow(dead_code)]
    fn resolve(&self, ch: char) -> Option<(usize, u16)> {
        for i in 0..self.faces.len() {
            let f = self.face(i);
            let gid = f.charmap().map(ch as u32);
            if gid != 0 {
                if std::env::var_os("KASATERM_FONT_DEBUG").is_some() {
                    eprintln!(
                        "[font] U+{:04X} → face[{}] gid={}",
                        ch as u32, i, gid
                    );
                }
                return Some((i, gid));
            }
        }
        if std::env::var_os("KASATERM_FONT_DEBUG").is_some() {
            eprintln!("[font] U+{:04X} → no face covers", ch as u32);
        }
        None
    }

    pub fn cell_advance(&mut self, size_px: f32) -> f32 {
        let key = size_px.to_bits();
        if let Some(v) = self.cell_adv.get(&key) {
            return *v;
        }
        let v = self
            .rasterize('M', size_px)
            .map(|r| r.advance)
            .unwrap_or(size_px * 0.6);
        self.cell_adv.insert(key, v);
        v
    }

    /// Advance width of `ch` at `size_px` straight from glyph metrics — works
    /// for blank glyphs (space) that `rasterize` returns None for. Walks the
    /// fallback chain to the first face that maps the codepoint.
    pub fn advance(&self, ch: char, size_px: f32) -> f32 {
        for i in 0..self.faces.len() {
            let f = self.face(i);
            let gid = f.charmap().map(ch as u32);
            if gid != 0 {
                return f.glyph_metrics(&[]).scale(size_px).advance_width(gid);
            }
        }
        size_px * 0.5
    }

    /// 폴백 페이스를 주 폰트의 설계 크기에 맞추는 배율 — 두 폰트의 cap height
    /// 비다. 폰트를 섞을 때 크기를 맞추는 표준 방법이고, advance(칸 폭)로
    /// 맞추면 왜 안 되는지는 `rasterize_inner` 의 호출부 주석에 있다.
    /// cap height 가 없는 폰트는 x-height, 그것도 없으면 1.0 으로 물러선다.
    fn cap_match(&self, font_data: &[u8], font_index: usize) -> f32 {
        let Some(f) = FontRef::from_index(font_data, font_index) else {
            return 1.0;
        };
        // scale(1.0) = em 대비 비율. upm 이 다른 폰트끼리도 그대로 비교된다.
        let pick = |m: &swash::Metrics| {
            if m.cap_height > 0.0 {
                m.cap_height
            } else {
                m.x_height
            }
        };
        let base = pick(&self.face(0).metrics(&[]).scale(1.0));
        let this = pick(&f.metrics(&[]).scale(1.0));
        if base <= 0.0 || this <= 0.0 {
            return 1.0;
        }
        if std::env::var_os("KASATERM_FONT_DEBUG").is_some() {
            eprintln!("[capmatch] base={base:.4} this={this:.4} → {:.4}", base / this);
        }
        // 상·하한은 메트릭이 망가진 폰트가 폴백에 끼어들었을 때의 안전장치일
        // 뿐이다 — 정상적인 짝이면 1.0 근처에서 논다.
        (base / this).clamp(0.8, 1.4)
    }

    /// Line height in pixels at `size_px` — primary face's
    /// ascent+descent+line_gap. Caller uses this directly for cell
    /// height so the grid metric matches the font's natural line
    /// instead of an arbitrary multiplier. Falls back to size_px*1.2
    /// if metrics aren't available.
    pub fn line_height(&self, size_px: f32) -> f32 {
        let font = self.face(0);
        let m = font.metrics(&[]).scale(size_px);
        let lh = m.ascent + m.descent + m.leading;
        if lh > 0.0 {
            lh
        } else {
            size_px * 1.2
        }
    }

    pub fn rasterize(&mut self, ch: char, size_px: f32) -> Option<Rasterized> {
        self.rasterize_styled(ch, size_px, false, false)
    }

    /// 폴백 슬롯의 한글 굵게가 모자란 만큼 얹을 윤곽 팽창(px). 한글은 라틴처럼
    /// 알파 팽창을 걸 수 없고(획이 촘촘해 빈틈이 메워진다), 폴백의 designed bold 는
    /// 약한 경우가 있다 — D2Coding Bold 는 Regular 대비 잉크 ×1.06~1.13 이라 같은
    /// 줄의 라틴 굵게(×1.35) 옆에서 한글만 안 굵어 보였다. 그래서 슬롯·크기마다 한 번 재서
    /// 목표 비율에 닿는 최소 팽창을 찾는다. 이미 충분히 굵은 얼굴은 0 이다.
    ///
    /// 크기마다 따로 잰다. 힌팅이 획을 픽셀에 붙이기 때문에 같은 D2Coding Bold 가
    /// 64px 에선 ×1.33 인데 24px 에선 ×1.1 이다 — 큰 크기 하나로 재면 정작 화면
    /// 크기에서 모자란다.
    ///
    /// 필드를 따로 받는 것은 부르는 자리가 이미 얼굴 바이트를 빌려 쥐고 있어서다.
    fn cjk_bold_embolden(
        cache: &mut std::collections::HashMap<(usize, u32), f32>,
        ctx: &mut ScaleContext,
        faces: &[(FontData, u32)],
        bold_faces: &[(FontData, u32)],
        slot: usize,
        px: f32,
    ) -> f32 {
        let key = (slot, px.to_bits());
        if let Some(&strength) = cache.get(&key) {
            return strength;
        }
        let (reg, reg_idx) = &faces[slot];
        let (bold, bold_idx) = match bold_faces.get(slot) {
            Some((b, i)) if !b.is_empty() => (b, *i),
            _ => (reg, *reg_idx),
        };
        let mut ink = |data: &[u8], idx: u32, strength: f32| -> f32 {
            CJK_BOLD_PROBE.iter().map(|&ch| outline_ink(ctx, data, idx, ch, px, strength)).sum()
        };
        let regular = ink(reg.as_slice(), *reg_idx, 0.0);
        let target = regular * CJK_BOLD_INK_TARGET;
        let strength = if regular <= 0.0 || ink(bold.as_slice(), bold_idx, 0.0) >= target {
            0.0
        } else {
            // 잉크는 팽창에 대해 단조 증가라 이분 탐색으로 충분하다.
            let (mut lo, mut hi) = (0.0f32, CJK_BOLD_EMBOLDEN_MAX * px);
            for _ in 0..8 {
                let mid = (lo + hi) / 2.0;
                if ink(bold.as_slice(), bold_idx, mid) >= target {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            hi
        };
        cache.insert(key, strength);
        strength
    }

    /// Style-aware variant. `bold=true` routes to the installed bold face
    /// when one was registered via `set_bold_face_path`; otherwise falls
    /// back to the regular face (no fake-bold here — the renderer can
    /// double-draw with an x-offset for a cheap synthesised bold).
    /// `italic=true` applies a 14° shear via swash's transform — D2Coding
    /// has no italic variant on disk, so synthesise unconditionally.
    pub fn rasterize_styled(
        &mut self,
        ch: char,
        size_px: f32,
        bold: bool,
        italic: bool,
    ) -> Option<Rasterized> {
        self.rasterize_inner(ch, size_px, bold, italic)
    }

    fn rasterize_inner(
        &mut self,
        ch: char,
        size_px: f32,
        bold: bool,
        italic: bool,
    ) -> Option<Rasterized> {
        // 라틴 한 칸의 폭. CJK 를 폴백 페이스가 그릴 때 "두 칸을 꽉 채우도록"
        // 키우는 기준이 된다 — primary 가 라틴 전용(JetBrains, 한글 글리프 없음)
        // 이면 한글은 폴백(D2Coding 논-Mono, 1.0em)에서 잡히는데, 칸은 라틴
        // 0.6em × 2 = 1.2em 이라 그대로 두면 양쪽에 0.1em 씩 빈 공간이 남아
        // 글자마다 벌어져 보인다. 이 값이 그 간극을 메우는 배율의 분자다.
        let latin_cell = if is_cjk_wide(ch) {
            self.advance('M', size_px)
        } else {
            0.0
        };
        // Pick the most specific face we have for the (bold, italic)
        // combo: bold_italic > italic > bold > regular. JetBrains Mono
        // ships all four with matching metrics so they layer cleanly.
        // `face_source`: 0 = regular, 1 = italic, 2 = bold. Italic is
        // the only one that influences geometry, so its index gets the
        // separate slot tracked in `from_italic_face`. Bold from real
        // bold face → no synthesised dilation needed.
        #[derive(Clone, Copy)]
        enum FaceKind {
            Regular,
            Italic,
            Bold,
        }
        let pick_face = |i: usize| -> (FaceKind, &[u8], u32) {
            // bold + italic both flags → prefer italic_face if it carries
            // its own bold weight (JetBrains BoldItalic landed there via
            // set_italic_face_path of the BoldItalic file). For the simple
            // setup we have, italic file is regular-italic — so for bold+
            // italic we use bold face and skip italic (or skew on top).
            if bold {
                if let Some((b, fi)) = self.bold_faces.get(i)
                    .filter(|(b, _)| !b.is_empty())
                    .map(|(b, fi)| (b.as_slice(), *fi))
                {
                    return (FaceKind::Bold, b, fi);
                }
            }
            if italic {
                if let Some((b, fi)) = self.italic_faces.get(i)
                    .filter(|(b, _)| !b.is_empty())
                    .map(|(b, fi)| (b.as_slice(), *fi))
                {
                    return (FaceKind::Italic, b, fi);
                }
            }
            let (b, fi) = &self.faces[i];
            (FaceKind::Regular, b.as_slice(), *fi)
        };
        let candidates: Vec<(usize, u16, f32, FaceKind)> = {
            let mut v = Vec::new();
            for i in 0..self.faces.len() {
                let (kind, bytes, fi) = pick_face(i);
                if let Some(f) = FontRef::from_index(bytes, fi as usize) {
                    let gid = f.charmap().map(ch as u32);
                    if gid != 0 {
                        let a = f.glyph_metrics(&[]).scale(size_px).advance_width(gid);
                        v.push((i, gid, a, kind));
                        continue;
                    }
                }
                // Selected style face didn't cover this char; fall back to
                // regular for the same slot before moving on to the next.
                let f = self.face(i);
                let gid = f.charmap().map(ch as u32);
                if gid == 0 {
                    continue;
                }
                let advance = f.glyph_metrics(&[]).scale(size_px).advance_width(gid);
                v.push((i, gid, advance, FaceKind::Regular));
            }
            v
        };
        if candidates.is_empty() {
            if std::env::var_os("KASATERM_FONT_DEBUG").is_some() {
                eprintln!("[font] U+{:04X} → no face covers", ch as u32);
            }
            return None;
        }
        let variation_weight = self.variation_weight;
        // Per-face size boost. Fallback faces (anything past index 0)
        // routinely design glyphs at a smaller fraction of the em
        // than monospace primaries — STIX Math's chevron lives at
        // ~50% of em while D2Coding's letters fill ~80%, so the
        // chevron reads as "tiny" at the same size_px. Scale
        // fallback raster sizes up a bit so visible glyph areas
        // come out comparable.
        for (face_idx, glyph_id, advance, kind) in candidates {
            // Fallback faces get a 1.25× boost so small symbol/icon glyphs
            // read at a comparable size. The primary face and CJK/Hangul stay
            // at size_px. Caveat: a non-CJK glyph that's already cell-sized in
            // a fallback (e.g. ① Enclosed Alphanumerics) would overrun its
            // cell when boosted, so we re-render it un-boosted if the boosted
            // raster is wider than the advance — no bleed into the next cell.
            let boost = face_idx != 0 && !is_cjk_wide(ch);
            // Borrow the face bytes (mmap-backed) instead of cloning — the
            // closure below holds this immutable borrow of `self.faces` while
            // `render_at` takes `&mut self.scale_ctx`; disjoint fields, so the
            // borrow checker is fine and we never copy a font into the heap.
            let (font_data, font_index): (&[u8], usize) = match kind {
                FaceKind::Bold => {
                    let (b, fi) = &self.bold_faces[face_idx];
                    (b.as_slice(), *fi as usize)
                }
                FaceKind::Italic => {
                    let (b, fi) = &self.italic_faces[face_idx];
                    (b.as_slice(), *fi as usize)
                }
                FaceKind::Regular => {
                    let (b, fi) = &self.faces[face_idx];
                    (b.as_slice(), *fi as usize)
                }
            };
            // Italic synthesis: if we wanted italic but landed on a
            // non-italic face (Regular or Bold), apply a 10° skew. With
            // a real Italic file we skip this. For bold+italic with
            // only Bold registered, the skew composes over Bold so the
            // glyph slants without losing weight.
            let want_skew = italic
                && !matches!(kind, FaceKind::Italic)
                && !is_cjk_wide(ch);
            let italic_skew = if want_skew {
                Some(swash::zeno::Transform::skew(
                    swash::zeno::Angle::from_degrees(10.0),
                    swash::zeno::Angle::from_degrees(0.0),
                ))
            } else {
                None
            };
            // Dilate only when this slot has no designed bold. A real bold
            // outline is already shaped at the right weight, so smearing it
            // wider gives the "fat blocky" look — the symptom the user flagged.
            //
            // 2026-07-26 에 이걸 `bold` 전체로 넓혔던 이유는 D2Coding 의 designed
            // bold 가 약해서(한글 1.12x) 팽창한 regular 보다도 얇았기 때문이다.
            // 라틴을 JetBrains 로 옮기면서 그 전제가 사라진다 — JetBrains Bold 는
            // 자체 굵기가 충분해 덧칠이 필요 없다. 그래서 게이팅을 되돌린다.
            let want_dilate = bold && !matches!(kind, FaceKind::Bold);
            // 폴백이 그리는 CJK 는 주 폰트의 **설계 크기**에 맞춘다(cap height 비).
            // 예전엔 "두 칸 폭 ÷ 글리프 advance" 로 맞췄는데, 그리드는 떨어져도
            // 글자가 통째로 부푼다 — JetBrains 칸 0.6em×2 ÷ D2Coding 한글 1.0em
            // = 1.2배가 걸려 한글 ink 가 0.895em → 1.07em, 라틴 대문자(0.73em)의
            // 1.5배로 보였다(사용자 지적, 실측 35px vs 23px). cap height 로 맞추면
            // 1.05배라 크기가 자연스럽다.
            // primary 가 직접 한글을 그리는 구성에서는 배율을 걸지 않는다.
            let cjk_fit = if is_cjk_wide(ch) && face_idx != 0 {
                let cap = self.cap_match(font_data, font_index);
                // 두 칸 폭에 딱 맞추는 배율. 여기선 목표가 아니라 **상한**이다 —
                // 칸을 좁힌 구성(`KASATERM_CELL_TIGHTEN`)에선 cap 배율이 두 칸을
                // 넘길 수 있는데, 그러면 한글이 옆 칸을 침범한다.
                let fill = if latin_cell > 0.0 && advance > 0.0 {
                    (latin_cell * 2.0) / advance
                } else {
                    cap
                };
                (cap + (fill - cap) * cjk_fit_bias()).min(fill.max(0.1))
            } else {
                1.0
            };
            let cjk_embolden = if bold && face_idx != 0 && is_cjk_wide(ch) {
                Self::cjk_bold_embolden(
                    &mut self.cjk_bold_embolden,
                    &mut self.scale_ctx,
                    &self.faces,
                    &self.bold_faces,
                    face_idx,
                    size_px * cjk_fit,
                )
            } else {
                0.0
            };
            let render_at = |scale_ctx: &mut ScaleContext, face_size: f32, embolden: f32| {
                let font = FontRef::from_index(font_data, font_index).unwrap();
                let builder = scale_ctx.builder(font).size(face_size).hint(true);
                let mut scaler = if let Some(weight) = variation_weight {
                    builder.variations([("wght", weight)]).build()
                } else {
                    builder.build()
                };
                // Color sources first so Apple Color Emoji (sbix), CBDT, and
                // COLR/CPAL faces render as full-color RGBA; swash falls
                // through to the outline / alpha bitmap for monochrome faces.
                let mut render = Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                    Source::Outline,
                    Source::Bitmap(StrikeWith::BestFit),
                ]);
                render.format(Format::Alpha).embolden(embolden);
                if let Some(t) = italic_skew {
                    render.transform(Some(t));
                }
                render.render(&mut scaler, glyph_id)
            };
            let first_size = if boost {
                size_px * 1.25
            } else {
                size_px * cjk_fit
            };
            let Some(mut image) = render_at(&mut self.scale_ctx, first_size, cjk_embolden) else {
                continue;
            };
            // 팽창량은 크기마다 하나지만 잉크 증가는 획 둘레에 비례한다 — 뷁·醫 처럼
            // 획이 촘촘한 글자는 같은 팽창에 잉크가 ×1.5 까지 불어 빈틈이 좁아졌다.
            // 그런 글자는 목표에 닿는 만큼만 남긴다(잉크는 팽창에 거의 선형이다).
            if cjk_embolden > 0.0 {
                let ink = |d: &[u8]| d.iter().map(|&a| a as f32 / 255.0).sum::<f32>();
                let full = ink(&image.data);
                let (reg, reg_idx) = &self.faces[face_idx];
                let target = outline_ink(&mut self.scale_ctx, reg.as_slice(), *reg_idx, ch, first_size, 0.0)
                    * CJK_BOLD_INK_TARGET;
                if full > target {
                    let bare = render_at(&mut self.scale_ctx, first_size, 0.0).map_or(0.0, |img| ink(&img.data));
                    if full > bare {
                        let s = cjk_embolden * ((target - bare) / (full - bare)).clamp(0.0, 1.0);
                        if let Some(img) = render_at(&mut self.scale_ctx, first_size, s) {
                            image = img;
                        }
                    }
                }
            }
            if image.placement.width == 0 || image.placement.height == 0 {
                // Empty outline — try next face.
                continue;
            }
            // Raster overruns its cell (advance) → shrink to fit. Covers a
            // fallback boost overshoot *and* a primary glyph that's simply
            // wider than a narrow cell — e.g. ① (Enclosed Alphanumerics),
            // which CascadiaCode NF draws cell-and-a-half wide while the
            // terminal lays it out as a single column.
            if !is_cjk_wide(ch) && advance > 0.0 && image.placement.width as f32 > advance {
                let fit = first_size * (advance / image.placement.width as f32);
                if let Some(img) = render_at(&mut self.scale_ctx, fit, 0.0) {
                    if img.placement.width > 0 && img.placement.height > 0 {
                        image = img;
                    }
                }
            }
            let is_color = image.content == Content::Color;
            if std::env::var_os("KASATERM_FONT_DEBUG").is_some() {
                eprintln!(
                    "[font] U+{:04X} → face[{}] gid={} {}×{} color={} bold={} italic={}",
                    ch as u32,
                    face_idx,
                    glyph_id,
                    image.placement.width,
                    image.placement.height,
                    is_color,
                    bold,
                    italic,
                );
            }
            // Synthesised dilation only when no designed bold face exists
            // (`want_dilate`). With a real bold (JetBrains Mono Bold) the
            // outline is already shaped at the right weight; smearing it
            // wider produces the "fat blocky" look the user flagged.
            if want_dilate && !is_color && !is_cjk_wide(ch) {
                widen_alpha_horizontal(
                    &mut image.data,
                    image.placement.width as usize,
                    image.placement.height as usize,
                );
            }
            return Some(Rasterized {
                data: image.data,
                width: image.placement.width,
                height: image.placement.height,
                bearing_x: image.placement.left,
                bearing_y: image.placement.top,
                advance,
                is_color,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(r: Rasterized) -> f32 {
        r.data.iter().map(|&a| a as f32 / 255.0).sum()
    }

    /// 폴백 한글 굵게가 라틴 굵게와 같은 비율로 진해져야 한다. 설치 글꼴에 기대므로
    /// 없으면 건너뛴다(CI·Windows).
    #[test]
    fn fallback_hangul_bold_matches_latin_weight_gain() {
        let Ok(home) = std::env::var("HOME") else { return };
        let f = |n: &str| format!("{home}/Library/Fonts/{n}");
        let paths = [
            f("JetBrainsMonoNerdFontMono-Regular.ttf"),
            f("JetBrainsMonoNerdFontMono-Bold.ttf"),
            f("D2CodingLigatureNerdFont-Regular.ttf"),
            f("D2CodingLigatureNerdFont-Bold.ttf"),
        ];
        if paths.iter().any(|p| !std::path::Path::new(p).exists()) {
            return;
        }
        let mut s = Shaper::from_path(&paths[0], 0).unwrap();
        s.set_bold_face_path(0, &paths[1], 0);
        s.add_fallback_with_bold(&paths[2], 0, Some((paths[3].clone(), 0)));
        for px in [20.0, 24.0, 28.0] {
            for ch in ['한', '글', '뷁', '醫'] {
                let regular = ink(s.rasterize_styled(ch, px, false, false).unwrap());
                let bold = ink(s.rasterize_styled(ch, px, true, false).unwrap());
                let ratio = bold / regular;
                assert!(
                    (1.25..=1.45).contains(&ratio),
                    "{ch} {px}px 굵게 잉크 x{ratio:.2} — 라틴 굵게(x1.35)와 어긋난다"
                );
            }
        }
    }
}
