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
use winit::window::Window;

use crate::quad::{QuadInstance, QuadRenderer};
use crate::{FloatingPane, PaneGrid};

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

pub const CELL_W: f32 = FONT_SIZE * 0.55;
pub const CELL_H: f32 = LINE_HEIGHT;

pub fn cells_for_size(width: u32, height: u32) -> (u16, u16) {
    let canvas_w = (width as f32 - PADDING * 2.0).max(1.0);
    let canvas_h =
        (height as f32 - PADDING * 2.0 - STATUS_HEIGHT - SESSION_BAR_HEIGHT).max(1.0);
    let cols = ((canvas_w / CELL_W).floor() as u16).max(20);
    let rows = ((canvas_h / CELL_H).floor() as u16).max(5);
    (cols, rows)
}

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
        let font_system = FontSystem::new();
        let swash = SwashCache::new();
        let quads = QuadRenderer::new(&device, format, width as f32, height as f32)?;

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
        })
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
    ) -> Result<()> {
        let sidebar_w = if sidebar_open { SIDEBAR_W } else { 0.0 };
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
                let mut name_buf =
                    Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
                name_buf.set_size(
                    &mut self.font_system,
                    Some(sidebar_w - 40.0),
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
                Some(fp.w.max(20.0)),
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
                top: fp.y + 2.0,
                bounds: TextBounds {
                    left: fp.x as i32,
                    top: fp.y as i32,
                    right: (fp.x + fp.w) as i32,
                    bottom: (fp.y + TITLE_HEIGHT) as i32,
                },
                color: if is_active { TEXT_PRI } else { TEXT_SEC },
            });

            // Body.
            let Some(pg) = panes.get(&fp.pane_id) else {
                continue;
            };
            let mut body = String::with_capacity(pg.grid.iter().map(|l| l.len() + 1).sum());
            for line in &pg.grid {
                body.push_str(line);
                body.push('\n');
            }
            let body_w = (fp.w - BOX_PAD * 2.0).max(1.0);
            let body_h = (fp.h - TITLE_HEIGHT - BOX_PAD).max(1.0);
            let mut body_buf =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            body_buf.set_size(&mut self.font_system, Some(body_w), Some(body_h));
            body_buf.set_text(&mut self.font_system, &body, attrs, Shaping::Advanced);
            body_buf.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: body_buf,
                left: fp.x + BOX_PAD,
                top: fp.y + TITLE_HEIGHT,
                bounds: TextBounds {
                    left: (fp.x + BOX_PAD) as i32,
                    top: (fp.y + TITLE_HEIGHT) as i32,
                    right: (fp.x + fp.w - BOX_PAD) as i32,
                    bottom: (fp.y + fp.h - BOX_PAD) as i32,
                },
                color: if is_active { TEXT_PRI } else { TEXT_SEC },
            });
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
        Ok(())
    }
}
