//! wgpu + glyphon based monospace text renderer for tmuxify.
//! No colors / no attributes yet — first goal is just legible D2Coding
//! text on a dark surface, plus a status line and preedit.

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

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 18.0;
const PADDING: f32 = 8.0;

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
        grid: &[String],
        _cursor_row: u16,
        _cursor_col: u16,
        hangul_mode: bool,
        preedit: Option<&str>,
    ) -> Result<()> {
        // Compose grid + status line into a single textual buffer for
        // this MVP. Cursor / colors / per-cell attrs come later.
        let mut text = String::with_capacity(grid.iter().map(|l| l.len() + 1).sum::<usize>() + 64);
        for line in grid {
            text.push_str(line);
            text.push('\n');
        }
        let mode = if hangul_mode { "한글" } else { "EN" };
        text.push_str(&format!("\n[{mode}]"));
        if let Some(p) = preedit {
            text.push(' ');
            text.push_str(p);
        }

        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        let inner_w = (self.width as f32 - PADDING * 2.0).max(1.0);
        let inner_h = (self.height as f32 - PADDING * 2.0).max(1.0);
        buffer.set_size(&mut self.font_system, Some(inner_w), Some(inner_h));
        let attrs = Attrs::new().family(Family::Name("D2Coding"));
        buffer.set_text(&mut self.font_system, &text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );
        let area = TextArea {
            buffer: &buffer,
            left: PADDING,
            top: PADDING,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: self.width as i32,
                bottom: self.height as i32,
            },
            default_color: GColor::rgb(0xe6, 0xe6, 0xe6),
            custom_glyphs: &[],
        };
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [area],
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
                            r: 0.10,
                            g: 0.11,
                            b: 0.13,
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
