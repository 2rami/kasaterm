//! GUI-agnostic screen snapshot types produced from a vt100 parser.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// One glyph per cell. `'\0'` is the blank/wide-char-spacer sentinel.
    /// A single `char` (4 bytes, inline) instead of a `String` avoids a heap
    /// allocation per cell — with millions of cells across scrollback + diff
    /// copies, the per-cell `String` was the dominant RSS cost.
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            dim: false,
        }
    }
}

pub type Row = Vec<Cell>;

/// Screen diff sent from the flusher thread to consumers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenUpdate {
    pub pane_id: String,
    pub rows: u16,
    pub cols: u16,
    /// Only changed rows. On size change this contains every row.
    pub dirty: Vec<(u16, Row)>,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub alt_screen: bool,
    /// True when the inner app enabled any xterm mouse-reporting mode
    /// (DECSET 1000/1002/1003/1006). Lets the host translate wheel
    /// events into proper mouse escapes instead of bouncing them off
    /// arrow keys or PgUp/PgDn, which page-jumps in claude TUI.
    pub mouse_enabled: bool,
    /// True when the app requested SGR encoding (DECSET 1006). Without
    /// this we'd have to fall back to the legacy X10 encoding which
    /// claude doesn't parse reliably.
    pub mouse_sgr: bool,
    /// True when the inner app enabled DECCKM (application cursor keys,
    /// DECSET 1). In this mode plain arrow keys must be sent as SS3
    /// (`ESC O A`) not CSI (`ESC [ A`) — claude code / vim / readline
    /// turn it on, and a CSI arrow silently no-ops their line navigation.
    pub app_cursor: bool,
    /// Window title set by shell OSC 0/2 (vt100 parser exposes it).
    pub title: Option<String>,
    /// Sentinel: the PTY reader hit EOF / error (shell or claude exited).
    /// Carries no screen data — it tells the host pump to reap this pane.
    /// Normal frames leave this false.
    pub eof: bool,
    /// Cursor (row, col) at the moment the shell emitted an OSC 133 `B`
    /// mark (prompt end / command-input start), if this frame contained
    /// one. The host uses it as the authoritative start of the editable
    /// command line for inline autosuggestion. `None` on frames without a
    /// fresh mark — the host keeps the last known one until it goes stale.
    pub prompt_end: Option<(u16, u16)>,
}

pub(crate) fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(i) => Color::Idx(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

pub(crate) fn vt_cell(c: &vt100::Cell) -> Cell {
    let contents = c.contents();
    // vt100 returns the cell's grapheme cluster; we keep only the base char
    // (the renderer already shapes a single char per cell). Empty → blank.
    let ch = contents.chars().next().unwrap_or(' ');
    Cell {
        ch,
        fg: vt_color(c.fgcolor()),
        bg: vt_color(c.bgcolor()),
        bold: c.bold(),
        italic: c.italic(),
        underline: c.underline(),
        inverse: c.inverse(),
        dim: false,
    }
}
