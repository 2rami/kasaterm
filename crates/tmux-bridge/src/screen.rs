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

/// A decoded inline image (iTerm2 OSC 1337) ready for GPU upload.
/// Held behind an `Arc` so cloning a `ScreenUpdate` — which the host
/// does once per frame — never copies the pixel buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
    /// Tightly-packed RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// One inline image to overlay on the cell grid this frame. The
/// backend recomputes this list on every snapshot from the session's
/// placed-image set against the current scroll position, so `row` is
/// already viewport-relative and scrollback-correct (an image scrolled
/// off the top simply isn't included). The renderer just draws each one
/// at the given cell box.
#[derive(Debug, Clone)]
pub struct ImagePlacement {
    /// Stable per-session id so the renderer caches the uploaded GPU
    /// texture across frames instead of re-uploading every redraw.
    pub id: u64,
    pub image: std::sync::Arc<DecodedImage>,
    /// Viewport top-left cell. `row` can be negative when the image's
    /// top has scrolled above the viewport but its lower part is still
    /// visible; the renderer clips.
    pub row: i32,
    pub col: u16,
    /// Cell span the image box occupies.
    pub cols: u16,
    pub rows: u16,
}

/// Screen diff sent from the flusher thread to consumers.
#[derive(Debug, Clone, Default)]
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
    /// Inline images (iTerm2 OSC 1337) currently visible in this pane,
    /// already mapped to viewport cell coordinates for the current
    /// scroll position. Empty on the common no-image frame.
    pub images: Vec<ImagePlacement>,
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
