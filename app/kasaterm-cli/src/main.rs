//! kasaterm-cli — minimal standalone host that drives kasaterm with no
//! UI framework. winit owns the window + events, wgpu owns the surface,
//! tmux-bridge feeds cells. This binary exists to prove that kasaterm is
//! genuinely framework-agnostic (no iced anywhere in the dep tree).

use std::sync::{Arc, Mutex};

use anyhow::Result;
use kasaterm::{PaneRender, Rect, Selection, TerminalPipeline, TerminalPrimitive};
use tmux_bridge::{ScreenUpdate, StartOptions, TmuxSession, Cell as GridCell};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const TERM_BG: [f32; 4] = [0x1c as f32 / 255.0, 0x20 as f32 / 255.0, 0x26 as f32 / 255.0, 1.0];
const TERM_FG: [f32; 4] = [0xea as f32 / 255.0, 0xee as f32 / 255.0, 0xf4 as f32 / 255.0, 1.0];

/// Mutable cell snapshot the redraw loop reads from.
#[derive(Default)]
struct Screen {
    cells: Arc<Vec<Vec<GridCell>>>,
    cols: u16,
    rows: u16,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
}

struct Gpu {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: TerminalPipeline,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    screen: Arc<Mutex<Screen>>,
    tmux: Option<Arc<TmuxSession>>,
    /// Live IME preedit text, painted in the cursor cell with an accent
    /// underline so Hangul / kana composition is visible while the user
    /// is still typing.
    preedit: String,
    /// Drag-selection state. Set on mouse-down, updated on mouse-move,
    /// committed on mouse-up. None = no selection (kasaterm paints nothing).
    selection: Option<Selection>,
    /// Mouse-down anchor cell — kept so mid-drag updates can rebuild the
    /// Selection without re-deriving from pixel state. Cleared on release.
    drag_anchor: Option<(u16, u16)>,
    /// Last-known cursor position in logical pixels. Used both for
    /// drag-end computation and Cmd+modifier shortcuts.
    cursor_px: (f32, f32),
    /// Current modifier state — winit 0.30 doesn't include modifiers in
    /// individual KeyEvent, so we track ModifiersChanged separately.
    modifiers: ModifiersState,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            screen: Arc::new(Mutex::new(Screen::default())),
            tmux: None,
            preedit: String::new(),
            selection: None,
            drag_anchor: None,
            cursor_px: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
        }
    }

    fn ensure_gpu(&mut self, window: Arc<Window>) -> Result<()> {
        if self.gpu.is_some() {
            return Ok(());
        }
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kasaterm-cli device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        }))?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb() == false)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let pipeline = TerminalPipeline::new(&device, format);
        self.gpu = Some(Gpu {
            instance,
            surface,
            adapter,
            device,
            queue,
            config,
            pipeline,
        });
        Ok(())
    }

    fn start_tmux(&mut self) -> Result<()> {
        let cwd = std::env::var("KASATERM_CWD")
            .ok()
            .or_else(|| std::env::current_dir().ok().and_then(|p| p.to_str().map(|s| s.to_string())));
        // Derive (cols, rows) from the window's logical size so cells
        // match the glyph advance from the start. Defaults if the
        // window hasn't been sized yet.
        let (cols, rows) = self
            .window
            .as_ref()
            .map(|w| {
                let size = w.inner_size();
                let scale = w.scale_factor() as f32;
                let lw = size.width as f32 / scale;
                let lh = size.height as f32 / scale;
                let cols = (lw / 7.6).floor().max(40.0) as u16;
                let rows = (lh / 18.0).floor().max(10.0) as u16;
                (cols, rows)
            })
            .unwrap_or((100, 32));
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("kasaterm-cli"),
            cols,
            rows,
            ..Default::default()
        })?;
        let screens = tmux.screens.clone();
        let screen = self.screen.clone();
        let window = self.window.clone();
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
                if s.cols != cols || s.rows != rows || s.cells.len() != rows as usize {
                    s.cols = cols;
                    s.rows = rows;
                    s.cells = Arc::new(vec![vec![GridCell::blank(); cols as usize]; rows as usize]);
                }
                let cells = Arc::make_mut(&mut s.cells);
                for (r, row) in dirty {
                    if let Some(dst) = cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                s.cursor_row = cursor_row;
                s.cursor_col = cursor_col;
                s.cursor_visible = cursor_visible;
                drop(s);
                if let Some(w) = &window {
                    w.request_redraw();
                }
            }
        });
        self.tmux = Some(Arc::new(tmux));
        Ok(())
    }

    /// Convert a logical-pixel position into a (col, row) cell. Used by
    /// the mouse-drag selection path. Returns None when no screen has
    /// landed yet (still booting).
    fn px_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let lw = size.width as f32 / scale;
        let lh = size.height as f32 / scale;
        let screen = self.screen.lock().unwrap();
        if screen.cols == 0 || screen.rows == 0 {
            return None;
        }
        let cell_w = lw / screen.cols as f32;
        let cell_h = lh / screen.rows as f32;
        let col = (px / cell_w).floor().max(0.0) as u16;
        let row = (py / cell_h).floor().max(0.0) as u16;
        Some((col.min(screen.cols - 1), row.min(screen.rows - 1)))
    }

    fn copy_selection(&self) {
        let Some(sel) = self.selection else { return };
        let screen = self.screen.lock().unwrap();
        let pane = PaneRender {
            rect: [0.0, 0.0, 0.0, 0.0],
            cells: screen.cells.clone(),
            cols: screen.cols,
            rows: screen.rows,
            cursor_row: screen.cursor_row,
            cursor_col: screen.cursor_col,
            cursor_visible: screen.cursor_visible,
            is_active: true,
        };
        drop(screen);
        let text = kasaterm::extract_selection(&[pane], &sel);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[kasaterm-cli] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[kasaterm-cli] clipboard open failed: {e}"),
        }
    }

    fn render(&mut self) -> Result<()> {
        let Some(gpu) = self.gpu.as_mut() else { return Ok(()) };
        let Some(window) = self.window.as_ref() else { return Ok(()) };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let scale = window.scale_factor() as f32;
        let logical_w = size.width as f32 / scale;
        let logical_h = size.height as f32 / scale;

        let screen = self.screen.lock().unwrap();
        let pane = PaneRender {
            rect: [0.0, 0.0, logical_w, logical_h],
            cells: screen.cells.clone(),
            cols: screen.cols.max(1),
            rows: screen.rows.max(1),
            cursor_row: screen.cursor_row,
            cursor_col: screen.cursor_col,
            cursor_visible: screen.cursor_visible,
            is_active: true,
        };
        let cell_w = logical_w / screen.cols.max(1) as f32;
        let cell_h = logical_h / screen.rows.max(1) as f32;
        let primitive = TerminalPrimitive {
            panes: vec![pane],
            bg_color: TERM_BG,
            fg_color: TERM_FG,
            cell_w,
            cell_h,
            font_size: (cell_h * 0.78).max(8.0),
            widget_bounds: [logical_w, logical_h],
            preedit: self.preedit.clone(),
            selection: self.selection,
        };
        drop(screen);

        gpu.pipeline.prepare(
            &gpu.device,
            &gpu.queue,
            Rect { x: 0.0, y: 0.0, width: logical_w, height: logical_h },
            &primitive,
            scale,
        );

        let frame = match gpu.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                gpu.surface.get_current_texture()?
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kasaterm-cli encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kasaterm-cli pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: TERM_BG[0] as f64,
                            g: TERM_BG[1] as f64,
                            b: TERM_BG[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.pipeline.draw(&mut pass);
        }
        gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("kasaterm-cli")
            .with_inner_size(LogicalSize::new(900.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        // Turn on IME so winit emits Preedit/Commit events for Hangul,
        // kana, pinyin etc. Without this, raw key events for jamo would
        // be sent and never compose into 안 / 한 / 글.
        window.set_ime_allowed(true);
        self.window = Some(window.clone());
        if let Err(e) = self.ensure_gpu(window) {
            eprintln!("[kasaterm-cli] gpu init failed: {e}");
            event_loop.exit();
            return;
        }
        if let Err(e) = self.start_tmux() {
            eprintln!("[kasaterm-cli] tmux start failed: {e}");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
                // Renegotiate the grid with tmux so apps inside (claude,
                // vim, less) rewrap to the visible cell pitch. Without
                // this every paint rebuilds the instance buffer against
                // a stale grid size, which both looks wrong and forces
                // a hot rebuild every frame.
                if let (Some(w), Some(tmux)) = (&self.window, &self.tmux) {
                    let size = w.inner_size();
                    let scale = w.scale_factor() as f32;
                    let lw = size.width as f32 / scale;
                    let lh = size.height as f32 / scale;
                    let cols = (lw / 7.6).floor().max(40.0) as u16;
                    let rows = (lh / 18.0).floor().max(10.0) as u16;
                    let _ = tmux.resize_client(cols, rows);
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render() {
                    eprintln!("[kasaterm-cli] render error: {e}");
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
                self.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                // Mid-drag: update selection.end to follow the pointer.
                if let (Some(anchor), Some(cell)) = (
                    self.drag_anchor,
                    self.px_to_cell(self.cursor_px.0, self.cursor_px.1),
                ) {
                    self.selection = Some(Selection { anchor, end: cell });
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => {
                        if let Some(cell) = self.px_to_cell(self.cursor_px.0, self.cursor_px.1) {
                            self.drag_anchor = Some(cell);
                            self.selection = Some(Selection { anchor: cell, end: cell });
                        }
                    }
                    ElementState::Released => {
                        self.drag_anchor = None;
                        // Selection stays set so Cmd+C still works after
                        // the drag ends. Click without drag = empty 1-cell
                        // selection; treat that as 'clear'.
                        if let Some(sel) = self.selection {
                            if sel.anchor == sel.end {
                                self.selection = None;
                            }
                        }
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Preedit(text, _range) => {
                        self.preedit = text;
                    }
                    Ime::Commit(text) => {
                        self.preedit.clear();
                        if let Some(tmux) = self.tmux.as_ref() {
                            let bytes = text.as_bytes();
                            let hex: String = bytes
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let _ = tmux.send_keys_hex(None, &hex);
                        }
                    }
                    Ime::Disabled | Ime::Enabled => {
                        self.preedit.clear();
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                // Cmd+C copies the current selection to the clipboard
                // and short-circuits the PTY write — otherwise the shell
                // would receive a literal 'c' or ETX.
                let is_cmd = self.modifiers.super_key() || self.modifiers.control_key();
                if is_cmd {
                    if let Key::Character(s) = &logical_key {
                        if s.eq_ignore_ascii_case("c") && self.selection.is_some() {
                            self.copy_selection();
                            return;
                        }
                        if s.eq_ignore_ascii_case("v") {
                            // Cmd+V — pull clipboard, send to PTY wrapped
                            // in bracketed-paste markers so modern shells
                            // can treat the whole blob as one paste rather
                            // than typed input (no inadvertent ⏎ from a
                            // multi-line copy, autocomplete suppressed).
                            // If the app hasn't enabled paste-bracketing
                            // it just sees the markers as escape garbage,
                            // a fair tradeoff vs. the safer common case.
                            let text = match arboard::Clipboard::new()
                                .and_then(|mut cb| cb.get_text())
                            {
                                Ok(t) => t,
                                Err(e) => {
                                    eprintln!("[kasaterm-cli] clipboard read failed: {e}");
                                    return;
                                }
                            };
                            if let Some(tmux) = self.tmux.as_ref() {
                                let mut payload = Vec::with_capacity(text.len() + 12);
                                payload.extend_from_slice(b"\x1b[200~");
                                payload.extend_from_slice(text.as_bytes());
                                payload.extend_from_slice(b"\x1b[201~");
                                let hex: String = payload
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let _ = tmux.send_keys_hex(None, &hex);
                            }
                            return;
                        }
                    }
                }
                let Some(tmux) = self.tmux.as_ref() else { return };
                let bytes: Vec<u8> = match logical_key {
                    Key::Named(NamedKey::Enter) => b"\r".to_vec(),
                    Key::Named(NamedKey::Backspace) => b"\x7f".to_vec(),
                    Key::Named(NamedKey::Tab) => b"\t".to_vec(),
                    Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
                    Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
                    Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
                    Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
                    Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
                    _ => match text {
                        Some(t) => {
                            // Skip non-ASCII keystrokes — they're CJK
                            // jamo before IME composition. Commit fires
                            // separately with the composed result.
                            if !t.chars().all(|c| c.is_ascii() && !c.is_control()) {
                                return;
                            }
                            t.as_bytes().to_vec()
                        }
                        None => return,
                    },
                };
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
                let _ = tmux.send_keys_hex(None, &hex);
            }
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}
