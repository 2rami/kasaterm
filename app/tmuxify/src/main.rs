//! tmuxify — single-pane terminal MVP.
//! winit + wgpu + glyphon for rendering, tmux-bridge for the shell,
//! hangul-ime for Korean input independent of OS IME.

mod render;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

use hangul_ime::{dubeolsik, Composer};
use tmux_bridge::{Cell, ScreenUpdate, StartOptions, TmuxSession};

use render::Renderer;

const POLL_INTERVAL: Duration = Duration::from_millis(16);

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tmux: Option<TmuxSession>,
    pane_id: Option<String>,
    grid: Vec<String>,
    cursor_row: u16,
    cursor_col: u16,
    /// Hangul mode toggle (Shift+Space).
    hangul_mode: bool,
    composer: Composer,
    shift: bool,
    last_poll: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            tmux: None,
            pane_id: None,
            grid: vec!["(connecting…)".into()],
            cursor_row: 0,
            cursor_col: 0,
            hangul_mode: false,
            composer: Composer::new(),
            shift: false,
            last_poll: Instant::now(),
        }
    }

    fn poll_tmux(&mut self) {
        let Some(t) = &self.tmux else { return };
        let drained: Vec<ScreenUpdate> = t.screens.try_iter().collect();
        if drained.is_empty() {
            return;
        }
        for u in drained {
            self.apply(u);
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn apply(&mut self, u: ScreenUpdate) {
        if self.pane_id.is_none() {
            self.pane_id = Some(u.pane_id.clone());
        }
        if self.pane_id.as_deref() != Some(&u.pane_id) {
            return;
        }
        if self.grid.len() != u.rows as usize {
            self.grid = vec![" ".repeat(u.cols as usize); u.rows as usize];
        }
        for (i, row) in u.dirty {
            if (i as usize) < self.grid.len() {
                self.grid[i as usize] = render_row(&row);
            }
        }
        self.cursor_row = u.cursor_row;
        self.cursor_col = u.cursor_col;
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
            let _ = t.send_keys_hex(self.pane_id.as_deref(), &hex);
        }
    }

    fn handle_key(&mut self, ev: KeyEvent) {
        let pressed = ev.state == ElementState::Pressed;
        if let Key::Named(NamedKey::Shift) = &ev.logical_key {
            self.shift = pressed;
            return;
        }
        if !pressed {
            return;
        }

        // Hangul toggle: WSLg routes the real 한/영 key as Right Alt,
        // which matches Korean keyboard convention anyway. Also accept
        // the rare HangulMode named key, and Shift+Space fallback.
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
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
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

        // Inform tmux of our terminal size — pick reasonable defaults; the
        // real value will be derived from window size after first redraw.
        if let Some(t) = &self.tmux {
            let _ = t.resize_client(80, 24);
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
                        &self.grid,
                        self.cursor_row,
                        self.cursor_col,
                        self.hangul_mode,
                        preedit.as_deref(),
                    );
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Light polling — drain tmux ScreenUpdate queue periodically.
        if self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = Instant::now();
            self.poll_tmux();
        }
        // Wake up again soon to keep polling smooth.
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
