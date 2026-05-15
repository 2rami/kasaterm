//! kasaterm-sugarloaf-cli — minimal sugarloaf-rendered terminal driven by
//! tmux-bridge. Phase A MVP: keyboard in, screen out, no scrollback /
//! IME / mouse / selection yet (those come in Task #13). Stays
//! framework-agnostic — no iced or kasaterm in the dep tree.

mod cells;

use anyhow::Result;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::error::Error;
use std::sync::{Arc, Mutex};
use sugarloaf::layout::RootStyle;
use sugarloaf::{Sugarloaf, SugarloafRenderer, SugarloafWindow, SugarloafWindowSize};
use tmux_bridge::screen::Cell as GridCell;
use tmux_bridge::{ScreenUpdate, StartOptions, TmuxSession};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

const FONT_SIZE: f32 = 14.0;
const CELL_W: f32 = 8.6;
const CELL_H: f32 = 18.0;

/// Snapshot the redraw loop reads from. Updated by the tmux event
/// thread on every ScreenUpdate.
#[derive(Default)]
struct Screen {
    rows: u16,
    cols: u16,
    cells: Vec<Vec<GridCell>>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
}

struct App {
    window: Option<Arc<Window>>,
    sugarloaf: Option<Sugarloaf<'static>>,
    tmux: Option<Arc<TmuxSession>>,
    screen: Arc<Mutex<Screen>>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            sugarloaf: None,
            tmux: None,
            screen: Arc::new(Mutex::new(Screen::default())),
        }
    }

    fn schedule_autocapture(&self) {
        let Ok(ms_str) = std::env::var("TMUXIFY_AUTOCAPTURE_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        let path = std::env::var("TMUXIFY_AUTOCAPTURE_PATH")
            .unwrap_or_else(|_| "/tmp/kasaterm-sugarloaf-cli.png".into());
        eprintln!("[autocapture] in {ms}ms → {path}");
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let pid = std::process::id();
            let _ = std::process::Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        "tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true"
                    ),
                ])
                .status();
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = std::process::Command::new("screencapture")
                .args(["-x", "-t", "png", &path])
                .status();
            eprintln!("[autocapture] captured {path}");
        });
    }

    fn schedule_autosend(&self) {
        let Ok(text) = std::env::var("TMUXIFY_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("TMUXIFY_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        let tmux = self.tmux.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            if let Some(t) = tmux.as_ref() {
                // Send the literal then a final Enter so the shell
                // executes it. Mirrors kasaterm-cli's autosend semantics.
                let mut payload = text.clone();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                let hex: String = payload
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = t.send_keys_hex(None, &hex);
            }
        });
    }

    fn start_tmux(&mut self) -> Result<()> {
        let window = self.window.as_ref().expect("window before tmux");
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let lw = size.width as f32 / scale;
        let lh = size.height as f32 / scale;
        let cols = (lw / CELL_W).floor().max(40.0) as u16;
        let rows = (lh / CELL_H).floor().max(10.0) as u16;
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("kasaterm-sugarloaf-cli"),
            cols,
            rows,
            ..Default::default()
        })?;
        let screens = tmux.screens.clone();
        let screen = self.screen.clone();
        let win = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(ScreenUpdate {
                rows,
                cols,
                dirty,
                cursor_row,
                cursor_col,
                cursor_visible,
                ..
            }) = screens.recv()
            {
                let mut s = screen.lock().unwrap();
                let resized = s.cols != cols
                    || s.rows != rows
                    || s.cells.len() != rows as usize;
                if resized {
                    s.cols = cols;
                    s.rows = rows;
                    s.cells = (0..rows as usize)
                        .map(|_| vec![GridCell::blank(); cols as usize])
                        .collect();
                }
                for (r, row) in dirty {
                    if let Some(dst) = s.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                s.cursor_row = cursor_row;
                s.cursor_col = cursor_col;
                s.cursor_visible = cursor_visible;
                drop(s);
                if let Some(w) = win.as_ref() {
                    w.request_redraw();
                }
            }
        });
        self.tmux = Some(Arc::new(tmux));
        Ok(())
    }

    fn forward_key(&self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let Some(tmux) = self.tmux.as_ref() else { return; };
        // Logical key → byte sequence to forward to the shell. Minimal
        // mapping for the MVP — full key handling lives in Task #13.
        let bytes: Vec<u8> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => b"\r".to_vec(),
            Key::Named(NamedKey::Backspace) => b"\x7f".to_vec(),
            Key::Named(NamedKey::Tab) => b"\t".to_vec(),
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
            Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
            Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
            Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
            Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
            Key::Character(s) => s.as_bytes().to_vec(),
            _ => return,
        };
        if bytes.is_empty() {
            return;
        }
        let hex: String = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = tmux.send_keys_hex(None, &hex);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        event_loop.set_control_flow(ControlFlow::Poll);
        let attrs = WindowAttributes::default()
            .with_title("kasaterm-sugarloaf-cli")
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create window"),
        );
        let sg_window = SugarloafWindow {
            handle: window.window_handle().unwrap().as_raw(),
            display: window.display_handle().unwrap().as_raw(),
            scale: window.scale_factor() as f32,
            size: SugarloafWindowSize {
                width: window.inner_size().width as f32,
                height: window.inner_size().height as f32,
            },
        };
        let font_library = sugarloaf::font::FontLibrary::default();
        let sugarloaf = Sugarloaf::new(
            sg_window,
            SugarloafRenderer::default(),
            &font_library,
            RootStyle::default(),
        )
        .expect("Sugarloaf instance");
        self.sugarloaf = Some(sugarloaf);
        self.window = Some(window);
        if let Err(e) = self.start_tmux() {
            eprintln!("[kasaterm-sugarloaf-cli] tmux start failed: {e}");
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
                if let Some(tmux) = self.tmux.as_ref() {
                    let scale = window.scale_factor() as f32;
                    let lw = size.width as f32 / scale;
                    let lh = size.height as f32 / scale;
                    let cols = (lw / CELL_W).floor().max(40.0) as u16;
                    let rows = (lh / CELL_H).floor().max(10.0) as u16;
                    let _ = tmux.resize_client(cols, rows);
                }
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.forward_key(&event);
            }
            WindowEvent::RedrawRequested => {
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
                // Snapshot under lock, render outside. Avoids holding
                // the screen mutex during sugarloaf's per-cell draw.
                let snap = {
                    let s = self.screen.lock().unwrap();
                    (s.cells.clone(), s.cursor_row, s.cursor_col, s.cursor_visible)
                };
                let (rows, cur_r, cur_c, cur_vis) = snap;
                if !rows.is_empty() {
                    cells::render_screen(sugarloaf, &rows, 8.0, 8.0, CELL_W, CELL_H, FONT_SIZE);
                    if cur_vis {
                        let cursor_x = 8.0 + cur_c as f32 * CELL_W;
                        let cursor_y = 8.0 + cur_r as f32 * CELL_H;
                        sugarloaf.rect(
                            None,
                            cursor_x,
                            cursor_y,
                            CELL_W,
                            CELL_H,
                            [
                                cells::DEFAULT_FG[0] as f32 / 255.0,
                                cells::DEFAULT_FG[1] as f32 / 255.0,
                                cells::DEFAULT_FG[2] as f32 / 255.0,
                                0.55,
                            ],
                            0.0,
                            0,
                        );
                    }
                }
                sugarloaf.render();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}
