//! Framework-neutral, retained-mode GPU cell renderer for
//! terminal-style grids — the rendering half of a terminal emulator,
//! with no terminal state machine attached. Pair it with a parser
//! crate (`alacritty_terminal`, `wezterm-term`, `vte`) for the cell
//! model, then hand the resulting cells to this crate to draw.
//!
//! Glyphs are baked into a swash atlas once per (codepoint, weight,
//! style, size) tuple; each frame issues one instance per cell from a
//! single quad pipeline. On a 164×63 grid this drops per-frame cost
//! from the ~30-50ms of shape-every-glyph paths down to a single
//! instance-buffer write plus one draw call.
//!
//! Pure GPU: it takes [`CellInstance`] arrays and a wgpu device — not
//! any caller-side grid type — so it embeds under winit, egui, iced,
//! or a bare wgpu surface without binding to a UI framework's paint
//! path.
//!
//! Included: per-cell RGBA color, bold/italic, CJK/wide-char layout,
//! emoji bitmaps, Nerd-icon cell fitting, box-drawing quads, and an
//! optional sRGB→DisplayP3 conversion in the shader. Two Nerd fonts
//! are bundled so icons render without a system-font install.
//!
//! See `examples/grid_bw.rs` for a self-contained winit window that
//! scrolls a 600-line buffer through the pipeline.

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
