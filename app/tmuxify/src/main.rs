//! tmuxify — single-session terminal in OS-window-as-session model.
//!
//! Mapping:
//!   session  = this OS window (one tmuxify process, one tmux session)
//!   window   = top tab inside this OS window
//!   pane     = floating box inside the active tab

mod quad;
mod render;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

use hangul_ime::{dubeolsik, Composer};
use tmux_bridge::{
    parse_layout, Cell, Layout, ScreenUpdate, StartOptions, TmuxEvent, TmuxSession,
};

use render::Renderer;

const POLL_INTERVAL: Duration = Duration::from_millis(16);
const RESIZE_HANDLE: f32 = 14.0;
// Mirror of render::TITLE_HEIGHT — hit_test uses these for the pane's
// title-bar strip (drag handle + × close button). If render bumps the
// title height, this must move with it or the close button drops out
// of the hit zone.
const TITLE_BAR: f32 = 52.0;

#[derive(Debug, Default, Clone)]
pub struct PaneGrid {
    pub rows: u16,
    pub cols: u16,
    /// Per-row cells; preserves fg/bg/attrs for the renderer.
    pub cells: Vec<Vec<Cell>>,
    pub cursor_row: u16,
    pub cursor_col: u16,
}

/// One floating box = one tmux pane.
#[derive(Debug, Clone)]
pub struct FloatingPane {
    pub pane_id: String,
    pub title: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One top-bar tab = one tmux window. Holds its own pane positions so
/// switching tabs restores layout immediately.
#[derive(Debug, Clone, Default)]
pub struct WindowTab {
    pub title: String,
    pub layout: Option<Layout>,
    pub active_pane: Option<String>,
    pub floating: BTreeMap<String, FloatingPane>,
    pub next_cascade: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneEdge {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl PaneEdge {
    fn affects_left(self) -> bool {
        matches!(self, Self::W | Self::NW | Self::SW)
    }
    fn affects_right(self) -> bool {
        matches!(self, Self::E | Self::NE | Self::SE)
    }
    fn affects_top(self) -> bool {
        matches!(self, Self::N | Self::NE | Self::NW)
    }
    fn affects_bottom(self) -> bool {
        matches!(self, Self::S | Self::SE | Self::SW)
    }
}

#[derive(Debug)]
enum HitTarget {
    Title(String),
    Resize(String, PaneEdge),
    Body(String),
    TabClose(String),
    TaskbarPane(String),
    Tab(String),
    NewTab,
    Min,
    MaxToggle,
    Close,
    Drag,
    ToggleSidebar,
    Session(u8),
    NewSession,
    Icon(usize),
    PaneClose(String),
    SessionClose(u8),
    SidebarResize,
    WindowEdgeResize(ResizeDirection),
}

#[derive(Debug, Clone)]
pub struct DesktopIcon {
    pub label: String,
    pub kind: IconKind,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub enum IconKind {
    /// Folder shortcut. Double-click opens a new window with this cwd.
    Folder { cwd: String },
    /// Claude shortcut. Double-click opens a new window and auto-runs
    /// `claude` so it streams claude code directly into a fresh pane.
    Claude { cwd: String },
}

#[derive(Debug, Clone)]
enum DragState {
    Move {
        pane_id: String,
        offset_x: f32,
        offset_y: f32,
    },
    Resize {
        pane_id: String,
        edge: PaneEdge,
        start_x: f32,
        start_y: f32,
        start_w: f32,
        start_h: f32,
        start_mouse: (f32, f32),
    },
    SidebarResize,
    /// User is mouse-dragging to extend a text selection in a pane body.
    Select { pane_id: String },
    WindowMove {
        /// Cursor position inside the window at drag start (px, py).
        /// We keep the cursor anchored to this point as we move the
        /// window — that's how Windows / macOS native drag feels.
        anchor: (f32, f32),
    },
}

/// Aero-Snap target for the outer (OS) window — set during a window
/// drag when the cursor is within `edge` px of the monitor border.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SnapZone {
    Left,
    Right,
    Top,
}

/// One sidebar entry = one tmux session = its own tmux -C subprocess.
pub struct SessionState {
    pub number: u8,
    pub tmux: TmuxSession,
    pub panes: HashMap<String, PaneGrid>,
    pub windows: BTreeMap<String, WindowTab>,
    pub active_window: Option<String>,
    pub last_cwd_query: Instant,
}

impl SessionState {
    fn new(number: u8) -> Result<Self> {
        let name = format!("tmuxify-{}-{}", std::process::id(), number);
        Self::new_with_name(number, &name)
    }

    fn new_with_name(number: u8, name: &str) -> Result<Self> {
        let cwd = std::env::var("HOME").ok();
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            auto_run: None,
            session_name: Some(name),
            flush_interval: Duration::from_millis(8),
            cols: 95,
            rows: 28,
        })?;
        Ok(Self {
            number,
            tmux,
            panes: HashMap::new(),
            windows: BTreeMap::new(),
            active_window: None,
            last_cwd_query: Instant::now() - Duration::from_secs(60),
        })
    }
}

fn list_tmux_sessions() -> Vec<String> {
    let output = std::process::Command::new("tmux")
        .args(["-L", "tmuxify", "list-sessions", "-F", "#{session_name}"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub pane_id: String,
    /// Anchor — where the drag started. `(row, col)` in pane cell coords.
    pub anchor: (u16, u16),
    /// Current end of the selection. Anchor may be after end (drag up/left).
    pub end: (u16, u16),
    /// Selection unit: char (drag), word (double-click), line (triple).
    pub mode: SelectionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Char,
    Word,
    Line,
}

impl Selection {
    /// Normalised (start, end) where start ≤ end in row-major order.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let a = self.anchor;
        let b = self.end;
        if (a.0, a.1) <= (b.0, b.1) { (a, b) } else { (b, a) }
    }
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    sessions: BTreeMap<u8, SessionState>,
    active_session: u8,
    mouse: (f32, f32),
    drag: Option<DragState>,
    hangul_mode: bool,
    composer: Composer,
    shift: bool,
    ctrl: bool,
    alt: bool,
    cmd: bool,
    last_poll: Instant,
    sidebar_open: bool,
    icons: Vec<DesktopIcon>,
    last_click: Option<(Instant, f32, f32)>,
    /// Blinking-cursor state — toggled every 500ms by about_to_wait.
    cursor_visible: bool,
    last_blink: Instant,
    /// User-resizable sidebar width (overrides self.sidebar_w default).
    sidebar_w: f32,
    auto_capture_at: Option<Instant>,
    capture_path: std::path::PathBuf,
    auto_send_at: Option<Instant>,
    auto_send_text: Option<String>,
    /// (pane_id, cols, rows) last sent to tmux during a live drag —
    /// suppresses redundant resize-pane spam at 60Hz cursor updates.
    last_live_resize: Option<(String, u16, u16)>,
    /// Timestamp of last live resize-window — throttle to ~20 Hz so the
    /// inner app (claude, vim) gets time to reflow without re-issuing
    /// SIGWINCH every cursor pixel.
    last_live_resize_at: Option<Instant>,
    /// Current snap target (for the outer window) shown during a drag.
    /// Renderer paints a translucent overlay matching the snap rect so
    /// the user sees where the window will land before release.
    snap_zone: Option<SnapZone>,
    /// Pre-snap window outer rect to restore on un-snap, if we ever add
    /// that. For now just kept so the renderer has the snap rect cached.
    pre_snap_rect: Option<(i32, i32, u32, u32)>,
    /// Rolling buffer of plain alphabetic chars the user has typed
    /// since the last "clearing" event (Enter, Esc, Tab, arrows, any
    /// Ctrl-combo). Used to recognise case variants of common shell
    /// commands (e.g. user types "CLAUDE\r" → we rewrite to "claude\r")
    /// without modifying the user's .zshrc.
    typed_buffer: String,
    /// Active mouse-drag text selection inside a pane body.
    selection: Option<Selection>,
    /// Last body-click time + position for double/triple-click detection.
    last_body_click: Option<(Instant, f32, f32, u32)>,
    /// System clipboard handle, lazily created.
    clipboard: Option<arboard::Clipboard>,
}

/// Shell commands tmuxify rewrites when the user typed them in a
/// non-lowercase form (and just them — anything else is passed through).
const CASE_REWRITE_COMMANDS: &[&str] = &["claude"];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' || c == ':'
}

fn cell_first_char(cell: &Cell) -> char {
    cell.ch.chars().next().unwrap_or(' ')
}

/// Expand a (row, col) into the word that surrounds it. Word = run of
/// alphanumeric / underscore / dash / dot / slash / colon characters.
fn expand_word(pg: &PaneGrid, pos: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    let (row, col) = pos;
    let Some(line) = pg.cells.get(row as usize) else {
        return ((row, col), (row, col));
    };
    let start_char = line.get(col as usize).map(cell_first_char).unwrap_or(' ');
    if !is_word_char(start_char) {
        return ((row, col), (row, col));
    }
    let mut s = col;
    while s > 0 {
        let prev = line.get((s - 1) as usize).map(cell_first_char).unwrap_or(' ');
        if !is_word_char(prev) { break }
        s -= 1;
    }
    let mut e = col;
    while (e as usize + 1) < line.len() {
        let next = line.get((e + 1) as usize).map(cell_first_char).unwrap_or(' ');
        if !is_word_char(next) { break }
        e += 1;
    }
    ((row, s), (row, e))
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            sessions: BTreeMap::new(),
            active_session: 1,
            mouse: (0.0, 0.0),
            drag: None,
            hangul_mode: false,
            composer: Composer::new(),
            shift: false,
            ctrl: false,
            alt: false,
            cmd: false,
            last_poll: Instant::now(),
            sidebar_open: true,
            icons: default_icons(),
            last_click: None,
            cursor_visible: true,
            last_blink: Instant::now(),
            sidebar_w: render::SIDEBAR_W,
            auto_capture_at: std::env::var("TMUXIFY_AUTOCAPTURE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + Duration::from_millis(ms)),
            capture_path: std::env::var("TMUXIFY_CAPTURE_PATH")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/tmuxify-screenshot.png")),
            auto_send_at: std::env::var("TMUXIFY_AUTOSEND_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + Duration::from_millis(ms)),
            auto_send_text: std::env::var("TMUXIFY_AUTOSEND").ok(),
            last_live_resize: None,
            last_live_resize_at: None,
            snap_zone: None,
            pre_snap_rect: None,
            typed_buffer: String::new(),
            selection: None,
            last_body_click: None,
            clipboard: arboard::Clipboard::new().ok(),
        }
    }

    fn record_typed(&mut self, c: char) {
        if c.is_ascii_alphabetic() && self.typed_buffer.len() < 32 {
            self.typed_buffer.push(c);
        } else {
            self.typed_buffer.clear();
        }
    }

    fn maybe_rewrite_command_on_enter(&mut self) {
        // Take and clear the buffer either way — Enter resets the line.
        let typed = std::mem::take(&mut self.typed_buffer);
        if typed.is_empty() {
            return;
        }
        let lower = typed.to_ascii_lowercase();
        // Already lowercase: nothing to do, the user's bytes already
        // ran a valid command.
        if typed == lower {
            return;
        }
        if !CASE_REWRITE_COMMANDS.iter().any(|c| *c == lower.as_str()) {
            return;
        }
        // Erase the original-cased typing (one backspace per char),
        // then send the lowercased command. We deliberately do NOT
        // emit Enter here — the caller is about to send Enter itself.
        let bs = vec![0x7fu8; typed.len()];
        self.send_bytes(&bs);
        self.send_bytes(lower.as_bytes());
    }

    fn ensure_session(&mut self, n: u8) {
        if !self.sessions.contains_key(&n) {
            match SessionState::new(n) {
                Ok(s) => {
                    self.sessions.insert(n, s);
                }
                Err(e) => println!("[session {n} create err] {e}"),
            }
        }
    }

    /// Discover and attach to every tmux session that already exists on
    /// this user's server but isn't tracked yet.
    fn discover_external_sessions(&mut self) {
        let mut next_n: u8 = (1..=99)
            .find(|n| !self.sessions.contains_key(n))
            .unwrap_or(99);
        for name in list_tmux_sessions() {
            // Skip names we already attached to.
            let already = self
                .sessions
                .values()
                .any(|s| s.tmux.session_name == name);
            if already {
                continue;
            }
            while self.sessions.contains_key(&next_n) && next_n < 99 {
                next_n += 1;
            }
            if next_n >= 99 {
                break;
            }
            match SessionState::new_with_name(next_n, &name) {
                Ok(s) => {
                    println!("[discover] attached external session {name} → slot {next_n}");
                    self.sessions.insert(next_n, s);
                    next_n += 1;
                }
                Err(e) => println!("[discover {name} err] {e}"),
            }
        }
    }

    fn switch_session(&mut self, n: u8) {
        if n == 0 {
            return;
        }
        self.ensure_session(n);
        if self.sessions.contains_key(&n) {
            self.active_session = n;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn new_session(&mut self) {
        let next = (1u8..=99).find(|n| !self.sessions.contains_key(n));
        if let Some(n) = next {
            self.switch_session(n);
        }
    }

    fn active(&self) -> Option<&SessionState> {
        self.sessions.get(&self.active_session)
    }

    fn active_mut(&mut self) -> Option<&mut SessionState> {
        self.sessions.get_mut(&self.active_session)
    }

    fn poll_tmux(&mut self) {
        let keys: Vec<u8> = self.sessions.keys().copied().collect();
        let mut any = false;
        for k in keys {
            let due = self
                .sessions
                .get(&k)
                .map(|s| s.last_cwd_query.elapsed() >= Duration::from_secs(2))
                .unwrap_or(false);
            let (screens, events, queries) = {
                let s = self.sessions.get(&k).unwrap();
                (
                    s.tmux.screens.try_iter().collect::<Vec<_>>(),
                    s.tmux.events.try_iter().collect::<Vec<_>>(),
                    s.tmux.queries.try_iter().collect::<Vec<_>>(),
                )
            };
            if !screens.is_empty() || !events.is_empty() || !queries.is_empty() {
                any = true;
            }
            for u in screens {
                Self::apply_screen(self.sessions.get_mut(&k).unwrap(), u);
            }
            let (cell_w_live, cell_h_live) = self.cell_metrics();
            let (canvas_left, canvas_top, canvas_w, canvas_h) = {
                let (win_w, win_h) = self
                    .window
                    .as_ref()
                    .map(|w| {
                        let s = w.inner_size();
                        (s.width as f32, s.height as f32)
                    })
                    .unwrap_or((1280.0, 760.0));
                let sidebar_w = if self.sidebar_open { self.sidebar_w } else { 0.0 };
                let cl = sidebar_w;
                let ct = render::SESSION_BAR_HEIGHT;
                let cw_ = (win_w - cl).max(120.0);
                let ch_ = (win_h - ct - render::STATUS_HEIGHT).max(80.0);
                (cl, ct, cw_, ch_)
            };
            for e in events {
                Self::apply_event(
                    self.sessions.get_mut(&k).unwrap(),
                    e,
                    canvas_left,
                    canvas_top,
                    canvas_w,
                    canvas_h,
                    cell_w_live,
                    cell_h_live,
                );
            }
            for resp in queries {
                Self::apply_cwd_query(self.sessions.get_mut(&k).unwrap(), resp);
            }
            if due {
                let s = self.sessions.get_mut(&k).unwrap();
                s.last_cwd_query = Instant::now();
                let _ = s.tmux.send_query(
                    "list-panes -s -F '#{window_id} #{pane_id} #{pane_current_path}'",
                );
            }
        }
        if any {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn apply_screen(s: &mut SessionState, u: ScreenUpdate) {
        let entry = s.panes.entry(u.pane_id.clone()).or_default();
        if entry.cells.len() != u.rows as usize || entry.cols != u.cols {
            entry.cells = vec![vec![Cell::blank(); u.cols as usize]; u.rows as usize];
            entry.rows = u.rows;
            entry.cols = u.cols;
        }
        for (i, row) in u.dirty {
            if (i as usize) < entry.cells.len() {
                entry.cells[i as usize] = row;
            }
        }
        entry.cursor_row = u.cursor_row;
        entry.cursor_col = u.cursor_col;
        // Deliberately ignore u.title (OSC 0 from the inner app) —
        // claude code rewrites it every spinner tick (✳ ✻ ✽ ✶ ⏺ ...)
        // which would re-render the title bar at 10-20Hz and look like
        // it's flickering. The cwd-derived title from apply_cwd_query
        // is steady and more useful (basename + full path).
        let _ = u.title;
    }

    fn apply_event(
        s: &mut SessionState,
        e: TmuxEvent,
        canvas_left: f32,
        canvas_top: f32,
        canvas_w: f32,
        canvas_h: f32,
        cell_w: f32,
        cell_h: f32,
    ) {
        match e {
            TmuxEvent::LayoutChange { window_id, layout } => {
                let first = layout.split_whitespace().next().unwrap_or("");
                let Ok(l) = parse_layout(first) else {
                    return;
                };
                let leaves: Vec<String> = l
                    .leaves()
                    .iter()
                    .filter_map(|n| match n {
                        Layout::Pane { id, .. } => Some(format!("%{id}")),
                        _ => None,
                    })
                    .collect();
                let tab = s.windows.entry(window_id.clone()).or_insert_with(|| {
                    WindowTab {
                        title: window_id.clone(),
                        ..Default::default()
                    }
                });
                // Add new panes at a cascaded default size so the desktop
                // icons stay visible underneath. The user can drag the
                // pane's edges or maximise it manually; we no longer fill
                // the canvas on the first pane because that hid every
                // desktop icon and broke the launcher flow.
                let _ = (canvas_left, canvas_top, canvas_w, canvas_h);
                for pid in &leaves {
                    if !tab.floating.contains_key(pid) {
                        let n = tab.next_cascade;
                        let step = (n % 4) as f32;
                        let fx = render::SIDEBAR_W + 220.0 + step * 30.0;
                        let fy = render::SESSION_BAR_HEIGHT + 30.0 + step * 30.0;
                        let fw = 95.0 * render::CELL_W + 16.0;
                        let fh = 28.0 * render::CELL_H + 34.0;
                        tab.next_cascade += 1;
                        tab.floating.insert(
                            pid.clone(),
                            FloatingPane {
                                pane_id: pid.clone(),
                                title: pid.clone(),
                                x: fx,
                                y: fy,
                                w: fw,
                                h: fh,
                            },
                        );
                        let cols = ((fw - 16.0) / cell_w).floor().max(20.0) as u16;
                        let rows = ((fh - 34.0) / cell_h).floor().max(5.0) as u16;
                        let _ = s
                            .tmux
                            .send_cmd(&format!("resize-window -t {pid} -x {cols} -y {rows}"));
                        let _ = s.tmux.resize_client(cols, rows);
                    }
                }
                // Drop panes that disappeared.
                let live: std::collections::HashSet<&String> = leaves.iter().collect();
                let to_drop: Vec<String> = tab
                    .floating
                    .keys()
                    .filter(|k| !live.contains(k))
                    .cloned()
                    .collect();
                for k in to_drop {
                    tab.floating.remove(&k);
                }
                if tab.active_pane.is_none() {
                    tab.active_pane = leaves.first().cloned();
                }
                tab.layout = Some(l);
                if s.active_window.is_none() {
                    s.active_window = Some(window_id);
                }
            }
            TmuxEvent::WindowAdd { window_id } => {
                s.windows
                    .entry(window_id.clone())
                    .or_insert_with(|| WindowTab {
                        title: window_id.clone(),
                        ..Default::default()
                    });
                // Force tmux to ship the new window's layout immediately
                // — it sometimes elides %layout-change for windows the
                // client just created. The cwd query path will hydrate
                // a FloatingPane the moment the response lands.
                let _ = s.tmux.send_query(&format!(
                    "list-panes -t '{window_id}' -F '#{{window_id}} #{{pane_id}} #{{pane_current_path}}'"
                ));
                s.active_window = Some(window_id);
            }
            TmuxEvent::WindowClose { window_id } => {
                s.windows.remove(&window_id);
                if s.active_window.as_deref() == Some(&window_id) {
                    s.active_window = s.windows.keys().next().cloned();
                }
            }
            TmuxEvent::WindowRenamed { window_id, name } => {
                if let Some(t) = s.windows.get_mut(&window_id) {
                    t.title = name;
                }
            }
            TmuxEvent::Error { ts, id, flags } => {
                println!("[tmux err evt] ts={ts} id={id} flags={flags}");
            }
            TmuxEvent::Unknown { raw } => {
                println!("[tmux unknown evt] {raw}");
            }
            TmuxEvent::NonProtocolLine { raw } => {
                println!("[tmux line] {raw}");
            }
            _ => {}
        }
    }

    fn apply_cwd_query(s: &mut SessionState, lines: Vec<String>) {
        for line in lines {
            let mut it = line.splitn(3, ' ');
            let Some(wid) = it.next() else { continue };
            let Some(pid) = it.next() else { continue };
            let Some(path) = it.next() else { continue };
            let basename = path.rsplit('/').find(|st| !st.is_empty()).unwrap_or(path);
            let tab = s.windows.entry(wid.to_string()).or_insert_with(|| WindowTab {
                title: wid.to_string(),
                ..Default::default()
            });
            // Late binding — tmux 3.4 sometimes elides %layout-change
            // for windows we created via new-window from control mode,
            // so the only signal we get back is the cwd query. Create
            // the FloatingPane on the fly if missing so the new tab
            // actually has something to draw.
            if !tab.floating.contains_key(pid) {
                let n = tab.next_cascade;
                tab.next_cascade += 1;
                let step = (n % 4) as f32;
                tab.floating.insert(
                    pid.to_string(),
                    FloatingPane {
                        pane_id: pid.to_string(),
                        title: format!("{basename}  ·  {path}"),
                        x: render::SIDEBAR_W + 130.0 + step * 30.0,
                        y: render::SESSION_BAR_HEIGHT + 30.0 + step * 30.0,
                        w: 95.0 * render::CELL_W + 16.0,
                        h: 28.0 * render::CELL_H + 34.0,
                    },
                );
                if tab.active_pane.is_none() {
                    tab.active_pane = Some(pid.to_string());
                }
            } else if let Some(fp) = tab.floating.get_mut(pid) {
                fp.title = format!("{basename}  ·  {path}");
            }
            if tab.active_pane.as_deref() == Some(pid) {
                tab.title = basename.to_string();
            }
        }
        // After updating all tabs from this query batch, disambiguate
        // tabs that ended up with the same basename — append " · #N"
        // (window-id number) so the user can tell them apart in the
        // top strip.
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for tab in s.windows.values() {
            *counts.entry(tab.title.clone()).or_insert(0) += 1;
        }
        for (wid, tab) in s.windows.iter_mut() {
            if counts.get(&tab.title).copied().unwrap_or(0) > 1 {
                let n = wid.trim_start_matches('@');
                tab.title = format!("{}  ·  #{}", tab.title, n);
            }
        }
    }

    fn send_bytes(&self, bytes: &[u8]) {
        let Some(s) = self.active() else { return };
        let target = s
            .active_window
            .as_ref()
            .and_then(|w| s.windows.get(w))
            .and_then(|tab| tab.active_pane.clone());
        let mut hex = String::with_capacity(bytes.len() * 3);
        for (i, b) in bytes.iter().enumerate() {
            if i > 0 {
                hex.push(' ');
            }
            let _ = write!(hex, "{:02x}", b);
        }
        let _ = s.tmux.send_keys_hex(target.as_deref(), &hex);
    }

    fn tmux_cmd(&mut self, cmd: &str) {
        let dead = match self.active() {
            Some(s) => match s.tmux.send_cmd(cmd) {
                Ok(_) => false,
                Err(_) => true,
            },
            None => false,
        };
        if dead {
            // tmux for this session is gone (e.g. last pane killed → session
            // ended). Drop it and ensure at least one live session exists.
            let n = self.active_session;
            println!("[session {n} dead] cleaning up");
            self.sessions.remove(&n);
            let next = self.sessions.keys().next().copied();
            self.active_session = next.unwrap_or(1);
            self.ensure_session(self.active_session);
            // Re-issue the command on the new active session.
            if let Some(s) = self.active() {
                let _ = s.tmux.send_cmd(cmd);
            }
        }
    }

    /// Current cell metrics — renderer's live values when available
    /// (which reflect any Ctrl+/+ zoom), otherwise the compile-time
    /// defaults from render::CELL_W/CELL_H.
    /// Resize every pane in every session so it fills the available
    /// canvas (everything between sidebar/status/title chrome).
    /// Invoked on OS-window Resized so the inner content tracks the
    /// outer frame — the floating "windows" feel like real windows
    /// instead of cards stranded mid-canvas.
    fn refit_panes_to_canvas(&mut self, win_w: f32, win_h: f32) {
        let sidebar_w = if self.sidebar_open { self.sidebar_w } else { 0.0 };
        let canvas_left = sidebar_w;
        let canvas_top = render::SESSION_BAR_HEIGHT;
        let canvas_w = (win_w - canvas_left).max(120.0);
        let canvas_h = (win_h - canvas_top - render::STATUS_HEIGHT).max(80.0);
        let (cw, ch) = self.cell_metrics();
        let mut plans: Vec<(u8, String, u16, u16)> = Vec::new();
        for (n, s) in self.sessions.iter_mut() {
            for tab in s.windows.values_mut() {
                for fp in tab.floating.values_mut() {
                    fp.x = canvas_left;
                    fp.y = canvas_top;
                    fp.w = canvas_w;
                    fp.h = canvas_h;
                    let cols = ((fp.w - 16.0) / cw).floor().max(20.0) as u16;
                    let rows = ((fp.h - 34.0) / ch).floor().max(5.0) as u16;
                    plans.push((*n, fp.pane_id.clone(), cols, rows));
                }
            }
        }
        for (n, pid, cols, rows) in plans {
            if let Some(s) = self.sessions.get(&n) {
                let _ = s
                    .tmux
                    .send_cmd(&format!("resize-window -t {pid} -x {cols} -y {rows}"));
            }
        }
    }

    fn cell_metrics(&self) -> (f32, f32) {
        match self.renderer.as_ref() {
            Some(r) => (r.cell_w, r.cell_h),
            None => (render::CELL_W, render::CELL_H),
        }
    }

    fn spawn_new_window(&self) {
        // New OS-level tmuxify window = new process, fresh tmux session.
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }

    /// Bump (delta>0) or shrink (delta<0) the body font size by 1 pt and
    /// re-issue resize-window for every visible pane so the inner app
    /// sees the new grid dimensions immediately.
    fn apply_zoom_delta(&mut self, delta: f32) {
        let Some(r) = self.renderer.as_mut() else { return };
        let new_size = r.body_font_size() + delta;
        let (cw, ch) = r.set_body_font_size(new_size);
        println!("[zoom] font_size={:.1} cell={:.2}x{:.2}", new_size, cw, ch);
        self.refresh_all_pane_sizes();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn reset_zoom(&mut self) {
        let Some(r) = self.renderer.as_mut() else { return };
        let (cw, ch) = r.set_body_font_size(13.0);
        println!("[zoom] reset → cell={:.2}x{:.2}", cw, ch);
        self.refresh_all_pane_sizes();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// For every pane on every session, compute the cell grid from the
    /// pane's current pixel geometry and the renderer's *current*
    /// cell_w/cell_h, then send resize-window so the inner app reflows.
    fn refresh_all_pane_sizes(&mut self) {
        let (cw, ch) = match self.renderer.as_ref() {
            Some(r) => (r.cell_w, r.cell_h),
            None => return,
        };
        let title_bar = TITLE_BAR;
        // Walk every session/window/pane and build resize commands.
        let plans: Vec<(u8, String, u16, u16)> = self
            .sessions
            .iter()
            .flat_map(|(n, s)| {
                s.windows.values().flat_map(move |tab| {
                    tab.floating.values().map(move |fp| {
                        let cols = ((fp.w - 16.0) / cw).floor().max(20.0) as u16;
                        let rows =
                            ((fp.h - 34.0) / ch).floor().max(5.0) as u16;
                        (*n, fp.pane_id.clone(), cols, rows)
                    })
                })
            })
            .collect();
        for (n, pid, cols, rows) in plans {
            if let Some(s) = self.sessions.get(&n) {
                let _ = s
                    .tmux
                    .send_cmd(&format!("resize-window -t {pid} -x {cols} -y {rows}"));
            }
        }
    }

    /// Find the pane at `pane_id` and return the (row, col) cell that
    /// the screen-space pixel (mx, my) falls on. Returns None if the
    /// pixel is outside the pane body.
    fn body_pixel_to_cell(&self, pane_id: &str, mx: f32, my: f32) -> Option<(u16, u16)> {
        let (cw, ch) = self.cell_metrics();
        let tab = self.active_tab()?;
        let fp = tab.floating.get(pane_id)?;
        let body_left = fp.x + render::BOX_PAD;
        let body_top = fp.y + render::TITLE_HEIGHT;
        if mx < body_left || my < body_top {
            return None;
        }
        let pg = self.active().and_then(|s| s.panes.get(pane_id))?;
        let col = ((mx - body_left) / cw).floor() as i32;
        let row = ((my - body_top) / ch).floor() as i32;
        if col < 0 || row < 0 {
            return None;
        }
        let col = (col as u16).min(pg.cols.saturating_sub(1));
        let row = (row as u16).min(pg.rows.saturating_sub(1));
        Some((row, col))
    }

    /// Given the current `selection` and its mode, expand `anchor` and
    /// `end` to cover the surrounding word or line. No-op for Char mode.
    fn expand_selection_mode(&mut self) {
        let Some(sel) = self.selection.clone() else { return };
        let Some(pg) = self.active().and_then(|s| s.panes.get(&sel.pane_id)) else { return };
        let (s, e) = sel.ordered();
        let (anchor, end) = match sel.mode {
            SelectionMode::Char => return,
            SelectionMode::Word => {
                let s = expand_word(pg, s);
                let e = expand_word(pg, e);
                (s.0, e.1)
            }
            SelectionMode::Line => {
                let s_col = 0u16;
                let e_col = pg.cols.saturating_sub(1);
                ((s.0, s_col), (e.0, e_col))
            }
        };
        if let Some(sel) = self.selection.as_mut() {
            sel.anchor = anchor;
            sel.end = end;
        }
    }

    /// Walk the selection grid in row-major order and gather the visible
    /// text. Trailing whitespace on each row is trimmed (matches what
    /// every other terminal does when copying).
    fn selection_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        let pg = self.active().and_then(|s| s.panes.get(&sel.pane_id))?;
        let (start, end) = sel.ordered();
        let mut out = String::new();
        for r in start.0..=end.0 {
            let row = pg.cells.get(r as usize)?;
            let c0 = if r == start.0 { start.1 as usize } else { 0 };
            let c1 = if r == end.0 { end.1 as usize } else { pg.cols as usize - 1 };
            let mut line = String::new();
            for c in c0..=c1 {
                if let Some(cell) = row.get(c) {
                    if cell.ch.is_empty() { line.push(' '); } else { line.push_str(&cell.ch); }
                }
            }
            if c1 + 1 == pg.cols as usize {
                while line.ends_with(' ') { line.pop(); }
            }
            if r != end.0 { line.push('\n'); }
            out.push_str(&line);
        }
        Some(out)
    }

    fn copy_selection_to_clipboard(&mut self) {
        let Some(text) = self.selection_text() else { return };
        if text.is_empty() { return }
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    fn paste_from_clipboard(&mut self) {
        let Some(cb) = self.clipboard.as_mut() else { return };
        let Ok(text) = cb.get_text() else { return };
        if text.is_empty() { return }
        self.send_bytes(text.as_bytes());
    }

    fn handle_key(&mut self, ev: KeyEvent) {
        let pressed = ev.state == ElementState::Pressed;
        if let Key::Named(NamedKey::Shift) = &ev.logical_key {
            self.shift = pressed;
            return;
        }
        if let Key::Named(NamedKey::Control) = &ev.logical_key {
            self.ctrl = pressed;
            return;
        }
        if let Key::Named(NamedKey::Alt) = &ev.logical_key {
            if matches!(ev.physical_key, PhysicalKey::Code(KeyCode::AltRight)) {
                // Right Alt doubles as the Korean 한/영 key on KR keyboards.
                // Fall through to the hangul-toggle handler below.
            } else {
                self.alt = pressed;
                return;
            }
        }
        if let Key::Named(NamedKey::Super) = &ev.logical_key {
            self.cmd = pressed;
            return;
        }
        if !pressed {
            return;
        }
        // Cmd+C / Cmd+V — clipboard. Match the character text (case-
        // insensitive) so Caps Lock or shift don't break it.
        if self.cmd {
            let key_str = match &ev.logical_key {
                Key::Character(s) => Some(s.as_str().to_ascii_lowercase()),
                _ => None,
            };
            if let Some(k) = key_str.as_deref() {
                if k == "c" {
                    self.copy_selection_to_clipboard();
                    return;
                }
                if k == "v" {
                    self.paste_from_clipboard();
                    return;
                }
            }
        }

        // Hangul toggle. Log every keydown so we can diagnose what
        // WSLg / macOS sends for the 한/영 key when toggle still fails.
        if std::env::var("TMUXIFY_KEY_DEBUG").is_ok() {
            println!("[key] phys={:?} logical={:?}", ev.physical_key, ev.logical_key);
        }
        let is_right_alt = matches!(ev.physical_key, PhysicalKey::Code(KeyCode::AltRight));
        let is_lang2 = matches!(ev.physical_key, PhysicalKey::Code(KeyCode::Lang2))
            || matches!(ev.physical_key, PhysicalKey::Code(KeyCode::Lang1));
        let is_hangul_toggle = is_right_alt
            || is_lang2
            || matches!(&ev.logical_key, Key::Named(NamedKey::HangulMode))
            || matches!(&ev.logical_key, Key::Named(NamedKey::NonConvert))
            || (matches!(&ev.logical_key, Key::Named(NamedKey::Space)) && self.shift);
        if is_hangul_toggle {
            if let Some(s) = self.composer.flush() {
                self.send_bytes(s.as_bytes());
            }
            self.hangul_mode = !self.hangul_mode;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        // F12 → screenshot for self-verification.
        if matches!(&ev.logical_key, Key::Named(NamedKey::F12)) {
            let path = self.capture_path.clone();
            if let Some(r) = self.renderer.as_mut() {
                r.request_capture(path);
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        if matches!(&ev.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.hangul_mode && self.composer.backspace() {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            // Keep typed_buffer in sync so CLAUDE→claude rewrite still
            // fires after the user backspaced and retyped a letter.
            self.typed_buffer.pop();
            self.send_bytes(&[0x7f]);
            return;
        }

        // Window/tab/pane shortcuts.
        if self.ctrl {
            // Any ctrl-combo means the user is issuing a command, not
            // typing a shell word — drop whatever was buffered.
            self.typed_buffer.clear();
            use PhysicalKey::Code as PC;
            // Ctrl+1..9 → select tab N.
            let tab_n: Option<u8> = match ev.physical_key {
                PC(KeyCode::Digit1) => Some(1),
                PC(KeyCode::Digit2) => Some(2),
                PC(KeyCode::Digit3) => Some(3),
                PC(KeyCode::Digit4) => Some(4),
                PC(KeyCode::Digit5) => Some(5),
                PC(KeyCode::Digit6) => Some(6),
                PC(KeyCode::Digit7) => Some(7),
                PC(KeyCode::Digit8) => Some(8),
                PC(KeyCode::Digit9) => Some(9),
                _ => None,
            };
            if let Some(n) = tab_n {
                self.tmux_cmd(&format!("select-window -t :{n}"));
                return;
            }
            // Ctrl + '+' / '=' → zoom in, Ctrl + '-' → zoom out,
            // Ctrl + '0' → reset. Use physical keys so layout doesn't matter.
            let zoom_delta: Option<f32> = match ev.physical_key {
                PC(KeyCode::Equal) | PC(KeyCode::NumpadAdd) => Some(1.0),
                PC(KeyCode::Minus) | PC(KeyCode::NumpadSubtract) => Some(-1.0),
                _ => None,
            };
            if let Some(d) = zoom_delta {
                self.apply_zoom_delta(d);
                return;
            }
            if matches!(ev.physical_key, PC(KeyCode::Digit0) | PC(KeyCode::Numpad0)) {
                self.reset_zoom();
                return;
            }
            // Ctrl + Alt + ← / → / ↑  →  Aero-Snap shortcuts. Win+arrow
            // is owned by Windows itself (WSLg doesn't forward it for
            // Linux surfaces), so we ride Ctrl+Alt instead.
            if self.alt {
                let zone = match ev.physical_key {
                    PC(KeyCode::ArrowLeft) => Some(SnapZone::Left),
                    PC(KeyCode::ArrowRight) => Some(SnapZone::Right),
                    PC(KeyCode::ArrowUp) => Some(SnapZone::Top),
                    _ => None,
                };
                if let Some(z) = zone {
                    self.apply_window_snap(z);
                    return;
                }
            }
            if self.shift {
                match ev.physical_key {
                    PC(KeyCode::KeyT) => {
                        self.new_session();
                        return;
                    }
                    PC(KeyCode::KeyN) => {
                        self.spawn_new_window();
                        return;
                    }
                    PC(KeyCode::KeyW) => {
                        self.tmux_cmd("kill-window");
                        return;
                    }
                    PC(KeyCode::KeyD) => {
                        self.tmux_cmd("split-window -v");
                        return;
                    }
                    PC(KeyCode::Tab) => {
                        self.tmux_cmd("previous-window");
                        return;
                    }
                    _ => {}
                }
            } else {
                match ev.physical_key {
                    PC(KeyCode::KeyT) => {
                        self.tmux_cmd("new-window");
                        return;
                    }
                    PC(KeyCode::KeyD) => {
                        self.tmux_cmd("split-window -h");
                        return;
                    }
                    PC(KeyCode::KeyW) => {
                        self.tmux_cmd("kill-pane");
                        return;
                    }
                    PC(KeyCode::Tab) => {
                        self.tmux_cmd("next-window");
                        return;
                    }
                    _ => {}
                }
            }
        }
        if self.alt {
            self.typed_buffer.clear();
            use PhysicalKey::Code as PC;
            if matches!(ev.physical_key, PC(KeyCode::Tab)) {
                let cmd = if self.shift {
                    "select-pane -t :.-"
                } else {
                    "select-pane -t :.+"
                };
                self.tmux_cmd(cmd);
                return;
            }
        }

        // Named keys → ANSI bytes (for keys that don't depend on
        // app-mode) OR tmux symbolic names (for arrows/Home/End/Delete
        // where DECCKM/keypad mode flips the sequence). Letting tmux
        // pick the right bytes is the only reliable way to make shell
        // history (Up/Down) AND claude-code's in-app navigation both
        // work without us tracking each pane's parser state.
        if let Key::Named(named) = &ev.logical_key {
            // Mode-sensitive keys: hand off to tmux send-keys <name>.
            let tmux_key: Option<&'static str> = match named {
                NamedKey::ArrowUp => Some("Up"),
                NamedKey::ArrowDown => Some("Down"),
                NamedKey::ArrowRight => Some("Right"),
                NamedKey::ArrowLeft => Some("Left"),
                NamedKey::Home => Some("Home"),
                NamedKey::End => Some("End"),
                NamedKey::Delete => Some("DC"),
                NamedKey::PageUp => Some("PPage"),
                NamedKey::PageDown => Some("NPage"),
                _ => None,
            };
            if let Some(name) = tmux_key {
                // Arrows / Home / End / PageUp etc. mean the user is
                // navigating, not typing a new command — clear buffer.
                self.typed_buffer.clear();
                if let Some(s) = self.composer.flush() {
                    self.send_bytes(s.as_bytes());
                }
                if let Some(sess) = self.active() {
                    let target = sess
                        .active_window
                        .as_ref()
                        .and_then(|w| sess.windows.get(w))
                        .and_then(|t| t.active_pane.clone());
                    let t = target
                        .as_deref()
                        .map(|p| format!("-t {p} "))
                        .unwrap_or_default();
                    let _ = sess.tmux.send_cmd(&format!("send-keys {t}{name}"));
                }
                return;
            }
            // Mode-independent keys: raw bytes are fine.
            let key_bytes: &[u8] = match named {
                NamedKey::Enter => &[0x0d],
                NamedKey::Space => &[b' '],
                NamedKey::Tab => &[0x09],
                NamedKey::Escape => &[0x1b],
                _ => &[],
            };
            if !key_bytes.is_empty() {
                if let Some(s) = self.composer.flush() {
                    self.send_bytes(s.as_bytes());
                }
                // Right before Enter goes out, see if the user just
                // typed a case variant of a known command (e.g.
                // CLAUDE) and rewrite to lowercase. Tab/Esc/Space all
                // reset the buffer so we don't fire on partial input.
                if matches!(named, NamedKey::Enter) {
                    self.maybe_rewrite_command_on_enter();
                } else {
                    self.typed_buffer.clear();
                }
                self.send_bytes(key_bytes);
                return;
            }
        }

        let Some(text) = ev.text.as_deref() else {
            return;
        };
        let Some(c) = text.chars().next() else { return };

        if !self.hangul_mode {
            self.record_typed(c);
            self.send_bytes(text.as_bytes());
            return;
        }
        let Some(jamo) = dubeolsik(c) else {
            if let Some(s) = self.composer.flush() {
                self.send_bytes(s.as_bytes());
            }
            self.send_bytes(text.as_bytes());
            return;
        };
        if let Some(commit) = self.composer.feed(jamo) {
            self.send_bytes(commit.as_bytes());
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<HitTarget> {
        let sidebar_w = if self.sidebar_open { self.sidebar_w } else { 0.0 };
        // OS-window edge resize zones (6px). Skipped near corner of title bar.
        let win_w = self
            .window
            .as_ref()
            .map(|w| w.inner_size().width as f32)
            .unwrap_or(0.0);
        let win_h = self
            .window
            .as_ref()
            .map(|w| w.inner_size().height as f32)
            .unwrap_or(0.0);
        // Taskbar pane buttons (checked before edge-resize so the bottom
        // hairline doesn't steal clicks meant for the buttons).
        if let Some(r) = &self.renderer {
            for hit in &r.taskbar_buttons {
                if x >= hit.x && x < hit.x + hit.w && y >= hit.y && y < hit.y + hit.h {
                    // × close glyph zone on the right of each button.
                    if x >= hit.close_x && x < hit.close_x + hit.close_w {
                        return Some(HitTarget::PaneClose(hit.pane_id.clone()));
                    }
                    return Some(HitTarget::TaskbarPane(hit.pane_id.clone()));
                }
            }
            // Top tab × close zones — checked before the Tab hit so the
            // close glyph doesn't get swallowed by tab-select.
            for hit in &r.tab_close_hits {
                if x >= hit.x && x < hit.x + hit.w && y >= hit.y && y < hit.y + hit.h {
                    return Some(HitTarget::TabClose(hit.window_id.clone()));
                }
            }
        }
        // Window-edge resize zone is 12px (≈ 6 logical on 2× Retina). The
        // pane body never extends into this strip — even when the pane
        // fills the canvas, the renderer leaves PADDING around it — so
        // there's no ambiguity between "resize window" and "resize pane".
        let edge = 12.0;
        // Bottom edge / corners.
        if y >= win_h - edge {
            if x <= edge {
                return Some(HitTarget::WindowEdgeResize(ResizeDirection::SouthWest));
            }
            if x >= win_w - edge {
                return Some(HitTarget::WindowEdgeResize(ResizeDirection::SouthEast));
            }
            return Some(HitTarget::WindowEdgeResize(ResizeDirection::South));
        }
        // Left edge (below the title bar).
        if x <= edge && y >= render::SESSION_BAR_HEIGHT {
            return Some(HitTarget::WindowEdgeResize(ResizeDirection::West));
        }
        // Right edge (below the title bar).
        if x >= win_w - edge && y >= render::SESSION_BAR_HEIGHT {
            return Some(HitTarget::WindowEdgeResize(ResizeDirection::East));
        }
        // Sidebar resize handle — 4px right edge of the sidebar.
        if self.sidebar_open
            && y >= render::SESSION_BAR_HEIGHT
            && (x - sidebar_w).abs() <= 3.0
        {
            return Some(HitTarget::SidebarResize);
        }
        // Sidebar zone (below title bar, left column). The constants here
        // mirror the renderer — bump together with render.rs's row_h /
        // close_w / search_h or clicks land on the wrong row.
        if self.sidebar_open && x < sidebar_w && y >= render::SESSION_BAR_HEIGHT {
            let row_h = 100.0;
            let row_gap = 8.0;
            let search_h = 56.0;
            let first_row_y = render::SESSION_BAR_HEIGHT + 14.0 + search_h + 14.0;
            let mut row_y = first_row_y;
            let close_w = 40.0;
            let close_left = sidebar_w - 14.0 - close_w;
            for n in self.sessions.keys() {
                if y >= row_y && y < row_y + row_h {
                    if x >= close_left && x <= close_left + close_w {
                        return Some(HitTarget::SessionClose(*n));
                    }
                    return Some(HitTarget::Session(*n));
                }
                row_y += row_h + row_gap;
            }
            row_y += 6.0;
            let new_h = 60.0;
            if y >= row_y && y < row_y + new_h {
                return Some(HitTarget::NewSession);
            }
            return None;
        }
        // Title-bar zone (decorationless OS chrome).
        if y < render::SESSION_BAR_HEIGHT {
            // Sidebar toggle = leftmost square in the title bar.
            let toggle_x_end = render::PADDING + 28.0;
            if x >= 0.0 && x <= toggle_x_end {
                return Some(HitTarget::ToggleSidebar);
            }
            // OS buttons on the far right.
            let width = self
                .window
                .as_ref()
                .map(|w| w.inner_size().width as f32)
                .unwrap_or(0.0);
            let btn_w = 46.0; // must match render.rs chrome button width
            let close_x = width - btn_w;
            let max_x = close_x - btn_w;
            let min_x = max_x - btn_w;
            if x >= close_x {
                return Some(HitTarget::Close);
            }
            if x >= max_x {
                return Some(HitTarget::MaxToggle);
            }
            if x >= min_x {
                return Some(HitTarget::Min);
            }
            // Tabs (offset by sidebar + toggle button).
            let tabs_origin = sidebar_w.max(toggle_x_end + 4.0);
            let mut tab_x = tabs_origin;
            let win_keys: Vec<String> = self
                .active()
                .map(|s| s.windows.keys().cloned().collect())
                .unwrap_or_default();
            for wid in &win_keys {
                if x >= tab_x && x <= tab_x + render::SESSION_TAB_W {
                    return Some(HitTarget::Tab(wid.clone()));
                }
                tab_x += render::SESSION_TAB_W + render::SESSION_TAB_GAP;
            }
            let plus_w = 28.0;
            if x >= tab_x && x <= tab_x + plus_w {
                return Some(HitTarget::NewTab);
            }
            if x < min_x {
                return Some(HitTarget::Drag);
            }
            return None;
        }
        // Sidebar zone — clicks consumed (no items wired yet).
        if x < sidebar_w {
            return None;
        }
        // Floating panes of the active session/window — only if any.
        let floating_hit = self.active().and_then(|s| {
            let wid = s.active_window.as_ref()?;
            let tab = s.windows.get(wid)?;
            let mut order: Vec<&FloatingPane> = tab.floating.values().collect();
            order.sort_by_key(|f| (tab.active_pane.as_deref() == Some(&f.pane_id)) as u8);
            // Resize grip band — INSIDE the pane only. Originally extended
            // outside too for easier grabbing, but that collided with the
            // OS-window edge resize zone whenever the pane filled the
            // canvas (its outer border = window border). Keeping the band
            // inside the pane leaves the outer ~12 px clear for window
            // edge resize and disambiguates the two gestures.
            let band: f32 = RESIZE_HANDLE; // 14 px
            let corner: f32 = 22.0;
            for fp in order.iter().rev() {
                // First the corners — they take precedence over edges.
                let near_l = x >= fp.x && x <= fp.x + corner;
                let near_r = x >= fp.x + fp.w - corner && x <= fp.x + fp.w;
                let near_t = y >= fp.y && y <= fp.y + corner;
                let near_b = y >= fp.y + fp.h - corner && y <= fp.y + fp.h;
                if near_l && near_t {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::NW));
                }
                if near_r && near_t {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::NE));
                }
                if near_l && near_b {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::SW));
                }
                if near_r && near_b {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::SE));
                }
                // Then the 4 edges, again INSIDE-only.
                let inside_x = x >= fp.x && x <= fp.x + fp.w;
                let inside_y = y >= fp.y && y <= fp.y + fp.h;
                if inside_y && x >= fp.x && x <= fp.x + band {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::W));
                }
                if inside_y && x >= fp.x + fp.w - band && x <= fp.x + fp.w {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::E));
                }
                if inside_x && y >= fp.y && y <= fp.y + band {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::N));
                }
                if inside_x && y >= fp.y + fp.h - band && y <= fp.y + fp.h {
                    return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::S));
                }
                // X close button — wider red zone on top-right of
                // title bar. Must stay in sync with the render.rs close
                // rect (close_w = 26, 2 px right margin) → bump 28 here
                // when render moves.
                let close_hit_w = 28.0;
                let cx = fp.x + fp.w - close_hit_w;
                if x >= cx
                    && x <= fp.x + fp.w - 2.0
                    && y >= fp.y + 3.0
                    && y <= fp.y + TITLE_BAR
                {
                    return Some(HitTarget::PaneClose(fp.pane_id.clone()));
                }
                if x >= fp.x && x <= fp.x + fp.w && y >= fp.y && y <= fp.y + TITLE_BAR {
                    return Some(HitTarget::Title(fp.pane_id.clone()));
                }
                if x >= fp.x && x <= fp.x + fp.w && y >= fp.y && y <= fp.y + fp.h {
                    return Some(HitTarget::Body(fp.pane_id.clone()));
                }
            }
            None
        });
        if floating_hit.is_some() {
            return floating_hit;
        }
        // Icons last (always checked, even when no session is up).
        for (i, ic) in self.icons.iter().enumerate() {
            let icon_w = render::ICON_W;
            let icon_h = render::ICON_H;
            if x >= ic.x && x <= ic.x + icon_w && y >= ic.y && y <= ic.y + icon_h {
                return Some(HitTarget::Icon(i));
            }
        }
        return None;
        #[allow(unreachable_code)]
        let _placeholder_for_old_loop = false;
        let mut order: Vec<&FloatingPane> = Vec::new();
        order.sort_by_key(|f: &&FloatingPane| (false) as u8);
        for fp in order.iter().rev() {
            if x >= fp.x + fp.w - RESIZE_HANDLE
                && x <= fp.x + fp.w
                && y >= fp.y + fp.h - RESIZE_HANDLE
                && y <= fp.y + fp.h
            {
                return Some(HitTarget::Resize(fp.pane_id.clone(), PaneEdge::SE));
            }
            // X close button — top-right of title bar.
            let cx = fp.x + fp.w - 22.0;
            if x >= cx && x <= fp.x + fp.w - 4.0 && y >= fp.y + 4.0 && y <= fp.y + TITLE_BAR - 2.0
            {
                return Some(HitTarget::PaneClose(fp.pane_id.clone()));
            }
            if x >= fp.x && x <= fp.x + fp.w && y >= fp.y && y <= fp.y + TITLE_BAR {
                return Some(HitTarget::Title(fp.pane_id.clone()));
            }
            if x >= fp.x && x <= fp.x + fp.w && y >= fp.y && y <= fp.y + fp.h {
                return Some(HitTarget::Body(fp.pane_id.clone()));
            }
        }
        // Icons last (only hit if not on top of any pane).
        for (i, ic) in self.icons.iter().enumerate() {
            let icon_w = render::ICON_W;
            let icon_h = render::ICON_H;
            if x >= ic.x && x <= ic.x + icon_w && y >= ic.y && y <= ic.y + icon_h {
                return Some(HitTarget::Icon(i));
            }
        }
        None
    }

    fn handle_mouse_press(&mut self) {
        let (x, y) = self.mouse;
        let now = Instant::now();
        let is_double = self
            .last_click
            .map(|(t, lx, ly)| {
                now.duration_since(t) < Duration::from_millis(400)
                    && (lx - x).abs() < 5.0
                    && (ly - y).abs() < 5.0
            })
            .unwrap_or(false);
        // When the second click fires, clear the anchor so a third click
        // does not piggy-back into another "double-click" — otherwise a
        // user who clicks 3× in a row spawns two new windows from one
        // icon. New anchor only when this click was the FIRST in a pair.
        if is_double {
            self.last_click = None;
        } else {
            self.last_click = Some((now, x, y));
        }
        let hit = self.hit_test(x, y);
        println!("[click] xy=({x:.0},{y:.0}) double={is_double} hit={hit:?}");
        let Some(hit) = hit else { return };
        if let HitTarget::Icon(idx) = &hit {
            if is_double {
                if let Some(ic) = self.icons.get(*idx).cloned() {
                    match ic.kind {
                        IconKind::Folder { cwd } => {
                            let cmd = format!("new-window -c {}", shell_quote(&cwd));
                            self.tmux_cmd(&cmd);
                        }
                        IconKind::Claude { cwd } => {
                            // Spawn a fresh pane in `cwd`, then send-keys
                            // "claude\r" to the just-created (and now-
                            // active) pane. We rely on the global
                            // interactive-login shell so the `claude`
                            // function in ~/.zshrc resolves correctly.
                            let cmd = format!("new-window -c {}", shell_quote(&cwd));
                            self.tmux_cmd(&cmd);
                            // tmux serialises commands per control client;
                            // the send-keys runs after new-window's begin/
                            // end pair completes, by which time the new
                            // pane is selected.
                            self.tmux_cmd("send-keys 'claude' Enter");
                        }
                    }
                }
                return;
            }
            return;
        }
        match hit {
            HitTarget::Tab(wid) => {
                self.tmux_cmd(&format!("select-window -t {wid}"));
                if let Some(s) = self.active_mut() {
                    s.active_window = Some(wid);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            HitTarget::Session(n) => {
                self.switch_session(n);
            }
            HitTarget::NewSession => {
                self.new_session();
            }
            HitTarget::NewTab => {
                self.tmux_cmd("new-window");
            }
            HitTarget::Min => {
                if let Some(w) = &self.window {
                    w.set_minimized(true);
                }
            }
            HitTarget::MaxToggle => {
                if let Some(w) = &self.window {
                    w.set_maximized(!w.is_maximized());
                }
            }
            HitTarget::Close => {
                if let Some(w) = &self.window {
                    // Mimic CloseRequested.
                    let _ = w;
                    std::process::exit(0);
                }
            }
            HitTarget::Drag => {
                // Hand off to winit / OS — on Wayland (WSLg) clients
                // CANNOT programmatically reposition themselves, so our
                // own anchor-tracked drag was a no-op. drag_window() is
                // the only path that actually moves the surface.
                if let Some(w) = &self.window {
                    let _ = w.drag_window();
                }
            }
            HitTarget::ToggleSidebar => {
                self.sidebar_open = !self.sidebar_open;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            HitTarget::Title(pid) => {
                if let Some(tab) = self.active_tab() {
                    if let Some(fp) = tab.floating.get(&pid).cloned() {
                        self.drag = Some(DragState::Move {
                            pane_id: pid.clone(),
                            offset_x: x - fp.x,
                            offset_y: y - fp.y,
                        });
                    }
                }
                self.activate_pane(&pid);
            }
            HitTarget::Resize(pid, edge) => {
                if let Some(tab) = self.active_tab() {
                    if let Some(fp) = tab.floating.get(&pid).cloned() {
                        self.drag = Some(DragState::Resize {
                            pane_id: pid.clone(),
                            edge,
                            start_x: fp.x,
                            start_y: fp.y,
                            start_w: fp.w,
                            start_h: fp.h,
                            start_mouse: (x, y),
                        });
                    }
                }
                self.activate_pane(&pid);
            }
            HitTarget::Body(pid) => {
                self.activate_pane(&pid);
                // Begin text selection. Multi-click (within 400ms and close
                // pixels) escalates: 1=char, 2=word, 3=line. Shift+click
                // extends the existing selection's end instead of resetting.
                let (mx, my) = self.mouse;
                let now = Instant::now();
                let click_count = match self.last_body_click {
                    Some((t, lx, ly, n)) if now.duration_since(t) < Duration::from_millis(400)
                        && (lx - mx).abs() < 6.0 && (ly - my).abs() < 6.0 => (n + 1).min(3),
                    _ => 1,
                };
                self.last_body_click = Some((now, mx, my, click_count));
                let mode = match click_count {
                    1 => SelectionMode::Char,
                    2 => SelectionMode::Word,
                    _ => SelectionMode::Line,
                };
                if let Some(cell) = self.body_pixel_to_cell(&pid, mx, my) {
                    if self.shift {
                        if let Some(sel) = self.selection.as_mut() {
                            if sel.pane_id == pid {
                                sel.end = cell;
                            }
                        }
                    } else {
                        self.selection = Some(Selection {
                            pane_id: pid.clone(),
                            anchor: cell,
                            end: cell,
                            mode,
                        });
                        if mode != SelectionMode::Char {
                            // Expand to whole word/line immediately so even a
                            // bare double/triple-click (no drag) highlights.
                            self.expand_selection_mode();
                        }
                    }
                    self.drag = Some(DragState::Select { pane_id: pid.clone() });
                }
            }
            HitTarget::TaskbarPane(pid) => {
                // activate_pane sets active_pane, and the render order
                // sorts active=last → naturally raises the pane.
                self.activate_pane(&pid);
            }
            HitTarget::Icon(_) => {
                // Single click handled (early-return) above; double opens.
            }
            HitTarget::PaneClose(pid) => {
                self.tmux_cmd(&format!("kill-pane -t {pid}"));
            }
            HitTarget::TabClose(wid) => {
                self.tmux_cmd(&format!("kill-window -t {wid}"));
            }
            HitTarget::WindowEdgeResize(dir) => {
                if let Some(w) = &self.window {
                    let _ = w.drag_resize_window(dir);
                }
            }
            HitTarget::SidebarResize => {
                self.drag = Some(DragState::SidebarResize);
            }
            HitTarget::SessionClose(n) => {
                let name = self.sessions.get(&n).map(|s| s.tmux.session_name.clone());
                if let Some(name) = name {
                    let _ = std::process::Command::new("tmux")
                        .args(["-L", "tmuxify", "kill-session", "-t", &name])
                        .status();
                    self.sessions.remove(&n);
                    if self.active_session == n {
                        self.active_session = self.sessions.keys().next().copied().unwrap_or(1);
                        self.ensure_session(self.active_session);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
    }

    fn active_tab(&self) -> Option<&WindowTab> {
        let s = self.active()?;
        s.active_window.as_ref().and_then(|w| s.windows.get(w))
    }

    fn active_tab_mut(&mut self) -> Option<&mut WindowTab> {
        let wid = self.active()?.active_window.clone()?;
        let s = self.active_mut()?;
        s.windows.get_mut(&wid)
    }

    fn activate_pane(&mut self, pane_id: &str) {
        if let Some(tab) = self.active_tab_mut() {
            tab.active_pane = Some(pane_id.to_string());
        }
        self.tmux_cmd(&format!("select-pane -t {pane_id}"));
        // Kick the running app (claude code, vim, etc.) with a fake
        // resize so it does a full clear+redraw. -y -1 shrinks one row,
        // -y +1 grows it back. SIGWINCH fires on both, which is what
        // forces TUI apps to repaint — `refresh-client` alone wouldn't.
        self.tmux_cmd(&format!("resize-pane -t {pane_id} -y -1"));
        self.tmux_cmd(&format!("resize-pane -t {pane_id} -y +1"));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn handle_mouse_move(&mut self) {
        let Some(drag) = self.drag.clone() else { return };
        let (x, _y) = self.mouse;
        if let DragState::SidebarResize = &drag {
            self.sidebar_w = x.clamp(160.0, 480.0);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        if let DragState::WindowMove { anchor } = &drag {
            self.handle_window_drag_move(*anchor);
            return;
        }
        if let DragState::Select { pane_id } = &drag {
            let (mx, my) = self.mouse;
            if let Some(cell) = self.body_pixel_to_cell(pane_id, mx, my) {
                if let Some(sel) = self.selection.as_mut() {
                    sel.end = cell;
                }
                // Re-expand to word/line boundaries if the original click
                // was a double/triple — so dragging extends one word at a
                // time rather than one char at a time.
                self.expand_selection_mode();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        let (x, y) = self.mouse;
        // Live resize-pane command for the inner app — recompute cell
        // grid from the new pane geometry and only push to tmux when
        // (cols, rows) actually changed since last send. Otherwise tmux
        // sees a flood of identical commands.
        let (cw, ch) = self.cell_metrics();
        let mut live_resize: Option<(String, u16, u16)> = None;
        let Some(tab) = self.active_tab_mut() else { return };
        match drag {
            DragState::SidebarResize => {}
            DragState::Move { pane_id, offset_x, offset_y } => {
                if let Some(fp) = tab.floating.get_mut(&pane_id) {
                    fp.x = (x - offset_x).max(0.0);
                    fp.y = (y - offset_y).max(render::SESSION_BAR_HEIGHT);
                }
            }
            DragState::Resize {
                pane_id,
                edge,
                start_x,
                start_y,
                start_w,
                start_h,
                start_mouse,
            } => {
                if let Some(fp) = tab.floating.get_mut(&pane_id) {
                    let dx = x - start_mouse.0;
                    let dy = y - start_mouse.1;
                    let min_w: f32 = 120.0;
                    let min_h: f32 = 80.0;
                    let max_y_for_top = start_y + start_h - min_h;
                    let max_x_for_left = start_x + start_w - min_w;
                    if edge.affects_left() {
                        let new_x = (start_x + dx).min(max_x_for_left).max(0.0);
                        let new_w = (start_x + start_w - new_x).max(min_w);
                        fp.x = new_x;
                        fp.w = new_w;
                    } else if edge.affects_right() {
                        fp.w = (start_w + dx).max(min_w);
                    }
                    if edge.affects_top() {
                        let new_y = (start_y + dy)
                            .min(max_y_for_top)
                            .max(render::SESSION_BAR_HEIGHT);
                        let new_h = (start_y + start_h - new_y).max(min_h);
                        fp.y = new_y;
                        fp.h = new_h;
                    } else if edge.affects_bottom() {
                        fp.h = (start_h + dy).max(min_h);
                    }
                    // Live tmux grid update: recompute cols/rows and
                    // queue a resize-window for the throttle gate below.
                    let cols = ((fp.w - 16.0) / cw).floor().max(20.0) as u16;
                    let rows = ((fp.h - 34.0) / ch).floor().max(5.0) as u16;
                    if self.last_live_resize != Some((pane_id.clone(), cols, rows)) {
                        live_resize = Some((pane_id.clone(), cols, rows));
                    }
                }
            }
            DragState::WindowMove { .. } => {
                // Handled by the early-return at the top of this fn.
                unreachable!();
            }
            DragState::Select { .. } => {
                // Handled by the early-return at the top of this fn.
                unreachable!();
            }
        }
        if let Some((pid, cols, rows)) = live_resize {
            // Throttle to ~30 Hz — the cell grid doesn't change every
            // pixel anyway (cell_w == 8), and tmux + claude can't keep
            // up with 60-Hz SIGWINCH storms. Skipped commands are fine
            // because the final size is reissued on mouse-release.
            let now = Instant::now();
            let ready = self
                .last_live_resize_at
                .map(|t| now.duration_since(t) >= Duration::from_millis(33))
                .unwrap_or(true);
            if ready {
                self.last_live_resize = Some((pid.clone(), cols, rows));
                self.last_live_resize_at = Some(now);
                self.tmux_cmd(&format!("resize-window -t {pid} -x {cols} -y {rows}"));
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Forward the vertical wheel delta to the inner app via SGR mouse
    /// wheel events at the hovered cell. Each "line" of delta emits one
    /// button-64 (up) or 65 (down) press. tmux passes these through to
    /// the focused TUI when its mouse-mode is on; for plain shells they
    /// just get swallowed, which matches XTerm/Alacritty behaviour.
    fn handle_scroll(&mut self, lines: f32) {
        if lines == 0.0 {
            return;
        }
        let steps = lines.abs().max(1.0) as u32;
        let button = if lines > 0.0 { 64 } else { 65 };
        let (mx, my) = self.mouse;
        let (cw, ch) = self.cell_metrics();
        // Find the floating pane under the cursor and compute (col, row)
        // inside its body. SGR mouse coords are 1-based.
        let Some(s) = self.active() else { return };
        let Some(wid) = s.active_window.clone() else { return };
        let Some(tab) = s.windows.get(&wid) else { return };
        let mut hovered: Option<(String, u16, u16)> = None;
        for fp in tab.floating.values() {
            if mx >= fp.x && mx <= fp.x + fp.w && my >= fp.y && my <= fp.y + fp.h {
                let body_x = mx - fp.x - 8.0;
                let body_y = my - fp.y - 26.0;
                if body_x >= 0.0 && body_y >= 0.0 {
                    let col = (body_x / cw).floor().max(0.0) as u16 + 1;
                    let row = (body_y / ch).floor().max(0.0) as u16 + 1;
                    hovered = Some((fp.pane_id.clone(), col, row));
                    break;
                }
            }
        }
        let Some((pid, col, row)) = hovered else { return };
        // Activate the pane so tmux routes the mouse event to it.
        let active = tab.active_pane.clone();
        if active.as_deref() != Some(pid.as_str()) {
            self.activate_pane(&pid);
        }
        // SGR mouse press sequence: ESC [ < button ; col ; row M
        for _ in 0..steps {
            let seq = format!("\x1b[<{button};{col};{row}M");
            self.send_bytes(seq.as_bytes());
        }
    }

    fn update_cursor(&self) {
        let Some(win) = &self.window else { return };
        let (x, y) = self.mouse;
        let icon = match self.hit_test(x, y) {
            Some(HitTarget::WindowEdgeResize(dir)) => match dir {
                ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
                ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
                ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
                ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
            },
            Some(HitTarget::Resize(_, edge)) => match edge {
                PaneEdge::N | PaneEdge::S => CursorIcon::NsResize,
                PaneEdge::E | PaneEdge::W => CursorIcon::EwResize,
                PaneEdge::NE | PaneEdge::SW => CursorIcon::NeswResize,
                PaneEdge::NW | PaneEdge::SE => CursorIcon::NwseResize,
            },
            Some(HitTarget::SidebarResize) => CursorIcon::EwResize,
            Some(HitTarget::Title(_)) | Some(HitTarget::Drag) => CursorIcon::Move,
            Some(HitTarget::Body(_)) => CursorIcon::Text,
            Some(HitTarget::Tab(_))
            | Some(HitTarget::NewTab)
            | Some(HitTarget::Session(_))
            | Some(HitTarget::NewSession)
            | Some(HitTarget::SessionClose(_))
            | Some(HitTarget::PaneClose(_))
            | Some(HitTarget::TabClose(_))
            | Some(HitTarget::Min)
            | Some(HitTarget::MaxToggle)
            | Some(HitTarget::Close)
            | Some(HitTarget::ToggleSidebar)
            | Some(HitTarget::Icon(_))
            | Some(HitTarget::TaskbarPane(_)) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        };
        win.set_cursor(icon);
    }

    fn handle_mouse_release(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        self.last_live_resize = None;
        self.last_live_resize_at = None;
        match drag {
            DragState::Resize { pane_id, .. } => {
                let (cw, ch) = self.cell_metrics();
                let cmd = self
                    .active_tab()
                    .and_then(|t| t.floating.get(&pane_id))
                    .map(|fp| {
                        let cols = ((fp.w - 16.0) / cw).floor().max(20.0) as u16;
                        let rows = ((fp.h - 34.0) / ch).floor().max(5.0) as u16;
                        format!("resize-window -t {pane_id} -x {cols} -y {rows}")
                    });
                if let Some(c) = cmd {
                    self.tmux_cmd(&c);
                    // Force the inner app to redraw from scratch —
                    // mid-drag SIGWINCH storms can leave partial cells
                    // from earlier grid sizes, refresh-client wipes them.
                    self.tmux_cmd("refresh-client");
                }
            }
            DragState::Move { pane_id, .. } => {
                self.maybe_snap_pane(&pane_id);
            }
            DragState::WindowMove { .. } => {
                // Apply whatever snap zone was previewed during the
                // drag, and clear the overlay.
                if let Some(zone) = self.snap_zone.take() {
                    self.apply_window_snap(zone);
                }
                self.pre_snap_rect = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// During a WindowMove drag — move the OS window so the cursor stays
    /// anchored to its press point, AND detect monitor-edge proximity
    /// for Aero-Snap-style preview.
    fn handle_window_drag_move(&mut self, anchor: (f32, f32)) {
        let Some(win) = self.window.clone() else { return };
        let (cx, cy) = self.mouse;
        let dx = cx - anchor.0;
        let dy = cy - anchor.1;
        if dx.abs() > 0.5 || dy.abs() > 0.5 {
            let pos = win.outer_position().ok();
            if let Some(p) = pos {
                let new_x = p.x + dx as i32;
                let new_y = p.y + dy as i32;
                let _ = win.set_outer_position(winit::dpi::PhysicalPosition::new(new_x, new_y));
            }
        }
        // Monitor-relative cursor for snap detection.
        let monitor = win
            .current_monitor()
            .or_else(|| win.primary_monitor());
        let new_zone = if let Some(m) = monitor {
            let mp = m.position();
            let ms = m.size();
            let scale = win.scale_factor() as f32;
            let outer = win
                .outer_position()
                .unwrap_or(winit::dpi::PhysicalPosition::new(0, 0));
            // Screen-space cursor (physical px).
            let screen_x = outer.x as f32 + cx * scale;
            let screen_y = outer.y as f32 + cy * scale;
            let edge: f32 = 24.0 * scale;
            let mx0 = mp.x as f32;
            let my0 = mp.y as f32;
            let mx1 = mx0 + ms.width as f32;
            let my1 = my0 + ms.height as f32;
            if screen_y <= my0 + edge {
                Some(SnapZone::Top)
            } else if screen_x <= mx0 + edge {
                Some(SnapZone::Left)
            } else if screen_x >= mx1 - edge {
                Some(SnapZone::Right)
            } else {
                let _ = my1;
                None
            }
        } else {
            None
        };
        if new_zone != self.snap_zone {
            self.snap_zone = new_zone;
            win.request_redraw();
        } else {
            win.request_redraw();
        }
    }

    fn apply_window_snap(&mut self, zone: SnapZone) {
        let Some(win) = self.window.clone() else { return };
        let monitor = win.current_monitor().or_else(|| win.primary_monitor());
        let Some(m) = monitor else { return };
        let mp = m.position();
        let ms = m.size();
        let (nx, ny, nw, nh) = match zone {
            SnapZone::Left => (mp.x, mp.y, ms.width / 2, ms.height),
            SnapZone::Right => (
                mp.x + (ms.width / 2) as i32,
                mp.y,
                ms.width / 2,
                ms.height,
            ),
            SnapZone::Top => (mp.x, mp.y, ms.width, ms.height),
        };
        let _ = win.set_outer_position(winit::dpi::PhysicalPosition::new(nx, ny));
        let _ = win.request_inner_size(winit::dpi::PhysicalSize::new(nw, nh));
    }

    /// Aero-Snap style edge tiling for inner panes. After a Move drag,
    /// if the cursor is within `edge` px of the canvas border, resize +
    /// reposition the pane to half-canvas (left/right) or full-canvas
    /// (top). Mouse release outside the snap zone leaves the pane alone.
    fn maybe_snap_pane(&mut self, pane_id: &str) {
        let (mx, my) = self.mouse;
        let edge: f32 = 20.0;
        let sidebar_w = if self.sidebar_open { self.sidebar_w } else { 0.0 };
        let (win_w, win_h) = match self.window.as_ref() {
            Some(w) => {
                let sz = w.inner_size();
                (sz.width as f32, sz.height as f32)
            }
            None => return,
        };
        let canvas_left = sidebar_w;
        let canvas_right = win_w;
        let canvas_top = render::SESSION_BAR_HEIGHT;
        let canvas_bottom = win_h - render::STATUS_HEIGHT;
        let canvas_w = (canvas_right - canvas_left).max(1.0);
        let canvas_h = (canvas_bottom - canvas_top).max(1.0);

        let new_rect: Option<(f32, f32, f32, f32)> = if my <= canvas_top + edge {
            // Top → maximize within canvas.
            Some((canvas_left, canvas_top, canvas_w, canvas_h))
        } else if mx <= canvas_left + edge {
            // Left → left half.
            Some((canvas_left, canvas_top, canvas_w * 0.5, canvas_h))
        } else if mx >= canvas_right - edge {
            // Right → right half.
            Some((
                canvas_left + canvas_w * 0.5,
                canvas_top,
                canvas_w * 0.5,
                canvas_h,
            ))
        } else {
            None
        };

        let Some((nx, ny, nw, nh)) = new_rect else { return };
        let (cw, ch) = self.cell_metrics();
        let cols = ((nw - 16.0) / cw).floor().max(20.0) as u16;
        let rows = ((nh - 34.0) / ch).floor().max(5.0) as u16;
        if let Some(tab) = self.active_tab_mut() {
            if let Some(fp) = tab.floating.get_mut(pane_id) {
                fp.x = nx;
                fp.y = ny;
                fp.w = nw;
                fp.h = nh;
            }
        }
        self.tmux_cmd(&format!("resize-window -t {pane_id} -x {cols} -y {rows}"));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("tmuxify")
            // Custom chrome. WSLg doesn't forward Windows Aero-Snap
            // preview to forwarded Linux surfaces, so we implement the
            // snap ourselves (see App::handle_window_drag_move).
            .with_decorations(false)
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 760.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));
        let renderer =
            pollster::block_on(Renderer::new(window.clone())).expect("renderer init");

        // First session always exists.
        self.ensure_session(1);
        self.active_session = 1;
        self.discover_external_sessions();
        // Tell every tmux client the (cols, rows) we actually show — the
        // default floating-pane size, NOT the OS-window size — so apps
        // wrap to the visible area instead of the whole 1100×700 canvas.
        let cols: u16 = 95;
        let rows: u16 = 28;
        let _ = renderer.cells_for_size(window.inner_size().width, window.inner_size().height);
        for s in self.sessions.values() {
            let _ = s.tmux.send_cmd("set -g window-size manual");
            let _ = s.tmux.send_cmd("set -g aggressive-resize on");
            // Pass mouse events (wheel, drag, click) through to the
            // inner app so our handle_scroll's SGR sequences are honored.
            let _ = s.tmux.send_cmd("set -g mouse on");
            let _ = s.tmux.send_cmd(&format!("set -g default-size {cols}x{rows}"));
            let _ = s
                .tmux
                .send_cmd(&format!("resize-window -A -x {cols} -y {rows}"));
            let _ = s.tmux.resize_client(cols, rows);
        }

        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    if let (Some(w), Some(h)) =
                        (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                    {
                        r.resize(w, h);
                    }
                }
                // Auto-fit panes to the new canvas. The model is
                // "single visible pane = full canvas". When the user
                // grows the OS window expecting the inner pane to
                // follow, this hands it the room. If there are
                // multiple panes in the active window they still each
                // refit to the canvas size (they overlap; the user can
                // rearrange manually).
                self.refit_panes_to_canvas(size.width as f32, size.height as f32);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(event);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let PhysicalPosition { x, y } = position;
                self.mouse = (x as f32, y as f32);
                self.update_cursor();
                self.handle_mouse_move();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => self.handle_mouse_press(),
                        ElementState::Released => self.handle_mouse_release(),
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
                };
                self.handle_scroll(lines);
            }
            WindowEvent::RedrawRequested => {
                let preedit = self.composer.preedit();
                let hangul_mode = self.hangul_mode;
                let sidebar_open = self.sidebar_open;
                let sidebar_w_for_render = self.sidebar_w;
                let active_session_n = self.active_session;
                let session_rows: Vec<(u8, String, usize)> = self
                    .sessions
                    .iter()
                    .map(|(k, s)| (*k, s.tmux.session_name.clone(), s.windows.len()))
                    .collect();
                let icons_clone = self.icons.clone();
                let cursor_visible = self.cursor_visible;
                let (active_window, active_pane, panes_clone, tabs_for_render, active_floating) =
                    match self.sessions.get(&active_session_n) {
                        Some(s) => {
                            let aw = s.active_window.clone();
                            let ap = aw
                                .as_ref()
                                .and_then(|w| s.windows.get(w))
                                .and_then(|t| t.active_pane.clone());
                            let pc = s.panes.clone();
                            let tabs: Vec<(String, String, BTreeMap<String, FloatingPane>)> = s
                                .windows
                                .iter()
                                .map(|(k, v)| (k.clone(), v.title.clone(), v.floating.clone()))
                                .collect();
                            let af = aw
                                .as_ref()
                                .and_then(|w| s.windows.get(w))
                                .map(|t| t.floating.clone())
                                .unwrap_or_default();
                            (aw, ap, pc, tabs, af)
                        }
                        None => (None, None, HashMap::new(), Vec::new(), BTreeMap::new()),
                    };
                let snap_overlay: Option<[f32; 4]> = self.snap_zone.map(|z| {
                    let w = self
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().width as f32)
                        .unwrap_or(0.0);
                    let h = self
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().height as f32)
                        .unwrap_or(0.0);
                    match z {
                        SnapZone::Left => [0.0, 0.0, w * 0.5, h],
                        SnapZone::Right => [w * 0.5, 0.0, w * 0.5, h],
                        SnapZone::Top => [0.0, 0.0, w, h],
                    }
                });
                let sel_for_render = self.selection.as_ref().map(|s| {
                    let (start, end) = s.ordered();
                    (s.pane_id.as_str(), start, end)
                });
                if let Some(r) = self.renderer.as_mut() {
                    let _ = r.render(
                        &active_floating,
                        &panes_clone,
                        active_pane.as_deref(),
                        &tabs_for_render,
                        active_window.as_deref(),
                        hangul_mode,
                        preedit.as_deref(),
                        sidebar_open,
                        &session_rows,
                        active_session_n,
                        &icons_clone,
                        cursor_visible,
                        sidebar_w_for_render,
                        snap_overlay,
                        sel_for_render,
                    );
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = Instant::now();
            self.poll_tmux();
        }
        // Auto-send (env-driven) — fire once after the elapsed delay.
        if let Some(at) = self.auto_send_at {
            if Instant::now() >= at {
                self.auto_send_at = None;
                if let Some(text) = self.auto_send_text.clone() {
                    let mut bytes = text.into_bytes();
                    bytes.push(b'\r');
                    self.send_bytes(&bytes);
                }
            }
        }
        // Auto-capture (env-driven) — fire once after the elapsed delay.
        if let Some(at) = self.auto_capture_at {
            if Instant::now() >= at {
                self.auto_capture_at = None;
                let path = self.capture_path.clone();
                if let Some(r) = self.renderer.as_mut() {
                    r.request_capture(path);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        if self.last_blink.elapsed() >= Duration::from_millis(500) {
            self.last_blink = Instant::now();
            self.cursor_visible = !self.cursor_visible;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL));
    }
}

fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn default_icons() -> Vec<DesktopIcon> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let projects = format!("{}/projects", home);
    let mut v = Vec::new();
    let row_x = render::SIDEBAR_W + 24.0;
    v.push(DesktopIcon {
        label: "Home".into(),
        kind: IconKind::Folder { cwd: home.clone() },
        x: row_x,
        y: render::SESSION_BAR_HEIGHT + 40.0,
    });
    if std::path::Path::new(&projects).exists() {
        v.push(DesktopIcon {
            label: "projects".into(),
            kind: IconKind::Folder { cwd: projects },
            x: row_x,
            y: render::SESSION_BAR_HEIGHT + 220.0,
        });
    }
    // Claude launcher — double-click spawns a new window and runs the
    // `claude` shell function (defined in the user's ~/.zshrc) inside
    // a fresh interactive login shell, so it Just Works™.
    v.push(DesktopIcon {
        label: "Claude".into(),
        kind: IconKind::Claude { cwd: home },
        x: row_x,
        y: render::SESSION_BAR_HEIGHT + 400.0,
    });
    v
}


fn main() -> Result<()> {
    let event_loop = EventLoop::new().context("event loop")?;
    let mut app = App::new();
    event_loop.run_app(&mut app).context("run_app")?;
    Ok(())
}
