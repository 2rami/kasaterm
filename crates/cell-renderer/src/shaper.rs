//! swash-driven glyph rasterizer. Loads one font from raw bytes,
//! returns alpha bitmaps the atlas can paste into its R8 texture.
//!
//! Phase 1 keeps the surface intentionally small — one font, no
//! fallback chain, no shaping for clusters (CJK / emoji / Nerd icons
//! arrive in later phases). The atlas is the side that caches; this
//! module is stateless per glyph so the atlas can decide which keys
//! to keep around.

use anyhow::{Context, Result};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

pub struct Shaper {
    /// Owned font bytes for each face in the fallback chain. The
    /// primary face sits at index 0; subsequent faces are tried in
    /// order whenever the previous one's `charmap.map(ch)` returns
    /// glyph 0 (a.k.a. "this font doesn't cover this codepoint").
    /// Matches the cosmic-text fallback chain we configured under
    /// sugarloaf: D2Coding → JetBrainsMono → Apple SD → Apple
    /// Color Emoji on macOS.
    faces: Vec<(Vec<u8>, u32)>,
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

impl Shaper {
    pub fn from_bytes(font_data: Vec<u8>, font_index: u32) -> Result<Self> {
        FontRef::from_index(&font_data, font_index as usize)
            .context("font bytes not a TTF/OTF/TTC entry")?;
        Ok(Self {
            faces: vec![(font_data, font_index)],
            scale_ctx: ScaleContext::new(),
        })
    }

    pub fn from_path(path: &str, index: u32) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read font {path}"))?;
        Self::from_bytes(bytes, index)
    }

    /// Append a fallback face. Tried after the primary + every face
    /// already added, in insertion order. Silently ignores paths
    /// that don't exist (so a caller can list optional fallbacks
    /// like Apple Color Emoji without erroring on Linux/Windows).
    pub fn add_fallback_path(&mut self, path: &str, index: u32) {
        let Ok(bytes) = std::fs::read(path) else { return };
        if FontRef::from_index(&bytes, index as usize).is_some() {
            self.faces.push((bytes, index));
        }
    }

    /// Append a fallback face from in-memory bytes. Used for fonts
    /// we ship inside the binary via `include_bytes!` — guarantees
    /// the chain has Misc-Technical / Nerd icon coverage regardless
    /// of what's installed on the user's system.
    pub fn add_fallback_bytes(&mut self, bytes: &'static [u8], index: u32) {
        let owned = bytes.to_vec();
        if FontRef::from_index(&owned, index as usize).is_some() {
            self.faces.push((owned, index));
        }
    }

    fn face(&self, idx: usize) -> FontRef<'_> {
        let (bytes, fi) = &self.faces[idx];
        FontRef::from_index(bytes, *fi as usize).unwrap()
    }

    /// Walk the fallback chain and return the first face that covers
    /// `ch` together with its glyph id. Returns None when no face
    /// has a glyph for the codepoint (caller skips the cell).
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
        // Walk every face whose charmap claims the codepoint. A face
        // might map the codepoint to a glyph id but ship an empty
        // outline (D2Coding's Nerd patch does this for a slice of
        // Material Design icons — gid is non-zero, glyph is blank).
        // Continuing past blanks lets later faces (Cascadia NF,
        // Symbols Nerd Font Mono) supply a real glyph.
        let candidates: Vec<(usize, u16, f32)> = {
            let mut v = Vec::new();
            for i in 0..self.faces.len() {
                let f = self.face(i);
                let gid = f.charmap().map(ch as u32);
                if gid != 0 {
                    let advance = f
                        .glyph_metrics(&[])
                        .scale(size_px)
                        .advance_width(gid);
                    v.push((i, gid, advance));
                }
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
        for (face_idx, glyph_id, advance) in candidates {
            // Fallback faces get a size boost so small symbol/icon
            // glyphs read at a comparable size — but CJK/Hangul are
            // full-width and must stay at size_px, otherwise the raster
            // grows past its advance and bleeds into the next cell.
            let face_size = if face_idx == 0 || is_cjk_wide(ch) {
                size_px
            } else {
                size_px * 1.25
            };
            let (font_data, font_index) = {
                let (bytes, fi) = &self.faces[face_idx];
                (bytes.clone(), *fi as usize)
            };
            let scale_ctx = &mut self.scale_ctx;
            let font = FontRef::from_index(&font_data, font_index).unwrap();
            let mut scaler = scale_ctx
                .builder(font)
                .size(face_size)
                .hint(true)
                .build();
            let mut render = Render::new(&[
                Source::Outline,
                Source::Bitmap(StrikeWith::BestFit),
            ]);
            let Some(image) = render.format(Format::Alpha).render(&mut scaler, glyph_id) else {
                continue;
            };
            if image.placement.width == 0 || image.placement.height == 0 {
                // Empty outline — try next face.
                continue;
            }
            if std::env::var_os("KASATERM_FONT_DEBUG").is_some() {
                eprintln!(
                    "[font] U+{:04X} → face[{}] gid={} {}×{}",
                    ch as u32,
                    face_idx,
                    glyph_id,
                    image.placement.width,
                    image.placement.height
                );
            }
            return Some(Rasterized {
                data: image.data,
                width: image.placement.width,
                height: image.placement.height,
                bearing_x: image.placement.left,
                bearing_y: image.placement.top,
                advance,
            });
        }
        None
    }
}
