//! GUI-agnostic screen snapshot types produced from a vt100 parser.

#[derive(Debug, Clone, PartialEq)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub ch: String,
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
            ch: " ".into(),
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
#[derive(Debug, Clone)]
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
    /// Window title set by shell OSC 0/2 (vt100 parser exposes it).
    pub title: Option<String>,
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
    let ch = if contents.is_empty() { " ".into() } else { contents };
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
