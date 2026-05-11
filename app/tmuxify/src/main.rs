//! tmuxify — multi-pane terminal MVP.
//! winit + wgpu + glyphon for rendering, tmux-bridge for the shell,
//! hangul-ime for Korean input independent of OS IME.

mod quad;
mod render;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::dpi::PhysicalPosition;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

use hangul_ime::{dubeolsik, Composer};
use tmux_bridge::{
    parse_layout, Cell, Layout, ScreenUpdate, StartOptions, TmuxEvent, TmuxSession,
};

use render::{cells_for_size, Renderer};
// `render` is also referenced as a module path for CELL_W/CELL_H constants.

const POLL_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct PaneGrid {
    pub rows: u16,
    pub cols: u16,
    pub grid: Vec<String>,
    pub cursor_row: u16,
    pub cursor_col: u16,
}

/// Bottom-right resize handle hit area (square, in pixels).
const RESIZE_HANDLE: f32 = 14.0;
/// Title-bar height in pixels (kept in sync with renderer constant).
const TITLE_BAR: f32 = 22.0;

enum HitTarget {
    Title(String),
    Resize(String),
    Body(String),
}

#[derive(Debug, Clone)]
enum DragState {
    Move {
        window_id: String,
        offset_x: f32,
        offset_y: f32,
    },
    Resize {
        window_id: String,
        start_w: f32,
        start_h: f32,
        start_mouse: (f32, f32),
    },
}

/// Desktop-mode floating window. One per tmux window (single pane each).
#[derive(Debug, Clone)]
pub struct FloatingWindow {
    pub window_id: String,
    pub pane_id: Option<String>,
    pub title: String,
    /// Pixel coordinates inside the desktop canvas.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tmux: Option<TmuxSession>,
    /// pane_id ("%N") → grid + cursor.
    panes: HashMap<String, PaneGrid>,
    /// Desktop-mode floating windows, keyed by tmux window id ("@N").
    floating: BTreeMap<String, FloatingWindow>,
    active_window: Option<String>,
    active_pane: Option<String>,
    /// Counter for cascading initial positions.
    next_cascade: u32,
    mouse: (f32, f32),
    drag: Option<DragState>,
    hangul_mode: bool,
    composer: Composer,
    shift: bool,
    ctrl: bool,
    alt: bool,
    /// Most-recently-closed window ids, for Ctrl+Shift+T (Chrome-style
    /// reopen). Restoring cwd needs a display-message round-trip and
    /// isn't wired yet — for now we just open a fresh window.
    closed_windows: Vec<String>,
    last_poll: Instant,
    last_cwd_query: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            tmux: None,
            panes: HashMap::new(),
            floating: BTreeMap::new(),
            active_window: None,
            active_pane: None,
            next_cascade: 0,
            hangul_mode: false,
            composer: Composer::new(),
            shift: false,
            ctrl: false,
            alt: false,
            closed_windows: Vec::new(),
            mouse: (0.0, 0.0),
            drag: None,
            last_poll: Instant::now(),
            last_cwd_query: Instant::now() - Duration::from_secs(60),
        }
    }

    fn poll_tmux(&mut self) {
        let (screens, events, queries, due_for_query) = match &self.tmux {
            Some(t) => (
                t.screens.try_iter().collect::<Vec<_>>(),
                t.events.try_iter().collect::<Vec<_>>(),
                t.queries.try_iter().collect::<Vec<_>>(),
                self.last_cwd_query.elapsed() >= Duration::from_secs(2),
            ),
            None => return,
        };
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
                let _ = t.send_query("list-windows -a -F '#{window_id} #{pane_current_path}'");
            }
        }
        if any {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn apply_cwd_query(&mut self, lines: Vec<String>) {
        for line in lines {
            let mut it = line.splitn(2, ' ');
            let Some(wid) = it.next() else { continue };
            let Some(path) = it.next() else { continue };
            if let Some(fw) = self.floating.get_mut(wid) {
                let basename = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
                fw.title = format!("{basename}  ·  {path}");
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
        // If we know which window owns this pane, push the OSC title.
        if let Some(new_title) = u.title.as_ref() {
            for fw in self.floating.values_mut() {
                if fw.pane_id.as_deref() == Some(&u.pane_id) {
                    fw.title = new_title.clone();
                    break;
                }
            }
        }
        if self.active_pane.is_none() {
            self.active_pane = Some(u.pane_id);
        }
    }

    fn apply_event(&mut self, e: TmuxEvent) {
        match e {
            TmuxEvent::LayoutChange { window_id, layout } => {
                let first = layout.split_whitespace().next().unwrap_or("");
                match parse_layout(first) {
                    Ok(l) => {
                        // Desktop mode: track only the first leaf as the
                        // window's pane. (Splits inside a tmux window are
                        // discouraged; new terminals go as new windows.)
                        let first_pane = l.leaves().iter().find_map(|n| match n {
                            Layout::Pane { id, .. } => Some(format!("%{id}")),
                            _ => None,
                        });
                        let entry = self.ensure_window(&window_id);
                        if entry.pane_id.is_none() {
                            entry.pane_id = first_pane.clone();
                        }
                        self.active_window = Some(window_id);
                        if self.active_pane.is_none() {
                            self.active_pane = first_pane;
                        }
                    }
                    Err(err) => println!("[layout parse err] {err}"),
                }
            }
            TmuxEvent::WindowAdd { window_id } => {
                self.ensure_window(&window_id);
            }
            TmuxEvent::WindowClose { window_id } => {
                self.floating.remove(&window_id);
                self.closed_windows.push(window_id.clone());
                if self.active_window.as_deref() == Some(&window_id) {
                    self.active_window = self.floating.keys().next().cloned();
                }
            }
            TmuxEvent::WindowRenamed { window_id, name } => {
                if let Some(w) = self.floating.get_mut(&window_id) {
                    w.title = name;
                }
            }
            _ => {}
        }
    }

    fn ensure_window(&mut self, window_id: &str) -> &mut FloatingWindow {
        if !self.floating.contains_key(window_id) {
            let n = self.next_cascade;
            self.next_cascade += 1;
            let cascade = (n % 8) as f32;
            self.floating.insert(
                window_id.to_string(),
                FloatingWindow {
                    window_id: window_id.to_string(),
                    pane_id: None,
                    title: window_id.to_string(),
                    x: 60.0 + cascade * 30.0,
                    y: 60.0 + cascade * 30.0,
                    // Default ~80x24 chars in our cell metric.
                    w: 80.0 * render::CELL_W,
                    h: 24.0 * render::CELL_H,
                },
            );
        }
        self.floating.get_mut(window_id).unwrap()
    }

    fn send_bytes(&self, bytes: &[u8]) {
        if let Some(t) = &self.tmux {
            let mut hex = String::with_capacity(bytes.len() * 3);
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    hex.push(' ');
                }
                let _ = write!(hex, "{:02x}", b);
            }
            let _ = t.send_keys_hex(self.active_pane.as_deref(), &hex);
        }
    }

    fn tmux_cmd(&self, cmd: &str) {
        if let Some(t) = &self.tmux {
            let _ = t.send_cmd(cmd);
        }
    }

    /// Window in front-to-back z-order: active last (topmost), so we
    /// reverse for hit-testing.
    fn hit_test(&self, x: f32, y: f32) -> Option<HitTarget> {
        // Iterate in render order (active last) then reverse for topmost-first.
        let mut order: Vec<&FloatingWindow> = self.floating.values().collect();
        order.sort_by_key(|w| (self.active_window.as_deref() == Some(&w.window_id)) as u8);
        for fw in order.iter().rev() {
            // Resize handle.
            if x >= fw.x + fw.w - RESIZE_HANDLE
                && x <= fw.x + fw.w
                && y >= fw.y + fw.h - RESIZE_HANDLE
                && y <= fw.y + fw.h
            {
                return Some(HitTarget::Resize(fw.window_id.clone()));
            }
            // Title bar.
            if x >= fw.x && x <= fw.x + fw.w && y >= fw.y && y <= fw.y + TITLE_BAR {
                return Some(HitTarget::Title(fw.window_id.clone()));
            }
            // Body.
            if x >= fw.x && x <= fw.x + fw.w && y >= fw.y && y <= fw.y + fw.h {
                return Some(HitTarget::Body(fw.window_id.clone()));
            }
        }
        None
    }

    fn handle_mouse_press(&mut self) {
        let (x, y) = self.mouse;
        let Some(hit) = self.hit_test(x, y) else { return };
        match hit {
            HitTarget::Title(id) => {
                let fw = self.floating.get(&id).cloned();
                if let Some(fw) = fw {
                    self.drag = Some(DragState::Move {
                        window_id: id.clone(),
                        offset_x: x - fw.x,
                        offset_y: y - fw.y,
                    });
                    self.activate(&id);
                }
            }
            HitTarget::Resize(id) => {
                let fw = self.floating.get(&id).cloned();
                if let Some(fw) = fw {
                    self.drag = Some(DragState::Resize {
                        window_id: id.clone(),
                        start_w: fw.w,
                        start_h: fw.h,
                        start_mouse: (x, y),
                    });
                    self.activate(&id);
                }
            }
            HitTarget::Body(id) => {
                self.activate(&id);
            }
        }
    }

    fn activate(&mut self, window_id: &str) {
        self.active_window = Some(window_id.to_string());
        if let Some(fw) = self.floating.get(window_id) {
            self.active_pane = fw.pane_id.clone();
        }
        // Tell tmux too so future commands target the right window.
        self.tmux_cmd(&format!("select-window -t {window_id}"));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn handle_mouse_move(&mut self) {
        let Some(drag) = self.drag.clone() else { return };
        let (x, y) = self.mouse;
        match drag {
            DragState::Move { window_id, offset_x, offset_y } => {
                if let Some(fw) = self.floating.get_mut(&window_id) {
                    fw.x = (x - offset_x).max(0.0);
                    fw.y = (y - offset_y).max(0.0);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            DragState::Resize { window_id, start_w, start_h, start_mouse } => {
                if let Some(fw) = self.floating.get_mut(&window_id) {
                    let dx = x - start_mouse.0;
                    let dy = y - start_mouse.1;
                    fw.w = (start_w + dx).max(120.0);
                    fw.h = (start_h + dy).max(80.0);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
    }

    fn handle_mouse_release(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if let DragState::Resize { window_id, .. } = drag {
            // Commit the new tmux size for that window (cells).
            if let Some(fw) = self.floating.get(&window_id) {
                let cols = ((fw.w - 12.0) / render::CELL_W).floor().max(20.0) as u16;
                let rows = ((fw.h - TITLE_BAR - 6.0) / render::CELL_H).floor().max(5.0) as u16;
                self.tmux_cmd(&format!("resize-window -t {window_id} -x {cols} -y {rows}"));
            }
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
            // physical Right Alt is the Hangul toggle, not modifier.
            if !matches!(ev.physical_key, PhysicalKey::Code(KeyCode::AltRight)) {
                self.alt = pressed;
            }
            return;
        }
        if !pressed {
            return;
        }

        // Chrome-style window/tab shortcuts. Pane shortcuts use Alt.
        if self.ctrl {
            use PhysicalKey::Code as PC;
            // Ctrl + 1..9 → select tab N
            let tab_idx = match ev.physical_key {
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
            if let Some(n) = tab_idx {
                self.tmux_cmd(&format!("select-window -t :{n}"));
                return;
            }
            if self.shift {
                match ev.physical_key {
                    PC(KeyCode::KeyT) => {
                        // Reopen — tmux can't truly restore the killed pane;
                        // best-effort is a fresh window.
                        if self.closed_windows.pop().is_some() {
                            self.tmux_cmd("new-window");
                        }
                        return;
                    }
                    PC(KeyCode::KeyD) => {
                        self.tmux_cmd("split-window -v");
                        return;
                    }
                    PC(KeyCode::KeyW) => {
                        self.tmux_cmd("kill-window");
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
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("tmuxify")
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 700.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));
        let renderer =
            pollster::block_on(Renderer::new(window.clone())).expect("renderer init");

        let cwd = std::env::var("HOME").ok();
        match TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            auto_run: None,
            flush_interval: Duration::from_millis(33),
        }) {
            Ok(s) => {
                println!("[tmux] {}", s.session_name);
                self.tmux = Some(s);
            }
            Err(e) => println!("[tmux] start failed: {e}"),
        }
        // Tell tmux our actual cell-grid size based on initial window.
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
                if let Some(r) = self.renderer.as_mut() {
                    let _ = r.render(
                        &self.floating,
                        &self.panes,
                        self.active_window.as_deref(),
                        self.hangul_mode,
                        preedit.as_deref(),
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
