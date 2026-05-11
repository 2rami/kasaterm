//! wgpu + glyphon based renderer for tmuxify's desktop mode.
//! Each tmux window is drawn as a free-floating box at coordinates owned
//! by the app (not by tmux's layout). One pane per window for now.

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

use crate::{FloatingWindow, PaneGrid};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 18.0;
const PADDING: f32 = 8.0;
const STATUS_HEIGHT: f32 = 22.0;
const TITLE_HEIGHT: f32 = 22.0;
const BOX_PAD: f32 = 6.0;
/// D2Coding monospace heuristic — width ≈ FONT_SIZE * 0.55 at 14pt.
pub const CELL_W: f32 = FONT_SIZE * 0.55;
pub const CELL_H: f32 = LINE_HEIGHT;

/// Convert (window pixels) → default (cols, rows) for tmux. In desktop
/// mode we still tell tmux a global size; per-window resize comes later.
pub fn cells_for_size(width: u32, height: u32) -> (u16, u16) {
    let canvas_w = (width as f32 - PADDING * 2.0).max(1.0);
    let canvas_h = (height as f32 - PADDING * 2.0 - STATUS_HEIGHT).max(1.0);
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
    }

    pub fn render(
        &mut self,
        floating: &BTreeMap<String, FloatingWindow>,
        panes: &HashMap<String, PaneGrid>,
        active_window: Option<&str>,
        hangul_mode: bool,
        preedit: Option<&str>,
    ) -> Result<()> {
        struct Built {
            buffer: Buffer,
            left: f32,
            top: f32,
            bounds: TextBounds,
            color: GColor,
        }
        let mut built: Vec<Built> = Vec::new();

        // Iterate windows so the active one renders last (on top, in text-
        // overlap terms). BTreeMap order is stable by id.
        let mut order: Vec<&FloatingWindow> = floating.values().collect();
        order.sort_by_key(|w| (active_window == Some(&w.window_id)) as u8);

        for fw in order {
            let is_active = active_window == Some(&fw.window_id);
            // Title.
            let title_text = if is_active {
                format!("● {}", fw.title)
            } else {
                format!("  {}", fw.title)
            };
            let mut title_buffer =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            title_buffer.set_size(
                &mut self.font_system,
                Some(fw.w.max(20.0)),
                Some(TITLE_HEIGHT),
            );
            let attrs = Attrs::new().family(Family::Name("D2Coding"));
            title_buffer.set_text(
                &mut self.font_system,
                &title_text,
                attrs,
                Shaping::Advanced,
            );
            title_buffer.shape_until_scroll(&mut self.font_system, false);
            built.push(Built {
                buffer: title_buffer,
                left: fw.x + BOX_PAD,
                top: fw.y + 2.0,
                bounds: TextBounds {
                    left: fw.x as i32,
                    top: fw.y as i32,
                    right: (fw.x + fw.w) as i32,
                    bottom: (fw.y + TITLE_HEIGHT) as i32,
                },
                color: if is_active {
                    GColor::rgb(0xff, 0xff, 0xff)
                } else {
                    GColor::rgb(0xb0, 0xb0, 0xb0)
                },
            });

            // Body — pane grid (if known).
            let Some(pid) = fw.pane_id.as_ref() else {
                continue;
            };
            let Some(pg) = panes.get(pid) else {
                continue;
            };
            let mut body = String::with_capacity(pg.grid.iter().map(|l| l.len() + 1).sum());
            for line in &pg.grid {
                body.push_str(line);
                body.push('\n');
            }
            let body_w = (fw.w - BOX_PAD * 2.0).max(1.0);
            let body_h = (fw.h - TITLE_HEIGHT - BOX_PAD).max(1.0);
            let mut body_buffer =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            body_buffer.set_size(&mut self.font_system, Some(body_w), Some(body_h));
            body_buffer.set_text(&mut self.font_system, &body, attrs, Shaping::Advanced);
            body_buffer.shape_until_scroll(&mut self.font_system, false);
            let body_left = fw.x + BOX_PAD;
            let body_top = fw.y + TITLE_HEIGHT;
            built.push(Built {
                buffer: body_buffer,
                left: body_left,
                top: body_top,
                bounds: TextBounds {
                    left: body_left as i32,
                    top: body_top as i32,
                    right: (body_left + body_w) as i32,
                    bottom: (body_top + body_h) as i32,
                },
                color: if is_active {
                    GColor::rgb(0xe6, 0xe6, 0xe6)
                } else {
                    GColor::rgb(0x88, 0x88, 0x88)
                },
            });
        }

        // Status line.
        let mode = if hangul_mode { "한글" } else { "EN" };
        let mut status = format!("[{mode}]  windows={}", floating.len());
        if let Some(p) = preedit {
            let _ = std::fmt::Write::write_fmt(&mut status, format_args!("  {p}"));
        }
        let mut status_buffer =
            Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        let canvas_w = (self.width as f32 - PADDING * 2.0).max(1.0);
        status_buffer.set_size(&mut self.font_system, Some(canvas_w), Some(STATUS_HEIGHT));
        let attrs = Attrs::new().family(Family::Name("D2Coding"));
        status_buffer.set_text(&mut self.font_system, &status, attrs, Shaping::Advanced);
        status_buffer.shape_until_scroll(&mut self.font_system, false);
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
            buffer: &status_buffer,
            left: PADDING,
            top: status_top,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: status_top as i32,
                right: self.width as i32,
                bottom: self.height as i32,
            },
            default_color: GColor::rgb(0xa0, 0xa0, 0xa0),
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
                label: Some("text-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            // Desktop background: warm dark slate.
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
