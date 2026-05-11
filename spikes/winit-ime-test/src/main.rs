//! Bare-minimum winit IME smoke test. softbuffer paints a blank dark
//! frame so the Wayland surface actually becomes visible. No real
//! rendering — events go to stdout. Goal: confirm that
//! `set_ime_allowed(true)` + standard winit IME handling delivers
//! Hangul commits under WSLg+ibus.

mod hangul;

use std::num::NonZeroU32;
use std::rc::Rc;

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use hangul::{dubeolsik, Composer};
use tmux_bridge::{StartOptions, TmuxSession};

use std::fmt::Write as _;
use std::time::Duration;

#[derive(Default)]
struct App {
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    hangul_mode: bool,
    composer: Composer,
    shift: bool,
    tmux: Option<TmuxSession>,
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{:02x}", b);
    }
    s
}

impl App {
    fn send_to_tmux(&self, bytes: &[u8]) {
        if let Some(t) = &self.tmux {
            let hex = bytes_to_hex(bytes);
            if let Err(e) = t.send_keys_hex(None, &hex) {
                println!("[tmux send err] {e}");
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
            .with_title("winit IME smoke — type Hangul, watch stdout")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0, 200.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create_window"));
        window.set_ime_allowed(true);
        let context = Context::new(window.clone()).expect("softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("softbuffer surface");
        println!("[setup] window created, set_ime_allowed(true) called");
        let cwd = std::env::var("HOME").ok();
        match TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            auto_run: None,
            flush_interval: Duration::from_millis(50),
        }) {
            Ok(s) => {
                println!(
                    "[tmux] attached: {}  (open another terminal: tmux attach -t {})",
                    s.session_name, s.session_name
                );
                self.tmux = Some(s);
            }
            Err(e) => println!("[tmux] start failed: {e}"),
        }
        self.window = Some(window.clone());
        self.surface = Some(surface);
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                println!("[close] requested");
                event_loop.exit();
            }
            WindowEvent::Focused(f) => println!("[focus] {f}"),
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => println!("[ime] Enabled"),
                Ime::Disabled => println!("[ime] Disabled"),
                Ime::Preedit(text, range) => {
                    println!("[ime] Preedit text={text:?} range={range:?}")
                }
                Ime::Commit(text) => println!("[ime] Commit text={text:?}"),
            },
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(event);
            }
            WindowEvent::Resized(_) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let (Some(window), Some(surface)) = (&self.window, self.surface.as_mut()) else {
                    return;
                };
                let size = window.inner_size();
                let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                else {
                    return;
                };
                surface.resize(w, h).expect("surface resize");
                let mut buf = surface.buffer_mut().expect("buffer_mut");
                // 0x202428 — dark slate, just to prove the surface is alive.
                buf.fill(0x00202428);
                buf.present().expect("present");
            }
            _ => {}
        }
    }
}

impl App {
    fn handle_key(&mut self, ev: KeyEvent) {
        let pressed = ev.state == ElementState::Pressed;

        // Track shift state for Shift+Space toggle and shifted jamo.
        if let Key::Named(NamedKey::Shift) = &ev.logical_key {
            self.shift = pressed;
            return;
        }
        if !pressed {
            return;
        }

        // Shift+Space toggles Hangul mode and flushes any pending preedit.
        if matches!(&ev.logical_key, Key::Named(NamedKey::Space)) && self.shift {
            if let Some(s) = self.composer.flush() {
                println!("[commit] {s:?} (mode-switch flush)");
            }
            self.hangul_mode = !self.hangul_mode;
            println!("[mode] hangul={}", self.hangul_mode);
            return;
        }

        // Backspace: in hangul mode, chip preedit; if empty, forward.
        if matches!(&ev.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.hangul_mode && self.composer.backspace() {
                println!(
                    "[preedit] {:?} (after backspace)",
                    self.composer.preedit().unwrap_or_default()
                );
                return;
            }
            println!("[forward] backspace");
            self.send_to_tmux(&[0x7f]);
            return;
        }

        // Enter / Space / Tab / Esc — flush preedit then forward.
        if let Key::Named(named) = &ev.logical_key {
            if matches!(
                named,
                NamedKey::Enter
                    | NamedKey::Space
                    | NamedKey::Tab
                    | NamedKey::Escape
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
            ) {
                if let Some(s) = self.composer.flush() {
                    println!("[commit] {s:?}");
                    let bytes = s.into_bytes();
                    self.send_to_tmux(&bytes);
                }
                let key_bytes: &[u8] = match named {
                    NamedKey::Enter => &[0x0d],
                    NamedKey::Space => &[b' '],
                    NamedKey::Tab => &[0x09],
                    NamedKey::Escape => &[0x1b],
                    NamedKey::ArrowUp => &[0x1b, b'[', b'A'],
                    NamedKey::ArrowDown => &[0x1b, b'[', b'B'],
                    NamedKey::ArrowRight => &[0x1b, b'[', b'C'],
                    NamedKey::ArrowLeft => &[0x1b, b'[', b'D'],
                    _ => &[],
                };
                if !key_bytes.is_empty() {
                    self.send_to_tmux(key_bytes);
                }
                println!("[forward] {named:?}");
                return;
            }
        }

        // Character key.
        let Some(text) = ev.text.as_deref() else {
            return;
        };
        let Some(c) = text.chars().next() else { return };

        if !self.hangul_mode {
            println!("[forward] char {c:?}");
            self.send_to_tmux(text.as_bytes());
            return;
        }

        // Hangul mode: map via dubeolsik. If unmapped (digits, punctuation),
        // flush + forward.
        let Some(jamo) = dubeolsik(c) else {
            if let Some(s) = self.composer.flush() {
                println!("[commit] {s:?}");
                self.send_to_tmux(s.as_bytes());
            }
            println!("[forward] char {c:?}");
            self.send_to_tmux(text.as_bytes());
            return;
        };
        if let Some(commit) = self.composer.feed(jamo) {
            println!("[commit] {commit:?}");
            self.send_to_tmux(commit.as_bytes());
        }
        if let Some(pre) = self.composer.preedit() {
            println!("[preedit] {pre:?}");
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run_app");
}
