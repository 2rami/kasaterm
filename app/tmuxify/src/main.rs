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
const TITLE_BAR: f32 = 22.0;

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

#[derive(Debug)]
enum HitTarget {
    Title(String),
    Resize(String),
    Body(String),
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
    pub cwd: String,
    pub x: f32,
    pub y: f32,
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
        start_w: f32,
        start_h: f32,
        start_mouse: (f32, f32),
    },
    SidebarResize,
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
            flush_interval: Duration::from_millis(33),
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
        .args(["list-sessions", "-F", "#{session_name}"])
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
        }
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
            for e in events {
                Self::apply_event(self.sessions.get_mut(&k).unwrap(), e);
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
        if let Some(t) = u.title.as_ref() {
            for tab in s.windows.values_mut() {
                if let Some(fp) = tab.floating.get_mut(&u.pane_id) {
                    fp.title = t.clone();
                    break;
                }
            }
        }
    }

    fn apply_event(s: &mut SessionState, e: TmuxEvent) {
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
                // Add new panes, preserve existing positions.
                for pid in &leaves {
                    if !tab.floating.contains_key(pid) {
                        let n = tab.next_cascade;
                        tab.next_cascade += 1;
                        let cascade = (n % 8) as f32;
                        // Cap cascade at 4 steps so newly-spawned panes
                        // stay on-screen even after many opens.
                        let step = (n % 4) as f32;
                        tab.floating.insert(
                            pid.clone(),
                            FloatingPane {
                                pane_id: pid.clone(),
                                title: pid.clone(),
                                x: render::SIDEBAR_W + 130.0 + step * 30.0,
                                y: render::SESSION_BAR_HEIGHT + 30.0 + step * 30.0,
                                w: 92.0 * render::CELL_W,
                                h: 28.0 * render::CELL_H,
                            },
                        );
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
            if let Some(tab) = s.windows.get_mut(wid) {
                if let Some(fp) = tab.floating.get_mut(pid) {
                    fp.title = format!("{basename}  ·  {path}");
                }
                if tab.active_pane.as_deref() == Some(pid) {
                    tab.title = basename.to_string();
                }
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

    fn spawn_new_window(&self) {
        // New OS-level tmuxify window = new process, fresh tmux session.
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
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
            if !matches!(ev.physical_key, PhysicalKey::Code(KeyCode::AltRight)) {
                self.alt = pressed;
            }
            return;
        }
        if !pressed {
            return;
        }

        // Hangul toggle.
        let is_right_alt = matches!(ev.physical_key, PhysicalKey::Code(KeyCode::AltRight));
        let is_hangul_toggle = is_right_alt
            || matches!(&ev.logical_key, Key::Named(NamedKey::HangulMode))
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
            self.send_bytes(&[0x7f]);
            return;
        }

        // Window/tab/pane shortcuts.
        if self.ctrl {
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

        // Named keys → ANSI bytes.
        if let Key::Named(named) = &ev.logical_key {
            let key_bytes: &[u8] = match named {
                NamedKey::Enter => &[0x0d],
                NamedKey::Space => &[b' '],
                NamedKey::Tab => &[0x09],
                NamedKey::Escape => &[0x1b],
                NamedKey::ArrowUp => &[0x1b, b'[', b'A'],
                NamedKey::ArrowDown => &[0x1b, b'[', b'B'],
                NamedKey::ArrowRight => &[0x1b, b'[', b'C'],
                NamedKey::ArrowLeft => &[0x1b, b'[', b'D'],
                NamedKey::Home => &[0x1b, b'[', b'H'],
                NamedKey::End => &[0x1b, b'[', b'F'],
                NamedKey::Delete => &[0x1b, b'[', b'3', b'~'],
                _ => &[],
            };
            if !key_bytes.is_empty() {
                if let Some(s) = self.composer.flush() {
                    self.send_bytes(s.as_bytes());
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
        let edge = 6.0;
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
        // Sidebar zone (below title bar, left column).
        if self.sidebar_open && x < sidebar_w && y >= render::SESSION_BAR_HEIGHT {
            let row_h = 60.0;
            let row_gap = 4.0;
            let first_row_y = render::SESSION_BAR_HEIGHT + 14.0 + 30.0 + 14.0;
            let mut row_y = first_row_y;
            // Each row's right side has a 24px "×" hit area.
            let close_w = 24.0;
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
            let new_h = 36.0;
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
            let btn_w = 32.0;
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
            for fp in order.iter().rev() {
                if x >= fp.x + fp.w - RESIZE_HANDLE
                    && x <= fp.x + fp.w
                    && y >= fp.y + fp.h - RESIZE_HANDLE
                    && y <= fp.y + fp.h
                {
                    return Some(HitTarget::Resize(fp.pane_id.clone()));
                }
                let cx = fp.x + fp.w - 22.0;
                if x >= cx
                    && x <= fp.x + fp.w - 4.0
                    && y >= fp.y + 4.0
                    && y <= fp.y + TITLE_BAR - 2.0
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
                return Some(HitTarget::Resize(fp.pane_id.clone()));
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
        self.last_click = Some((now, x, y));
        let hit = self.hit_test(x, y);
        println!("[click] xy=({x:.0},{y:.0}) double={is_double} hit={hit:?}");
        let Some(hit) = hit else { return };
        if let HitTarget::Icon(idx) = &hit {
            if is_double {
                if let Some(ic) = self.icons.get(*idx) {
                    // New "창" = new pane in the active window.
                    let cmd = format!("split-window -c {}", shell_quote(&ic.cwd));
                    self.tmux_cmd(&cmd);
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
            HitTarget::Resize(pid) => {
                if let Some(tab) = self.active_tab() {
                    if let Some(fp) = tab.floating.get(&pid).cloned() {
                        self.drag = Some(DragState::Resize {
                            pane_id: pid.clone(),
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
            }
            HitTarget::Icon(_) => {
                // Single click handled (early-return) above; double opens.
            }
            HitTarget::PaneClose(pid) => {
                self.tmux_cmd(&format!("kill-pane -t {pid}"));
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
                        .args(["kill-session", "-t", &name])
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
        let (x, y) = self.mouse;
        let Some(tab) = self.active_tab_mut() else { return };
        match drag {
            DragState::SidebarResize => {}
            DragState::Move { pane_id, offset_x, offset_y } => {
                if let Some(fp) = tab.floating.get_mut(&pane_id) {
                    fp.x = (x - offset_x).max(0.0);
                    fp.y = (y - offset_y).max(render::SESSION_BAR_HEIGHT);
                }
            }
            DragState::Resize { pane_id, start_w, start_h, start_mouse } => {
                if let Some(fp) = tab.floating.get_mut(&pane_id) {
                    let dx = x - start_mouse.0;
                    let dy = y - start_mouse.1;
                    fp.w = (start_w + dx).max(120.0);
                    fp.h = (start_h + dy).max(80.0);
                }
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
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
            Some(HitTarget::Resize(_)) => CursorIcon::NwseResize,
            Some(HitTarget::SidebarResize) => CursorIcon::EwResize,
            Some(HitTarget::Title(_)) | Some(HitTarget::Drag) => CursorIcon::Move,
            Some(HitTarget::Body(_)) => CursorIcon::Text,
            Some(HitTarget::Tab(_))
            | Some(HitTarget::NewTab)
            | Some(HitTarget::Session(_))
            | Some(HitTarget::NewSession)
            | Some(HitTarget::SessionClose(_))
            | Some(HitTarget::PaneClose(_))
            | Some(HitTarget::Min)
            | Some(HitTarget::MaxToggle)
            | Some(HitTarget::Close)
            | Some(HitTarget::ToggleSidebar)
            | Some(HitTarget::Icon(_)) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        };
        win.set_cursor(icon);
    }

    fn handle_mouse_release(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if let DragState::Resize { pane_id, .. } = drag {
            let cmd = self
                .active_tab()
                .and_then(|t| t.floating.get(&pane_id))
                .map(|fp| {
                    let cols = ((fp.w - 12.0) / render::CELL_W).floor().max(20.0) as u16;
                    let rows = ((fp.h - TITLE_BAR - 6.0) / render::CELL_H).floor().max(5.0) as u16;
                    format!("resize-pane -t {pane_id} -x {cols} -y {rows}")
                });
            if let Some(c) = cmd {
                self.tmux_cmd(&c);
            }
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
            .with_decorations(false)
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 700.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));
        let renderer =
            pollster::block_on(Renderer::new(window.clone())).expect("renderer init");

        // First session always exists.
        self.ensure_session(1);
        self.active_session = 1;
        self.discover_external_sessions();
        let (cols, rows) = renderer.cells_for_size(window.inner_size().width, window.inner_size().height);
        for s in self.sessions.values() {
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
                let (cols, rows) = self
                    .renderer
                    .as_ref()
                    .map(|r| r.cells_for_size(size.width, size.height))
                    .unwrap_or((80, 24));
                for s in self.sessions.values() {
                    let _ = s.tmux.resize_client(cols, rows);
                }
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
    v.push(DesktopIcon {
        label: "Home".into(),
        cwd: home.clone(),
        x: render::SIDEBAR_W + 24.0,
        y: render::SESSION_BAR_HEIGHT + 40.0,
    });
    if std::path::Path::new(&projects).exists() {
        v.push(DesktopIcon {
            label: "projects".into(),
            cwd: projects,
            x: render::SIDEBAR_W + 24.0,
            y: render::SESSION_BAR_HEIGHT + 130.0,
        });
    }
    v
}


fn main() -> Result<()> {
    let event_loop = EventLoop::new().context("event loop")?;
    let mut app = App::new();
    event_loop.run_app(&mut app).context("run_app")?;
    Ok(())
}
