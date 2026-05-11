//! tmuxify — multi-pane terminal MVP.
//! winit + wgpu + glyphon for rendering, tmux-bridge for the shell,
//! hangul-ime for Korean input independent of OS IME.

mod render;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
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
            last_poll: Instant::now(),
        }
    }

    fn poll_tmux(&mut self) {
        let Some(t) = &self.tmux else { return };
        let screens: Vec<ScreenUpdate> = t.screens.try_iter().collect();
        let events: Vec<TmuxEvent> = t.events.try_iter().collect();
        let any = !screens.is_empty() || !events.is_empty();
        for u in screens {
            self.apply_screen(u);
        }
        for e in events {
            self.apply_event(e);
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
