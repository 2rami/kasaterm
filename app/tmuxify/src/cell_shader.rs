//! Thin re-export shim. All terminal cell rendering lives in the
//! `kasaterm` crate (see `crates/kasaterm`). The iced glue is enabled via
//! kasaterm's `iced` feature and lives in `kasaterm::iced_glue` — this
//! file just re-exports the public surface so existing call sites in
//! `main.rs` keep working unchanged.

pub use kasaterm::{FONT_SIZE, LINE_HEIGHT, PaneRender, PaneSnapshot, Rect, TerminalPipeline, TerminalPrimitive};
