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

impl Shaper {
    pub fn from_bytes(font_data: Vec<u8>, font_index: u32) -> Result<Self> {
        FontRef::from_index(&font_data, font_index as usize)
            .context("font bytes not a TTF/OTF/TTC entry")?;
        Ok(Self {
            faces: vec![(FontData::Owned(font_data), font_index)],
            bold_faces: Vec::new(),
            italic_faces: Vec::new(),
            scale_ctx: ScaleContext::new(),
        })
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
        })
    }

    /// Append a fallback face. Tried after the primary + every face
    /// already added, in insertion order. Silently ignores paths
    /// that don't exist (so a caller can list optional fallbacks
    /// like Apple Color Emoji without erroring on Linux/Windows).
    pub fn add_fallback_path(&mut self, path: &str, index: u32) {
        if let Some(data) = map_font(path, index) {
            self.faces.push((data, index));
        }
    }

    /// Append a fallback face from in-memory bytes. Used for fonts
    /// we ship inside the binary via `include_bytes!` — guarantees
    /// the chain has Misc-Technical / Nerd icon coverage regardless
    /// of what's installed on the user's system.
    pub fn add_fallback_bytes(&mut self, bytes: &'static [u8], index: u32) {
        if FontRef::from_index(bytes, index as usize).is_some() {
            self.faces.push((FontData::Owned(bytes.to_vec()), index));
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
        self.rasterize('M', size_px)
            .map(|r| r.advance)
            .unwrap_or(size_px * 0.6)
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
            // Synthesised stroke-widen for bold when we DIDN'T land on a
            // real bold face. Keeps consistent visual weight when JetBrains
            // Bold is missing on disk (D2Coding-only systems etc).
            let want_dilate = bold && !matches!(kind, FaceKind::Bold);
            let render_at = |scale_ctx: &mut ScaleContext, face_size: f32| {
                let font = FontRef::from_index(font_data, font_index).unwrap();
                let mut scaler = scale_ctx.builder(font).size(face_size).hint(true).build();
                // Color sources first so Apple Color Emoji (sbix), CBDT, and
                // COLR/CPAL faces render as full-color RGBA; swash falls
                // through to the outline / alpha bitmap for monochrome faces.
                let mut render = Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                    Source::Outline,
                    Source::Bitmap(StrikeWith::BestFit),
                ]);
                render.format(Format::Alpha);
                if let Some(t) = italic_skew {
                    render.transform(Some(t));
                }
                render.render(&mut scaler, glyph_id)
            };
            let first_size = if boost { size_px * 1.25 } else { size_px };
            let Some(mut image) = render_at(&mut self.scale_ctx, first_size) else {
                continue;
            };
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
                if let Some(img) = render_at(&mut self.scale_ctx, fit) {
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
