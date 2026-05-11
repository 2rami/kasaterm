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
use winit::window::{Window, WindowId};

use hangul_ime::{dubeolsik, Composer};
use tmux_bridge::{
    parse_layout, Cell, Layout, ScreenUpdate, StartOptions, TmuxEvent, TmuxSession,
};

use render::{cells_for_size, Renderer};

const POLL_INTERVAL: Duration = Duration::from_millis(16);
const RESIZE_HANDLE: f32 = 14.0;
const TITLE_BAR: f32 = 22.0;

#[derive(Debug, Default, Clone)]
pub struct PaneGrid {
    pub rows: u16,
    pub cols: u16,
    pub grid: Vec<String>,
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
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tmux: Option<TmuxSession>,
    /// pane_id ("%N") → grid + cursor. All panes across all tabs.
    panes: HashMap<String, PaneGrid>,
    /// window_id ("@N") → tab state.
    windows: BTreeMap<String, WindowTab>,
    active_window: Option<String>,
    mouse: (f32, f32),
    drag: Option<DragState>,
    hangul_mode: bool,
    composer: Composer,
    shift: bool,
    ctrl: bool,
    alt: bool,
    last_poll: Instant,
    last_cwd_query: Instant,
    sidebar_open: bool,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            tmux: None,
            panes: HashMap::new(),
            windows: BTreeMap::new(),
            active_window: None,
            mouse: (0.0, 0.0),
            drag: None,
            hangul_mode: false,
            composer: Composer::new(),
            shift: false,
            ctrl: false,
            alt: false,
            last_poll: Instant::now(),
            last_cwd_query: Instant::now() - Duration::from_secs(60),
            sidebar_open: true,
        }
    }

    fn poll_tmux(&mut self) {
        let Some(t) = &self.tmux else { return };
        let screens: Vec<ScreenUpdate> = t.screens.try_iter().collect();
        let events: Vec<TmuxEvent> = t.events.try_iter().collect();
        let queries: Vec<Vec<String>> = t.queries.try_iter().collect();
        let due_for_query = self.last_cwd_query.elapsed() >= Duration::from_secs(2);
        let any = !screens.is_empty() || !events.is_empty() || !queries.is_empty();
        for u in screens {
            self.apply_screen(u);
        }
        for e in events {
            self.apply_event(e);
        }
        for resp in queries {
            self.apply_cwd_query(resp);
        }
        if due_for_query {
            self.last_cwd_query = Instant::now();
            if let Some(t) = &self.tmux {
                let _ = t.send_query("list-panes -s -F '#{window_id} #{pane_id} #{pane_current_path}'");
            }
        }
        if any {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn apply_screen(&mut self, u: ScreenUpdate) {
        let entry = self.panes.entry(u.pane_id.clone()).or_default();
        if entry.grid.len() != u.rows as usize || entry.cols != u.cols {
            entry.grid = vec![" ".repeat(u.cols as usize); u.rows as usize];
            entry.rows = u.rows;
            entry.cols = u.cols;
        }
        for (i, row) in u.dirty {
            if (i as usize) < entry.grid.len() {
                entry.grid[i as usize] = render_row(&row);
            }
        }
        entry.cursor_row = u.cursor_row;
        entry.cursor_col = u.cursor_col;
        // Push OSC title to the matching floating pane.
        if let Some(t) = u.title.as_ref() {
            for tab in self.windows.values_mut() {
                if let Some(fp) = tab.floating.get_mut(&u.pane_id) {
                    fp.title = t.clone();
                    break;
                }
            }
        }
    }

    fn apply_event(&mut self, e: TmuxEvent) {
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
                let tab = self.windows.entry(window_id.clone()).or_insert_with(|| {
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
                        tab.floating.insert(
                            pid.clone(),
                            FloatingPane {
                                pane_id: pid.clone(),
                                title: pid.clone(),
                                x: render::SIDEBAR_W + 60.0 + cascade * 30.0,
                                y: render::SESSION_BAR_HEIGHT + 30.0 + cascade * 30.0,
                                w: 80.0 * render::CELL_W,
                                h: 24.0 * render::CELL_H,
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
                if self.active_window.is_none() {
                    self.active_window = Some(window_id);
                }
            }
            TmuxEvent::WindowAdd { window_id } => {
                self.windows
                    .entry(window_id.clone())
                    .or_insert_with(|| WindowTab {
                        title: window_id.clone(),
                        ..Default::default()
                    });
            }
            TmuxEvent::WindowClose { window_id } => {
                self.windows.remove(&window_id);
                if self.active_window.as_deref() == Some(&window_id) {
                    self.active_window = self.windows.keys().next().cloned();
                }
            }
            TmuxEvent::WindowRenamed { window_id, name } => {
                if let Some(t) = self.windows.get_mut(&window_id) {
                    t.title = name;
                }
            }
            _ => {}
        }
    }

    fn apply_cwd_query(&mut self, lines: Vec<String>) {
        for line in lines {
            let mut it = line.splitn(3, ' ');
            let Some(wid) = it.next() else { continue };
            let Some(pid) = it.next() else { continue };
            let Some(path) = it.next() else { continue };
            let basename = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
            if let Some(tab) = self.windows.get_mut(wid) {
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
        let Some(t) = &self.tmux else { return };
        let target = self
            .active_window
            .as_ref()
            .and_then(|w| self.windows.get(w))
            .and_then(|tab| tab.active_pane.clone());
        let mut hex = String::with_capacity(bytes.len() * 3);
        for (i, b) in bytes.iter().enumerate() {
            if i > 0 {
                hex.push(' ');
            }
            let _ = write!(hex, "{:02x}", b);
        }
        let _ = t.send_keys_hex(target.as_deref(), &hex);
    }

    fn tmux_cmd(&self, cmd: &str) {
        if let Some(t) = &self.tmux {
            let _ = t.send_cmd(cmd);
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
                    PC(KeyCode::KeyT) | PC(KeyCode::KeyN) => {
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
        let sidebar_w = if self.sidebar_open {
            render::SIDEBAR_W
        } else {
            0.0
        };
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
            for wid in self.windows.keys() {
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
        // Floating panes (active window only) — topmost (active) first.
        let Some(wid) = self.active_window.as_ref() else { return None };
        let Some(tab) = self.windows.get(wid) else { return None };
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
            if x >= fp.x && x <= fp.x + fp.w && y >= fp.y && y <= fp.y + TITLE_BAR {
                return Some(HitTarget::Title(fp.pane_id.clone()));
            }
            if x >= fp.x && x <= fp.x + fp.w && y >= fp.y && y <= fp.y + fp.h {
                return Some(HitTarget::Body(fp.pane_id.clone()));
            }
        }
        None
    }

    fn handle_mouse_press(&mut self) {
        let (x, y) = self.mouse;
        let Some(hit) = self.hit_test(x, y) else { return };
        match hit {
            HitTarget::Tab(wid) => {
                self.tmux_cmd(&format!("select-window -t {wid}"));
                self.active_window = Some(wid);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
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
        }
    }

    fn active_tab(&self) -> Option<&WindowTab> {
        self.active_window
            .as_ref()
            .and_then(|w| self.windows.get(w))
    }

    fn active_tab_mut(&mut self) -> Option<&mut WindowTab> {
        let wid = self.active_window.clone()?;
        self.windows.get_mut(&wid)
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
        let (x, y) = self.mouse;
        let Some(tab) = self.active_tab_mut() else { return };
        match drag {
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

        // Each OS window owns its own tmux session; name uniquely per pid.
        let cwd = std::env::var("HOME").ok();
        let name = format!("tmuxify-{}", std::process::id());
        match TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            auto_run: None,
            session_name: Some(&name),
            flush_interval: Duration::from_millis(33),
        }) {
            Ok(s) => {
                println!("[tmux] {}", s.session_name);
                self.tmux = Some(s);
            }
            Err(e) => println!("[tmux] start failed: {e}"),
        }
        let (cols, rows) = cells_for_size(window.inner_size().width, window.inner_size().height);
        if let Some(t) = &self.tmux {
            let _ = t.resize_client(cols, rows);
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
                let (cols, rows) = cells_for_size(size.width, size.height);
                if let Some(t) = &self.tmux {
                    let _ = t.resize_client(cols, rows);
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
                let active_window = self.active_window.clone();
                let active_pane = self
                    .active_window
                    .as_ref()
                    .and_then(|w| self.windows.get(w))
                    .and_then(|t| t.active_pane.clone());
                let panes_clone = self.panes.clone();
                let tabs_for_render: Vec<(String, String, BTreeMap<String, FloatingPane>)> = self
                    .windows
                    .iter()
                    .map(|(k, v)| (k.clone(), v.title.clone(), v.floating.clone()))
                    .collect();
                let active_floating = active_window
                    .as_ref()
                    .and_then(|w| self.windows.get(w))
                    .map(|t| t.floating.clone())
                    .unwrap_or_default();
                let hangul_mode = self.hangul_mode;
                let sidebar_open = self.sidebar_open;
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
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL));
    }
}

fn render_row(cells: &[Cell]) -> String {
    let mut s = String::with_capacity(cells.len());
    for c in cells {
        if c.ch.is_empty() {
            s.push(' ');
        } else {
            s.push_str(&c.ch);
        }
    }
    s
}

fn main() -> Result<()> {
    let event_loop = EventLoop::new().context("event loop")?;
    let mut app = App::new();
    event_loop.run_app(&mut app).context("run_app")?;
    Ok(())
}
