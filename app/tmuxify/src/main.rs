//! tmuxify — sugarloaf-rendered terminal driven by
//! tmux-bridge. Multi-pane: tmux's split-window creates additional
//! panes, layout-change events tell us how to lay them out, and we
//! render each pane inside its rect from the parsed Layout tree.
//! Phase A Task #13/14: wheel + scrollback, IME, selection + clipboard,
//! cursor blink, OSC titles, multi-pane render + focus routing.

mod cells;
mod socket;

use anyhow::Result;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sugarloaf::layout::RootStyle;
use sugarloaf::{Sugarloaf, SugarloafRenderer, SugarloafWindow, SugarloafWindowSize};
use tmux_bridge::layout::{parse_layout, Layout};
use tmux_bridge::screen::Cell as GridCell;
use tmux_bridge::{ScreenUpdate, StartOptions, TmuxEvent, TmuxSession};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT_MULT: f32 = 1.3;
const SCROLLBACK_MAX: usize = 5000;
const WHEEL_THROTTLE_MS: u64 = 8;
/// Half-period of the cursor blink in milliseconds. macOS uses 530 by
/// default; iTerm2 uses 500. 530 matches the platform feel.
const BLINK_HALF_PERIOD_MS: u64 = 530;
/// While the user is actively typing we keep the cursor solid for this
/// long after the last keystroke so it's easy to follow the caret. Same
/// idea as iTerm2's "smart cursor" pause.
const BLINK_PAUSE_AFTER_INPUT_MS: u64 = 700;

/// Cell width / height / baseline in logical pixels. Filled at startup
/// from `Sugarloaf::compute_cell_metrics` so columns align with the
/// actual font advance instead of a hardcoded guess. Falls back to a
/// reasonable default before the first measurement lands.
#[derive(Copy, Clone, Debug)]
struct CellGeom {
    w: f32,
    h: f32,
    baseline: f32,
}

impl Default for CellGeom {
    fn default() -> Self {
        Self { w: 8.6, h: 18.0, baseline: 14.0 }
    }
}

/// (col, row) anchor + end for drag selection. Both ends in cell units.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Selection {
    anchor: (u16, u16),
    end: (u16, u16),
}

/// Normalise so (start.row, start.col) <= (end.row, end.col) in reading
/// order. Used both for highlight rendering and clipboard extraction.
fn normalise(sel: Selection) -> ((u16, u16), (u16, u16)) {
    let a = sel.anchor;
    let b = sel.end;
    if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Pull the selected text out of the visible row grid. Joined with `\n`,
/// trailing spaces trimmed per row. Mirrors kasaterm::extract_selection.
fn extract_selection(rows: &[Vec<GridCell>], sel: Selection) -> String {
    let (start, end) = normalise(sel);
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let r = r as u16;
        if r < start.1 || r > end.1 {
            continue;
        }
        let (cs, ce) = if start.1 == end.1 {
            (start.0 as usize, end.0 as usize)
        } else if r == start.1 {
            (start.0 as usize, row.len().saturating_sub(1))
        } else if r == end.1 {
            (0, end.0 as usize)
        } else {
            (0, row.len().saturating_sub(1))
        };
        for cell in row.iter().take(ce + 1).skip(cs) {
            if cell.ch.is_empty() {
                out.push(' ');
            } else {
                out.push_str(&cell.ch);
            }
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Wheel accumulator. Returns Some(lines) when an emit fires, None while
/// accumulating sub-cell ticks or while the throttle window is open.
/// Mirrors kasaterm-cli's wheel_step semantics exactly.
fn wheel_step(
    accum: &mut f32,
    dy_cells: f32,
    last_emit: &mut Instant,
    now: Instant,
) -> Option<i32> {
    if accum.signum() != dy_cells.signum() && *accum != 0.0 && dy_cells != 0.0 {
        *accum = 0.0;
    }
    *accum += dy_cells;
    let lines = accum.trunc() as i32;
    if lines == 0 {
        return None;
    }
    if now.duration_since(*last_emit) < std::time::Duration::from_millis(WHEEL_THROTTLE_MS) {
        return None;
    }
    *accum -= lines as f32;
    *last_emit = now;
    Some(lines)
}

/// Per-pane render state. One of these per tmux pane (`%N`). Holds the
/// cell grid, scrollback ring, cursor, and the flags we need to route
/// wheel events correctly (alt-screen / SGR mouse mode are per-pane in
/// real terminals — claude in pane 0 can be in alt-screen while a
/// shell prompt sits in pane 1).
#[derive(Default)]
struct PaneState {
    rows: u16,
    cols: u16,
    cells: Vec<Vec<GridCell>>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    alt_screen: bool,
    mouse_enabled: bool,
    mouse_sgr: bool,
    history: VecDeque<Vec<GridCell>>,
    /// Scrollback offset in rows. `0` = live tail; positive = N rows
    /// back into history visible at the top.
    scroll_offset: usize,
    /// Cached previous cells used by the shift-detection heuristic that
    /// promotes scrolled-off rows into `history`. Per-pane because the
    /// shifts are pane-local.
    prev_cells: Vec<Vec<GridCell>>,
    /// OSC 0/2 title — `printf '\e]0;hello\a'` from a shell inside this
    /// pane lands here. The active pane's title is applied to the
    /// window chrome.
    title: Option<String>,
}

/// Whole-window state: HashMap of panes keyed by tmux pane id, the
/// most recently parsed Layout tree, and which pane is active for
/// keyboard / selection / cursor display.
#[derive(Default)]
struct Workspace {
    panes: HashMap<String, PaneState>,
    layout: Option<Layout>,
    active_pane: Option<String>,
}

impl Workspace {
    fn pane_mut(&mut self, id: &str) -> &mut PaneState {
        self.panes
            .entry(id.to_string())
            .or_insert_with(PaneState::default)
    }

    fn active(&self) -> Option<&PaneState> {
        self.active_pane
            .as_deref()
            .and_then(|id| self.panes.get(id))
    }

    fn active_mut(&mut self) -> Option<&mut PaneState> {
        let id = self.active_pane.clone()?;
        self.panes.get_mut(&id)
    }
}

struct App {
    window: Option<Arc<Window>>,
    sugarloaf: Option<Sugarloaf<'static>>,
    tmux: Option<Arc<TmuxSession>>,
    /// Phase C backend. Mutually exclusive with `tmux` — exactly one
    /// is `Some` after `start_backend`. Selection driven by the
    /// TMUXIFY_BACKEND env var; defaults to tmux.
    pty: Option<Arc<pty_backend::PtySession>>,
    ws: Arc<Mutex<Workspace>>,
    /// Measured cell geometry from sugarloaf — see `compute_cell_metrics`.
    cell: CellGeom,
    preedit: String,
    in_preedit: bool,
    selection: Option<Selection>,
    drag_anchor: Option<(u16, u16)>,
    cursor_px: (f32, f32),
    modifiers: ModifiersState,
    wheel_accum_y: f32,
    last_wheel_emit: Instant,
    /// Last keystroke / IME / mouse press timestamp. Resets the blink
    /// phase so the cursor stays solid while the user is actively
    /// interacting and only fades back to blinking on idle.
    last_input_at: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            sugarloaf: None,
            tmux: None,
            pty: None,
            ws: Arc::new(Mutex::new(Workspace::default())),
            cell: CellGeom::default(),
            preedit: String::new(),
            in_preedit: false,
            selection: None,
            drag_anchor: None,
            cursor_px: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            wheel_accum_y: 0.0,
            last_wheel_emit: Instant::now() - std::time::Duration::from_secs(1),
            last_input_at: Instant::now(),
        }
    }

    /// True when the cursor block should be visible this frame.
    /// Solid for `BLINK_PAUSE_AFTER_INPUT_MS` after any input event, then
    /// toggles every `BLINK_HALF_PERIOD_MS`.
    fn cursor_blink_on(&self, now: Instant) -> bool {
        let since_input = now.saturating_duration_since(self.last_input_at);
        if since_input.as_millis() < BLINK_PAUSE_AFTER_INPUT_MS as u128 {
            return true;
        }
        let elapsed = since_input.as_millis() - BLINK_PAUSE_AFTER_INPUT_MS as u128;
        (elapsed / BLINK_HALF_PERIOD_MS as u128) % 2 == 0
    }

    fn schedule_autocapture(&self) {
        let Ok(ms_str) = std::env::var("TMUXIFY_AUTOCAPTURE_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        let path = std::env::var("TMUXIFY_AUTOCAPTURE_PATH")
            .unwrap_or_else(|_| "/tmp/tmuxify.png".into());
        eprintln!("[autocapture] in {ms}ms → {path}");
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let pid = std::process::id();
            // Force our window to the front, then capture just its region.
            // Full-desktop screencapture grabs whatever app is on top —
            // useless in headless verify runs where another app may have
            // focus. Window-bounded capture sidesteps that.
            let _ = std::process::Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        "tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true"
                    ),
                ])
                .status();
            std::thread::sleep(std::time::Duration::from_millis(400));
            // Query window bounds {x, y, w, h} via System Events.
            let bounds_script = format!(
                "tell application \"System Events\" to tell (first process whose unix id is {pid}) to get {{position, size}} of window 1"
            );
            let bounds_out = std::process::Command::new("osascript")
                .args(["-e", &bounds_script])
                .output();
            let region = bounds_out.ok().and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // Format: "x, y, w, h" (System Events returns flattened list).
                let parts: Vec<i32> = s
                    .split(',')
                    .filter_map(|p| p.trim().parse::<i32>().ok())
                    .collect();
                if parts.len() == 4 {
                    Some(format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]))
                } else {
                    None
                }
            });
            let mut cmd = std::process::Command::new("screencapture");
            cmd.args(["-x", "-t", "png"]);
            if let Some(r) = region.as_deref() {
                cmd.args(["-R", r]);
            }
            cmd.arg(&path);
            let _ = cmd.status();
            eprintln!("[autocapture] captured {path} region={:?}", region);
        });
    }

    fn schedule_autosend(&self) {
        let Ok(text) = std::env::var("TMUXIFY_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("TMUXIFY_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        // Capture whichever backend is wired so we don't need access
        // to self inside the timer thread.
        let tmux = self.tmux.clone();
        let pty = self.pty.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let mut payload = text.clone();
            if !payload.ends_with('\n') {
                payload.push('\n');
            }
            if let Some(t) = tmux.as_ref() {
                let hex: String = payload
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = t.send_keys_hex(None, &hex);
            } else if let Some(p) = pty.as_ref() {
                let _ = p.send_bytes(payload.as_bytes());
            }
        });
    }

    /// Phase C path. Spawns the shell into a direct PTY (no tmux),
    /// hooks the screens channel into the same per-pane state the
    /// renderer expects. Single-pane MVP — the workspace holds one
    /// PaneState keyed "%0" and the layout is `None` (the render path
    /// falls back to single-pane when no layout has arrived).
    fn start_pty(&mut self) -> Result<()> {
        let window = self.window.as_ref().expect("window before pty");
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let lw = size.width as f32 / scale;
        let lh = size.height as f32 / scale;
        let cols = (lw / self.cell.w).floor().max(40.0) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
            shell: std::env::var("SHELL").ok(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
        })?;
        let screens = session.screens.clone();
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(update) = screens.recv() {
                let mut ws = ws_screens.lock().unwrap();
                if ws.active_pane.is_none() {
                    ws.active_pane = Some(update.pane_id.clone());
                }
                let pane = ws.pane_mut(&update.pane_id);
                let resized = pane.cols != update.cols
                    || pane.rows != update.rows
                    || pane.cells.len() != update.rows as usize;
                if resized {
                    pane.cols = update.cols;
                    pane.rows = update.rows;
                    pane.cells = (0..update.rows as usize)
                        .map(|_| vec![GridCell::blank(); update.cols as usize])
                        .collect();
                    pane.prev_cells.clear();
                }
                for (r, row) in update.dirty {
                    if let Some(dst) = pane.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection on the pty side too — alacritty emits
                // the full grid each frame, so this catches scroll runs
                // (matches how the tmux branch promotes scrolled-off
                // rows into the history ring).
                if !update.alt_screen
                    && !pane.prev_cells.is_empty()
                    && pane.prev_cells.len() == pane.cells.len()
                {
                    let n = pane.prev_cells.len();
                    let mut shifted = 0usize;
                    for k in 1..n {
                        if pane.prev_cells[k..] == pane.cells[..n - k] {
                            shifted = k;
                            break;
                        }
                    }
                    if shifted > 0 {
                        for row in &pane.prev_cells[..shifted] {
                            pane.history.push_back(row.clone());
                        }
                        while pane.history.len() > SCROLLBACK_MAX {
                            pane.history.pop_front();
                        }
                    }
                }
                pane.prev_cells = pane.cells.clone();
                pane.cursor_row = update.cursor_row;
                pane.cursor_col = update.cursor_col;
                pane.cursor_visible = update.cursor_visible;
                pane.alt_screen = update.alt_screen;
                pane.mouse_enabled = update.mouse_enabled;
                pane.mouse_sgr = update.mouse_sgr;
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    w.request_redraw();
                }
            }
        });
        self.pty = Some(Arc::new(session));
        Ok(())
    }

    fn start_tmux(&mut self) -> Result<()> {
        let window = self.window.as_ref().expect("window before tmux");
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let lw = size.width as f32 / scale;
        let lh = size.height as f32 / scale;
        let cols = (lw / self.cell.w).floor().max(40.0) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("tmuxify"),
            cols,
            rows,
            ..Default::default()
        })?;
        // Screens thread: each ScreenUpdate carries a pane_id; routes to
        // the matching PaneState in the workspace. New pane ids appear
        // automatically when tmux split-window creates them.
        let screens = tmux.screens.clone();
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(ScreenUpdate {
                pane_id,
                rows,
                cols,
                dirty,
                cursor_row,
                cursor_col,
                cursor_visible,
                alt_screen,
                mouse_enabled,
                mouse_sgr,
                title,
                ..
            }) = screens.recv()
            {
                let mut ws = ws_screens.lock().unwrap();
                // First-seen pane becomes the active one so the user
                // doesn't open into a workspace with no focus.
                if ws.active_pane.is_none() {
                    ws.active_pane = Some(pane_id.clone());
                }
                let is_active = ws.active_pane.as_deref() == Some(pane_id.as_str());
                let pane = ws.pane_mut(&pane_id);
                let resized = pane.cols != cols
                    || pane.rows != rows
                    || pane.cells.len() != rows as usize;
                if resized {
                    pane.cols = cols;
                    pane.rows = rows;
                    pane.cells = (0..rows as usize)
                        .map(|_| vec![GridCell::blank(); cols as usize])
                        .collect();
                    pane.prev_cells.clear();
                }
                for (r, row) in dirty {
                    if let Some(dst) = pane.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection per pane — alt-screen apps manage their
                // own scrollback so we skip there.
                if !alt_screen
                    && !pane.prev_cells.is_empty()
                    && pane.prev_cells.len() == pane.cells.len()
                {
                    let n = pane.prev_cells.len();
                    let mut shifted = 0usize;
                    for k in 1..n {
                        if pane.prev_cells[k..] == pane.cells[..n - k] {
                            shifted = k;
                            break;
                        }
                    }
                    if shifted > 0 {
                        for row in &pane.prev_cells[..shifted] {
                            pane.history.push_back(row.clone());
                        }
                        while pane.history.len() > SCROLLBACK_MAX {
                            pane.history.pop_front();
                        }
                    }
                }
                pane.prev_cells = pane.cells.clone();
                pane.cursor_row = cursor_row;
                pane.cursor_col = cursor_col;
                pane.cursor_visible = cursor_visible;
                pane.alt_screen = alt_screen;
                pane.mouse_enabled = mouse_enabled;
                pane.mouse_sgr = mouse_sgr;
                let new_title = title.filter(|t| !t.is_empty());
                let title_changed = pane.title != new_title;
                if title_changed {
                    pane.title = new_title.clone();
                }
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    // Only the active pane's title shows in the window
                    // chrome — background panes change silently.
                    if title_changed && is_active {
                        let display =
                            new_title.unwrap_or_else(|| "tmuxify".into());
                        w.set_title(&display);
                    }
                    w.request_redraw();
                }
            }
        });
        // Events thread: parses %layout-change messages so render_frame
        // can lay panes out. Without this, splits would create panes
        // we have screen state for but no rect to draw them at.
        let events = tmux.events.clone();
        let ws_events = self.ws.clone();
        let win_events = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(evt) = events.recv() {
                match evt {
                    TmuxEvent::LayoutChange { layout, .. } => {
                        // tmux's %layout-change emits both the visible
                        // and default layouts in one message,
                        // space-separated, plus a trailing flag.
                        // parse_layout wants exactly one layout
                        // string, so take the first token.
                        let first = layout
                            .split_whitespace()
                            .next()
                            .unwrap_or(&layout);
                        match parse_layout(first) {
                            Ok(parsed) => {
                                let mut ws = ws_events.lock().unwrap();
                                ws.layout = Some(parsed);
                                drop(ws);
                                if let Some(w) = win_events.as_ref() {
                                    w.request_redraw();
                                }
                            }
                            Err(e) => {
                                eprintln!("[layout] parse failed: {e} ({first:?})");
                            }
                        }
                    }
                    TmuxEvent::WindowPaneChanged { pane_id, .. } => {
                        // tmux flipped the active pane (most commonly:
                        // a split-window just landed and the new pane
                        // grabbed focus). Mirror that into our state
                        // so the cursor + active border + outgoing key
                        // target all move together.
                        let mut ws = ws_events.lock().unwrap();
                        if ws.active_pane.as_deref() != Some(pane_id.as_str()) {
                            ws.active_pane = Some(pane_id);
                            drop(ws);
                            if let Some(w) = win_events.as_ref() {
                                w.request_redraw();
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        let tmux_arc = Arc::new(tmux);
        self.tmux = Some(tmux_arc.clone());
        self.start_socket(tmux_arc);
        Ok(())
    }

    /// Bring up the cmux-compatible JSON-RPC server so external agents
    /// (Claude Code teammateMode, ad-hoc CLI scripts) can drive this
    /// pane. The server is best-effort — a bind failure logs and the
    /// rest of the binary keeps working without it. Two env names are
    /// exported on the spawned shell:
    ///   - TMUXIFY_SOCKET_PATH (our brand)
    ///   - CMUX_SOCKET_PATH (so cmux-aware clients auto-detect us)
    /// Both point at the same socket; the second is the cmux-protocol
    /// convention from issue anthropics/claude-code#36926.
    fn start_socket(&self, tmux: Arc<tmux_bridge::TmuxSession>) {
        let path = std::env::var("TMUXIFY_SOCKET_PATH")
            .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
            .unwrap_or_else(|_| {
                format!(
                    "{}/tmuxify-{}.sock",
                    std::env::temp_dir().to_string_lossy(),
                    std::process::id()
                )
            });
        let server = match agent_socket::Server::bind(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[agent-socket] bind {path:?} failed: {e:#}");
                return;
            }
        };
        let resolved = server.socket_path().to_string_lossy().to_string();
        eprintln!("[agent-socket] listening on {resolved}");
        std::env::set_var("TMUXIFY_SOCKET_PATH", &resolved);
        std::env::set_var("CMUX_SOCKET_PATH", &resolved);
        let backend: Arc<dyn agent_socket::Backend> =
            Arc::new(socket::TmuxBackend::new(tmux));
        let _join = server.spawn(backend);
    }

    /// Convert logical-pixel position into a (pane_id, col, row) cell
    /// inside the pane the click landed in. Multi-pane aware: walks the
    /// parsed Layout to find the pane whose rect contains the click,
    /// then translates the pixel into that pane's cell-local coords.
    /// Returns None when the workspace has no panes or the click missed
    /// every pane (gutter between split borders, padding, etc).
    fn px_to_pane_cell(&self, px: f32, py: f32) -> Option<(String, u16, u16)> {
        let ws = self.ws.lock().unwrap();
        // The 8.0 padding lines up with render_frame's origin offset —
        // both the grid and the click reuse it so the math stays
        // consistent on the boundary cells.
        let col = ((px - 8.0).max(0.0) / self.cell.w).floor() as u16;
        let row = ((py - 8.0).max(0.0) / self.cell.h).floor() as u16;
        if let Some(layout) = ws.layout.as_ref() {
            for leaf in layout.leaves() {
                if let Layout::Pane { id, x, y, w, h } = leaf {
                    if col >= *x && col < x + w && row >= *y && row < y + h {
                        let local_col = col - x;
                        let local_row = row - y;
                        return Some((format!("%{id}"), local_col, local_row));
                    }
                }
            }
            return None;
        }
        // No layout yet — treat the whole window as the active pane.
        // First-pane lookup matches what the render path falls back to.
        let id = ws.active_pane.clone().or_else(|| ws.panes.keys().next().cloned())?;
        let pane = ws.panes.get(&id)?;
        if pane.cols == 0 || pane.rows == 0 {
            return None;
        }
        Some((id, col.min(pane.cols - 1), row.min(pane.rows - 1)))
    }

    /// Convenience wrapper that returns only the active pane's local
    /// cell coords. Most callers (wheel, selection drag) only care
    /// about the active pane.
    fn px_to_cell_active(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let (pane_id, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let active_match = ws.active_pane.as_deref() == Some(pane_id.as_str());
        active_match.then_some((col, row))
    }

    /// Target pane for outgoing key/text. When the workspace has an
    /// active pane, we name it explicitly so tmux doesn't fall back to
    /// "last-active" semantics that disagree with our UI.
    fn target_pane(&self) -> Option<String> {
        self.ws.lock().unwrap().active_pane.clone()
    }

    /// Resize whichever backend is wired up. Tmux gets its
    /// `resize-client`, the direct PTY gets `TIOCSWINSZ` via
    /// `portable-pty::resize`. Renderer code calls this uniformly
    /// from the WindowEvent::Resized handler.
    fn resize_backend(&self, cols: u16, rows: u16) {
        if let Some(tmux) = self.tmux.as_ref() {
            let _ = tmux.resize_client(cols, rows);
        } else if let Some(pty) = self.pty.as_ref() {
            let _ = pty.resize(cols, rows);
        }
    }

    fn send_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Dispatch by which backend is wired up. The hex encoding is
        // a tmux send-keys quirk (the daemon decodes hex pairs back
        // to bytes itself); for the pty backend we hand the raw bytes
        // straight to the PTY writer.
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let target = self.target_pane();
            let _ = tmux.send_keys_hex(target.as_deref(), &hex);
        } else if let Some(pty) = self.pty.as_ref() {
            let _ = pty.send_bytes(bytes);
        }
    }

    fn copy_selection(&self) {
        let Some(sel) = self.selection else { return; };
        let rows = {
            let ws = self.ws.lock().unwrap();
            match ws.active() {
                Some(p) => p.cells.clone(),
                None => return,
            }
        };
        let text = extract_selection(&rows, sel);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[tmuxify] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[tmuxify] clipboard open failed: {e}"),
        }
    }

    fn paste_clipboard(&self) {
        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[tmuxify] clipboard read failed: {e}");
                return;
            }
        };
        let mut payload = Vec::with_capacity(text.len() + 12);
        payload.extend_from_slice(b"\x1b[200~");
        payload.extend_from_slice(text.as_bytes());
        payload.extend_from_slice(b"\x1b[201~");
        self.send_bytes(&payload);
    }

    fn handle_wheel(&mut self, delta: MouseScrollDelta) {
        let dy_cells = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 0.3,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / self.cell.h.max(1.0) * 0.3,
        };
        let lines = match wheel_step(
            &mut self.wheel_accum_y,
            dy_cells,
            &mut self.last_wheel_emit,
            Instant::now(),
        ) {
            Some(l) => l,
            None => return,
        };
        // Decide which pane handles this wheel: the pane the pointer is
        // hovering over. Falls back to the active pane if the pointer
        // is in a gutter. Multi-pane lets the user scroll inside any
        // pane regardless of which one currently has keyboard focus.
        let target_pane_id = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        let (alt, hist_len, mouse_on, mouse_sgr) = {
            let ws = self.ws.lock().unwrap();
            let pane = target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id));
            match pane {
                Some(p) => (p.alt_screen, p.history.len(), p.mouse_enabled, p.mouse_sgr),
                None => return,
            }
        };
        if mouse_on && mouse_sgr {
            let (col, row) = self
                .px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                .unwrap_or((1, 1));
            let button = if lines > 0 { 64 } else { 65 };
            let count = lines.unsigned_abs().min(8) as usize;
            let single = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
            let payload: Vec<u8> = single.as_bytes().repeat(count.max(1));
            // For the tmux backend we name the pane explicitly so an
            // inactive-but-hovered pane scrolls instead of the focused
            // one. The pty backend is single-pane: the pane id is
            // already implicit.
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = payload
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(pty) = self.pty.as_ref() {
                let _ = pty.send_bytes(&payload);
            }
            return;
        }
        if alt {
            let esc: &[u8] = if lines > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = esc
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(pty) = self.pty.as_ref() {
                let _ = pty.send_bytes(esc);
            }
            return;
        }
        // Normal screen: scroll the targeted pane's history.
        let step = lines.unsigned_abs().min(8) as usize;
        if let (Some(id), Ok(mut ws)) = (target_pane_id, self.ws.lock()) {
            if let Some(pane) = ws.panes.get_mut(&id) {
                if lines > 0 {
                    pane.scroll_offset = (pane.scroll_offset + step).min(hist_len);
                } else {
                    pane.scroll_offset = pane.scroll_offset.saturating_sub(step);
                }
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn forward_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Touch the input timer so the cursor stays solid for a beat and
        // the blink phase re-starts from "on" once it kicks in.
        self.last_input_at = Instant::now();
        // Typing snaps the active pane back to live tail. Other panes'
        // scroll offsets are left alone — switching focus by clicking
        // doesn't disturb where the user was reading.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(pane) = ws.active_mut() {
                if pane.scroll_offset != 0 {
                    pane.scroll_offset = 0;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
        let is_cmd = self.modifiers.super_key() || self.modifiers.control_key();
        if is_cmd {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("c") && self.selection.is_some() {
                    self.copy_selection();
                    return;
                }
                if s.eq_ignore_ascii_case("v") {
                    self.paste_clipboard();
                    return;
                }
            }
        }
        let bytes: Vec<u8> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => b"\r".to_vec(),
            Key::Named(NamedKey::Backspace) => b"\x7f".to_vec(),
            Key::Named(NamedKey::Tab) => b"\t".to_vec(),
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
            Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
            Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
            Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
            Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
            _ => match event.text.as_ref() {
                Some(t) => {
                    // Drop non-ASCII Character events ONLY while the IME
                    // is actively composing — those are the jamo that
                    // Preedit/Commit will deliver again. Outside an active
                    // preedit (e.g. very first key), the Character event
                    // is the only channel, so let it pass.
                    let non_ascii_during_preedit = self.in_preedit
                        && !t.chars().all(|c| c.is_ascii() && !c.is_control());
                    if non_ascii_during_preedit {
                        return;
                    }
                    t.as_bytes().to_vec()
                }
                None => return,
            },
        };
        self.send_bytes(&bytes);
    }

    fn render_frame(&mut self) {
        let now = Instant::now();
        let blink_on = self.cursor_blink_on(now);
        let Some(window) = self.window.as_ref() else { return; };
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        let size = window.inner_size();
        sugarloaf.rect(
            None,
            0.0,
            0.0,
            size.width as f32,
            size.height as f32,
            [
                cells::DEFAULT_BG[0] as f32 / 255.0,
                cells::DEFAULT_BG[1] as f32 / 255.0,
                cells::DEFAULT_BG[2] as f32 / 255.0,
                1.0,
            ],
            0.0,
            0,
        );

        // Snapshot the per-pane render data under one lock so the
        // sugarloaf draw calls below can run without re-locking. Each
        // entry carries the pane's resolved rect (in cells), the cell
        // grid we'll actually paint (history + live composed), and the
        // cursor / title info the renderer reads.
        struct PaneFrame {
            id: String,
            x_cells: u16,
            y_cells: u16,
            w_cells: u16,
            h_cells: u16,
            rows: Vec<Vec<GridCell>>,
            cursor_row: u16,
            cursor_col: u16,
            cursor_visible: bool,
        }
        let (pane_frames, active_id) = {
            let ws = self.ws.lock().unwrap();
            let active_id = ws.active_pane.clone();
            let leaves: Vec<(String, u16, u16, u16, u16)> =
                if let Some(layout) = ws.layout.as_ref() {
                    layout
                        .leaves()
                        .into_iter()
                        .filter_map(|n| match n {
                            Layout::Pane { id, x, y, w, h } => {
                                Some((format!("%{id}"), *x, *y, *w, *h))
                            }
                            _ => None,
                        })
                        .collect()
                } else {
                    // No layout yet: fall back to one full-window pane.
                    // First key in the map keeps things deterministic so
                    // the active-pane lookup below points at the same one.
                    match ws.panes.iter().next() {
                        Some((id, p)) => vec![(id.clone(), 0, 0, p.cols, p.rows)],
                        None => Vec::new(),
                    }
                };
            let mut frames = Vec::with_capacity(leaves.len());
            for (id, x, y, w, h) in leaves {
                let Some(pane) = ws.panes.get(&id) else { continue };
                let total = pane.rows.max(1) as usize;
                let offset = pane.scroll_offset.min(pane.history.len());
                let composed: Vec<Vec<GridCell>> = if offset == 0 {
                    pane.cells.clone()
                } else {
                    let mut out: Vec<Vec<GridCell>> = Vec::with_capacity(total);
                    let hist_start = pane.history.len() - offset;
                    for row in pane.history.iter().skip(hist_start) {
                        out.push(row.clone());
                        if out.len() >= total {
                            break;
                        }
                    }
                    let need = total.saturating_sub(out.len());
                    for row in pane.cells.iter().take(need) {
                        out.push(row.clone());
                    }
                    out
                };
                frames.push(PaneFrame {
                    id,
                    x_cells: x,
                    y_cells: y,
                    w_cells: w,
                    h_cells: h,
                    rows: composed,
                    cursor_row: pane.cursor_row,
                    cursor_col: pane.cursor_col,
                    cursor_visible: offset == 0 && pane.cursor_visible,
                });
            }
            (frames, active_id)
        };

        if pane_frames.is_empty() {
            sugarloaf.render();
            return;
        }

        // Origin offset matches kasaterm-cli — 8px padding around the
        // outer grid so chrome doesn't crowd the cells.
        let origin_x = 8.0;
        let origin_y = 8.0;

        // Pass 1: walk each pane and render its cell grid at its rect.
        for frame in &pane_frames {
            let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
            let pane_px_y = origin_y + frame.y_cells as f32 * self.cell.h;
            cells::render_screen(
                sugarloaf,
                &frame.rows,
                pane_px_x,
                pane_px_y,
                self.cell.w,
                self.cell.h,
                FONT_SIZE,
                self.cell.baseline,
            );
        }

        // Pass 2: inactive pane border (dim grey, 1px) — gives the user a
        // visual cue that a split exists and which side they're typing
        // into. Active pane gets a slightly brighter accent border.
        if pane_frames.len() > 1 {
            for frame in &pane_frames {
                let is_active = active_id.as_deref() == Some(frame.id.as_str());
                let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
                let pane_px_y = origin_y + frame.y_cells as f32 * self.cell.h;
                let pane_px_w = frame.w_cells as f32 * self.cell.w;
                let pane_px_h = frame.h_cells as f32 * self.cell.h;
                let color = if is_active {
                    [0.36, 0.51, 0.95, 0.55]
                } else {
                    [0.24, 0.26, 0.30, 0.45]
                };
                // Top + bottom + left + right strokes — each a 1px rect.
                sugarloaf.rect(None, pane_px_x, pane_px_y, pane_px_w, 1.0, color, 0.0, 0);
                sugarloaf.rect(
                    None,
                    pane_px_x,
                    pane_px_y + pane_px_h - 1.0,
                    pane_px_w,
                    1.0,
                    color,
                    0.0,
                    0,
                );
                sugarloaf.rect(None, pane_px_x, pane_px_y, 1.0, pane_px_h, color, 0.0, 0);
                sugarloaf.rect(
                    None,
                    pane_px_x + pane_px_w - 1.0,
                    pane_px_y,
                    1.0,
                    pane_px_h,
                    color,
                    0.0,
                    0,
                );
            }
        }

        // Pass 3: selection overlay + cursor block + preedit on the
        // active pane only. Inactive panes show no cursor — matches the
        // tmux / iTerm2 convention where the unfocused split fades its
        // caret.
        let active_frame = active_id
            .as_deref()
            .and_then(|id| pane_frames.iter().find(|f| f.id == id));
        if let Some(frame) = active_frame {
            let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
            let pane_px_y = origin_y + frame.y_cells as f32 * self.cell.h;
            if let Some(sel) = self.selection {
                cells::render_selection_overlay(
                    sugarloaf,
                    sel.anchor,
                    sel.end,
                    pane_px_x,
                    pane_px_y,
                    self.cell.w,
                    self.cell.h,
                );
            }
            if frame.cursor_visible && (blink_on || !self.preedit.is_empty()) {
                let cursor_x = pane_px_x + frame.cursor_col as f32 * self.cell.w;
                let cursor_y = pane_px_y + frame.cursor_row as f32 * self.cell.h;
                // Cursor color from the user's iTerm2 profile
                // (`Cursor Color` = pure black). 55% alpha matches
                // iTerm2's "block cursor on light bg" feel — full
                // opacity would hide the glyph underneath.
                sugarloaf.rect(
                    None,
                    cursor_x,
                    cursor_y,
                    self.cell.w,
                    self.cell.h,
                    [
                        cells::ITERM_CURSOR[0] as f32 / 255.0,
                        cells::ITERM_CURSOR[1] as f32 / 255.0,
                        cells::ITERM_CURSOR[2] as f32 / 255.0,
                        0.55,
                    ],
                    0.0,
                    0,
                );
            }
            if frame.cursor_visible && !self.preedit.is_empty() {
                let px = pane_px_x + frame.cursor_col as f32 * self.cell.w;
                let py = pane_px_y + frame.cursor_row as f32 * self.cell.h;
                cells::render_preedit(
                    sugarloaf,
                    &self.preedit,
                    px,
                    py,
                    self.cell.w,
                    self.cell.h,
                    FONT_SIZE,
                );
            }
        }

        sugarloaf.render();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // WaitUntil so the cursor blink ticks even when no terminal output
        // is arriving — the redraw inside RedrawRequested re-arms the
        // schedule. Pure Wait would freeze the blink mid-phase, Poll would
        // burn CPU on idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
        let attrs = WindowAttributes::default()
            .with_title("tmuxify")
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create window"),
        );
        // Without IME enabled, Hangul / kana would arrive as raw key
        // events instead of composing into 안 / 한 / 글.
        window.set_ime_allowed(true);
        let sg_window = SugarloafWindow {
            handle: window.window_handle().unwrap().as_raw(),
            display: window.display_handle().unwrap().as_raw(),
            scale: window.scale_factor() as f32,
            size: SugarloafWindowSize {
                width: window.inner_size().width as f32,
                height: window.inner_size().height as f32,
            },
        };
        // Match the user's active macOS Terminal.app profile
        // (Default Window Settings = "GitHub Dark Dimmed"). The plist
        // stores the font as the NSKeyedArchiver bytes of an NSFont
        // whose fontName() is "D2CodingLigatureNFM" — the PostScript
        // name of D2CodingLigature Nerd Font Mono. CoreText / swash
        // resolve through family names, so we point at the family the
        // system registry lists for that .ttf and fall through to
        // sugarloaf's bundled Cascadia when the face isn't installed.
        let mut fonts = sugarloaf::font::fonts::SugarloafFonts::default();
        fonts.family = Some("D2CodingLigature Nerd Font Mono".to_string());
        let (font_library, font_err) = sugarloaf::font::FontLibrary::new(fonts);
        if let Some(err) = font_err {
            if !err.fonts_not_found.is_empty() {
                eprintln!(
                    "[font] requested fonts not found, sugarloaf will fall back: {:?}",
                    err.fonts_not_found
                );
            }
        }
        let sugarloaf = Sugarloaf::new(
            sg_window,
            SugarloafRenderer::default(),
            &font_library,
            RootStyle::default(),
        )
        .expect("Sugarloaf instance");
        // Replace the CellGeom default with the actual font advance /
        // ascent so columns align right of col ~80, where the 8.6
        // estimate started drifting visibly.
        let scale = window.scale_factor() as f32;
        // line_height here is a multiplier (1.0 = font ascent+descent only),
        // *not* a pixel value — rio's default is 1.0. Pass the multiplier
        // directly; passing pixels produces absurd cell sizes.
        let (_dim, metrics) =
            sugarloaf.compute_cell_metrics(FONT_SIZE, LINE_HEIGHT_MULT, scale);
        // compute_cell_metrics returns u32 physical pixels — divide by
        // scale to land back in logical units the rest of the renderer
        // works with.
        self.cell = CellGeom {
            w: (metrics.cell_width as f32) / scale,
            h: (metrics.cell_height as f32) / scale,
            baseline: (metrics.cell_baseline as f32) / scale,
        };
        eprintln!(
            "[startup] cell_geom w={:.2} h={:.2} baseline={:.2} (scale={scale})",
            self.cell.w, self.cell.h, self.cell.baseline
        );
        self.sugarloaf = Some(sugarloaf);
        self.window = Some(window);
        // Backend selection. TMUXIFY_BACKEND=pty opts into the
        // Phase C direct-PTY path (single pane, no tmux daemon). Any
        // other value (or unset) keeps the tmux-bridge path, which is
        // currently the only one with multi-pane support.
        let want_pty = std::env::var("TMUXIFY_BACKEND")
            .map(|v| v.eq_ignore_ascii_case("pty"))
            .unwrap_or(false);
        let backend_result = if want_pty {
            self.start_pty()
        } else {
            self.start_tmux()
        };
        if let Err(e) = backend_result {
            eprintln!("[tmuxify] backend start failed: {e}");
        }
        self.schedule_autosend();
        self.schedule_autocapture();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else { return; };
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                sugarloaf.rescale(scale_factor as f32);
                let size = window.inner_size();
                sugarloaf.resize(size.width, size.height);
                window.request_redraw();
            }
            WindowEvent::Resized(size) => {
                sugarloaf.resize(size.width, size.height);
                let scale = window.scale_factor() as f32;
                let lw = size.width as f32 / scale;
                let lh = size.height as f32 / scale;
                let cols = (lw / self.cell.w).floor().max(40.0) as u16;
                let rows = (lh / self.cell.h).floor().max(10.0) as u16;
                self.resize_backend(cols, rows);
                window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = window.scale_factor() as f32;
                self.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                if let (Some(anchor), Some(cell)) = (
                    self.drag_anchor,
                    self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1),
                ) {
                    self.selection = Some(Selection { anchor, end: cell });
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => {
                        // Switch active pane to wherever the click
                        // landed, then start a drag selection inside
                        // that pane. Clicking the inactive pane focuses
                        // it without yet selecting (anchor == end so
                        // the release handler clears the empty range).
                        if let Some((pane_id, col, row)) =
                            self.px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
                        {
                            let switched = {
                                let mut ws = self.ws.lock().unwrap();
                                let switched =
                                    ws.active_pane.as_deref() != Some(pane_id.as_str());
                                ws.active_pane = Some(pane_id.clone());
                                switched
                            };
                            if switched {
                                // New focus: drop any selection that
                                // was held in the previously-focused
                                // pane, otherwise the highlight rect
                                // would float on the wrong pane.
                                self.selection = None;
                                self.drag_anchor = None;
                            } else {
                                self.drag_anchor = Some((col, row));
                                self.selection = Some(Selection {
                                    anchor: (col, row),
                                    end: (col, row),
                                });
                            }
                            self.last_input_at = Instant::now();
                            // Tmux side: tell the daemon we're now on
                            // this pane so any future split-window /
                            // send-keys without a target lands here.
                            if let Some(tmux) = self.tmux.as_ref() {
                                let _ =
                                    tmux.send_cmd(&format!("select-pane -t '{pane_id}'"));
                            }
                        }
                    }
                    ElementState::Released => {
                        self.drag_anchor = None;
                        if let Some(sel) = self.selection {
                            if sel.anchor == sel.end {
                                self.selection = None;
                            }
                        }
                    }
                }
                window.request_redraw();
            }
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Preedit(text, _range) => {
                        self.in_preedit = !text.is_empty();
                        self.preedit = text;
                    }
                    Ime::Commit(text) => {
                        self.in_preedit = false;
                        self.preedit.clear();
                        self.send_bytes(text.as_bytes());
                    }
                    Ime::Disabled | Ime::Enabled => {
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                }
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.forward_key(&event);
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Re-arm the blink deadline every loop turn so the cursor toggles
        // even on an idle terminal. WaitUntil(deadline) keeps the runtime
        // asleep — no CPU until either the deadline lands or fresh input
        // arrives. The actual redraw happens in new_events on the
        // ResumeTimeReached branch — about_to_wait should NOT request a
        // redraw here, since that would queue work and turn the wait
        // into a busy loop.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
    }

    fn new_events(
        &mut self,
        _event_loop: &ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        // The blink-timer fire path. When winit wakes us because the
        // WaitUntil deadline elapsed (no other events arrived), repaint
        // so the cursor block toggles its phase. Other wake causes
        // (input, redraw, init) drive their own redraws.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    #[test]
    fn wheel_sub_cell_ticks_accumulate() {
        let mut accum = 0.0;
        let mut last = Instant::now();
        let t0 = ms(last, 100);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 0)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 20)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 40)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 60)), Some(1));
    }

    #[test]
    fn wheel_direction_flip_drops_residual() {
        let mut accum = 0.0;
        let mut last = Instant::now();
        let t0 = ms(last, 100);
        wheel_step(&mut accum, 0.6, &mut last, ms(t0, 0));
        let out = wheel_step(&mut accum, -1.0, &mut last, ms(t0, 50));
        assert_eq!(out, Some(-1));
    }

    #[test]
    fn selection_extract_single_row() {
        let mut row = vec![GridCell::blank(); 10];
        for (i, c) in "hello".chars().enumerate() {
            row[i] = GridCell {
                ch: c.to_string(),
                ..GridCell::blank()
            };
        }
        let sel = Selection { anchor: (0, 0), end: (4, 0) };
        let s = extract_selection(&[row], sel);
        assert_eq!(s, "hello");
    }

    #[test]
    fn selection_normalise_reverses_when_needed() {
        let sel = Selection { anchor: (5, 2), end: (1, 0) };
        let (a, b) = normalise(sel);
        assert_eq!(a, (1, 0));
        assert_eq!(b, (5, 2));
    }
}
