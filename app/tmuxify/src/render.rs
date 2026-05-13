//! wgpu + glyphon renderer.
//! Top session-bar = tabs (one per tmux window).
//! Below it = floating panes of the active window.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use glyphon::{
    cosmic_text::Align, Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics,
    Resolution, Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
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
use crate::{DesktopIcon, FloatingPane, IconKind, PaneGrid};
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

fn term_to_glyphon_inner(c: &TermColor, default: GColor, lift_floor: bool) -> GColor {
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
                // 232..255 grayscale. The default tmux ramp starts at
                // value 8 (near-black), which on our #22272e body bg
                // produces unreadable placeholder/dim text. When this
                // colour is going to a FOREGROUND glyph, lift the floor
                // so anything claude writes in "dim grey" is still
                // legible. Background fills (lift_floor = false) stay
                // accurate.
                let raw = 8u16 + 10 * (*i as u16 - 232);
                let v = if lift_floor {
                    raw.max(110).min(255) as u8
                } else {
                    raw.min(255) as u8
                };
                GColor::rgb(v, v, v)
            }
        }
        TermColor::Rgb(r, g, b) => GColor::rgb(*r, *g, *b),
    }
}

fn term_to_glyphon(c: &TermColor, default: GColor) -> GColor {
    term_to_glyphon_inner(c, default, true)
}

fn term_to_glyphon_bg(c: &TermColor, default: GColor) -> GColor {
    term_to_glyphon_inner(c, default, false)
}

// 16pt with a 22px leading — matches native Windows Terminal /
// Warp default and gives glyphon enough vertical bitmap to render
// D2Coding crisply (small sizes were getting smeared by the AA).
// All sizes below are PHYSICAL pixels, not logical. The window is
// configured at LogicalSize 1280×760 which becomes 2560×1520 physical
// on a 2× Retina display, and the wgpu surface is the physical size.
// Constants are therefore doubled (≈ 2× their natural "logical" value)
// so the UI lands at a normal screen size on Retina rather than half-
// scale. A future scale-factor refactor would replace this with a
// runtime multiplier.
const FONT_SIZE: f32 = 32.0;
const FONT_SIZE_SM: f32 = 26.0;
const LINE_HEIGHT: f32 = 44.0;
pub const PADDING: f32 = 20.0;
pub const STATUS_HEIGHT: f32 = 72.0;
pub const TASKBAR_BTN_H: f32 = 56.0;
pub const TASKBAR_START_W: f32 = 72.0;
pub const TASKBAR_PANE_W: f32 = 360.0;
pub const TASKBAR_GAP: f32 = 8.0;
pub const TITLE_HEIGHT: f32 = 52.0;
pub const BOX_PAD: f32 = 16.0;
pub const SESSION_BAR_HEIGHT: f32 = 56.0;
pub const SESSION_TAB_W: f32 = 260.0;
pub const SESSION_TAB_GAP: f32 = 4.0;
pub const SIDEBAR_W: f32 = 320.0;
pub const ICON_W: f32 = 152.0;
pub const ICON_H: f32 = 156.0;

// === Palette (One Dark — matches the user's terminal #22272e) ===
const BG: [f32; 4] = [0.094, 0.110, 0.133, 1.0]; // app bg — #181c22
const CHROME_BG: [f32; 4] = [0.110, 0.125, 0.149, 1.0]; // title-bar #1c2026
const SIDEBAR_BG: [f32; 4] = [0.110, 0.125, 0.149, 1.0]; // #1c2026
const PANEL_BG: [f32; 4] = [0.133, 0.153, 0.180, 1.0]; // unstyled tile — #22272e
const PANEL_HOVER: [f32; 4] = [0.157, 0.180, 0.212, 1.0];
const PANEL_ACTIVE: [f32; 4] = [0.180, 0.220, 0.290, 1.0];
const BORDER: [f32; 4] = [0.220, 0.250, 0.290, 1.0]; // #38404a
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

/// Glyphon GColor → [r,g,b,a] for QuadInstance. cosmic-text packs
/// channels ARGB into u32 (A=bits24-31, R=16-23, G=8-15, B=0-7), so
/// the byte at bits 16 is RED, not BLUE — swapping these turns #d78787
/// pink into #8787d7 light purple.
fn gcolor_to_rgba(c: GColor) -> [f32; 4] {
    let r = ((c.0 >> 16) & 0xff) as f32 / 255.0;
    let g = ((c.0 >> 8) & 0xff) as f32 / 255.0;
    let b = ((c.0 >> 0) & 0xff) as f32 / 255.0;
    let a = ((c.0 >> 24) & 0xff) as f32 / 255.0;
    [r, g, b, a]
}

/// Cell-relative fill rects for a Unicode block-drawing char. Returns
/// rectangles in (x, y, w, h) normalized 0..1 to the cell. Empty means
/// the char isn't a block we draw directly (let the font glyph handle).
/// We draw upper-half (▀), lower-half (▄), full (█), left-half (▌),
/// right-half (▐), eighth blocks (▁..▇, ▉..▏), the eighth top/right
/// strips (▔ ▕), and all 16 quadrant combinations (▖ ▗ ▘ ▙ ▚ ▛ ▜ ▝ ▞ ▟).
fn block_rects(ch: char) -> &'static [[f32; 4]] {
    // Each constant is a list of (x, y, w, h) tuples normalized to the cell.
    match ch {
        // Top eighth, then 1/4, 3/8, 1/2 (▀ ▄ is half), 5/8, 3/4, 7/8, full
        '\u{2580}' => &[[0.0, 0.0, 1.0, 0.5]],        // ▀ upper half
        '\u{2581}' => &[[0.0, 0.875, 1.0, 0.125]],   // ▁ lower 1/8
        '\u{2582}' => &[[0.0, 0.75, 1.0, 0.25]],     // ▂ lower 1/4
        '\u{2583}' => &[[0.0, 0.625, 1.0, 0.375]],   // ▃ lower 3/8
        '\u{2584}' => &[[0.0, 0.5, 1.0, 0.5]],        // ▄ lower half
        '\u{2585}' => &[[0.0, 0.375, 1.0, 0.625]],   // ▅
        '\u{2586}' => &[[0.0, 0.25, 1.0, 0.75]],     // ▆
        '\u{2587}' => &[[0.0, 0.125, 1.0, 0.875]],   // ▇
        '\u{2588}' => &[[0.0, 0.0, 1.0, 1.0]],        // █ full
        '\u{2589}' => &[[0.0, 0.0, 0.875, 1.0]],     // ▉ left 7/8
        '\u{258A}' => &[[0.0, 0.0, 0.75, 1.0]],      // ▊
        '\u{258B}' => &[[0.0, 0.0, 0.625, 1.0]],     // ▋
        '\u{258C}' => &[[0.0, 0.0, 0.5, 1.0]],        // ▌ left half
        '\u{258D}' => &[[0.0, 0.0, 0.375, 1.0]],     // ▍
        '\u{258E}' => &[[0.0, 0.0, 0.25, 1.0]],      // ▎
        '\u{258F}' => &[[0.0, 0.0, 0.125, 1.0]],     // ▏
        '\u{2590}' => &[[0.5, 0.0, 0.5, 1.0]],        // ▐ right half
        '\u{2594}' => &[[0.0, 0.0, 1.0, 0.125]],     // ▔ upper 1/8
        '\u{2595}' => &[[0.875, 0.0, 0.125, 1.0]],   // ▕ right 1/8
        '\u{2596}' => &[[0.0, 0.5, 0.5, 0.5]],        // ▖ lower-left quadrant
        '\u{2597}' => &[[0.5, 0.5, 0.5, 0.5]],        // ▗ lower-right
        '\u{2598}' => &[[0.0, 0.0, 0.5, 0.5]],        // ▘ upper-left
        '\u{2599}' => &[                              // ▙ ul + ll + lr
            [0.0, 0.0, 0.5, 0.5],
            [0.0, 0.5, 1.0, 0.5],
        ],
        '\u{259A}' => &[                              // ▚ ul + lr
            [0.0, 0.0, 0.5, 0.5],
            [0.5, 0.5, 0.5, 0.5],
        ],
        '\u{259B}' => &[                              // ▛ ul + ur + ll
            [0.0, 0.0, 1.0, 0.5],
            [0.0, 0.5, 0.5, 0.5],
        ],
        '\u{259C}' => &[                              // ▜ ul + ur + lr
            [0.0, 0.0, 1.0, 0.5],
            [0.5, 0.5, 0.5, 0.5],
        ],
        '\u{259D}' => &[[0.5, 0.0, 0.5, 0.5]],        // ▝ upper-right
        '\u{259E}' => &[                              // ▞ ur + ll
            [0.5, 0.0, 0.5, 0.5],
            [0.0, 0.5, 0.5, 0.5],
        ],
        '\u{259F}' => &[                              // ▟ ur + ll + lr
            [0.5, 0.0, 0.5, 0.5],
            [0.0, 0.5, 1.0, 0.5],
        ],
        // Light box-drawing — claude's welcome banner uses ─ heavily, and
        // every font on the user's system maps U+2500 to a different (often
        // wrong) glyph through cosmic-text's fallback. Paint them as quads
        // so the border looks like a real line, not a chevron mosaic.
        '\u{2500}' => &[[0.0, 0.46, 1.0, 0.08]],       // ─ horizontal
        '\u{2502}' => &[[0.46, 0.0, 0.08, 1.0]],       // │ vertical
        '\u{250C}' => &[                                // ┌ down-right
            [0.46, 0.46, 0.54, 0.08],
            [0.46, 0.46, 0.08, 0.54],
        ],
        '\u{2510}' => &[                                // ┐ down-left
            [0.0, 0.46, 0.54, 0.08],
            [0.46, 0.46, 0.08, 0.54],
        ],
        '\u{2514}' => &[                                // └ up-right
            [0.46, 0.46, 0.54, 0.08],
            [0.46, 0.0, 0.08, 0.54],
        ],
        '\u{2518}' => &[                                // ┘ up-left
            [0.0, 0.46, 0.54, 0.08],
            [0.46, 0.0, 0.08, 0.54],
        ],
        '\u{251C}' => &[                                // ├
            [0.46, 0.0, 0.08, 1.0],
            [0.46, 0.46, 0.54, 0.08],
        ],
        '\u{2524}' => &[                                // ┤
            [0.46, 0.0, 0.08, 1.0],
            [0.0, 0.46, 0.54, 0.08],
        ],
        '\u{252C}' => &[                                // ┬
            [0.0, 0.46, 1.0, 0.08],
            [0.46, 0.46, 0.08, 0.54],
        ],
        '\u{2534}' => &[                                // ┴
            [0.0, 0.46, 1.0, 0.08],
            [0.46, 0.0, 0.08, 0.54],
        ],
        '\u{253C}' => &[                                // ┼
            [0.0, 0.46, 1.0, 0.08],
            [0.46, 0.0, 0.08, 1.0],
        ],
        // Arc corners — claude uses these for its welcome banner edges
        // instead of the sharp ┌┐└┘ set. Approximated as two short
        // perpendicular strokes meeting at the cell centre.
        '\u{256D}' => &[                                // ╭ arc down-right
            [0.46, 0.46, 0.54, 0.08],
            [0.46, 0.46, 0.08, 0.54],
        ],
        '\u{256E}' => &[                                // ╮ arc down-left
            [0.0, 0.46, 0.54, 0.08],
            [0.46, 0.46, 0.08, 0.54],
        ],
        '\u{256F}' => &[                                // ╯ arc up-left
            [0.0, 0.46, 0.54, 0.08],
            [0.46, 0.0, 0.08, 0.54],
        ],
        '\u{2570}' => &[                                // ╰ arc up-right
            [0.46, 0.46, 0.54, 0.08],
            [0.46, 0.0, 0.08, 0.54],
        ],
        // Heavy box-drawing variants — claude occasionally mixes them in.
        '\u{2501}' => &[[0.0, 0.42, 1.0, 0.16]],        // ━ heavy horizontal
        '\u{2503}' => &[[0.42, 0.0, 0.16, 1.0]],        // ┃ heavy vertical
        _ => &[],
    }
}

/// Measure D2Coding cell metrics by laying out 20 'M' chars at the
/// requested font size and dividing. Same routine as before, but
/// parameterised so the zoom hotkeys can recompute at runtime.
fn measure_cell_at(font_system: &mut FontSystem, font_size: f32, line_height: f32) -> (f32, f32) {
    let mut buf = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buf.set_size(font_system, Some(2000.0), Some(line_height * 2.0));
    let attrs = Attrs::new().family(Family::Name("D2CodingLigature Nerd Font Mono"));
    buf.set_text(font_system, "MMMMMMMMMMMMMMMMMMMM", attrs, Shaping::Advanced);
    buf.shape_until_scroll(font_system, false);
    let total = buf
        .layout_runs()
        .next()
        .map(|r| r.line_w)
        .unwrap_or(font_size * 0.55 * 20.0);
    let measured = total / 20.0;
    // Round to whole pixels — anything fractional smears glyphs across
    // sub-pixel positions and tanks legibility. ceil() over the glyph
    // advance is enough breathing room for the side-bearing without
    // wasting a full pixel.
    let cw = (measured.max(font_size * 0.55)).ceil();
    let ch = line_height.round().max(1.0);
    (cw, ch)
}

fn measure_cell(font_system: &mut FontSystem) -> (f32, f32) {
    measure_cell_at(font_system, FONT_SIZE, LINE_HEIGHT)
}

/// Heuristic fallback before the renderer measures the real D2Coding advance.
// Matches measure_cell(D2Coding @ FONT_SIZE) on Linux/macOS — keep these in
// sync with the runtime probe so layout math (cols ↔ pixels) doesn't drift.
// These match measure_cell() at the default FONT_SIZE/LINE_HEIGHT.
// They're constants because main.rs needs them to size newly-spawned
// panes before a Renderer instance exists. Keep them in sync with the
// FONT_SIZE / LINE_HEIGHT constants above.
pub const CELL_W: f32 = 18.0;
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
    /// Active body font size — bumped/shrunk by Ctrl+/Ctrl-/Ctrl+0.
    /// Affects only terminal cell glyphs, not chrome (titles, tabs).
    pub body_font_size: f32,
    pub body_line_height: f32,
    /// Taskbar pane-button hit rects, refreshed every frame. Used by the
    /// click handler in main.rs to map a (x, y) hit to a pane id.
    pub taskbar_buttons: Vec<TaskbarHit>,
    /// Top-strip tab × close hit rects, one per tab, refreshed every
    /// frame. main.rs hit_test consults these so the user can close a
    /// tmux window straight from the tab strip.
    pub tab_close_hits: Vec<TabCloseHit>,
    /// When set, the next frame is also drawn to an offscreen texture
    /// and saved as PNG. Cleared after.
    pending_capture: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug)]
pub struct TabCloseHit {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub window_id: String,
}

#[derive(Clone, Debug)]
pub struct TaskbarHit {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub pane_id: String,
    /// Inset on the right side that hosts the × close glyph. main.rs
    /// hit_test routes clicks inside this rect to `PaneClose` instead
    /// of `TaskbarPane` so the user can dismiss the pane from the bar.
    pub close_x: f32,
    pub close_w: f32,
}

impl Renderer {
    pub fn request_capture(&mut self, path: std::path::PathBuf) {
        self.pending_capture = Some(path);
    }

    /// Set the terminal body font/line size and re-measure cell metrics.
    /// Returns the new (cell_w, cell_h) so the caller can re-issue
    /// resize-window for every pane.
    pub fn set_body_font_size(&mut self, size: f32) -> (f32, f32) {
        let size = size.clamp(7.0, 64.0);
        // Keep the leading proportional to the type body — same ratio
        // we use for the default (13:17 → ~1.31).
        let leading_ratio = LINE_HEIGHT / FONT_SIZE;
        self.body_font_size = size;
        self.body_line_height = size * leading_ratio;
        let (cw, ch) = measure_cell_at(
            &mut self.font_system,
            self.body_font_size,
            self.body_line_height,
        );
        self.cell_w = cw;
        self.cell_h = ch;
        (cw, ch)
    }

    pub fn body_font_size(&self) -> f32 {
        self.body_font_size
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
                    required_limits: adapter.limits(),
                    label: Some("tmuxify-device"),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("request_device")?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer non-sRGB surface — our color constants are sRGB hex
        // values stored as 0..1, and we want the GPU to pass them through
        // verbatim. With an sRGB surface, wgpu auto-encodes shader output
        // (treated as linear) into sRGB on store, which would brighten
        // every dark color (e.g. #22272e → #65737f) and make panes look
        // foggy. Falling back to sRGB only if no Unorm surface is exposed.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm))
            .or_else(|| {
                caps.formats
                    .iter()
                    .copied()
                    .find(|f| matches!(f, TextureFormat::Bgra8UnormSrgb | TextureFormat::Rgba8UnormSrgb))
            })
            .unwrap_or(caps.formats[0]);
        println!("[surface] format={format:?}");
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
            body_font_size: FONT_SIZE,
            body_line_height: LINE_HEIGHT,
            taskbar_buttons: Vec::new(),
            tab_close_hits: Vec::new(),
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
        body_w: f32,
        body_h: f32,
        default_color: GColor,
        occluders: &[(f32, f32, f32, f32)],
        quads: &mut Vec<QuadInstance>,
    ) -> Vec<(Buffer, f32, f32, GColor, f32)> {
        let attrs = Attrs::new().family(Family::Name("D2CodingLigature Nerd Font Mono"));
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let body_fs = self.body_font_size;
        let body_lh = self.body_line_height;
        let mut out: Vec<(Buffer, f32, f32, GColor, f32)> = Vec::new();
        // Anchor the body origin on integer pixels. fp.x/fp.y comes from
        // drag math and is often fractional; without this, every cell
        // would inherit a sub-pixel offset and glyphs would smear.
        let body_left = body_left.round();
        let body_top = body_top.round();
        let body_right = body_left + body_w;
        let body_bottom = body_top + body_h;
        for (row_i, row) in pg.cells.iter().enumerate() {
            let row_top = body_top + row_i as f32 * cell_h;
            // Cells whose top edge is at/past the body bottom would
            // spill out of the pane (e.g. while the user is shrinking
            // the pane and tmux hasn't re-flowed the inner app yet).
            if row_top >= body_bottom {
                break;
            }
            let mut col_i = 0usize;
            while col_i < row.len() {
                let cell = &row[col_i];
                let raw = if cell.ch.is_empty() { " " } else { cell.ch.as_str() };
                let first = raw.chars().next().unwrap_or(' ');
                let width_cells = UnicodeWidthChar::width(first).unwrap_or(1).max(1) as f32;
                let color = term_to_glyphon(&cell.fg, default_color);
                let pixel_w = cell_w * width_cells;
                // Snap cell origin to integer pixels — anything else
                // makes glyphon antialias the glyph across a half-pixel
                // boundary, blurring every character and shaving
                // perceived contrast. Terminal cells should always be
                // pixel-aligned.
                let glyph_left = (body_left + col_i as f32 * cell_w).round();
                let glyph_top = row_top.round();
                // Same idea horizontally — drop cells past the right edge.
                if glyph_left >= body_right {
                    break;
                }
                // Skip cells whose center sits under a pane stacked above
                // us — otherwise glyphon's single text pass would render
                // them right through the upper pane's body quad.
                let cx = glyph_left + pixel_w * 0.5;
                let cy = glyph_top + cell_h * 0.5;
                if occluders
                    .iter()
                    .any(|&(l, t, r, b)| cx >= l && cx < r && cy >= t && cy < b)
                {
                    col_i += width_cells as usize;
                    continue;
                }
                // Background fill — when claude (or any TUI) explicitly
                // sets a bg color, draw it before glyphs. The Anthropic
                // logo's black backdrop comes from this.
                if !matches!(cell.bg, TermColor::Default) {
                    let bg = term_to_glyphon_bg(&cell.bg, default_color);
                    quads.push(QuadInstance {
                        rect: [glyph_left, glyph_top, pixel_w, cell_h],
                        color: gcolor_to_rgba(bg),
                    });
                }
                // Unicode block-drawing chars — paint as cell-precise
                // rectangles instead of font glyphs. Eliminates hairline
                // gaps between cells in big logo blocks.
                let rects = block_rects(first);
                if !rects.is_empty() {
                    let rgba = gcolor_to_rgba(color);
                    for r in rects {
                        quads.push(QuadInstance {
                            rect: [
                                glyph_left + r[0] * pixel_w,
                                glyph_top + r[1] * cell_h,
                                r[2] * pixel_w,
                                r[3] * cell_h,
                            ],
                            color: rgba,
                        });
                    }
                    col_i += width_cells as usize;
                    continue;
                }
                if first == ' ' {
                    col_i += 1;
                    continue;
                }
                let mut b = Buffer::new(&mut self.font_system, Metrics::new(body_fs, body_lh));
                b.set_size(&mut self.font_system, Some(pixel_w + 4.0), Some(cell_h * 1.5));
                b.set_text(&mut self.font_system, raw, attrs.color(color), Shaping::Advanced);
                b.shape_until_scroll(&mut self.font_system, false);
                out.push((
                    b,
                    glyph_left,
                    glyph_top,
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
        let attrs = Attrs::new().family(Family::Name("D2CodingLigature Nerd Font Mono"));
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
        let attrs = Attrs::new().family(Family::Name("D2CodingLigature Nerd Font Mono"));
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
        snap_overlay: Option<[f32; 4]>,
        // (pane_id, start_cell, end_cell) ordered start ≤ end. Paints
        // translucent quads behind the glyphs to show what Cmd+C copies.
        selection: Option<(&str, (u16, u16), (u16, u16))>,
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
        let attrs = Attrs::new().family(Family::Name("D2CodingLigature Nerd Font Mono"));

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

        self.tab_close_hits.clear();
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
            // Reserve right-side close-glyph zone.
            let close_w = 22.0;
            let close_x = tab_x + SESSION_TAB_W - close_w - 4.0;
            let label_w = (SESSION_TAB_W - 12.0 - close_w).max(20.0);
            let mut tab_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            tab_buf.set_size(
                &mut self.font_system,
                Some(label_w),
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
                    right: (tab_x + 8.0 + label_w) as i32,
                    bottom: (tab_y + tab_h) as i32,
                },
                // Brighter idle-tab text — TEXT_SEC was too washed for
                // the user to tell two tabs apart at a glance.
                color: if active { TEXT_PRI } else { GColor::rgb(0xc8, 0xcf, 0xd8) },
            });
            // × close glyph + hit zone — red so it reads as "destroy".
            quads.push(QuadInstance {
                rect: [close_x, tab_y + 4.0, close_w, tab_h - 8.0],
                color: [0.78, 0.22, 0.22, 1.0],
            });
            let mut x_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            x_buf.set_size(&mut self.font_system, Some(close_w), Some(tab_h));
            x_buf.set_text(
                &mut self.font_system,
                "×",
                attrs.color(GColor::rgb(0xff, 0xee, 0xee)),
                Shaping::Advanced,
            );
            x_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: x_buf,
                left: close_x + 6.0,
                top: tab_y + 3.0,
                bounds: TextBounds {
                    left: close_x as i32,
                    top: tab_y as i32,
                    right: (close_x + close_w) as i32,
                    bottom: (tab_y + tab_h) as i32,
                },
                color: GColor::rgb(0xff, 0xee, 0xee),
            });
            self.tab_close_hits.push(TabCloseHit {
                x: close_x,
                y: tab_y,
                w: close_w,
                h: tab_h,
                window_id: wid.clone(),
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
        // Drawn with persistent button rects + a brighter glyph so
        // they're clearly clickable (previously the glyphs sat alone
        // on the title bar with no affordance).
        let btn_w = 46.0;
        let close_x = self.width as f32 - btn_w;
        let max_x = close_x - btn_w;
        let min_x = max_x - btn_w;
        for (i, (label, btn_bg, glyph_color)) in [
            (" ─ ", PANEL_BG, TEXT_PRI),
            (" ▢ ", PANEL_BG, TEXT_PRI),
            (" × ", [0.78, 0.22, 0.22, 1.0], GColor::rgb(0xff, 0xee, 0xee)),
        ]
        .iter()
        .enumerate()
        {
            let bx = [min_x, max_x, close_x][i];
            // Full-height button background — gives the user a target
            // and signals "click here".
            quads.push(QuadInstance {
                rect: [bx, 0.0, btn_w, SESSION_BAR_HEIGHT],
                color: *btn_bg,
            });
            let mut buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            buf.set_size(
                &mut self.font_system,
                Some(btn_w),
                Some(SESSION_BAR_HEIGHT),
            );
            buf.set_text(
                &mut self.font_system,
                label,
                attrs.color(*glyph_color),
                Shaping::Advanced,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: buf,
                left: bx + 12.0,
                top: 9.0,
                bounds: TextBounds {
                    left: bx as i32,
                    top: 0,
                    right: (bx + btn_w) as i32,
                    bottom: SESSION_BAR_HEIGHT as i32,
                },
                color: *glyph_color,
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
            let search_h = 56.0;
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
            let row_h = 100.0;
            let row_gap = 8.0;
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
                let close_w = 40.0;
                let close_left = sidebar_w - 14.0 - close_w;
                quads.push(QuadInstance {
                    rect: [close_left + 2.0, row_y + 28.0, close_w - 4.0, 44.0],
                    color: [0.04, 0.05, 0.07, 1.0],
                });
                let mut x_buf =
                    Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
                x_buf.set_size(&mut self.font_system, Some(close_w), Some(48.0));
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
                    Some(48.0),
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
                    Some(36.0),
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
                    top: row_y + 56.0,
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
            let new_h = 60.0;
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

        // === Desktop icons (shape drawn as quads, no emoji font) ===
        // Icons live "on the desktop". Skip any icon whose rect is covered
        // by an existing floating pane — otherwise the icon label leaks
        // through the pane background in the text pass, since glyphon
        // doesn't z-order against quads.
        let icon_covered = |icon: &DesktopIcon| -> bool {
            let l = icon.x;
            let t = icon.y;
            let r = icon.x + ICON_W;
            let b = icon.y + ICON_H;
            floating.values().any(|fp| {
                fp.x <= l && fp.y <= t && fp.x + fp.w >= r && fp.y + fp.h >= b
            })
        };
        for icon in icons {
            if icon_covered(icon) { continue }
            let tile = 96.0;
            let tx = icon.x + (ICON_W - tile) * 0.5;
            let ty = icon.y;
            match &icon.kind {
                IconKind::Folder { .. } => {
                    let body_top = ty + 24.0;
                    let body_h = tile - 28.0;
                    let tab_w = 36.0;
                    let tab_h = 12.0;
                    let folder_face = [0.27, 0.42, 0.62, 1.0];
                    let folder_edge = [0.45, 0.65, 0.95, 1.0];
                    quads.push(QuadInstance {
                        rect: [tx + 8.0, ty + 12.0, tab_w, tab_h + 8.0],
                        color: folder_edge,
                    });
                    quads.push(QuadInstance {
                        rect: [tx, body_top, tile, body_h],
                        color: folder_face,
                    });
                    quads.push(QuadInstance {
                        rect: [tx, body_top, tile, 4.0],
                        color: folder_edge,
                    });
                }
                IconKind::Claude { .. } => {
                    // Anthropic-orange filled tile with a soft inner halo
                    // and a small "C" mark — visually distinguishes the
                    // launcher from generic folder shortcuts.
                    let body = [0.96, 0.45, 0.20, 1.0]; // Anthropic-ish orange
                    let body_dim = [0.75, 0.34, 0.14, 1.0];
                    let highlight = [1.0, 0.62, 0.32, 1.0];
                    quads.push(QuadInstance {
                        rect: [tx, ty + 6.0, tile, tile - 6.0],
                        color: body,
                    });
                    quads.push(QuadInstance {
                        rect: [tx, ty + 6.0, tile, 6.0],
                        color: highlight,
                    });
                    quads.push(QuadInstance {
                        rect: [tx, ty + tile - 8.0, tile, 8.0],
                        color: body_dim,
                    });
                    // Two stylised "eye" pixels so it reads as a character.
                    let eye = [0.18, 0.10, 0.06, 1.0];
                    quads.push(QuadInstance {
                        rect: [tx + tile * 0.30 - 8.0, ty + tile * 0.42, 14.0, 14.0],
                        color: eye,
                    });
                    quads.push(QuadInstance {
                        rect: [tx + tile * 0.70 - 6.0, ty + tile * 0.42, 14.0, 14.0],
                        color: eye,
                    });
                }
            }
            // Label below.
            let mut l_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE_SM, FONT_SIZE_SM + 3.0));
            l_buf.set_size(&mut self.font_system, Some(ICON_W), Some(40.0));
            l_buf.set_text(&mut self.font_system, &icon.label, attrs, Shaping::Advanced);
            for line in l_buf.lines.iter_mut() {
                line.set_align(Some(Align::Center));
            }
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
        // Per-pane rect so each pane can skip cells covered by any pane
        // stacked above it. order is back-to-front so indices > i are
        // the panes drawn on top of pane i.
        let pane_rects: Vec<(f32, f32, f32, f32)> = order
            .iter()
            .map(|f| (f.x, f.y, f.x + f.w, f.y + f.h))
            .collect();

        for (idx, fp) in order.iter().enumerate() {
            let fp = *fp;
            let occluders: &[(f32, f32, f32, f32)] = &pane_rects[idx + 1..];
            let is_active = active_pane == Some(&fp.pane_id);
            let border_color = if is_active { ACCENT_DIM } else { BORDER };
            let title_bg = if is_active { PANEL_ACTIVE } else { PANEL_BG };
            let body_bg = [0.133, 0.153, 0.180, 1.0]; // #22272e — matches reference terminal
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

            // Title — but only if no pane above covers our title bar.
            // Quick rect test against each occluder; skip pushing the
            // title glyphs if any upper pane fully spans the title row.
            let title_l = fp.x;
            let title_r = fp.x + fp.w;
            let title_t = fp.y;
            let title_b = fp.y + TITLE_HEIGHT;
            // Any overlap with a pane above hides the whole title bar.
            // Title is one glyphon Buffer so we can't selectively clip
            // half of it — drop the lot rather than letting it leak.
            let title_hidden = occluders.iter().any(|&(l, t, r, b)| {
                l < title_r && r > title_l && t < title_b && b > title_t
            });
            if !title_hidden {
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
            // X close button — bigger hit-area, persistent red bg so
            // the user can spot it immediately.
            let close_w = 26.0;
            let cx = fp.x + fp.w - close_w - 2.0;
            let cy = fp.y + 3.0;
            let close_h = TITLE_HEIGHT - 6.0;
            quads.push(QuadInstance {
                rect: [cx, cy, close_w, close_h],
                color: [0.78, 0.22, 0.22, 1.0],
            });
            let mut x_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            x_buf.set_size(&mut self.font_system, Some(close_w), Some(TITLE_HEIGHT));
            x_buf.set_text(
                &mut self.font_system,
                "×",
                attrs.color(GColor::rgb(0xff, 0xee, 0xee)),
                Shaping::Advanced,
            );
            x_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: x_buf,
                left: cx + 8.0,
                top: cy,
                bounds: TextBounds {
                    left: cx as i32,
                    top: cy as i32,
                    right: (cx + close_w) as i32,
                    bottom: (cy + close_h) as i32,
                },
                color: GColor::rgb(0xff, 0xee, 0xee),
            });
            } // !title_hidden

            // Body.
            let Some(pg) = panes.get(&fp.pane_id) else {
                continue;
            };
            let body_w = (fp.w - BOX_PAD * 2.0).max(1.0);
            let body_h = (fp.h - TITLE_HEIGHT - BOX_PAD).max(1.0);
            // Block cursor on the active pane (only when blink-on). Use
            // the renderer's measured cell metrics so the block lines up
            // with the cell-by-cell glyphs.
            if is_active && cursor_visible && pg.cols > 0 && pg.rows > 0 {
                let cw = self.cell_w;
                let ch = self.cell_h;
                let cx = fp.x + BOX_PAD + pg.cursor_col as f32 * cw;
                let cy = fp.y + TITLE_HEIGHT + pg.cursor_row as f32 * ch;
                quads.push(QuadInstance {
                    rect: [cx, cy, cw, ch],
                    color: [0.45, 0.65, 0.95, 0.55],
                });
            }
            // Text-selection highlight. Paint one quad per fully-selected
            // row + a partial-row quad for the start/end rows. Anchored
            // behind the glyphs so antialiased text still reads cleanly.
            if let Some((sel_pid, (sr, sc), (er, ec))) = selection {
                if sel_pid == fp.pane_id.as_str() && pg.cols > 0 {
                    let cw = self.cell_w;
                    let ch = self.cell_h;
                    let max_cols = pg.cols.saturating_sub(1);
                    let body_left = fp.x + BOX_PAD;
                    let body_top = fp.y + TITLE_HEIGHT;
                    for r in sr..=er {
                        if r >= pg.rows { break }
                        let c0 = if r == sr { sc } else { 0 };
                        let c1 = if r == er { ec } else { max_cols };
                        let c1 = c1.min(max_cols);
                        if c1 < c0 { continue }
                        let count = (c1 - c0 + 1) as f32;
                        quads.push(QuadInstance {
                            rect: [
                                body_left + c0 as f32 * cw,
                                body_top + r as f32 * ch,
                                count * cw,
                                ch,
                            ],
                            color: [0.30, 0.50, 0.85, 0.40],
                        });
                    }
                }
            }
            let default_text_color = if is_active { TEXT_PRI } else { TEXT_SEC };
            let segs = self.build_body_cells(
                pg,
                fp.x + BOX_PAD,
                fp.y + TITLE_HEIGHT,
                body_w,
                body_h,
                default_text_color,
                occluders,
                &mut quads,
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
                        bottom: (top + self.body_line_height * 1.5) as i32,
                    },
                    color,
                });
            }
        }

        // === Taskbar (windows-style: start | pane buttons | tray) ===
        self.taskbar_buttons.clear();
        let bar_top = self.height as f32 - STATUS_HEIGHT;
        let bar_h = STATUS_HEIGHT;
        let bar_left = sidebar_w;
        let bar_w = (self.width as f32 - bar_left).max(1.0);
        // Bar background — only covers the right side of the sidebar so
        // it doesn't paint over the session list.
        quads.push(QuadInstance {
            rect: [bar_left, bar_top, bar_w, bar_h],
            color: CHROME_BG,
        });
        // Top hairline
        quads.push(QuadInstance {
            rect: [bar_left, bar_top, bar_w, 1.0],
            color: BORDER,
        });

        let btn_y = bar_top + (bar_h - TASKBAR_BTN_H) * 0.5;
        // Start button removed — felt purely decorative, the user
        // navigates via the tab strip / desktop icons instead.
        let _ = TASKBAR_START_W;

        // --- Right tray: clock + IME ---
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let kst = now + 9 * 3600;
        let h = (kst / 3600) % 24;
        let m = (kst / 60) % 60;
        let clock = format!("{:02}:{:02}", h, m);
        let mode = if hangul_mode { "한" } else { "EN" };

        let clock_w = 56.0_f32;
        let ime_w = 36.0_f32;
        let tray_right = self.width as f32 - TASKBAR_GAP;
        let clock_x = tray_right - clock_w;
        let ime_x = clock_x - TASKBAR_GAP - ime_w;

        let mut clock_buf =
            Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        clock_buf.set_size(&mut self.font_system, Some(clock_w), Some(TASKBAR_BTN_H));
        clock_buf.set_text(&mut self.font_system, &clock, attrs.color(TEXT_PRI), Shaping::Advanced);
        clock_buf.shape_until_scroll(&mut self.font_system, false);
        built.push(Built {
            buffer: clock_buf,
            left: clock_x + 4.0,
            top: btn_y + 5.0,
            bounds: TextBounds {
                left: clock_x as i32,
                top: btn_y as i32,
                right: (clock_x + clock_w) as i32,
                bottom: (btn_y + TASKBAR_BTN_H) as i32,
            },
            color: TEXT_PRI,
        });

        let ime_color = if hangul_mode { ACCENT_DIM_TEXT } else { TEXT_MUT };
        let mut ime_buf =
            Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        ime_buf.set_size(&mut self.font_system, Some(ime_w), Some(TASKBAR_BTN_H));
        ime_buf.set_text(&mut self.font_system, mode, attrs.color(ime_color), Shaping::Advanced);
        ime_buf.shape_until_scroll(&mut self.font_system, false);
        built.push(Built {
            buffer: ime_buf,
            left: ime_x + 6.0,
            top: btn_y + 5.0,
            bounds: TextBounds {
                left: ime_x as i32,
                top: btn_y as i32,
                right: (ime_x + ime_w) as i32,
                bottom: (btn_y + TASKBAR_BTN_H) as i32,
            },
            color: ime_color,
        });

        // --- Pane buttons (middle, between start and tray) ---
        let panes_left = bar_left + TASKBAR_GAP;
        let panes_right = ime_x - TASKBAR_GAP * 2.0;
        let max_btn = TASKBAR_PANE_W;
        let count = floating.len() as f32;
        let avail = (panes_right - panes_left).max(0.0);
        let btn_w = if count > 0.0 {
            ((avail - TASKBAR_GAP * (count - 1.0).max(0.0)) / count).min(max_btn).max(60.0)
        } else {
            0.0
        };
        let mut bx = panes_left;
        for fp in floating.values() {
            if bx + btn_w > panes_right {
                break;
            }
            let is_active = active_pane == Some(&fp.pane_id);
            let bg = if is_active { PANEL_ACTIVE } else { PANEL_BG };
            quads.push(QuadInstance {
                rect: [bx, btn_y, btn_w, TASKBAR_BTN_H],
                color: bg,
            });
            // Active indicator bar at the bottom
            if is_active {
                quads.push(QuadInstance {
                    rect: [bx, btn_y + TASKBAR_BTN_H - 2.0, btn_w, 2.0],
                    color: ACCENT_DIM,
                });
            }
            // Reserve space for the × close glyph on the right.
            let close_w = 22.0;
            let label_w = (btn_w - 12.0 - close_w).max(20.0);
            let mut label_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            label_buf.set_size(&mut self.font_system, Some(label_w), Some(TASKBAR_BTN_H));
            let label_color = if is_active { TEXT_PRI } else { TEXT_SEC };
            label_buf.set_text(
                &mut self.font_system,
                &fp.title,
                attrs.color(label_color),
                Shaping::Advanced,
            );
            label_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: label_buf,
                left: bx + 8.0,
                top: btn_y + 5.0,
                bounds: TextBounds {
                    left: bx as i32,
                    top: btn_y as i32,
                    right: (bx + 8.0 + label_w) as i32,
                    bottom: (btn_y + TASKBAR_BTN_H) as i32,
                },
                color: label_color,
            });
            // × close glyph on the right edge of the button — red bg
            // for consistency with the pane / OS-window close buttons.
            let close_x = bx + btn_w - close_w - 2.0;
            quads.push(QuadInstance {
                rect: [close_x, btn_y + 3.0, close_w, TASKBAR_BTN_H - 6.0],
                color: [0.78, 0.22, 0.22, 1.0],
            });
            let mut x_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            x_buf.set_size(&mut self.font_system, Some(close_w), Some(TASKBAR_BTN_H));
            x_buf.set_text(
                &mut self.font_system,
                "×",
                attrs.color(GColor::rgb(0xff, 0xee, 0xee)),
                Shaping::Advanced,
            );
            x_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: x_buf,
                left: close_x + 6.0,
                top: btn_y + 3.0,
                bounds: TextBounds {
                    left: close_x as i32,
                    top: btn_y as i32,
                    right: (close_x + close_w) as i32,
                    bottom: (btn_y + TASKBAR_BTN_H) as i32,
                },
                color: GColor::rgb(0xff, 0xee, 0xee),
            });
            self.taskbar_buttons.push(TaskbarHit {
                x: bx,
                y: btn_y,
                w: btn_w,
                h: TASKBAR_BTN_H,
                pane_id: fp.pane_id.clone(),
                close_x,
                close_w,
            });
            bx += btn_w + TASKBAR_GAP;
        }

        // Preedit floats above the bar when typing Hangul.
        if let Some(p) = preedit {
            let mut pre_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            pre_buf.set_size(&mut self.font_system, Some(200.0), Some(TASKBAR_BTN_H));
            pre_buf.set_text(&mut self.font_system, p, attrs.color(ACCENT_DIM_TEXT), Shaping::Advanced);
            pre_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: pre_buf,
                left: ime_x - 200.0 - TASKBAR_GAP,
                top: btn_y + 5.0,
                bounds: TextBounds {
                    left: (ime_x - 200.0 - TASKBAR_GAP) as i32,
                    top: btn_y as i32,
                    right: ime_x as i32,
                    bottom: (btn_y + TASKBAR_BTN_H) as i32,
                },
                color: ACCENT_DIM_TEXT,
            });
        }

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
        let all: Vec<TextArea> = win_areas.collect();

        // Aero-Snap preview overlay — translucent accent rect that
        // hints where the OS window will land if the user releases the
        // drag now. Drawn last so it sits on top of all panes/chrome.
        if let Some(rect) = snap_overlay {
            quads.push(QuadInstance {
                rect,
                color: [0.353, 0.510, 0.953, 0.28],
            });
            // Inner border to make it read as a "preview" frame.
            let stroke = 2.0;
            quads.push(QuadInstance {
                rect: [rect[0], rect[1], rect[2], stroke],
                color: [0.353, 0.510, 0.953, 0.85],
            });
            quads.push(QuadInstance {
                rect: [rect[0], rect[1] + rect[3] - stroke, rect[2], stroke],
                color: [0.353, 0.510, 0.953, 0.85],
            });
            quads.push(QuadInstance {
                rect: [rect[0], rect[1], stroke, rect[3]],
                color: [0.353, 0.510, 0.953, 0.85],
            });
            quads.push(QuadInstance {
                rect: [rect[0] + rect[2] - stroke, rect[1], stroke, rect[3]],
                color: [0.353, 0.510, 0.953, 0.85],
            });
        }

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
