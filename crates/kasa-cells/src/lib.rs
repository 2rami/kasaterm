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
//! optional sRGB→DisplayP3 conversion in the shader. Fonts come from
//! the host — a path, or bytes via `Shaper::add_fallback_bytes`. The
//! crate ships no font files, so it builds the same from a git
//! dependency (cargo does not fetch git-lfs objects).
//!
//! See `examples/grid_bw.rs` for a self-contained winit window that
//! scrolls a 600-line buffer through the pipeline.

pub mod atlas;
pub mod pipeline;
pub mod shaper;

pub use atlas::{Atlas, AtlasEntry, GlyphKey};
pub use pipeline::{CellInstance, Pipeline};
pub use shaper::{Rasterized, Shaper};
