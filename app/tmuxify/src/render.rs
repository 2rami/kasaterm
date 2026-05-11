//! wgpu + glyphon renderer.
//! Top session-bar = tabs (one per tmux window).
//! Below it = floating panes of the active window.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::{
    Backends, CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance,
    InstanceDescriptor, LoadOp, MultisampleState, Operations, PowerPreference,
    PresentMode, RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions,
    StoreOp, Surface, SurfaceConfiguration, TextureFormat, TextureUsages,
    TextureViewDescriptor,
};
use unicode_width::UnicodeWidthChar;
use winit::window::Window;

use crate::quad::{QuadInstance, QuadRenderer};
use crate::{DesktopIcon, FloatingPane, PaneGrid};
use tmux_bridge::Color as TermColor;

const ANSI16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xa6, 0x4d, 0x4d),
    (0x6b, 0xa6, 0x4d),
    (0xa6, 0x8a, 0x4d),
    (0x4d, 0x6b, 0xa6),
    (0x8a, 0x4d, 0xa6),
    (0x4d, 0xa6, 0xa6),
    (0xc0, 0xc4, 0xcc),
    (0x6e, 0x73, 0x80),
    (0xff, 0x6b, 0x6b),
    (0x9f, 0xe6, 0x6e),
    (0xff, 0xc7, 0x66),
    (0x6e, 0x9c, 0xff),
    (0xc7, 0x6e, 0xff),
    (0x6e, 0xea, 0xea),
    (0xff, 0xff, 0xff),
];

fn term_to_glyphon(c: &TermColor, default: GColor) -> GColor {
    match c {
        TermColor::Default => default,
        TermColor::Idx(i) => {
            if (*i as usize) < ANSI16.len() {
                let (r, g, b) = ANSI16[*i as usize];
                GColor::rgb(r, g, b)
            } else if *i >= 16 && *i <= 231 {
                // 6×6×6 cube.
                let n = i - 16;
                let r = n / 36;
                let g = (n % 36) / 6;
                let b = n % 6;
                let scale = |v: u8| -> u8 {
                    if v == 0 {
                        0
                    } else {
                        55 + 40 * v
                    }
                };
                GColor::rgb(scale(r), scale(g), scale(b))
            } else {
                // 232..255 grayscale
                let v = 8 + 10 * (i - 232);
                GColor::rgb(v, v, v)
            }
        }
        TermColor::Rgb(r, g, b) => GColor::rgb(*r, *g, *b),
    }
}

const FONT_SIZE: f32 = 13.0;
const FONT_SIZE_SM: f32 = 11.0;
const LINE_HEIGHT: f32 = 17.0;
pub const PADDING: f32 = 10.0;
const STATUS_HEIGHT: f32 = 22.0;
const TITLE_HEIGHT: f32 = 26.0;
const BOX_PAD: f32 = 8.0;
pub const SESSION_BAR_HEIGHT: f32 = 38.0;
pub const SESSION_TAB_W: f32 = 170.0;
pub const SESSION_TAB_GAP: f32 = 2.0;
pub const SIDEBAR_W: f32 = 240.0;
pub const ICON_W: f32 = 76.0;
pub const ICON_H: f32 = 78.0;

// === Palette (Warp-ish dark neutrals) ===
const BG: [f32; 4] = [0.043, 0.051, 0.063, 1.0]; // app bg
const CHROME_BG: [f32; 4] = [0.063, 0.075, 0.094, 1.0]; // title-bar
const SIDEBAR_BG: [f32; 4] = [0.051, 0.063, 0.078, 1.0];
const PANEL_BG: [f32; 4] = [0.094, 0.110, 0.133, 1.0]; // unstyled tile
const PANEL_HOVER: [f32; 4] = [0.117, 0.137, 0.165, 1.0];
const PANEL_ACTIVE: [f32; 4] = [0.137, 0.180, 0.247, 1.0];
const BORDER: [f32; 4] = [0.184, 0.204, 0.235, 1.0];
const ACCENT: [f32; 4] = [0.353, 0.510, 0.953, 1.0]; // brand blue
const ACCENT_DIM: [f32; 4] = [0.353, 0.510, 0.953, 0.45];
const TEXT_PRI: GColor = GColor::rgb(0xea, 0xee, 0xf4);
const TEXT_SEC: GColor = GColor::rgb(0x9b, 0xa3, 0xb0);
const TEXT_MUT: GColor = GColor::rgb(0x60, 0x68, 0x76);
const TEXT_DANGER: GColor = GColor::rgb(0xff, 0x9a, 0x9a);
const ACCENT_DIM_TEXT: GColor = GColor::rgb(0x6e, 0x8a, 0xc8);

fn color_eq(a: GColor, b: GColor) -> bool {
    a.0 == b.0
}

/// Measure D2Coding cell metrics by laying out 20 'M' chars and dividing.
fn measure_cell(font_system: &mut FontSystem) -> (f32, f32) {
    let mut buf = Buffer::new(font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
    buf.set_size(font_system, Some(2000.0), Some(LINE_HEIGHT * 2.0));
    let attrs = Attrs::new().family(Family::Name("D2Coding"));
    buf.set_text(font_system, "MMMMMMMMMMMMMMMMMMMM", attrs, Shaping::Advanced);
    buf.shape_until_scroll(font_system, false);
    let total = buf
        .layout_runs()
        .next()
        .map(|r| r.line_w)
        .unwrap_or(FONT_SIZE * 0.55 * 20.0);
    let cw = total / 20.0;
    (cw, LINE_HEIGHT)
}

/// Heuristic fallback before the renderer measures the real D2Coding advance.
pub const CELL_W: f32 = FONT_SIZE * 0.55;
pub const CELL_H: f32 = LINE_HEIGHT;

pub struct Renderer {
    surface: Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    quads: QuadRenderer,
    width: u32,
    height: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// When set, the next frame is also drawn to an offscreen texture
    /// and saved as PNG. Cleared after.
    pending_capture: Option<std::path::PathBuf>,
}

impl Renderer {
    pub fn request_capture(&mut self, path: std::path::PathBuf) {
        self.pending_capture = Some(path);
    }
}

impl Renderer {
    pub fn cells_for_size(&self, width: u32, height: u32) -> (u16, u16) {
        let canvas_w = (width as f32 - PADDING * 2.0).max(1.0);
        let canvas_h =
            (height as f32 - PADDING * 2.0 - STATUS_HEIGHT - SESSION_BAR_HEIGHT).max(1.0);
        let cols = ((canvas_w / self.cell_w).floor() as u16).max(20);
        let rows = ((canvas_h / self.cell_h).floor() as u16).max(5);
        (cols, rows)
    }
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::all(),
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .context("create_surface")?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow!("no adapter"))?;
        let (device, queue) = adapter
            .request_device(
                &DeviceDescriptor {
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                    label: Some("tmuxify-device"),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("request_device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, TextureFormat::Bgra8UnormSrgb | TextureFormat::Rgba8UnormSrgb))
            .unwrap_or(caps.formats[0]);
        let present_mode = if caps.present_modes.contains(&PresentMode::Mailbox) {
            PresentMode::Mailbox
        } else {
            PresentMode::Fifo
        };
        let alpha_mode = caps
            .alpha_modes
            .iter()
            .copied()
            .find(|m| *m == CompositeAlphaMode::Opaque)
            .unwrap_or(caps.alpha_modes[0]);
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let viewport = Viewport::new(&device, &cache);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let mut font_system = FontSystem::new();
        let swash = SwashCache::new();
        let quads = QuadRenderer::new(&device, format, width as f32, height as f32)?;
        // Measure the real D2Coding advance so cell width matches glyph width.
        let (cell_w, cell_h) = measure_cell(&mut font_system);
        println!("[metrics] cell_w={cell_w:.2} cell_h={cell_h:.2}");

        Ok(Self {
            surface,
            device,
            queue,
            config,
            font_system,
            swash,
            atlas,
            text_renderer,
            viewport,
            quads,
            width,
            height,
            cell_w,
            cell_h,
            pending_capture: None,
        })
    }

    /// Build one tiny Buffer per terminal cell, anchored to the exact
    /// pixel position cell occupies. Any glyph — ASCII, box drawing,
    /// CJK — drops into its slot regardless of font advance.
    fn build_body_cells(
        &mut self,
        pg: &PaneGrid,
        body_left: f32,
        body_top: f32,
        default_color: GColor,
    ) -> Vec<(Buffer, f32, f32, GColor, f32)> {
        let attrs = Attrs::new().family(Family::Name("D2Coding"));
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let mut out: Vec<(Buffer, f32, f32, GColor, f32)> = Vec::new();
        for (row_i, row) in pg.cells.iter().enumerate() {
            let row_top = body_top + row_i as f32 * cell_h;
            let mut col_i = 0usize;
            while col_i < row.len() {
                let cell = &row[col_i];
                let raw = if cell.ch.is_empty() { " " } else { cell.ch.as_str() };
                let first = raw.chars().next().unwrap_or(' ');
                let width_cells = UnicodeWidthChar::width(first).unwrap_or(1).max(1) as f32;
                let color = term_to_glyphon(&cell.fg, default_color);
                if first == ' ' {
                    col_i += 1;
                    continue;
                }
                let pixel_w = cell_w * width_cells;
                let mut b = Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
                b.set_size(&mut self.font_system, Some(pixel_w + 4.0), Some(cell_h * 1.5));
                b.set_text(&mut self.font_system, raw, attrs.color(color), Shaping::Advanced);
                b.shape_until_scroll(&mut self.font_system, false);
                out.push((
                    b,
                    body_left + col_i as f32 * cell_w,
                    row_top,
                    color,
                    pixel_w,
                ));
                col_i += width_cells as usize;
            }
        }
        out
    }

    fn _build_body_segments_unused(
        &mut self,
        pg: &PaneGrid,
        body_left: f32,
        body_top: f32,
        default_color: GColor,
    ) -> Vec<(Buffer, f32, f32, GColor, f32)> {
        let attrs = Attrs::new().family(Family::Name("D2Coding"));
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let mut out: Vec<(Buffer, f32, f32, GColor, f32)> = Vec::new();
        for (row_i, row) in pg.cells.iter().enumerate() {
            let row_top = body_top + row_i as f32 * cell_h;
            let mut col_start: usize = 0;
            let mut seg_text = String::new();
            let mut seg_cells: usize = 0;
            let mut cur_color: Option<GColor> = None;
            let mut flush = |out: &mut Vec<(Buffer, f32, f32, GColor, f32)>,
                             font_system: &mut FontSystem,
                             cell_w: f32,
                             text: &str,
                             cells_count: usize,
                             color: GColor,
                             col_start: usize,
                             row_top: f32| {
                if text.is_empty() {
                    return;
                }
                let width = cell_w * cells_count as f32 + 4.0;
                let mut b = Buffer::new(font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
                b.set_size(font_system, Some(width), Some(LINE_HEIGHT * 1.5));
                b.set_monospace_width(font_system, Some(cell_w));
                b.set_text(font_system, text, attrs.color(color), Shaping::Advanced);
                b.set_monospace_width(font_system, Some(cell_w));
                b.shape_until_scroll(font_system, false);
                out.push((
                    b,
                    body_left + col_start as f32 * cell_w,
                    row_top,
                    color,
                    width,
                ));
            };
            for (col_i, cell) in row.iter().enumerate() {
                let c = term_to_glyphon(&cell.fg, default_color);
                let same = cur_color.map(|cc| color_eq(cc, c)).unwrap_or(false);
                if !same {
                    if let Some(prev) = cur_color {
                        flush(
                            &mut out,
                            &mut self.font_system,
                            cell_w,
                            &seg_text,
                            seg_cells,
                            prev,
                            col_start,
                            row_top,
                        );
                    }
                    col_start = col_i;
                    seg_text.clear();
                    seg_cells = 0;
                    cur_color = Some(c);
                }
                if cell.ch.is_empty() {
                    seg_text.push(' ');
                } else {
                    seg_text.push_str(&cell.ch);
                }
                seg_cells += 1;
            }
            if let Some(c) = cur_color {
                flush(
                    &mut out,
                    &mut self.font_system,
                    cell_w,
                    &seg_text,
                    seg_cells,
                    c,
                    col_start,
                    row_top,
                );
            }
        }
        out
    }

    fn build_body_buffer(
        &mut self,
        pg: &PaneGrid,
        w: f32,
        h: f32,
        default_color: GColor,
    ) -> Buffer {
        // Build the full text once; record byte ranges for color spans.
        let mut text = String::with_capacity((pg.cols as usize + 1) * pg.rows as usize);
        // Each entry: (byte_start, byte_end, GColor).
        let mut spans: Vec<(usize, usize, GColor)> = Vec::new();
        for row in &pg.cells {
            let mut run_start = text.len();
            let mut run_color: Option<GColor> = None;
            for cell in row {
                let color = term_to_glyphon(&cell.fg, default_color);
                let same = run_color.map(|c| color_eq(c, color)).unwrap_or(false);
                if !same {
                    if let Some(prev) = run_color {
                        if run_start < text.len() {
                            spans.push((run_start, text.len(), prev));
                        }
                    }
                    run_start = text.len();
                    run_color = Some(color);
                }
                if cell.ch.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(&cell.ch);
                }
            }
            if let Some(prev) = run_color {
                if run_start < text.len() {
                    spans.push((run_start, text.len(), prev));
                }
            }
            text.push('\n');
        }
        let attrs = Attrs::new().family(Family::Name("D2Coding"));
        let mut buf = Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        buf.set_size(&mut self.font_system, Some(w), Some(h));
        let span_iter = spans
            .iter()
            .map(|(s, e, c)| (&text[*s..*e], attrs.color(*c)));
        buf.set_rich_text(
            &mut self.font_system,
            span_iter,
            attrs,
            Shaping::Advanced,
        );
        // Apply AFTER set_rich_text — set_rich_text resets layout state.
        buf.set_monospace_width(&mut self.font_system, Some(self.cell_w));
        buf.shape_until_scroll(&mut self.font_system, false);
        buf
    }

    pub fn resize(&mut self, w: NonZeroU32, h: NonZeroU32) {
        self.config.width = w.get();
        self.config.height = h.get();
        self.width = w.get();
        self.height = h.get();
        self.surface.configure(&self.device, &self.config);
        self.quads.resize(&self.queue, w.get() as f32, h.get() as f32);
    }

    pub fn render(
        &mut self,
        floating: &BTreeMap<String, FloatingPane>,
        panes: &HashMap<String, PaneGrid>,
        active_pane: Option<&str>,
        tabs: &[(String, String, BTreeMap<String, FloatingPane>)],
        active_window: Option<&str>,
        hangul_mode: bool,
        preedit: Option<&str>,
        sidebar_open: bool,
        sessions: &[(u8, String, usize)],
        active_session: u8,
        icons: &[DesktopIcon],
        cursor_visible: bool,
        sidebar_w_user: f32,
    ) -> Result<()> {
        let sidebar_w = if sidebar_open { sidebar_w_user } else { 0.0 };
        struct Built {
            buffer: Buffer,
            left: f32,
            top: f32,
            bounds: TextBounds,
            color: GColor,
        }
        let mut built: Vec<Built> = Vec::new();
        let mut quads: Vec<QuadInstance> = Vec::new();
        let attrs = Attrs::new().family(Family::Name("D2Coding"));

        // === Top chrome strip ===
        quads.push(QuadInstance {
            rect: [0.0, 0.0, self.width as f32, SESSION_BAR_HEIGHT],
            color: CHROME_BG,
        });
        quads.push(QuadInstance {
            rect: [0.0, SESSION_BAR_HEIGHT - 1.0, self.width as f32, 1.0],
            color: BORDER,
        });
        // Sidebar toggle button (leftmost in title bar).
        let toggle_x = PADDING;
        let toggle_w = 28.0;
        let toggle_y = 6.0;
        let toggle_h = SESSION_BAR_HEIGHT - 12.0;
        quads.push(QuadInstance {
            rect: [toggle_x, toggle_y, toggle_w, toggle_h],
            color: if sidebar_open { PANEL_ACTIVE } else { PANEL_BG },
        });
        let mut tg_buf =
            Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        tg_buf.set_size(&mut self.font_system, Some(toggle_w), Some(toggle_h));
        // Simple ASCII glyph for the toggle (open/closed sidebar).
        tg_buf.set_text(
            &mut self.font_system,
            if sidebar_open { " ◧ " } else { " ◨ " },
            attrs,
            Shaping::Advanced,
        );
        tg_buf.shape_until_scroll(&mut self.font_system, false);
        built.push(Built {
            buffer: tg_buf,
            left: toggle_x,
            top: toggle_y + 4.0,
            bounds: TextBounds {
                left: toggle_x as i32,
                top: toggle_y as i32,
                right: (toggle_x + toggle_w) as i32,
                bottom: (toggle_y + toggle_h) as i32,
            },
            color: TEXT_PRI,
        });

        let tabs_origin = sidebar_w.max(toggle_x + toggle_w + 8.0);
        let mut last_tab_x = tabs_origin;
        for (i, (wid, title, _)) in tabs.iter().enumerate() {
            let active = active_window == Some(wid.as_str());
            let tab_x = tabs_origin + i as f32 * (SESSION_TAB_W + SESSION_TAB_GAP);
            last_tab_x = tab_x + SESSION_TAB_W + SESSION_TAB_GAP;
            let tab_y = 6.0;
            let tab_h = SESSION_BAR_HEIGHT - 6.0; // bottom flush with chrome strip
            let tab_color = if active { BG } else { CHROME_BG };
            quads.push(QuadInstance {
                rect: [tab_x, tab_y, SESSION_TAB_W, tab_h],
                color: tab_color,
            });
            if active {
                // top accent bar + bottom edge merged with body bg.
                quads.push(QuadInstance {
                    rect: [tab_x, tab_y, SESSION_TAB_W, 2.0],
                    color: ACCENT,
                });
            }
            // tab dividers
            if i + 1 < tabs.len() && !active {
                quads.push(QuadInstance {
                    rect: [tab_x + SESSION_TAB_W, tab_y + 8.0, 1.0, tab_h - 16.0],
                    color: BORDER,
                });
            }
            let mut tab_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            tab_buf.set_size(
                &mut self.font_system,
                Some(SESSION_TAB_W - 12.0),
                Some(tab_h),
            );
            tab_buf.set_text(&mut self.font_system, title, attrs, Shaping::Advanced);
            tab_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: tab_buf,
                left: tab_x + 8.0,
                top: tab_y + 5.0,
                bounds: TextBounds {
                    left: tab_x as i32,
                    top: tab_y as i32,
                    right: (tab_x + SESSION_TAB_W) as i32,
                    bottom: (tab_y + tab_h) as i32,
                },
                color: if active { TEXT_PRI } else { TEXT_SEC },
            });
        }

        // "+" new-tab button.
        let plus_w = 28.0;
        let plus_x = last_tab_x + 4.0;
        let plus_h = SESSION_BAR_HEIGHT - 12.0;
        quads.push(QuadInstance {
            rect: [plus_x, 6.0, plus_w, plus_h],
            color: PANEL_BG,
        });
        let mut plus_buf =
            Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        plus_buf.set_size(&mut self.font_system, Some(plus_w), Some(plus_h));
        plus_buf.set_text(&mut self.font_system, "  +", attrs, Shaping::Advanced);
        plus_buf.shape_until_scroll(&mut self.font_system, false);
        built.push(Built {
            buffer: plus_buf,
            left: plus_x,
            top: 9.0,
            bounds: TextBounds {
                left: plus_x as i32,
                top: 6,
                right: (plus_x + plus_w) as i32,
                bottom: (6.0 + plus_h) as i32,
            },
            color: TEXT_SEC,
        });

        // OS controls on the far right: minimise, max-toggle, close.
        let btn_w = 32.0;
        let close_x = self.width as f32 - btn_w;
        let max_x = close_x - btn_w;
        let min_x = max_x - btn_w;
        for (i, (label, hover)) in [
            (" ─ ", [0.18, 0.20, 0.25, 1.0]),
            (" ▢ ", [0.18, 0.20, 0.25, 1.0]),
            (" × ", [0.85, 0.30, 0.30, 1.0]),
        ]
        .iter()
        .enumerate()
        {
            let bx = [min_x, max_x, close_x][i];
            // Tiny bg accent on the close button area only on the lower row;
            // a tooltip/hover treatment is a follow-up.
            let _ = hover;
            let mut buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            buf.set_size(
                &mut self.font_system,
                Some(btn_w),
                Some(SESSION_BAR_HEIGHT),
            );
            buf.set_text(&mut self.font_system, label, attrs, Shaping::Advanced);
            buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: buf,
                left: bx,
                top: 5.0,
                bounds: TextBounds {
                    left: bx as i32,
                    top: 0,
                    right: (bx + btn_w) as i32,
                    bottom: SESSION_BAR_HEIGHT as i32,
                },
                color: if i == 2 { TEXT_DANGER } else { TEXT_SEC },
            });
        }

        // === Sidebar ===
        if sidebar_open {
            quads.push(QuadInstance {
                rect: [
                    0.0,
                    SESSION_BAR_HEIGHT,
                    sidebar_w,
                    self.height as f32 - SESSION_BAR_HEIGHT,
                ],
                color: SIDEBAR_BG,
            });
            quads.push(QuadInstance {
                rect: [
                    sidebar_w - 1.0,
                    SESSION_BAR_HEIGHT,
                    1.0,
                    self.height as f32 - SESSION_BAR_HEIGHT,
                ],
                color: BORDER,
            });
            // Search bar placeholder.
            let search_y = SESSION_BAR_HEIGHT + 14.0;
            let search_h = 30.0;
            quads.push(QuadInstance {
                rect: [14.0, search_y, sidebar_w - 28.0, search_h],
                color: PANEL_BG,
            });
            let mut search_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            search_buf.set_size(
                &mut self.font_system,
                Some(sidebar_w - 24.0),
                Some(search_h),
            );
            search_buf.set_text(
                &mut self.font_system,
                "  Search sessions…",
                attrs,
                Shaping::Advanced,
            );
            search_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: search_buf,
                left: 14.0,
                top: search_y + 7.0,
                bounds: TextBounds {
                    left: 14,
                    top: search_y as i32,
                    right: (sidebar_w - 14.0) as i32,
                    bottom: (search_y + search_h) as i32,
                },
                color: TEXT_MUT,
            });
            // Real session list.
            let row_h = 60.0;
            let row_gap = 4.0;
            let first_row_y = search_y + search_h + 14.0;
            let mut row_y = first_row_y;
            for (n, name, win_count) in sessions {
                let active = *n == active_session;
                let bg = if active { PANEL_ACTIVE } else { PANEL_BG };
                quads.push(QuadInstance {
                    rect: [14.0, row_y, sidebar_w - 28.0, row_h],
                    color: bg,
                });
                if active {
                    quads.push(QuadInstance {
                        rect: [14.0, row_y, 3.0, row_h],
                        color: ACCENT,
                    });
                }
                // Always-visible × close button.
                let close_w = 24.0;
                let close_left = sidebar_w - 14.0 - close_w;
                quads.push(QuadInstance {
                    rect: [close_left + 2.0, row_y + 18.0, close_w - 4.0, 24.0],
                    color: [0.04, 0.05, 0.07, 1.0],
                });
                let mut x_buf =
                    Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
                x_buf.set_size(&mut self.font_system, Some(close_w), Some(24.0));
                x_buf.set_text(&mut self.font_system, " ×", attrs, Shaping::Advanced);
                x_buf.shape_until_scroll(&mut self.font_system, false);
                built.push(Built {
                    buffer: x_buf,
                    left: close_left + 2.0,
                    top: row_y + 20.0,
                    bounds: TextBounds {
                        left: close_left as i32,
                        top: row_y as i32,
                        right: (close_left + close_w) as i32,
                        bottom: (row_y + row_h) as i32,
                    },
                    color: TEXT_SEC,
                });
                let mut name_buf =
                    Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
                name_buf.set_size(
                    &mut self.font_system,
                    Some(sidebar_w - 40.0 - close_w),
                    Some(20.0),
                );
                name_buf.set_text(&mut self.font_system, name, attrs, Shaping::Advanced);
                name_buf.shape_until_scroll(&mut self.font_system, false);
                built.push(Built {
                    buffer: name_buf,
                    left: 26.0,
                    top: row_y + 12.0,
                    bounds: TextBounds {
                        left: 14,
                        top: row_y as i32,
                        right: (sidebar_w - 14.0) as i32,
                        bottom: (row_y + row_h) as i32,
                    },
                    color: if active { TEXT_PRI } else { TEXT_SEC },
                });
                let mut sub_buf =
                    Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE_SM, FONT_SIZE_SM + 3.0));
                sub_buf.set_size(
                    &mut self.font_system,
                    Some(sidebar_w - 40.0),
                    Some(16.0),
                );
                let sub = format!(
                    "{} window{}",
                    win_count,
                    if *win_count == 1 { "" } else { "s" }
                );
                sub_buf.set_text(&mut self.font_system, &sub, attrs, Shaping::Advanced);
                sub_buf.shape_until_scroll(&mut self.font_system, false);
                built.push(Built {
                    buffer: sub_buf,
                    left: 26.0,
                    top: row_y + 32.0,
                    bounds: TextBounds {
                        left: 14,
                        top: row_y as i32,
                        right: (sidebar_w - 14.0) as i32,
                        bottom: (row_y + row_h) as i32,
                    },
                    color: TEXT_MUT,
                });
                row_y += row_h + row_gap;
            }
            // "+ New session" button.
            let new_h = 36.0;
            row_y += 6.0;
            quads.push(QuadInstance {
                rect: [14.0, row_y, sidebar_w - 28.0, new_h],
                color: PANEL_BG,
            });
            let mut new_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            new_buf.set_size(
                &mut self.font_system,
                Some(sidebar_w - 32.0),
                Some(new_h),
            );
            new_buf.set_text(
                &mut self.font_system,
                "  + New session",
                attrs,
                Shaping::Advanced,
            );
            new_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: new_buf,
                left: 22.0,
                top: row_y + 10.0,
                bounds: TextBounds {
                    left: 14,
                    top: row_y as i32,
                    right: (sidebar_w - 14.0) as i32,
                    bottom: (row_y + new_h) as i32,
                },
                color: ACCENT_DIM_TEXT,
            });
        }

        // === Desktop icons (folder shape drawn directly, no emoji font) ===
        for icon in icons {
            let tile = 48.0;
            let tx = icon.x + (ICON_W - tile) * 0.5;
            let ty = icon.y;
            // Folder body with a tab on the upper-left.
            let body_top = ty + 12.0;
            let body_h = tile - 14.0;
            let tab_w = 18.0;
            let tab_h = 6.0;
            let folder_face = [0.27, 0.42, 0.62, 1.0];
            let folder_edge = [0.45, 0.65, 0.95, 1.0];
            // Tab.
            quads.push(QuadInstance {
                rect: [tx + 4.0, ty + 6.0, tab_w, tab_h + 4.0],
                color: folder_edge,
            });
            // Body.
            quads.push(QuadInstance {
                rect: [tx, body_top, tile, body_h],
                color: folder_face,
            });
            // Top edge highlight.
            quads.push(QuadInstance {
                rect: [tx, body_top, tile, 2.0],
                color: folder_edge,
            });
            // Label below.
            let mut l_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE_SM, FONT_SIZE_SM + 3.0));
            l_buf.set_size(&mut self.font_system, Some(ICON_W), Some(20.0));
            l_buf.set_text(&mut self.font_system, &icon.label, attrs, Shaping::Advanced);
            l_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: l_buf,
                left: icon.x,
                top: ty + tile + 4.0,
                bounds: TextBounds {
                    left: icon.x as i32,
                    top: (ty + tile) as i32,
                    right: (icon.x + ICON_W) as i32,
                    bottom: (icon.y + ICON_H) as i32,
                },
                color: TEXT_SEC,
            });
        }

        // === Floating panes (active window only) ===
        let mut order: Vec<&FloatingPane> = floating.values().collect();
        order.sort_by_key(|f| (active_pane == Some(&f.pane_id)) as u8);

        for fp in order {
            let is_active = active_pane == Some(&fp.pane_id);
            let border_color = if is_active { ACCENT_DIM } else { BORDER };
            let title_bg = if is_active { PANEL_ACTIVE } else { PANEL_BG };
            let body_bg = [0.078, 0.090, 0.110, 1.0];
            quads.push(QuadInstance {
                rect: [fp.x, fp.y, fp.w, fp.h],
                color: body_bg,
            });
            quads.push(QuadInstance {
                rect: [fp.x, fp.y, fp.w, TITLE_HEIGHT],
                color: title_bg,
            });
            let b = 1.0;
            quads.push(QuadInstance {
                rect: [fp.x, fp.y, fp.w, b],
                color: border_color,
            });
            quads.push(QuadInstance {
                rect: [fp.x, fp.y + fp.h - b, fp.w, b],
                color: border_color,
            });
            quads.push(QuadInstance {
                rect: [fp.x, fp.y, b, fp.h],
                color: border_color,
            });
            quads.push(QuadInstance {
                rect: [fp.x + fp.w - b, fp.y, b, fp.h],
                color: border_color,
            });
            quads.push(QuadInstance {
                rect: [fp.x + fp.w - 12.0, fp.y + fp.h - 12.0, 12.0, 12.0],
                color: if is_active { ACCENT_DIM } else { [0.30, 0.32, 0.38, 0.5] },
            });

            // Title.
            let title_text = if is_active {
                format!("● {}", fp.title)
            } else {
                format!("  {}", fp.title)
            };
            let mut title_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            title_buf.set_size(
                &mut self.font_system,
                Some((fp.w - 28.0).max(20.0)),
                Some(TITLE_HEIGHT),
            );
            title_buf.set_text(
                &mut self.font_system,
                &title_text,
                attrs,
                Shaping::Advanced,
            );
            title_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: title_buf,
                left: fp.x + BOX_PAD,
                top: fp.y + 4.0,
                bounds: TextBounds {
                    left: fp.x as i32,
                    top: fp.y as i32,
                    right: (fp.x + fp.w - 24.0) as i32,
                    bottom: (fp.y + TITLE_HEIGHT) as i32,
                },
                color: if is_active { TEXT_PRI } else { TEXT_SEC },
            });
            // X close button.
            let cx = fp.x + fp.w - 22.0;
            let cy = fp.y + 4.0;
            quads.push(QuadInstance {
                rect: [cx, cy, 18.0, TITLE_HEIGHT - 8.0],
                color: if is_active { PANEL_BG } else { CHROME_BG },
            });
            let mut x_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            x_buf.set_size(&mut self.font_system, Some(18.0), Some(TITLE_HEIGHT));
            x_buf.set_text(&mut self.font_system, " ×", attrs, Shaping::Advanced);
            x_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: x_buf,
                left: cx,
                top: cy + 1.0,
                bounds: TextBounds {
                    left: cx as i32,
                    top: cy as i32,
                    right: (cx + 18.0) as i32,
                    bottom: (cy + TITLE_HEIGHT) as i32,
                },
                color: if is_active { TEXT_DANGER } else { TEXT_MUT },
            });

            // Body.
            let Some(pg) = panes.get(&fp.pane_id) else {
                continue;
            };
            let body_w = (fp.w - BOX_PAD * 2.0).max(1.0);
            let body_h = (fp.h - TITLE_HEIGHT - BOX_PAD).max(1.0);
            // Block cursor on the active pane (only when blink-on).
            if is_active && cursor_visible && pg.cols > 0 && pg.rows > 0 {
                let cw = body_w / pg.cols as f32;
                let ch = body_h / pg.rows as f32;
                let cx = fp.x + BOX_PAD + pg.cursor_col as f32 * cw;
                let cy = fp.y + TITLE_HEIGHT + pg.cursor_row as f32 * ch;
                quads.push(QuadInstance {
                    rect: [cx, cy, cw, ch],
                    color: [0.45, 0.65, 0.95, 0.55],
                });
            }
            let default_text_color = if is_active { TEXT_PRI } else { TEXT_SEC };
            let _ = body_w;
            let _ = body_h;
            let segs = self.build_body_cells(
                pg,
                fp.x + BOX_PAD,
                fp.y + TITLE_HEIGHT,
                default_text_color,
            );
            for (buf, left, top, color, width) in segs {
                built.push(Built {
                    buffer: buf,
                    left,
                    top,
                    bounds: TextBounds {
                        left: left as i32,
                        top: top as i32,
                        right: (left + width) as i32,
                        bottom: (top + LINE_HEIGHT * 1.5) as i32,
                    },
                    color,
                });
            }
        }

        // === Status line ===
        let mode = if hangul_mode { "한글" } else { "EN" };
        let mut status = format!("[{mode}]  windows={}", tabs.len());
        if let Some(p) = preedit {
            let _ = std::fmt::Write::write_fmt(&mut status, format_args!("  {p}"));
        }
        let canvas_w = (self.width as f32 - PADDING * 2.0).max(1.0);
        let mut status_buf =
            Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        status_buf.set_size(&mut self.font_system, Some(canvas_w), Some(STATUS_HEIGHT));
        status_buf.set_text(&mut self.font_system, &status, attrs, Shaping::Advanced);
        status_buf.shape_until_scroll(&mut self.font_system, false);
        let status_top = self.height as f32 - PADDING - STATUS_HEIGHT;

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );

        let win_areas = built.iter().map(|b| TextArea {
            buffer: &b.buffer,
            left: b.left,
            top: b.top,
            scale: 1.0,
            bounds: b.bounds,
            default_color: b.color,
            custom_glyphs: &[],
        });
        let status_area = TextArea {
            buffer: &status_buf,
            left: PADDING,
            top: status_top,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: status_top as i32,
                right: self.width as i32,
                bottom: self.height as i32,
            },
            default_color: TEXT_MUT,
            custom_glyphs: &[],
        };
        let all: Vec<TextArea> = win_areas.chain(std::iter::once(status_area)).collect();

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                all,
                &mut self.swash,
            )
            .map_err(|e| anyhow!("prepare: {e:?}"))?;

        let frame = self
            .surface
            .get_current_texture()
            .context("get_current_texture")?;
        let view = frame
            .texture
            .create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.08,
                            b: 0.10,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.quads
                .draw(&self.device, &self.queue, &mut pass, &quads);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|e| anyhow!("render: {e:?}"))?;
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.atlas.trim();
        if let Some(path) = self.pending_capture.take() {
            if let Err(e) = self.capture_offscreen(&path, &quads, sidebar_w_user, sidebar_open) {
                eprintln!("[capture err] {e}");
            } else {
                println!("[capture] {}", path.display());
            }
        }
        Ok(())
    }

    /// Render the same scene to an offscreen RGBA texture and write it
    /// as PNG. Simpler than reading back the swapchain (which often
    /// isn't COPY_SRC-capable on present-only surfaces).
    fn capture_offscreen(
        &mut self,
        path: &std::path::Path,
        scene_quads: &[QuadInstance],
        _sidebar_w_user: f32,
        _sidebar_open: bool,
    ) -> Result<()> {
        use wgpu::{
            BufferDescriptor, BufferUsages, Extent3d, ImageCopyBuffer, ImageDataLayout,
            TextureDescriptor, TextureDimension, TextureFormat,
        };
        let w = self.width;
        let h = self.height;
        let target = self.device.create_texture(&TextureDescriptor {
            label: Some("capture-target"),
            size: Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            // Match the surface format so existing pipelines work.
            format: self.config.format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let format_is_bgra = matches!(
            self.config.format,
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb
        );
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Re-prepare text and quads against the capture format if it
        // differs; here both atlas + quad pipelines were built for the
        // surface format, which may not equal Rgba8Unorm. We accept any
        // visual mismatch — the capture is for layout debugging only.
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: Some("capture") });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("capture-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.08,
                            b: 0.10,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.quads
                .draw(&self.device, &self.queue, &mut pass, scene_quads);
            // glyphon TextRenderer was prepared with the surface format;
            // we can still issue the same render call against any colour
            // attachment in the same color space.
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|e| anyhow!("capture text render: {e:?}"))?;
        }

        // Bytes per row must be a multiple of 256 (wgpu requirement).
        let bytes_per_pixel = 4u32;
        let bytes_per_row_unpadded = w * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = ((bytes_per_row_unpadded + align - 1) / align) * align;
        let buf_size = (padded * h) as u64;
        let staging = self.device.create_buffer(&BufferDescriptor {
            label: Some("capture-staging"),
            size: buf_size,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            ImageCopyBuffer {
                buffer: &staging,
                layout: ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().context("map_async recv")?.map_err(|e| anyhow!("map: {e:?}"))?;
        let data = slice.get_mapped_range();
        // Compact padded rows; BGRA → RGBA byte swap if needed.
        let mut tight = Vec::with_capacity((bytes_per_row_unpadded * h) as usize);
        for row in 0..h {
            let s = (row * padded) as usize;
            let e = s + bytes_per_row_unpadded as usize;
            if format_is_bgra {
                for px in data[s..e].chunks_exact(4) {
                    tight.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            } else {
                tight.extend_from_slice(&data[s..e]);
            }
        }
        drop(data);
        staging.unmap();

        let img: image::RgbaImage =
            image::ImageBuffer::from_raw(w, h, tight).ok_or_else(|| anyhow!("imgbuf"))?;
        img.save(path).context("png save")?;
        Ok(())
    }
}
