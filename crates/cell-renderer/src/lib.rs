//! Retained-mode GPU cell renderer for terminal-style grids.
//!
//! Replaces the `sugarloaf` immediate-mode `text.draw` path that
//! shapes each visible glyph every frame (~30-50ms on a 164×63 grid).
//! Here glyphs are baked into an atlas once per (codepoint, weight,
//! style, size) tuple; rendering issues one instance per cell from a
//! single quad pipeline.
//!
//! Phase 1 surface: B&W ASCII only. Single font, no fallback, no
//! per-cell RGB. Bigger pieces (color, bold/italic, CJK fallback,
//! selection/preedit overlays) land in later phases.

pub mod atlas;
pub mod pipeline;
pub mod shaper;

pub use atlas::{Atlas, AtlasEntry, GlyphKey};
pub use pipeline::{CellInstance, Pipeline};
pub use shaper::{Rasterized, Shaper};

/// CascadiaCodeNF — broad Misc-Technical + Nerd icon coverage. Same
/// bundled font sugarloaf 0.4.4 shipped, so we get parity on the
/// claude code icon set without a system-font install requirement.
pub const CASCADIA_CODE_NF: &[u8] = include_bytes!("../assets/CascadiaCodeNF.ttf");

/// SymbolsNerdFontMono — fills any remaining nerd icon (U+E000..F8FF
/// + U+F0000..1FFFD) holes when D2Coding's Nerd patch shipped an
/// empty outline for the codepoint.
pub const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");
