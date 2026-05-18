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
use winit::window::{Theme, Window, WindowAttributes, WindowId};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;

const FONT_SIZE: f32 = 14.0;
/// Line spacing multiplier passed to `compute_cell_metrics`. 1.0 keeps
/// rows at font ascent+descent so the cell aspect ratio stays close to
/// 1:2 — that's what makes half-block sprite art (Claude Code's mascot,
/// `▀▄▌▐` characters) read as squares instead of tall rectangles. The
/// earlier 1.3 stretched cells to 1:3 and made the sprite look
/// elongated next to Ghostty / iTerm2.
const LINE_HEIGHT_MULT: f32 = 1.0;
/// Logical-pixel padding between the window edge and the cell grid on
/// every side. Mirrors what Terminal.app / Ghostty give the content so
/// text doesn't jam against the chrome. Must match `render_frame`'s
/// origin and `px_to_pane_cell`'s offset.
const WINDOW_PADDING: f32 = 10.0;
/// Logical-pixel height of the custom chrome strip that sits above the
/// cell grid (traffic light row + future tab bar). macOS's traffic
/// light buttons end around y ≈ 28 in logical units, so 38 leaves a
/// few pixels of breathing room. Cells start at y = TITLE_HEIGHT (not
/// at WINDOW_PADDING) so the title strip is fully clear of glyph
/// drawing.
const TITLE_HEIGHT: f32 = 36.0;
/// Width of the macOS traffic-light cluster (close/min/zoom) measured
/// from the window's left edge. Mouse events inside this rectangle are
/// reserved for the native buttons; our drag handler ignores them so a
/// click on the red dot still closes the window.
const TRAFFIC_LIGHT_WIDTH: f32 = 78.0;
/// iTerm-style per-pane header height in logical pixels. Each split
/// pane gets one of these strips above its cell grid; a single
/// un-split window renders no header at all (matches the iTerm
/// behavior the user pointed at).
const PANE_HEADER_HEIGHT: f32 = 28.0;
/// One tab's slot width in the title strip. Tabs flow left-to-right
/// starting just past the traffic lights.
const TAB_WIDTH: f32 = 180.0;
/// "+" new-tab button width on the right of the tab list.
const TAB_PLUS_WIDTH: f32 = 44.0;
/// Vertical inset for tab rects inside the title strip (top + bottom).
const TAB_VERTICAL_INSET: f32 = 4.0;
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

/// One tab in the title strip. Visual data only — the heavy state
/// (PTY map, workspace, layout) lives in `TabBackend` and gets
/// swapped in/out of the App when the user switches tabs.
struct TabState {
    title: String,
}

/// All per-tab backend state that has to swap in/out when the user
/// changes tabs. The active tab's TabBackend is "live" — its fields
/// are inlined directly on `App` so the existing hot paths
/// (key/mouse handlers, render_frame) keep their cheap field access.
/// Inactive tabs sit in `App::stashed_tabs[idx]`, and the swap is a
/// few `std::mem::replace` calls in `switch_tab`.
struct TabBackend {
    pty: HashMap<String, Arc<pty_backend::PtySession>>,
    pty_layout: Option<pty_backend::PtyLayout>,
    next_pane_id: u32,
    autosplit_plan: Vec<pty_backend::SplitDir>,
    autosplit_at: Option<Instant>,
    dead_panes: Arc<Mutex<Vec<String>>>,
    socket_handle: Option<socket::PtyBackendHandle>,
    ws: Arc<Mutex<Workspace>>,
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
    // Trim blank lines at both ends. A drag across an alt-screen TUI
    // (Claude Code, less, etc.) usually picks up empty padding rows
    // above/below the visible text — strip those so what lands in the
    // clipboard matches what the user actually highlighted.
    let trimmed: Vec<&str> = out
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    trimmed.join("\n")
}

/// True for any codepoint the Hangul IME composes. Covers the four
/// Unicode blocks that hold Korean syllables and jamo. Used to drop
/// keyboard-side Character events when the IME is the authoritative
/// channel — winit emits both `KeyboardInput.text` and `Ime::Preedit`
/// for the first keystroke after a script switch, and forwarding both
/// would echo the jamo twice (or, more often, leak the first 자모 to
/// the shell as raw `ㅎ` / `ㅇ` before the Commit lands).
fn is_hangul_codepoint(c: char) -> bool {
    let cp = c as u32;
    (0x1100..=0x11FF).contains(&cp) // Hangul Jamo
        || (0x3130..=0x318F).contains(&cp) // Hangul Compat Jamo
        || (0xA960..=0xA97F).contains(&cp) // Hangul Jamo Extended-A
        || (0xAC00..=0xD7A3).contains(&cp) // Hangul Syllables
        || (0xD7B0..=0xD7FF).contains(&cp) // Hangul Jamo Extended-B
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
    /// Frame-dirty flag. Set whenever a PTY update lands new bytes,
    /// the user scrolls, or focus switches; cleared after the next
    /// render. When *every* pane is clean and no chrome-level anim
    /// is pending, the render loop skips the GPU pass entirely —
    /// matches Rio's `TerminalDamage::Noop` short-circuit.
    dirty: bool,
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
    /// KASATERM_BACKEND env var; defaults to PTY now that the Phase C
    /// path is the recommended one (no tmux daemon, no focus-events
    /// warnings from Claude Code).
    /// All live PTY sessions, keyed by pane id. Empty when running in
    /// tmux mode. Multi-pane PTY mode inserts one entry per split.
    pty: HashMap<String, Arc<pty_backend::PtySession>>,
    /// BSP layout tree for multi-pane PTY mode. `None` in tmux mode —
    /// the tmux daemon owns the layout there and ships it via
    /// `%layout-change` instead.
    pty_layout: Option<pty_backend::PtyLayout>,
    /// Monotonic counter for the next "%N" pane id when splitting.
    next_pane_id: u32,
    /// Queued split directions driven by KASATERM_AUTOSPLIT — headless
    /// repro for the multi-pane render path. Empty in normal use.
    autosplit_plan: Vec<pty_backend::SplitDir>,
    autosplit_at: Option<Instant>,
    /// Pane ids whose PTY reader thread has disconnected (shell exited
    /// or PTY closed). Drained on the main thread in `about_to_wait`
    /// so the tree mutation runs without holding the workspace lock
    /// across a session drop.
    dead_panes: Arc<Mutex<Vec<String>>>,
    /// Bridge to the cmux-compatible socket worker. The socket thread
    /// pushes commands here; the main thread drains them in
    /// `about_to_wait`. None until `start_socket_pty` wires it up.
    socket_handle: Option<socket::PtyBackendHandle>,
    ws: Arc<Mutex<Workspace>>,
    /// Measured cell geometry from sugarloaf — see `compute_cell_metrics`.
    cell: CellGeom,
    preedit: String,
    in_preedit: bool,
    /// True between `Ime::Enabled` and `Ime::Disabled`. Tracks whether
    /// the OS IME owns this keyboard at all — when active, Hangul (and
    /// other CJK) keystrokes are double-delivered (KeyboardInput.text
    /// + Ime::Preedit/Commit) and we have to drop the keyboard side
    /// even before the first Preedit lands.
    ime_active: bool,
    /// In-process Hangul jamo → syllable composer. We drive this from
    /// the KeyboardInput path whenever the OS keyboard layout hands us
    /// a Hangul jamo — macOS's NSTextInputContext doesn't fire
    /// Ime::Preedit for the *first* keystroke after a script switch
    /// (the jamo arrives only via KeyboardInput.text), so to compose
    /// "ㄱ + ㅏ → 가" reliably from the very first key we route every
    /// jamo through our own Composer instead of trusting macOS to
    /// queue it for us.
    hangul: hangul_ime::Composer,
    /// Title-strip tabs. Always has at least one entry — the initial
    /// session lives in `tabs[0]`. `active_tab` indexes into this.
    tabs: Vec<TabState>,
    active_tab: usize,
    /// Backend state for every tab except the currently active one.
    /// The active tab's backend lives directly on `App`. `Some` for
    /// inactive tabs, `None` at the active index. Length matches
    /// `tabs`.
    stashed_tabs: Vec<Option<TabBackend>>,
    /// Cached hit rects for the tab strip from the last render pass.
    /// Mouse press uses these to decide between "click on tab N",
    /// "click on the + button", or "title-strip drag". Stored as
    /// (x, y, w, h) tuples in logical pixels.
    tab_rects: Vec<(f32, f32, f32, f32)>,
    plus_rect: Option<(f32, f32, f32, f32)>,
    /// (pane_id, close_rect) for every visible pane header. Populated
    /// by `render_frame` and consumed by the MouseInput handler so a
    /// click on the × button closes that pane.
    pane_header_rects: Vec<(String, (f32, f32, f32, f32))>,
    selection: Option<Selection>,
    drag_anchor: Option<(u16, u16)>,
    /// Pane that owns the in-flight mouse reporting drag. `Some(pane_id)`
    /// when we forwarded a button-press into a mouse-reporting TUI and
    /// are now relaying motion + release into the same pane. None means
    /// no mouse-reporting drag is active; selection logic owns the
    /// pointer.
    mouse_forward_pane: Option<String>,
    /// Last left-click timestamp + position. Used only for the
    /// title-strip double-click → window-zoom shortcut. macOS handles
    /// this for us when the OS owns the titlebar, but our
    /// fullsize_content_view setup means we intercept those clicks.
    last_left_click: Option<(Instant, (f32, f32))>,
    /// Cached value of the OS window title — `window.set_title` is
    /// cheap but not free, so we only call it when the resolved
    /// label actually changes.
    last_window_title: Option<String>,
    /// Deadline keeping the Claude "busy" anim alive after the
    /// spinner row briefly disappears from the grid. Without this,
    /// fast redraws toggle between "✱ claude" and the live status
    /// every frame because Claude Code repaints the spinner phase
    /// across separate cells. 800ms of stickiness smooths it out.
    claude_busy_until: Option<Instant>,
    /// Most recent claude status line we lifted from the grid. Kept
    /// so the titlebar stays on the last "✻ Brewed for Ns" frame
    /// while Claude Code is mid-repaint and the marker row briefly
    /// vanishes. Cleared when the busy window expires.
    last_claude_status: Option<String>,
    /// When we last recomputed the macOS window title. Rate-limits
    /// `maybe_update_window_title` to ~200ms because it locks the
    /// workspace + calls `ps -A` (process-tree lookup) on every hit,
    /// and a wheel burst fires `RedrawRequested` 60+ times per
    /// second.
    last_window_title_check: Option<Instant>,
    /// Cursor-blink phase captured at the last successful render.
    /// Used by `render_frame`'s early-return: a blink toggle counts
    /// as "something changed" and forces the GPU pass even when
    /// every pane is clean.
    last_blink_on: bool,
    /// Chrome-level dirty flag. Set on any non-PTY state change that
    /// needs the next frame to repaint (selection, preedit, focus
    /// shifts, resize, mouse hover, etc.). PTY changes set the
    /// per-pane `PaneState::dirty` instead.
    chrome_dirty: bool,
    cursor_px: (f32, f32),
    modifiers: ModifiersState,
    wheel_accum_y: f32,
    last_wheel_emit: Instant,
    /// Last keystroke / IME / mouse press timestamp. Resets the blink
    /// phase so the cursor stays solid while the user is actively
    /// interacting and only fades back to blinking on idle.
    last_input_at: Instant,
    /// Currently-applied logical font size. Mutated by the host_mod+=
    /// / Ctrl+- shortcuts (see `change_font_size`). Starts at the
    /// `FONT_SIZE` constant so first-frame layout matches the original
    /// behavior before any zoom.
    font_size: f32,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            sugarloaf: None,
            tmux: None,
            pty: HashMap::new(),
            pty_layout: None,
            next_pane_id: 1, // %0 is the initial pane created in start_pty
            autosplit_plan: Vec::new(),
            autosplit_at: None,
            dead_panes: Arc::new(Mutex::new(Vec::new())),
            socket_handle: None,
            ws: Arc::new(Mutex::new(Workspace::default())),
            cell: CellGeom::default(),
            preedit: String::new(),
            in_preedit: false,
            ime_active: false,
            hangul: hangul_ime::Composer::new(),
            tabs: vec![TabState { title: "shell".into() }],
            active_tab: 0,
            stashed_tabs: vec![None],
            tab_rects: Vec::new(),
            plus_rect: None,
            pane_header_rects: Vec::new(),
            selection: None,
            drag_anchor: None,
            mouse_forward_pane: None,
            last_left_click: None,
            last_window_title: None,
            claude_busy_until: None,
            last_claude_status: None,
            last_window_title_check: None,
            last_blink_on: false,
            chrome_dirty: true,
            cursor_px: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            wheel_accum_y: 0.0,
            last_wheel_emit: Instant::now() - std::time::Duration::from_secs(1),
            last_input_at: Instant::now(),
            font_size: FONT_SIZE,
        }
    }

    /// Adjust the live font size by `delta` (in logical points) and
    /// reflow the cell grid + PTY size accordingly. Clamped to a sane
    /// terminal range so the user can't shrink past readability or
    /// blow the window contents out by accident.
    fn change_font_size(&mut self, delta: f32) {
        let new = (self.font_size + delta).clamp(8.0, 40.0);
        if (new - self.font_size).abs() < 0.05 {
            return;
        }
        self.font_size = new;
        if let (Some(window), Some(sugarloaf)) = (self.window.as_ref(), self.sugarloaf.as_ref()) {
            let scale = window.scale_factor() as f32;
            let (_dim, metrics) =
                sugarloaf.compute_cell_metrics(new, LINE_HEIGHT_MULT, scale);
            self.cell = CellGeom {
                w: (metrics.cell_width as f32) / scale,
                h: (metrics.cell_height as f32) / scale,
                baseline: 0.0,
            };
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            window.request_redraw();
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

    /// "Host modifier" chord that opens the kasaterm shortcut layer
    /// (split / close / focus / copy-paste). macOS conventions reserve
    /// Cmd for this; Windows and Linux terminals overwhelmingly use
    /// Ctrl+Shift instead so Ctrl+letter stays free to deliver control
    /// bytes to the shell.
    fn host_mod(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key() && self.modifiers.shift_key()
        }
    }

    /// Secondary modifier that flips a host shortcut into its alternate
    /// behavior (e.g. `Cmd+Shift+D` = stacked split on macOS). The host
    /// chord on Windows/Linux already owns Shift, so Alt fills the same
    /// role there.
    fn host_mod_alt(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.shift_key()
        } else {
            self.modifiers.alt_key()
        }
    }

    fn schedule_autocapture(&self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOCAPTURE_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        let path = std::env::var("KASATERM_AUTOCAPTURE_PATH").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("tmuxify.png")
                .to_string_lossy()
                .into_owned()
        });
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
        let Ok(text) = std::env::var("KASATERM_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        // Capture whichever backend is wired so we don't need access
        // to self inside the timer thread.
        let tmux = self.tmux.clone();
        // Autosend always targets the currently-focused pane. In tmux
        // mode we leave pane targeting to the daemon; in pty mode we
        // grab the active session here so the closure doesn't need
        // self access.
        let pty = self.active_pty().cloned();
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

    /// Drain a PtySession's screen-update channel into shared workspace
    /// state. Used both by `start_pty` (initial pane) and by
    /// `split_active_pane` (every additional pane), so the per-pane
    /// state arrives through the same path no matter when the session
    /// was spawned.
    fn pump_pty_screens(
        &self,
        screens: pty_backend::ScreenReceiver<tmux_bridge::screen::ScreenUpdate>,
        pane_id: String,
    ) {
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        let dead = self.dead_panes.clone();
        std::thread::spawn(move || {
            // Coalesce request_redraw calls to ~60 Hz. Claude Code
            // streaming responses can fire hundreds of ScreenUpdates
            // per second, each triggering a redraw — winit can keep
            // up, but the GPU + cell-render path can't, and a wheel
            // event landing mid-burst gets stalled behind queued
            // frames. One redraw per 16ms is plenty for a terminal.
            let mut last_redraw = std::time::Instant::now()
                - std::time::Duration::from_millis(100);
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
                // OSC 0/2 title from the inner program (Claude Code's
                // conversation summary, vim filename, etc.). Carry it
                // through to PaneState so the chrome header + the
                // macOS window title see the freshest value.
                if let Some(t) = update.title.clone() {
                    pane.title = Some(t);
                }
                // Mark this pane dirty so the next render frame
                // actually emits cells. render_frame short-circuits
                // when every pane is clean, which is what makes
                // wheel-scroll feel smooth during Claude Code
                // streaming bursts: the PTY thread keeps pushing
                // updates but the GPU only redraws once per 16ms.
                pane.dirty = true;
                drop(ws);
                let now = std::time::Instant::now();
                if now.duration_since(last_redraw).as_millis() >= 16 {
                    if let Some(w) = win_screens.as_ref() {
                        w.request_redraw();
                    }
                    last_redraw = now;
                }
            }
            // Channel disconnected — the reader thread exited because
            // the PTY hit EOF (shell quit) or errored. Flag this pane
            // for the main thread to remove on its next tick.
            dead.lock().unwrap().push(pane_id);
            if let Some(w) = win_screens.as_ref() {
                w.request_redraw();
            }
        });
    }

    /// Phase C path. Spawns the shell into a direct PTY (no tmux),
    /// hooks the screens channel into the same per-pane state the
    /// renderer expects. Single-pane MVP — the workspace holds one
    /// PaneState keyed "%0" and the layout is `None` (the render path
    /// falls back to single-pane when no layout has arrived).
    fn start_pty(&mut self) -> Result<()> {
        let _window = self.window.as_ref().expect("window before pty");
        let (cols, rows) = self.window_cells();
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        // Export the agent-socket path BEFORE the first PtySession
        // spawn so the initial shell inherits a working
        // KASATERM_SOCKET_PATH. start_socket_pty (called below) binds
        // the actual server at this same path — set_var here just
        // wins the race against PtyBackend's env::var lookup at spawn
        // time.
        let socket_path = resolve_kasaterm_socket_path();
        std::env::set_var("KASATERM_SOCKET_PATH", &socket_path);
        std::env::set_var("CMUX_SOCKET_PATH", &socket_path);
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: "%0".to_string(),
        })?;
        self.pump_pty_screens(session.screens.clone(), "%0".to_string());
        self.pty.insert("%0".to_string(), Arc::new(session));
        self.pty_layout = Some(pty_backend::PtyLayout::single("%0"));
        // Seed active_pane immediately so split / focus shortcuts work
        // before the first ScreenUpdate lands. pump_pty_screens won't
        // overwrite a non-None active_pane.
        self.ws.lock().unwrap().active_pane = Some("%0".to_string());
        // Bring up the cmux-compat socket *after* the initial pane is
        // wired so the very first surface.list call sees %0.
        self.start_socket_pty();
        // No ws.layout update here — a single pane uses the
        // single-pane fallback in the render path, same as tmux mode
        // does before its first %layout-change arrives.
        Ok(())
    }

    /// Headless verification helper. Reads `KASATERM_AUTOSPLIT` ("h" / "v"
    /// / "hv" / "vh" ...) and fires the matching splits from
    /// `about_to_wait` after `KASATERM_AUTOSPLIT_MS` (default 2500ms),
    /// so a background `cargo run` can prove multi-pane rendering
    /// without a human pressing Cmd+D.
    fn run_pending_autosplits(&mut self) {
        if self.autosplit_plan.is_empty() {
            return;
        }
        let now = Instant::now();
        let due = match self.autosplit_at {
            Some(t) => t,
            None => return,
        };
        if now < due {
            return;
        }
        let dir = self.autosplit_plan.remove(0);
        if let Err(e) = self.split_active_pane(dir) {
            eprintln!("[autosplit] split failed: {e}");
        }
        // Chain the next split 500ms later so the renderer has time to
        // settle and a screenshot can capture intermediate states.
        self.autosplit_at = if self.autosplit_plan.is_empty() {
            None
        } else {
            Some(now + std::time::Duration::from_millis(500))
        };
    }

    fn arm_autosplit(&mut self) {
        let Ok(plan) = std::env::var("KASATERM_AUTOSPLIT") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSPLIT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        let dirs: Vec<pty_backend::SplitDir> = plan
            .chars()
            .filter_map(|c| match c {
                'h' | 'H' => Some(pty_backend::SplitDir::Horizontal),
                'v' | 'V' => Some(pty_backend::SplitDir::Vertical),
                _ => None,
            })
            .collect();
        if dirs.is_empty() {
            return;
        }
        eprintln!("[autosplit] armed: {plan:?} in {ms}ms");
        self.autosplit_plan = dirs;
        self.autosplit_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }

    fn start_tmux(&mut self) -> Result<()> {
        let _window = self.window.as_ref().expect("window before tmux");
        let (cols, rows) = self.window_cells();
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("kasaterm"),
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
                            new_title.unwrap_or_else(|| "kasaterm".into());
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
        self.start_socket_tmux(tmux_arc);
        Ok(())
    }

    /// Bring up the cmux-compatible JSON-RPC server so external agents
    /// (Claude Code teammateMode, ad-hoc CLI scripts) can drive this
    /// pane. The server is best-effort — a bind failure logs and the
    /// rest of the binary keeps working without it. Two env names are
    /// exported on the spawned shell:
    ///   - KASATERM_SOCKET_PATH (our brand)
    ///   - CMUX_SOCKET_PATH (so cmux-aware clients auto-detect us)
    /// Both point at the same socket; the second is the cmux-protocol
    /// convention from issue anthropics/claude-code#36926.
    /// Bind the unix socket + export env vars. Common to both backend
    /// modes — the caller decides which concrete `Backend` impl to plug
    /// in (TmuxBackend in tmux mode, PtyBackend in PTY mode).
    fn start_socket_with(&self, backend: Arc<dyn agent_socket::Backend>) {
        let path = resolve_kasaterm_socket_path();
        let server = match agent_socket::Server::bind(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[agent-socket] bind {path:?} failed: {e:#}");
                return;
            }
        };
        let resolved = server.socket_path().to_string_lossy().to_string();
        eprintln!("[agent-socket] listening on {resolved}");
        std::env::set_var("KASATERM_SOCKET_PATH", &resolved);
        std::env::set_var("CMUX_SOCKET_PATH", &resolved);
        let _join = server.spawn(backend);
    }

    fn start_socket_tmux(&self, tmux: Arc<tmux_bridge::TmuxSession>) {
        self.start_socket_with(Arc::new(socket::TmuxBackend::new(tmux)));
    }

    /// PTY-mode socket wiring. Builds the shared inbox + snapshot,
    /// stores the handle on self so the main loop can drain commands,
    /// then spawns the server with a PtyBackend that routes through
    /// that same handle.
    fn start_socket_pty(&mut self) {
        let window = self.window.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(w) = window.as_ref() {
                w.request_redraw();
            }
        });
        let handle = socket::PtyBackendHandle {
            inbox: Arc::new(Mutex::new(Vec::new())),
            snapshot: Arc::new(Mutex::new(socket::PtySnapshot::default())),
            wake,
        };
        self.socket_handle = Some(handle.clone());
        self.refresh_socket_snapshot();
        self.start_socket_with(Arc::new(socket::PtyBackend::new(handle)));
    }

    /// Publish the current pane state to the shared snapshot the
    /// PtyBackend reads from. Call after every mutation that adds /
    /// removes a pane or shifts focus, so external agents see fresh
    /// `surface.list` results on the very next poll.
    fn refresh_socket_snapshot(&self) {
        let Some(handle) = self.socket_handle.as_ref() else { return; };
        let ws = self.ws.lock().unwrap();
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_else(|| self.pty.keys().cloned().collect());
        let surfaces = leaves
            .iter()
            .map(|id| agent_socket::backend::SurfaceInfo {
                id: id.clone(),
                workspace_id: "local-0".to_string(),
                title: ws
                    .panes
                    .get(id)
                    .and_then(|p| p.title.clone()),
            })
            .collect();
        let mut snap = handle.snapshot.lock().unwrap();
        snap.surfaces = surfaces;
        snap.active_pane = ws.active_pane.clone();
    }

    /// Drain pending socket commands and run them on the main thread.
    /// Called once per loop turn from `about_to_wait`.
    fn drain_socket_inbox(&mut self) {
        let cmds: Vec<socket::PtyCommand> = match self.socket_handle.as_ref() {
            Some(h) => std::mem::take(&mut *h.inbox.lock().unwrap()),
            None => return,
        };
        if cmds.is_empty() {
            return;
        }
        for cmd in cmds {
            match cmd {
                socket::PtyCommand::Focus { pane_id, reply } => {
                    let known = self.pty.contains_key(&pane_id);
                    if known {
                        self.ws.lock().unwrap().active_pane = Some(pane_id);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "no such surface: {pane_id}"
                        )));
                    }
                }
                socket::PtyCommand::Split { axis, reply } => {
                    let dir = match axis {
                        socket::PtySplitAxis::Horizontal => pty_backend::SplitDir::Horizontal,
                        socket::PtySplitAxis::Vertical => pty_backend::SplitDir::Vertical,
                    };
                    let split_res = self.split_active_pane(dir);
                    let answer = split_res.map(|_| {
                        // split_active_pane sets active_pane to the new
                        // leaf; that's the id the client wants back.
                        self.ws
                            .lock()
                            .unwrap()
                            .active_pane
                            .clone()
                            .unwrap_or_default()
                    });
                    let _ = reply.send(answer);
                }
                socket::PtyCommand::SendBytes { pane_id, bytes, reply } => {
                    let target = pane_id.or_else(|| {
                        self.ws.lock().unwrap().active_pane.clone()
                    });
                    let res = match target.and_then(|id| self.pty.get(&id).cloned()) {
                        Some(pty) => pty.send_bytes(&bytes).map_err(anyhow::Error::from),
                        None => Err(anyhow::anyhow!("no surface to send to")),
                    };
                    let _ = reply.send(res);
                }
            }
        }
        self.refresh_socket_snapshot();
    }

    /// Convert logical-pixel position into a (pane_id, col, row) cell
    /// inside the pane the click landed in. Multi-pane aware: walks the
    /// parsed Layout to find the pane whose rect contains the click,
    /// then translates the pixel into that pane's cell-local coords.
    /// Returns None when the workspace has no panes or the click missed
    /// every pane (gutter between split borders, padding, etc).
    fn px_to_pane_cell(&self, px: f32, py: f32) -> Option<(String, u16, u16)> {
        let ws = self.ws.lock().unwrap();
        // The WINDOW_PADDING offset must match render_frame's origin —
        // both the grid and the click reuse it so the math stays
        // consistent on the boundary cells.
        let col = ((px - WINDOW_PADDING).max(0.0) / self.cell.w).floor() as u16;
        let row = ((py - TITLE_HEIGHT).max(0.0) / self.cell.h).floor() as u16;
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

    /// The PtySession that currently has keyboard focus, if any. Used
    /// by every routing-by-active-pane code path in PTY mode.
    fn active_pty(&self) -> Option<&Arc<pty_backend::PtySession>> {
        let id = self.ws.lock().unwrap().active_pane.clone()?;
        self.pty.get(&id)
    }

    /// Window size in cell coordinates. Source of truth for resize
    /// distribution + new-pane sizing. The grid lives inside
    /// `WINDOW_PADDING` on every side, so subtract 2× padding from the
    /// logical viewport before dividing — otherwise we tell the PTY it
    /// has N rows but only N-1 fit before clipping, and the last row
    /// (where most TUIs paint their statusline) gets cut in half.
    /// Falls back to (80, 24) when the window isn't ready yet.
    fn window_cells(&self) -> (u16, u16) {
        let Some(window) = self.window.as_ref() else {
            return (80, 24);
        };
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let raw_lw = size.width as f32 / scale;
        let raw_lh = size.height as f32 / scale;
        let lw = (raw_lw - 2.0 * WINDOW_PADDING).max(0.0);
        // Top: TITLE_HEIGHT (chrome strip). Bottom: WINDOW_PADDING. The
        // asymmetry is intentional — the strip replaces the top padding.
        let lh = (raw_lh - TITLE_HEIGHT - WINDOW_PADDING).max(0.0);
        let cols = (lw / self.cell.w).floor().max(40.0) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        if std::env::var_os("KASATERM_LOG_LAYOUT").is_some() {
            eprintln!(
                "[layout] win=({raw_lw:.0}x{raw_lh:.0}) usable=({lw:.0}x{lh:.0}) cell=({:.1}x{:.1}) cells=({cols}x{rows})",
                self.cell.w, self.cell.h
            );
        }
        (cols, rows)
    }

    /// Push the current PtyLayout into `ws.layout` so the renderer
    /// (which only knows the tmux Layout shape) picks up the splits.
    /// A single-leaf tree leaves `ws.layout` empty — the render path's
    /// single-pane fallback handles that case.
    fn publish_pty_layout(&self) {
        if let Some(tree) = self.pty_layout.as_ref() {
            let (cols, rows) = self.window_cells();
            let mut ws = self.ws.lock().unwrap();
            if tree.leaves().len() <= 1 {
                ws.layout = None;
            } else {
                ws.layout = Some(tree.to_tmux_layout(cols, rows));
            }
        }
        // Keep the socket snapshot in lockstep with the renderer view —
        // every code path that adds/removes panes or moves focus goes
        // through publish_pty_layout, so this is the one spot we have
        // to wire the cmux mirror.
        self.refresh_socket_snapshot();
    }

    /// Resize every backend session so its grid matches the new window
    /// size. In tmux mode the daemon redistributes for us. In PTY mode
    /// we walk the BSP tree and SIGWINCH each leaf to its own rect.
    fn resize_backend(&self, cols: u16, rows: u16) {
        if let Some(tmux) = self.tmux.as_ref() {
            let _ = tmux.resize_client(cols, rows);
            return;
        }
        let Some(tree) = self.pty_layout.as_ref() else { return; };
        // When the workspace is split, every pane wears a per-pane
        // header strip. That strip eats a few cell rows at the top of
        // each pane's bounding box, so the PTY's usable grid shrinks
        // by the same amount — otherwise claude code paints its
        // statusline / `bypass…` row off the bottom edge.
        let leaves = tree.leaves().len();
        let header_cells: u16 = if leaves > 1 {
            ((PANE_HEADER_HEIGHT / self.cell.h.max(1.0)).ceil() as u16).max(1)
        } else {
            0
        };
        for (id, _x, _y, w, h) in tree.leaf_rects(cols, rows) {
            if let Some(sess) = self.pty.get(&id) {
                let _ = sess.resize(w, h.saturating_sub(header_cells).max(1));
            }
        }
        // Re-publish the layout because rect proportions may have
        // shifted (rounding) and the renderer caches the previous tree.
        self.publish_pty_layout();
    }

    /// Split the focused pane in PTY mode. Spawns a new shell into a
    /// fresh PTY, inserts it into the BSP tree on the right (Horizontal)
    /// or bottom (Vertical) of the focused leaf, then resizes every
    /// session so each one matches its new rect. Becomes a no-op in
    /// tmux mode — splits there go through the cmux socket / tmux
    /// `split-window` instead.
    fn split_active_pane(&mut self, dir: pty_backend::SplitDir) -> Result<()> {
        if self.tmux.is_some() {
            return Ok(());
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return Ok(());
        };
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;

        // Spawn the new session at a placeholder size — the resize
        // pass right after `split_leaf` puts every leaf at its real
        // rect, so the initial cols/rows here only matters for the
        // first bytes the shell prints before SIGWINCH lands.
        let (win_cols, win_rows) = self.window_cells();
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols: win_cols,
            rows: win_rows,
            env: Vec::new(),
            pane_id: new_id.clone(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_id.clone());
        self.pty.insert(new_id.clone(), Arc::new(session));

        let layout = self.pty_layout.as_mut().expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, dir, new_id.clone()) {
            // Active pane isn't in the tree — shouldn't happen, but
            // bail without leaking the spawned session entry.
            self.pty.remove(&new_id);
            self.next_pane_id -= 1;
            return Ok(());
        }
        self.ws.lock().unwrap().active_pane = Some(new_id);
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }

    /// Drain `dead_panes` and remove each from the BSP tree + pty map.
    /// Called on the main thread from `about_to_wait` so the mutation
    /// runs without competing with the per-session reader threads.
    /// If removing all panes empties the tree, exit the event loop.
    fn reap_dead_panes(&mut self, event_loop: &ActiveEventLoop) {
        let ids: Vec<String> = std::mem::take(&mut *self.dead_panes.lock().unwrap());
        if ids.is_empty() {
            return;
        }
        for id in ids {
            if !self.pty.contains_key(&id) {
                continue;
            }
            self.remove_pane(&id);
        }
        // Last pane closed (e.g. user typed `exit` in the only shell):
        // shut the window so tmuxify exits cleanly the way users
        // expect from a regular terminal.
        if self.tmux.is_none() && self.pty.is_empty() {
            event_loop.exit();
        }
    }

    /// Internal: drop a pane regardless of whether it's the active one.
    /// Used by both `close_active_pane` (Cmd+W) and `reap_dead_panes`
    /// (shell exit). Picks a survivor focus when removing the focused
    /// pane.
    fn remove_pane(&mut self, target: &str) {
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => return,
        };
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            if cur_idx + 1 < leaves.len() {
                Some(leaves[cur_idx + 1].clone())
            } else {
                Some(leaves[cur_idx - 1].clone())
            }
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            // Last leaf — drop the tree entirely so single-pane
            // fallback re-engages if a future split repopulates it.
            self.pty_layout = None;
        }
        self.pty.remove(target);
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            if was_active {
                ws.active_pane = next_focus;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Remove the focused pane from the BSP tree and drop its PTY
    /// session. Focus moves to the next pane in document order
    /// (wrapping to the previous when we just closed the last one).
    /// Last-pane close is a no-op — quitting the window is the
    /// user's exit there.
    fn close_active_pane(&mut self) {
        if self.tmux.is_some() {
            return;
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return;
        };
        // Last-pane Cmd+W is a no-op — the OS close button is how
        // users quit a single-pane window. The shell-exit path takes
        // care of the cascade close when the last shell `exit`s.
        let leaves = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().len(),
            None => 0,
        };
        if leaves <= 1 {
            return;
        }
        self.remove_pane(&active);
    }

    /// Cycle focus to the previous (delta=-1) or next (delta=+1) pane
    /// in document order. No-op when there's only one pane.
    fn cycle_focus(&self, delta: i32) {
        let Some(tree) = self.pty_layout.as_ref() else { return; };
        let leaves: Vec<String> = tree.leaves().iter().map(|s| s.to_string()).collect();
        if leaves.len() < 2 {
            return;
        }
        let mut ws = self.ws.lock().unwrap();
        let cur_idx = ws
            .active_pane
            .as_deref()
            .and_then(|id| leaves.iter().position(|l| l == id))
            .unwrap_or(0);
        let n = leaves.len() as i32;
        let new_idx = ((cur_idx as i32 + delta).rem_euclid(n)) as usize;
        ws.active_pane = Some(leaves[new_idx].clone());
        drop(ws);
        self.refresh_socket_snapshot();
        if let Some(w) = &self.window {
            w.request_redraw();
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
        } else if let Some(pty) = self.active_pty() {
            let _ = pty.send_bytes(bytes);
        }
    }

    /// Snapshot the active tab's backend out of `App` into a
    /// `TabBackend`. Used by `switch_tab` and `add_tab` to stash the
    /// current state before swapping in another.
    fn take_active_backend(&mut self) -> TabBackend {
        TabBackend {
            pty: std::mem::take(&mut self.pty),
            pty_layout: self.pty_layout.take(),
            next_pane_id: std::mem::replace(&mut self.next_pane_id, 1),
            autosplit_plan: std::mem::take(&mut self.autosplit_plan),
            autosplit_at: self.autosplit_at.take(),
            dead_panes: std::mem::replace(
                &mut self.dead_panes,
                Arc::new(Mutex::new(Vec::new())),
            ),
            socket_handle: self.socket_handle.take(),
            ws: std::mem::replace(
                &mut self.ws,
                Arc::new(Mutex::new(Workspace::default())),
            ),
        }
    }

    /// Restore a stashed `TabBackend` into the active App fields.
    fn install_backend(&mut self, b: TabBackend) {
        self.pty = b.pty;
        self.pty_layout = b.pty_layout;
        self.next_pane_id = b.next_pane_id;
        self.autosplit_plan = b.autosplit_plan;
        self.autosplit_at = b.autosplit_at;
        self.dead_panes = b.dead_panes;
        self.socket_handle = b.socket_handle;
        self.ws = b.ws;
    }

    /// Switch to a different tab by index. No-op when `idx` is already
    /// active or out of range. The PTYs of the stashed tab keep
    /// running in the background; their %output just doesn't paint
    /// until the user switches back.
    fn switch_tab(&mut self, idx: usize) {
        if idx == self.active_tab || idx >= self.tabs.len() {
            return;
        }
        let current = self.take_active_backend();
        self.stashed_tabs[self.active_tab] = Some(current);
        let next = self
            .stashed_tabs[idx]
            .take()
            .expect("inactive tab must have a stashed backend");
        self.install_backend(next);
        self.active_tab = idx;
        self.selection = None;
        self.drag_anchor = None;
        self.mouse_forward_pane = None;
        // The new tab's PTY size is whatever it was last; resize it to
        // the current window so reads pick up the right COLUMNS/LINES.
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Push a brand-new tab and switch to it. The new tab spawns a
    /// fresh PTY immediately so the user sees a shell prompt without
    /// having to do anything else.
    fn add_tab(&mut self) {
        let stash = self.take_active_backend();
        self.stashed_tabs[self.active_tab] = Some(stash);
        let n = self.tabs.len() + 1;
        self.tabs.push(TabState { title: format!("tab {n}") });
        self.stashed_tabs.push(None);
        self.active_tab = self.tabs.len() - 1;
        self.selection = None;
        self.drag_anchor = None;
        self.mouse_forward_pane = None;
        // start_pty fills self.pty / self.ws / self.pty_layout from
        // the empty active state we just installed.
        if let Err(e) = self.start_pty() {
            eprintln!("[kasaterm] new tab pty failed: {e}");
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// True when the pane has mouse reporting + SGR encoding enabled
    /// (claude code / vim / less in alt-screen). Shift-held overrides
    /// to false so the user has an iTerm-style escape hatch back to
    /// our own selection logic.
    fn pane_takes_mouse(&self, pane_id: &str) -> bool {
        if self.modifiers.shift_key() {
            return false;
        }
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .map(|p| p.mouse_enabled && p.mouse_sgr)
            .unwrap_or(false)
    }

    /// Encode an SGR mouse event and ship it to the pane. `button` is
    /// the SGR button code (0 = left press/motion/release, +32 for
    /// motion-with-button-held). `press` toggles the final byte
    /// between `M` (press / motion) and `m` (release).
    fn send_mouse_sgr(&self, pane_id: &str, button: u8, col: u16, row: u16, press: bool) {
        let final_byte = if press { 'M' } else { 'm' };
        let payload = format!("\x1b[<{button};{};{}{final_byte}", col + 1, row + 1);
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = payload
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = tmux.send_keys_hex(Some(pane_id), &hex);
        } else if let Some(pty) = self.pty.get(pane_id) {
            let _ = pty.send_bytes(payload.as_bytes());
        }
    }

    /// Resolve the user-visible label for a pane: OSC 0/2 title set
    /// by the shell or a TUI → foreground process comm → cwd. Used by
    /// both the per-pane header strip and the single-pane native
    /// window title so the two stay consistent.
    /// Scan the bottom of the active pane's grid for a Braille
    /// spinner glyph (U+2800..U+28FF). Tools like Claude Code, oh-my-
    /// zsh's pure-prompt, npm, etc. paint these one cell at a time
    /// to animate progress — picking the glyph straight from the
    /// grid lets us mirror their phase in the window title without
    /// any extra timing math. Returns None when no spinner is
    /// currently visible.
    /// Pull Claude Code's progress line ("✻ Brewed for 5s",
    /// "✶ Thinking…", etc.) straight out of the cell grid. We scan
    /// the bottom of the active pane for a row that starts with a
    /// star/asterisk glyph and trim that row to its text. The
    /// rendered grid is the only signal Claude Code gives us — it
    /// doesn't push these as OSC titles — so this is how we mirror
    /// the live status into the macOS titlebar.
    fn active_claude_status(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let rows = pane.cells.len();
        let start = rows.saturating_sub(10);
        for row in pane.cells[start..].iter() {
            let mut text = String::new();
            let mut has_marker = false;
            for cell in row {
                if cell.ch.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(&cell.ch);
                    if let Some(c) = cell.ch.chars().next() {
                        let cp = c as u32;
                        if (0x2731..=0x274F).contains(&cp) {
                            has_marker = true;
                        }
                    }
                }
            }
            if has_marker {
                let trimmed = text.trim();
                if trimmed.len() > 4 && trimmed.len() < 80 {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    fn active_spinner_glyph(&self) -> Option<char> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let rows = pane.cells.len();
        let start = rows.saturating_sub(8);
        for row in &pane.cells[start..] {
            for cell in row {
                if let Some(c) = cell.ch.chars().next() {
                    let cp = c as u32;
                    // Braille spinners (npm, pure-prompt, etc.) +
                    // Dingbats asterisks/stars (Claude Code uses
                    // ✻/✶/✷/✸/✹/✺ as its "thinking" indicator).
                    if (0x2800..=0x28FF).contains(&cp)
                        || (0x2731..=0x274F).contains(&cp)
                    {
                        return Some(c);
                    }
                }
            }
        }
        None
    }

    /// Sync the macOS window title (Dock label, ⌘-Tab switcher) with
    /// the active pane's resolved label when only one pane is open.
    /// Skipped for multi-pane workspaces — there the per-pane header
    /// strip carries the same information and the OS title gets
    /// noisy as the user shuffles focus between splits.
    fn maybe_update_window_title(&mut self) {
        // Throttle: scroll bursts can fire RedrawRequested 60+ times
        // per second, and every call here takes a workspace lock and
        // may shell out to `ps` for the process name. 200ms is fast
        // enough for "title follows focus" but cheap enough that a
        // wheel sweep stays smooth.
        let now = Instant::now();
        if let Some(t) = self.last_window_title_check {
            if now.duration_since(t).as_millis() < 200 {
                return;
            }
        }
        self.last_window_title_check = Some(now);
        // Native window title always tracks the focused pane. In a
        // split workspace this means macOS's Dock / ⌘-Tab label
        // updates when the user clicks a different split — matching
        // iTerm / Terminal.app.
        let active = {
            let ws = self.ws.lock().unwrap();
            let id = ws
                .active_pane
                .clone()
                .or_else(|| ws.panes.keys().next().cloned());
            let osc = id.as_ref().and_then(|i| ws.panes.get(i)).and_then(|p| p.title.clone());
            id.map(|i| (i, osc))
        };
        let Some((id, osc)) = active else { return };
        let mut label = Self::resolve_pane_label(&self.pty, &id, osc.as_deref());
        // Claude Code response indicator. Priority:
        //   1. Lift Claude Code's own status line straight from the
        //      cell grid ("✻ Brewed for 5s") so the user sees the
        //      same words they would on the prompt — including the
        //      live elapsed time.
        //   2. Fallback when only a spinner glyph is detected but no
        //      status text: cycle our own asterisk sequence
        //      ✶ ✳ ✶ · ✽ next to the "claude" label.
        // Note: we intentionally do NOT scrape the grid for the
        // "✻ Brewed for Ns" status line here. iTerm-style behavior is
        // to let the inner program drive the title via OSC 0/2 only
        // — Claude Code sends the conversation summary that way, and
        // the per-question status is meant to stay inside the pane.
        // Scraping the grid would clobber the conversation title
        // every few hundred ms with whatever Claude was rendering.
        if self.last_window_title.as_deref() == Some(&label) {
            return;
        }
        if let Some(w) = self.window.as_ref() {
            w.set_title(&label);
        }
        self.last_window_title = Some(label);
    }

    fn resolve_pane_label(
        pty: &HashMap<String, Arc<pty_backend::PtySession>>,
        pane_id: &str,
        osc_title: Option<&str>,
    ) -> String {
        if let Some(t) = osc_title.filter(|s| !s.is_empty()) {
            return t.to_string();
        }
        if let Some(name) = pty.get(pane_id).and_then(|p| p.active_process_name()) {
            return decorate_process_name(&name);
        }
        std::env::current_dir()
            .ok()
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                match std::env::var("HOME").ok() {
                    Some(home) if s.starts_with(&home) => s.replacen(&home, "~", 1),
                    _ => s,
                }
            })
            .unwrap_or_else(|| "shell".to_string())
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
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(pty) = self.pty.get(id) {
                    let _ = pty.send_bytes(&payload);
                }
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
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(pty) = self.pty.get(id) {
                    let _ = pty.send_bytes(esc);
                }
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
                pane.dirty = true;
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
        // Modifier-bearing keys must NEVER reach the Hangul composer.
        // In Korean keyboard layout the C key still produces 'ㅊ' as
        // text, but Ctrl+C is meant for SIGINT / "copy" and Cmd+V for
        // paste — we look at the *physical* key for these, not the
        // IME-resolved logical key. While we're here, also forward the
        // standard control-letter byte for any Ctrl+<letter> combo
        // (Ctrl+L clears, Ctrl+D = EOF, etc), so shells and TUI apps
        // behave as users expect regardless of which keyboard layout
        // happens to be active.
        let host = self.host_mod();
        let ctrl = self.modifiers.control_key();
        if host || ctrl {
            use winit::keyboard::{KeyCode, PhysicalKey};
            if let PhysicalKey::Code(code) = event.physical_key {
                // Host-modifier shortcuts. macOS uses Cmd, Windows/Linux
                // use Ctrl+Shift — see `host_mod()`.
                if host {
                    if code == KeyCode::KeyC && self.selection.is_some() {
                        self.copy_selection();
                        return;
                    }
                    if code == KeyCode::KeyV {
                        self.paste_clipboard();
                        return;
                    }
                    // Terminal.app-style split shortcuts. PTY mode only
                    // — tmux mode lets the daemon handle its own keys.
                    //   D       → horizontal (stacked, default)
                    //   Shift+D → vertical (side-by-side, macOS chord)
                    //   E       → vertical (Windows-friendly chord that
                    //              avoids the Shift-on-Shift conflict)
                    // On macOS host_mod_alt resolves to Shift so
                    // Cmd+Shift+D still flips to vertical. On
                    // Windows/Linux host_mod already owns Shift, so the
                    // dedicated KeyE binding is the practical one.
                    if code == KeyCode::KeyD {
                        let dir = if self.host_mod_alt() {
                            pty_backend::SplitDir::Vertical
                        } else {
                            pty_backend::SplitDir::Horizontal
                        };
                        if let Err(e) = self.split_active_pane(dir) {
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    if code == KeyCode::KeyE {
                        if let Err(e) = self.split_active_pane(pty_backend::SplitDir::Vertical) {
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    // Close the focused pane. Last-pane close is left
                    // to the OS close button.
                    if code == KeyCode::KeyW {
                        self.close_active_pane();
                        return;
                    }
                    // `[` / `]` cycle focus through panes in document
                    // order.
                    if code == KeyCode::BracketLeft {
                        self.cycle_focus(-1);
                        return;
                    }
                    if code == KeyCode::BracketRight {
                        self.cycle_focus(1);
                        return;
                    }
                    // Font zoom: host_mod + = (or Shift = +) increases,
                    // host_mod + - (or Shift = _) decreases. `0` resets
                    // to the default. Layout the same as VS Code,
                    // Windows Terminal, and most browsers.
                    if code == KeyCode::Equal || code == KeyCode::NumpadAdd {
                        self.change_font_size(1.0);
                        return;
                    }
                    if code == KeyCode::Minus || code == KeyCode::NumpadSubtract {
                        self.change_font_size(-1.0);
                        return;
                    }
                    if code == KeyCode::Digit0 || code == KeyCode::Numpad0 {
                        self.change_font_size(FONT_SIZE - self.font_size);
                        return;
                    }
                }
                // Ctrl+letter → the corresponding ASCII control byte.
                // This covers Ctrl+C → 0x03 (SIGINT), Ctrl+D → 0x04 (EOF),
                // Ctrl+L → 0x0c (clear), Ctrl+R → 0x12 (reverse search), etc.
                // Suppressed when host is engaged so Ctrl+Shift+letter
                // shortcuts on Windows/Linux don't double-fire as both a
                // shortcut and a control byte.
                if ctrl && !host {
                    let letter = match code {
                        KeyCode::KeyA => Some(b'\x01'),
                        KeyCode::KeyB => Some(b'\x02'),
                        KeyCode::KeyC => Some(b'\x03'),
                        KeyCode::KeyD => Some(b'\x04'),
                        KeyCode::KeyE => Some(b'\x05'),
                        KeyCode::KeyF => Some(b'\x06'),
                        KeyCode::KeyG => Some(b'\x07'),
                        KeyCode::KeyH => Some(b'\x08'),
                        KeyCode::KeyI => Some(b'\x09'),
                        KeyCode::KeyJ => Some(b'\x0a'),
                        KeyCode::KeyK => Some(b'\x0b'),
                        KeyCode::KeyL => Some(b'\x0c'),
                        KeyCode::KeyM => Some(b'\x0d'),
                        KeyCode::KeyN => Some(b'\x0e'),
                        KeyCode::KeyO => Some(b'\x0f'),
                        KeyCode::KeyP => Some(b'\x10'),
                        KeyCode::KeyQ => Some(b'\x11'),
                        KeyCode::KeyR => Some(b'\x12'),
                        KeyCode::KeyS => Some(b'\x13'),
                        KeyCode::KeyT => Some(b'\x14'),
                        KeyCode::KeyU => Some(b'\x15'),
                        KeyCode::KeyV => Some(b'\x16'),
                        KeyCode::KeyW => Some(b'\x17'),
                        KeyCode::KeyX => Some(b'\x18'),
                        KeyCode::KeyY => Some(b'\x19'),
                        KeyCode::KeyZ => Some(b'\x1a'),
                        _ => None,
                    };
                    if let Some(b) = letter {
                        // Flush any pending Hangul syllable before
                        // sending the control byte — typing Enter
                        // mid-syllable already does this; control
                        // letters should too.
                        if let Some(flushed) = self.hangul.flush() {
                            self.send_bytes(flushed.as_bytes());
                            self.preedit.clear();
                            self.in_preedit = false;
                        }
                        self.send_bytes(&[b]);
                        return;
                    }
                }
            }
        }
        // Backspace special: when the in-process Hangul composer is
        // mid-syllable, eat the backspace to chip a jamo off the
        // preedit rather than forwarding `\x7f` to the shell (which
        // would erase already-committed text instead).
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.hangul.backspace() {
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.in_preedit = !self.preedit.is_empty();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }
        // Any non-character control flushes the composer first so a
        // pending 자모 doesn't get stranded when the user hits Enter /
        // arrow / escape mid-syllable.
        let is_control_key = matches!(
            event.logical_key,
            Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::Tab)
                | Key::Named(NamedKey::Escape)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::ArrowDown)
                | Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowRight)
        );
        if is_control_key {
            if let Some(flushed) = self.hangul.flush() {
                self.send_bytes(flushed.as_bytes());
            }
            self.preedit.clear();
            self.in_preedit = false;
        }
        // Readline-style delete shortcuts. Defaults match iTerm2 /
        // Terminal.app on macOS and Windows Terminal on Windows:
        //   Option/Alt+Backspace → `\e\x7f`  (backward-kill-word)
        //   host_mod+Backspace   → `\x15`    (unix-line-discard, Ctrl+U)
        // host_mod resolves to Cmd on macOS, Ctrl+Shift on Windows/Linux.
        // We match physical key so the Korean layout's mapped char
        // ('ㅣ' etc.) doesn't interfere.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.host_mod() {
                self.send_bytes(b"\x15");
                return;
            }
            if self.modifiers.alt_key() {
                self.send_bytes(b"\x1b\x7f");
                return;
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
                    if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                        eprintln!(
                            "[key] text={t:?} logical_key={:?} ime_active={} in_preedit={}",
                            event.logical_key, self.ime_active, self.in_preedit
                        );
                    }
                    // Hangul branch (macOS only). On macOS our
                    // set_ime_allowed(false) means the Korean keyboard
                    // layout hands jamo codepoints straight through on
                    // KeyboardInput.text; we feed them into the local
                    // Composer to dodge the NSTextInputContext first-
                    // key drop. Windows / Linux let the OS IME handle
                    // the whole composition (Ime::Preedit/Commit), so
                    // we skip this branch and forward whatever text the
                    // keyboard layer produced as-is.
                    #[cfg(target_os = "macos")]
                    if t.chars().count() == 1 {
                        if let Some(c) = t.chars().next() {
                            if (0x3130..=0x318F).contains(&(c as u32)) {
                                if let Some(commit) = self.hangul.feed(c) {
                                    self.send_bytes(commit.as_bytes());
                                }
                                self.preedit = self.hangul.preedit().unwrap_or_default();
                                self.in_preedit = !self.preedit.is_empty();
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    // Non-Hangul ASCII / control characters: flush any
                    // pending Hangul syllable to the shell first, then
                    // forward the new character verbatim.
                    if !t.chars().all(|c| c.is_ascii() && !c.is_control()) {
                        if (self.ime_active || self.in_preedit)
                            && t.chars().any(is_hangul_codepoint)
                        {
                            return;
                        }
                    }
                    if let Some(flushed) = self.hangul.flush() {
                        self.send_bytes(flushed.as_bytes());
                        self.preedit.clear();
                        self.in_preedit = false;
                    }
                    t.as_bytes().to_vec()
                }
                None => return,
            },
        };
        self.send_bytes(&bytes);
    }

    fn render_frame(&mut self) {
        let t0 = Instant::now();
        let trace = std::env::var_os("KASATERM_PROFILE").is_some();
        let now = Instant::now();
        let blink_on = self.cursor_blink_on(now);
        // Damage gate: skip the GPU pass when nothing changed since
        // the last frame. winit keeps showing the previous swapchain
        // image, so the user sees the same picture without us
        // emitting 10k+ sugarloaf calls. PTY updates flag the per-
        // pane dirty bit; chrome events flag `self.chrome_dirty`;
        // cursor blink phase toggles count separately.
        let blink_changed = blink_on != self.last_blink_on;
        let pty_dirty = self.ws.lock().unwrap().panes.values().any(|p| p.dirty);
        if !pty_dirty && !self.chrome_dirty && !blink_changed {
            return;
        }
        self.last_blink_on = blink_on;
        let Some(window) = self.window.as_ref() else { return; };
        let scale = window.scale_factor() as f32;
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        let size = window.inner_size();
        let win_w = size.width as f32 / scale;
        let win_h = size.height as f32 / scale;
        sugarloaf.rect(
            None,
            0.0,
            0.0,
            win_w,
            win_h,
            [
                cells::DEFAULT_BG[0] as f32 / 255.0,
                cells::DEFAULT_BG[1] as f32 / 255.0,
                cells::DEFAULT_BG[2] as f32 / 255.0,
                1.0,
            ],
            0.0,
            0,
        );
        // Tab strip rendering. iTerm-style: single-tab session shows
        // *no* strip — only the traffic-light cluster sits in the
        // unified-titlebar area, and the cell grid uses the full
        // height. The strip appears only when there are 2+ tabs (or
        // when the user has explicitly added a second). Drag /
        // double-click / drag-resize keep working in the traffic-light
        // band height regardless.
        let show_strip = self.tabs.len() > 1;
        self.tab_rects.clear();
        self.plus_rect = None;
        if show_strip {
            // Sugarloaf takes logical pixels — match cells.rs which
            // already passes logical origins. Earlier code multiplied
            // by `scale` and produced double-sized chrome that
            // overlapped the cell grid.
            sugarloaf.rect(
                None,
                0.0,
                0.0,
                win_w,
                TITLE_HEIGHT,
                [0.09, 0.10, 0.13, 1.0],
                0.0,
                0,
            );
            sugarloaf.rect(
                None,
                0.0,
                TITLE_HEIGHT - 1.0,
                win_w,
                1.0,
                [0.04, 0.05, 0.07, 1.0],
                0.0,
                0,
            );
            let inset_y = TAB_VERTICAL_INSET;
            let tab_h = TITLE_HEIGHT - inset_y * 2.0;
            for (i, tab) in self.tabs.iter().enumerate() {
                let x = TRAFFIC_LIGHT_WIDTH + i as f32 * TAB_WIDTH;
                let rect = (x, inset_y, TAB_WIDTH - 4.0, tab_h);
                let color = if i == self.active_tab {
                    [0.20, 0.45, 0.70, 1.0]
                } else {
                    [0.14, 0.16, 0.20, 1.0]
                };
                sugarloaf.rect(None, rect.0, rect.1, rect.2, rect.3, color, 6.0, 0);
                sugarloaf.text_mut().draw(
                    rect.0 + 12.0,
                    rect.1 + tab_h - 4.0,
                    &tab.title,
                    &sugarloaf::text::DrawOpts {
                        font_size: self.font_size * 0.5,
                        color: if i == self.active_tab {
                            [245, 247, 250, 255]
                        } else {
                            [180, 185, 192, 255]
                        },
                        bold: i == self.active_tab,
                        italic: false,
                        font_id: None,
                    },
                );
                self.tab_rects.push(rect);
            }
            let plus_x = TRAFFIC_LIGHT_WIDTH + self.tabs.len() as f32 * TAB_WIDTH;
            let pr = (plus_x, inset_y, TAB_PLUS_WIDTH, tab_h);
            sugarloaf.rect(None, pr.0, pr.1, pr.2, pr.3, [0.14, 0.16, 0.20, 1.0], 6.0, 0);
            sugarloaf.text_mut().draw(
                pr.0 + 12.0,
                pr.1 + tab_h - 4.0,
                "+",
                &sugarloaf::text::DrawOpts {
                    font_size: self.font_size * 0.6,
                    color: [220, 225, 230, 255],
                    bold: true,
                    italic: false,
                    font_id: None,
                },
            );
            self.plus_rect = Some(pr);
        }

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
            title: Option<String>,
        }
        // Hold the workspace lock across the entire render so we can
        // `mem::take` each pane's cell grid into the PaneFrame
        // without the PTY thread observing an empty Vec. The lock
        // is released at the end of the function, after we restore
        // the grids — sugarloaf.render() inside the held region
        // pauses the PTY pump for one frame, well below the 16 ms
        // budget.
        let mut ws_guard = self.ws.lock().unwrap();
        let (pane_frames, active_id) = {
            let ws = &mut *ws_guard;
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
                // Pre-fetch pane metadata under an immutable borrow,
                // then drop it so we can take a mutable borrow to
                // move the cell grid out without cloning. The pump
                // thread can't observe the gap because it would need
                // the same ws lock we already hold.
                let (total, offset, cursor_row, cursor_col, cursor_visible, title) = {
                    let Some(pane) = ws.panes.get(&id) else { continue };
                    (
                        pane.rows.max(1) as usize,
                        pane.scroll_offset.min(pane.history.len()),
                        pane.cursor_row,
                        pane.cursor_col,
                        pane.cursor_visible,
                        pane.title.clone(),
                    )
                };
                let composed: Vec<Vec<GridCell>> = if offset == 0 {
                    // Hot path: move (not clone) the live grid out
                    // for rendering. ~10 000 GridCells used to be
                    // cloned every frame; now it's a Vec pointer
                    // swap. The grid goes back into the PaneState
                    // after the for-loop (`mem::swap` below).
                    std::mem::take(&mut ws.panes.get_mut(&id).unwrap().cells)
                } else {
                    let pane = ws.panes.get(&id).unwrap();
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
                    cursor_row,
                    cursor_col,
                    cursor_visible: offset == 0 && cursor_visible,
                    title,
                });
            }
            (frames, active_id)
        };

        if pane_frames.is_empty() {
            sugarloaf.render();
            return;
        }

        // Resolve every pane's display label up front so the header
        // rendering loop doesn't need to call back into `self`
        // (resolve_pane_label borrows self.pty, which conflicts with
        // the long-lived sugarloaf mutable borrow).
        let pane_labels: Vec<String> = pane_frames
            .iter()
            .map(|f| Self::resolve_pane_label(&self.pty, &f.id, f.title.as_deref()))
            .collect();

        // Origin offset: TITLE_HEIGHT replaces the top padding so the
        // cell grid starts immediately below the custom chrome strip.
        // Add a small breathing margin so the first text row never
        // bleeds into the strip rect on systems where sugarloaf
        // interprets these coordinates slightly differently.
        let origin_x = WINDOW_PADDING;
        let origin_y = TITLE_HEIGHT + 6.0;

        // Pass 1: walk each pane and render its cell grid at its rect.
        let log_layout = std::env::var_os("KASATERM_LOG_LAYOUT").is_some();
        let show_headers = pane_frames.len() > 1;
        let header_shift = if show_headers { PANE_HEADER_HEIGHT } else { 0.0 };
        for frame in &pane_frames {
            let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
            let pane_px_y =
                origin_y + frame.y_cells as f32 * self.cell.h + header_shift;
            if log_layout {
                let total = frame.rows.len();
                eprintln!(
                    "[render] pane={} rows={total} cols={} px=({pane_px_x:.0},{pane_px_y:.0})",
                    frame.id,
                    frame.rows.first().map(|r| r.len()).unwrap_or(0),
                );
                for (i, row) in frame.rows.iter().enumerate().rev().take(8) {
                    let preview: String = row
                        .iter()
                        .take(80)
                        .map(|c| match c.ch.chars().next() {
                            Some(ch) if !ch.is_whitespace() => ch,
                            _ => '.',
                        })
                        .collect();
                    let nonblank = row
                        .iter()
                        .filter(|c| !c.ch.is_empty() && c.ch != " ")
                        .count();
                    eprintln!("[render]   row[{i:>2}] non={nonblank:>3} {preview}");
                }
            }
            cells::render_screen(
                sugarloaf,
                &frame.rows,
                pane_px_x,
                pane_px_y,
                self.cell.w,
                self.cell.h,
                self.font_size,
                self.cell.baseline,
            );
        }

        // Pass 2: per-pane iTerm-style header bar. Only when the
        // workspace is actually split — a single pane stays
        // header-less so the first session reads as a plain terminal.
        // The header sits *above* the cell grid (cell origin already
        // shifted by `header_shift` in Pass 1), so painting here
        // covers the gap between the pane top and the first text row.
        if show_headers {
            self.pane_header_rects = Vec::with_capacity(pane_frames.len());
            for (idx, frame) in pane_frames.iter().enumerate() {
                let is_active = active_id.as_deref() == Some(frame.id.as_str());
                let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
                let pane_top = origin_y + frame.y_cells as f32 * self.cell.h;
                let pane_px_w = frame.w_cells as f32 * self.cell.w;
                let bg = if is_active {
                    [0.16, 0.20, 0.28, 1.0]
                } else {
                    [0.10, 0.12, 0.15, 1.0]
                };
                sugarloaf.rect(
                    None,
                    pane_px_x,
                    pane_top,
                    pane_px_w,
                    PANE_HEADER_HEIGHT,
                    bg,
                    0.0,
                    0,
                );
                // Hairline at the bottom of the header so it reads as
                // a separate band from the cell grid.
                sugarloaf.rect(
                    None,
                    pane_px_x,
                    pane_top + PANE_HEADER_HEIGHT - 1.0,
                    pane_px_w,
                    1.0,
                    [0.04, 0.05, 0.07, 1.0],
                    0.0,
                    0,
                );
                // Close button + title share the same font size and
                // y baseline so they read as one row of chrome
                // controls. Sugarloaf draws text from the bitmap
                // top-left; we anchor it 8px below the header top so
                // a ~0.85× cell-height glyph sits visually centered
                // in the 28px strip.
                // Match font size between close glyph and title so
                // their bitmap tops sit on the same y. Centering math:
                // a `chrome_font` glyph is ~chrome_font * 1.0 logical
                // tall in this font, so the vertical inset that
                // visually centers it in PANE_HEADER_HEIGHT is
                // (PANE_HEADER_HEIGHT - chrome_font) / 2.
                let chrome_font = 14.0;
                let text_y = pane_top + (PANE_HEADER_HEIGHT - chrome_font) / 2.0;
                let close_size = chrome_font + 4.0;
                let close_rect = (
                    pane_px_x + 6.0,
                    pane_top + (PANE_HEADER_HEIGHT - close_size) / 2.0,
                    close_size,
                    close_size,
                );
                let chrome_color: [u8; 4] = if is_active {
                    [230, 232, 238, 255]
                } else {
                    [160, 165, 172, 255]
                };
                sugarloaf.text_mut().draw(
                    close_rect.0,
                    text_y,
                    "x",
                    &sugarloaf::text::DrawOpts {
                        font_size: chrome_font,
                        color: chrome_color,
                        bold: false,
                        italic: false,
                        font_id: None,
                    },
                );
                let title = pane_labels[idx].clone();
                sugarloaf.text_mut().draw(
                    close_rect.0 + close_rect.2 + 8.0,
                    text_y,
                    &title,
                    &sugarloaf::text::DrawOpts {
                        font_size: chrome_font,
                        color: chrome_color,
                        bold: is_active,
                        italic: false,
                        font_id: None,
                    },
                );
                self.pane_header_rects.push((frame.id.clone(), close_rect));
            }
        } else {
            self.pane_header_rects.clear();
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
            let pane_px_y =
                origin_y + frame.y_cells as f32 * self.cell.h + header_shift;
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
            // Skip the cursor rect while preedit is active — the
            // preedit overlay below paints its own opaque background +
            // accent underline, which would be hidden underneath the
            // translucent cursor and produce the "한글 합치는 중에
            // 안 보이는" symptom the user reported.
            if frame.cursor_visible && blink_on && self.preedit.is_empty() {
                let cursor_x = pane_px_x + frame.cursor_col as f32 * self.cell.w;
                let cursor_y = pane_px_y + frame.cursor_row as f32 * self.cell.h;
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
            // Preedit must render regardless of `cursor_visible` —
            // alt-screen TUIs (Claude Code, vim, lazygit, htop) hide
            // the terminal cursor with `\e[?25l` while they draw their
            // own input chrome. Gating on cursor_visible there caused
            // the in-progress Hangul to disappear entirely. The
            // reported cursor row/col still points at the active
            // input position, so use it unconditionally.
            if !self.preedit.is_empty() {
                // First-keystroke quirk: a TUI like Claude Code parks
                // its cursor on a statusline row (\effort, etc.) while
                // it waits for input, so the cursor_row/col we snapshot
                // points at that statusline instead of the prompt row.
                // The first OS IME Preedit then lands beside `/effort`
                // off in the right margin.
                //
                // Scan bottom-up for a row that starts with one of the
                // common prompt sigils (`>`, `›`, `$`, `#`) and rebind
                // the preedit anchor to the end of *that* row. Falls
                // back to the reported cursor when no prompt-shaped
                // row is found so non-TUI shells (cmd.exe, plain
                // bash) keep their existing behavior.
                let (anchor_row, anchor_col) = find_prompt_anchor(&frame.rows)
                    .unwrap_or((frame.cursor_row, frame.cursor_col));
                let px = pane_px_x + anchor_col as f32 * self.cell.w;
                let py = pane_px_y + anchor_row as f32 * self.cell.h;
                if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                    eprintln!(
                        "[preedit] text={:?} cursor=(row={}, col={}) anchor=(row={anchor_row}, col={anchor_col}) px=({px:.1},{py:.1}) cell=({:.1}x{:.1})",
                        self.preedit, frame.cursor_row, frame.cursor_col, self.cell.w, self.cell.h
                    );
                }
                cells::render_preedit(
                    sugarloaf,
                    &self.preedit,
                    px,
                    py,
                    self.cell.w,
                    self.cell.h,
                    self.font_size,
                    cells::ITERM_CURSOR,
                    self.cell.baseline,
                );
            }
        }

        // Overlay re-pass dropped: with the coordinate-unit fix above
        // (logical pixels everywhere), the strip already paints in the
        // right place on the first pass and doesn't need an overdraw
        // to mask cell-grid bleed.

        let t_emit = t0.elapsed().as_micros();
        let t_emit = t0.elapsed().as_micros();
        sugarloaf.render();
        if trace {
            let t_total = t0.elapsed().as_micros();
            eprintln!(
                "[render] emit={t_emit}us render={t_present}us total={t_total}us frames={n}",
                t_present = t_total - t_emit,
                n = pane_frames.len(),
            );
        }
        // Move the cell grids back and clear damage flags under the
        // same lock we held throughout the render.
        for frame in pane_frames {
            if let Some(pane) = ws_guard.panes.get_mut(&frame.id) {
                if pane.cells.is_empty() {
                    pane.cells = frame.rows;
                }
            }
        }
        for pane in ws_guard.panes.values_mut() {
            pane.dirty = false;
        }
        drop(ws_guard);
        self.chrome_dirty = false;
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
            .with_title("kasaterm")
            // Force dark appearance so the system titlebar paints its
            // text in light gray. Default is "follow OS", which would
            // give black text on our dark content view and make the
            // process-name label nearly invisible in light mode.
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(1100.0, 860.0));
        // Custom chrome: traffic-light row sits inside the content view
        // so we can paint tabs and drag handles right next to the
        // native buttons. OS still owns the traffic lights themselves
        // and the resize edges — we only paint and route drag from the
        // strip above the cell grid.
        #[cfg(target_os = "macos")]
        let attrs = attrs
            .with_titlebar_transparent(true)
            .with_title_hidden(false)
            .with_fullsize_content_view(true);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create window"),
        );
        // Without IME enabled, Hangul / kana would arrive as raw key
        // events instead of composing into 안 / 한 / 글.
        // We compose Hangul ourselves via the in-process hangul-ime
        // Composer, so the OS IME stays out of the way. Leaving the
        // platform IME on means macOS fires its own Preedit one key
        // late (the very first jamo after a script switch comes only
        // through KeyboardInput), which produced the "조합이 첫 글자만
        // 안 돼" symptom. With the platform IME disabled we still
        // receive the Hangul jamo on KeyboardInput.text because the
        // selected keyboard layout produces them — we just take the
        // composition into our own hands from there.
        // IME ownership splits per-platform:
        //   macOS: NSTextInputContext drops the first jamo after a
        //     script switch (only KeyboardInput.text fires), so we
        //     refuse OS IME and run hangul-ime/Composer ourselves.
        //   Windows / Linux: the OS IME is the only path that gets us
        //     completed Hangul syllables — set_ime_allowed(true) so
        //     Ime::Preedit / Ime::Commit drive composition.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
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
        // D2CodingLigature Nerd Font Mono — the macOS profile font,
        // ships with the full Hangul / Latin / Nerd-icon glyph
        // coverage we want on Windows too. dhnam/d2coding-nerd-font
        // hosts the patched TTFs; install them into
        // %LOCALAPPDATA%\Microsoft\Windows\Fonts and sugarloaf picks
        // up the face by name. Falls back to sugarloaf's bundled
        // Cascadia when the face isn't installed.
        fonts.family = Some("D2CodingLigature Nerd Font Mono".to_string());
        // Symbols-Only Nerd Font as an extra fallback so users who
        // ship only the small Symbols variant still get the PUA
        // icons claude code's statusline expects. The primary
        // D2CodingLigature Nerd Font Mono already has these, so this
        // is a safety net rather than the main path.
        //
        // Segoe UI Symbol carries U+23F5 ⏵ (the chevron in front of
        // bypass-permissions) — no Nerd Font ships that glyph. cells.rs
        // already breaks the run-batch on the U+2300–U+27BF range so
        // the proportional glyph gets its own draw call instead of
        // dragging neighbour ASCII through propo advances.
        fonts.symbol_map = Some(vec![
            sugarloaf::font::fonts::SymbolMap {
                start: "2300".to_string(),
                end: "23FF".to_string(),
                font_family: "Segoe UI Symbol".to_string(),
            },
            sugarloaf::font::fonts::SymbolMap {
                start: "E000".to_string(),
                end: "F8FF".to_string(),
                font_family: "Symbols Nerd Font Mono".to_string(),
            },
            sugarloaf::font::fonts::SymbolMap {
                start: "F0000".to_string(),
                end: "1FFFD".to_string(),
                font_family: "Symbols Nerd Font Mono".to_string(),
            },
        ]);
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
        // sugarloaf's `text.draw(x, y, ...)` treats `y` as the
        // **text bounding box top-left**, not the baseline (see
        // sugarloaf::components::text::TextInstance docs: bearings
        // shift down to the bitmap top from `pos`). Passing row_top
        // directly is enough — the per-glyph bearings already place
        // the bitmap at the right vertical offset inside the cell.
        // Stored as 0 so cells::render_screen / render_preedit's
        // `y = origin_y + baseline_offset` formula collapses to
        // `y = origin_y`.
        self.cell = CellGeom {
            w: (metrics.cell_width as f32) / scale,
            h: (metrics.cell_height as f32) / scale,
            baseline: 0.0,
        };
        eprintln!(
            "[startup] cell_geom w={:.2} h={:.2} baseline={:.2} (scale={scale})",
            self.cell.w, self.cell.h, self.cell.baseline
        );
        self.sugarloaf = Some(sugarloaf);
        self.window = Some(window);
        // Backend selection. Defaults to the Phase C direct-PTY path —
        // no tmux daemon, no `set -g focus-events` warnings inside
        // Claude Code, no kasaterm-cli's tmux quirks. KASATERM_BACKEND=tmux
        // opts back into the tmux-bridge multiplexer when the user wants
        // the multi-pane layout features that the in-process pty
        // multiplexer doesn't have yet.
        let want_tmux = std::env::var("KASATERM_BACKEND")
            .map(|v| v.eq_ignore_ascii_case("tmux"))
            .unwrap_or(false);
        let backend_result = if want_tmux {
            self.start_tmux()
        } else {
            self.start_pty()
        };
        if let Err(e) = backend_result {
            eprintln!("[tmuxify] backend start failed: {e}");
        }
        self.schedule_autosend();
        self.schedule_autocapture();
        self.arm_autosplit();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else { return; };
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        // Any winit event that *isn't* RedrawRequested counts as a
        // chrome change for the damage gate. RedrawRequested itself
        // never sets the flag — otherwise the early-return at the
        // top of render_frame could never short-circuit a pure-PTY
        // burst.
        if !matches!(event, WindowEvent::RedrawRequested) {
            self.chrome_dirty = true;
        }
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
                // window_cells() already subtracts WINDOW_PADDING on
                // both sides — using inline raw math here told the PTY
                // there were 2 more rows than we actually paint, and
                // the last two lines (Claude Code's `bypass…` row)
                // landed past our grid bottom.
                let (cols, rows) = self.window_cells();
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
                // Drag inside a mouse-reporting TUI: relay motion as
                // SGR button-32 (left button held) into the same pane
                // we sent the press to, so Claude Code / vim / less
                // sees a continuous drag.
                if let Some(pane_id) = self.mouse_forward_pane.clone() {
                    if let Some((col, row)) =
                        self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                    {
                        self.send_mouse_sgr(&pane_id, 32, col, row, true);
                    }
                } else if let (Some(anchor), Some(cell)) = (
                    self.drag_anchor,
                    self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1),
                ) {
                    self.selection = Some(Selection { anchor, end: cell });
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // Pane header × close button. Catches clicks anywhere
                // in the multi-pane workspace before we drop into the
                // cell-grid click path.
                if matches!(state, ElementState::Pressed) {
                    let cx = self.cursor_px.0;
                    let cy = self.cursor_px.1;
                    let hit = self
                        .pane_header_rects
                        .iter()
                        .find(|(_, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
                        .map(|(id, _)| id.clone());
                    if let Some(id) = hit {
                        if let Some(pty) = self.pty.get(&id) {
                            let _ = pty.send_bytes(b"exit\n");
                        }
                        return;
                    }
                }
                // Custom chrome: tab clicks → switch active tab;
                // "+" click → spawn a new tab (UI only for now);
                // otherwise title-strip clicks past the traffic
                // lights start a window drag. macOS owns the
                // close/min/zoom buttons so clicks in their reserved
                // x range fall through to the native handlers.
                if matches!(state, ElementState::Pressed)
                    && self.cursor_px.1 < TITLE_HEIGHT
                    && self.cursor_px.0 >= TRAFFIC_LIGHT_WIDTH
                {
                    let (cx, cy) = self.cursor_px;
                    let in_rect = |r: &(f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    if let Some(idx) =
                        self.tab_rects.iter().position(in_rect)
                    {
                        self.switch_tab(idx);
                        return;
                    }
                    if let Some(pr) = self.plus_rect {
                        if in_rect(&pr) {
                            self.add_tab();
                            return;
                        }
                    }
                    // Double-click on the title strip → toggle window
                    // zoom (macOS native behavior we lost when we
                    // turned on fullsize_content_view).
                    let now = Instant::now();
                    let is_double = match self.last_left_click {
                        Some((t, (x, y)))
                            if now.duration_since(t).as_millis() < 400
                                && (x - cx).abs() < 5.0
                                && (y - cy).abs() < 5.0 =>
                        {
                            true
                        }
                        _ => false,
                    };
                    self.last_left_click = Some((now, (cx, cy)));
                    if is_double {
                        window.set_maximized(!window.is_maximized());
                        self.last_left_click = None;
                        return;
                    }
                    let _ = window.drag_window();
                    return;
                }
                match state {
                    ElementState::Pressed => {
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
                                self.selection = None;
                                self.drag_anchor = None;
                                self.mouse_forward_pane = None;
                            } else if self.pane_takes_mouse(&pane_id) {
                                // Hand the press to the TUI. Its own
                                // selection / copy-on-select kicks in
                                // (Claude Code spawns `pbcopy`).
                                self.selection = None;
                                self.drag_anchor = None;
                                self.send_mouse_sgr(&pane_id, 0, col, row, true);
                                self.mouse_forward_pane = Some(pane_id.clone());
                            } else {
                                self.drag_anchor = Some((col, row));
                                self.selection = Some(Selection {
                                    anchor: (col, row),
                                    end: (col, row),
                                });
                            }
                            self.last_input_at = Instant::now();
                            if let Some(tmux) = self.tmux.as_ref() {
                                let _ =
                                    tmux.send_cmd(&format!("select-pane -t '{pane_id}'"));
                            }
                        }
                    }
                    ElementState::Released => {
                        // Mouse-reporting drag end: forward the release
                        // so the TUI can finalize its selection /
                        // copy-on-select.
                        if let Some(pane_id) = self.mouse_forward_pane.take() {
                            if let Some((col, row)) =
                                self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                            {
                                self.send_mouse_sgr(&pane_id, 0, col, row, false);
                            }
                        } else {
                            self.drag_anchor = None;
                            if let Some(sel) = self.selection {
                                if sel.anchor == sel.end {
                                    self.selection = None;
                                } else {
                                    self.copy_selection();
                                }
                            }
                        }
                    }
                }
                window.request_redraw();
            }
            WindowEvent::Ime(ime) => {
                if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                    eprintln!("[ime] event={ime:?}");
                }
                match ime {
                    Ime::Enabled => {
                        // OS IME just took ownership of the keyboard
                        // (script switch / app focus). Mark active so
                        // the KeyboardInput branch drops any echo of
                        // text the IME will deliver via Preedit/Commit.
                        self.ime_active = true;
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                    Ime::Disabled => {
                        self.ime_active = false;
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                    Ime::Preedit(text, _range) => {
                        // Receiving a Preedit implies the IME is
                        // active — winit doesn't always emit Enabled
                        // first on macOS, so we set both flags here.
                        self.ime_active = true;
                        self.in_preedit = !text.is_empty();
                        self.preedit = text;
                    }
                    Ime::Commit(text) => {
                        self.in_preedit = false;
                        self.preedit.clear();
                        self.send_bytes(text.as_bytes());
                    }
                }
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.forward_key(&event);
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
                self.maybe_update_window_title();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Reap dead pty sessions before anything else — a closed shell
        // should disappear from the layout on the very next loop turn
        // so the user sees the gap collapse immediately.
        self.reap_dead_panes(event_loop);
        // Drain socket commands from external cmux clients. These run
        // through the same split/focus/send paths Cmd+D etc use, so
        // visible behavior is identical regardless of whether the
        // trigger came from a keystroke or a JSON-RPC call.
        self.drain_socket_inbox();
        // Fire any due KASATERM_AUTOSPLIT step before parking. No-op
        // when no plan is queued.
        self.run_pending_autosplits();
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
    // Wire up the tmux shim before anything spawns a shell — every
    // PtySession reads the env vars we set here. install_tmux_shim is
    // best-effort: a missing shim binary just logs and skips, the rest
    // of the binary still works (tmux calls inside the PTY fall back to
    // the real tmux on the user's PATH).
    install_tmux_shim();
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Find the bundled tmux shim binary and stage it in a private dir we
/// prepend to child shells' PATH. Also fakes `$TMUX` so a TUI that
/// checks "am I inside tmux?" answers yes, which makes Claude Code's
/// teammateMode route through `tmux split-window` etc — landing every
/// call on our shim instead of going down its own path-finding logic.
fn install_tmux_shim() {
    let shim_src = locate_shim_binary();
    let Some(shim_src) = shim_src else {
        eprintln!("[shim] tmux shim binary not found near {:?}; skipping", std::env::current_exe().ok());
        return;
    };
    let shim_dir = std::env::temp_dir().join(format!("kasaterm-shim-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&shim_dir) {
        eprintln!("[shim] mkdir {shim_dir:?} failed: {e}");
        return;
    }
    let shim_target_name = if cfg!(windows) { "tmux.exe" } else { "tmux" };
    let target = shim_dir.join(shim_target_name);
    let _ = std::fs::remove_file(&target);
    // Symlink first so we don't pay for a copy each launch and so
    // updates to the shim binary propagate without a reinstall. On
    // Windows symlinks need Developer Mode or admin — fall back to a
    // plain copy so we always end up with a usable shim binary.
    if let Err(e) = stage_shim(&shim_src, &target) {
        eprintln!("[shim] stage {shim_src:?} -> {target:?} failed: {e}");
        return;
    }
    // Cross-pane RPC: stage cmux-compat next to the tmux shim so it is
    // discoverable on the child shell's PATH. A pane can then run
    // `cmux-compat send --surface %1 "..."` to drive a sibling pane
    // without needing to know the absolute target/debug path. Failure
    // is non-fatal — the shim already works without it.
    if let Some(cmux_src) = locate_cmux_compat_binary() {
        let cmux_name = if cfg!(windows) {
            "cmux-compat.exe"
        } else {
            "cmux-compat"
        };
        let cmux_target = shim_dir.join(cmux_name);
        let _ = std::fs::remove_file(&cmux_target);
        if let Err(e) = stage_shim(&cmux_src, &cmux_target) {
            eprintln!("[shim] stage cmux-compat {cmux_src:?} -> {cmux_target:?} failed: {e}");
        }
    }
    let trace = std::env::var("KASATERM_TMUX_TRACE").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("kasaterm-tmux-calls.log")
            .to_string_lossy()
            .into_owned()
    });
    let real = std::env::var("KASATERM_REAL_TMUX").unwrap_or_else(|_| {
        real_tmux_candidates()
            .iter()
            .copied()
            .find(|p| std::path::Path::new(p).is_file())
            .unwrap_or("")
            .to_string()
    });
    let fake_tmux = format!(
        "{},{},0",
        std::env::temp_dir()
            .join(format!("kasaterm-tmux-{}.sock", std::process::id()))
            .display(),
        std::process::id()
    );
    std::env::set_var("KASATERM_TMUX_SHIM_DIR", &shim_dir);
    std::env::set_var("KASATERM_TMUX_SHIM_TMUX", &fake_tmux);
    std::env::set_var("KASATERM_TMUX_TRACE", &trace);
    if !real.is_empty() {
        std::env::set_var("KASATERM_REAL_TMUX", &real);
    }
    eprintln!(
        "[shim] dir={shim_dir:?} trace={trace} real_tmux={real:?} fake_tmux={fake_tmux}"
    );
}

/// Look for the `tmux` shim binary next to our own executable. Covers
/// both the dev case (target/debug/tmux sibling to target/debug/kasaterm)
/// and the .app bundle case (Contents/MacOS/tmux sibling to kasaterm).
fn locate_shim_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_TMUX_SHIM_BIN") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // Match the bare binary on Unix and the .exe-suffixed binary that
    // cargo produces on Windows.
    let candidates = if cfg!(windows) {
        ["tmux.exe", "tmux"]
    } else {
        ["tmux", "tmux"]
    };
    for name in candidates {
        let c = dir.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    // dev fallback: target/debug/tmux when running via `cargo run`
    // from somewhere odd. current_exe usually already points there
    // but be defensive.
    None
}

/// Find a prompt-shaped row in the visible grid and return
/// `(row, col_just_past_last_text)` for it. Used as the preedit anchor
/// when the snapshot cursor is parked on an unrelated statusline (a
/// Claude Code / alt-screen TUI quirk on first-keystroke IME activation
/// — the IME fires Preedit before any output moves the cursor back to
/// the prompt row, so we can't trust cursor_row/col yet).
fn find_prompt_anchor(rows: &[Vec<GridCell>]) -> Option<(u16, u16)> {
    // Bottom-up: claude code, helix, and most REPLs put the prompt
    // on the last few visible rows. We stop at the first match so a
    // history line that happens to start with `>` doesn't outrank the
    // live prompt.
    for (r, row) in rows.iter().enumerate().rev() {
        let first_non_blank = row.iter().position(|c| !c.ch.is_empty() && c.ch != " ");
        let Some(i) = first_non_blank else { continue };
        let ch = row[i].ch.as_str();
        let is_prompt = matches!(ch, ">" | "›" | "❯" | "$" | "#" | "λ");
        if !is_prompt {
            continue;
        }
        // Width-aware last-filled tracker. alacritty marks wide-char
        // right halves with an empty `ch`, so a naive `rposition`
        // returns the wide LEFT cell and the preedit anchor lands one
        // cell short — the box visually clips into the syllable next
        // to it. Bump by 2 for any CJK-range first codepoint, by 1
        // otherwise. Falls back to the column right after the prompt
        // sigil when the prompt is empty.
        let last_advance = row
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.ch.is_empty() && c.ch != " ")
            .last()
            .map(|(idx, c)| {
                let wide = c
                    .ch
                    .chars()
                    .next()
                    .is_some_and(|ch| (ch as u32) >= 0x1100);
                (idx + if wide { 2 } else { 1 }) as u16
            })
            .unwrap_or(0);
        let col = last_advance.max((i as u16) + 2);
        return Some((r as u16, col));
    }
    None
}

/// Pick the shell to spawn inside a PTY. claude code's teammate mode
/// emits Unix-quoted commands (`cd 'path' && env VAR=val cmd`), so a
/// cmd.exe default leaves teammate spawns dead on arrival. Honor
/// KASATERM_SHELL / SHELL when set, otherwise auto-discover Git for
/// Windows' bash so users with a stock setup get a working unix-style
/// shell without configuration. Returns None to let portable-pty's
/// `new_default_prog` pick (cmd.exe on Windows, $SHELL on Unix).
/// Prefix well-known interactive programs with a small sigil so the
/// pane header reads at a glance. Mirrors how the programs themselves
/// brand their own OSC titles in other terminals (Claude Code ships
/// "✱ Claude Code", vim/less label themselves with their name). For
/// anything we don't have an opinion on, just return the comm as-is.
fn decorate_process_name(comm: &str) -> String {
    match comm {
        "claude" => "✱ claude".to_string(),
        "node" | "deno" | "bun" => format!("⬢ {comm}"),
        "vim" | "nvim" => format!("⌨ {comm}"),
        "less" | "more" => format!("☰ {comm}"),
        "git" => format!("⎇ {comm}"),
        _ => comm.to_string(),
    }
}

fn resolve_default_shell() -> Option<String> {
    if let Ok(s) = std::env::var("KASATERM_SHELL") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    #[cfg(windows)]
    {
        for candidate in &[
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            if std::path::Path::new(candidate).is_file() {
                return Some((*candidate).to_string());
            }
        }
    }
    None
}

/// Decide where the agent-socket should live. Honors caller-supplied
/// overrides first (`KASATERM_SOCKET_PATH`, then the cmux convention),
/// and falls back to a per-pid socket under the system temp dir. Used
/// in two places — the early env-var seed in `start_pty` so the very
/// first shell sees a stable value, and the actual server bind in
/// `start_socket_with` — and must return the same path in both.
fn resolve_kasaterm_socket_path() -> String {
    std::env::var("KASATERM_SOCKET_PATH")
        .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
        .unwrap_or_else(|_| {
            format!(
                "{}/kasaterm-{}.sock",
                std::env::temp_dir().to_string_lossy(),
                std::process::id()
            )
        })
}

/// Locate the cmux-compat binary so we can stage it alongside the
/// tmux shim. Same lookup pattern as `locate_shim_binary` — env
/// override first, then sibling of the current exe.
fn locate_cmux_compat_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_CMUX_COMPAT_BIN") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["cmux-compat.exe", "cmux-compat"] {
        let c = dir.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// Place the shim binary at `target` so child shells can find it.
/// Symlink first, fall back to a plain copy when the platform refuses
/// (Windows without Developer Mode or admin will reject CreateSymbolicLink).
fn stage_shim(src: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, target)
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(src, target) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Symlink path failed (likely a non-admin, non-dev-mode
                // user). Copy the bytes so we still end up with a
                // working tmux.exe in the shim dir.
                std::fs::copy(src, target).map(|_| ())
            }
        }
    }
}

/// Common install locations for the real tmux binary. Used when no
/// `KASATERM_REAL_TMUX` env override is provided.
#[cfg(unix)]
fn real_tmux_candidates() -> &'static [&'static str] {
    &["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"]
}

#[cfg(windows)]
fn real_tmux_candidates() -> &'static [&'static str] {
    // Windows has no canonical tmux install path; rely on the env
    // override when the user has wired up WSL-tmux or a custom build.
    &[]
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
