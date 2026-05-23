//! Phase 1 deliverable: a winit window that draws a tall ASCII
//! buffer in B&W via cell-renderer's retained-mode wgpu pipeline,
//! plus arrow-key / wheel scroll so we can feel the perf delta
//! against the old sugarloaf path.
//!
//! Goal is *not* feature parity — it's proving the architecture:
//! glyphs bake into the atlas once, the per-frame cost is one
//! instance-buffer write + one draw call. Frame time prints to
//! stdout each frame so the 30-50ms sugarloaf number is directly
//! comparable.
//!
//! Run:
//!   KASATERM_GRID_FONT=/System/Library/Fonts/Menlo.ttc \
//!     cargo run --release -p cell-renderer --example grid_bw

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use cell_renderer::{pipeline::CellInstance, Atlas, GlyphKey, Pipeline, Shaper};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const FONT_SIZE_PX: f32 = 28.0;

fn build_buffer() -> Vec<String> {
    // Synthetic "git log + cargo build" mash so the scroll has long
    // lines, prompts, status output, the kind of churn a real claude
    // streaming session produces. 600 lines is more than fits on
    // screen at any practical window size — exactly the cell count
    // where shape-per-cell starts dominating frame time.
    let mut out = Vec::with_capacity(600);
    let sigils = ["$ ", "> ", "# ", "  "];
    let snippets = [
        "cargo run --release -p cell-renderer --example grid_bw",
        "fn main() -> Result<()> { let event_loop = EventLoop::new()?; }",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789",
        "Finished `release` profile [optimized] target(s) in 4.21s",
        "running 10 tests ... ok",
        "+++ tmuxify/crates/cell-renderer/src/pipeline.rs",
        "--- a/src/main.rs",
        "       8c5a9bcf feat(kasaterm): OSC title plumbing",
        "let mut scaler = scale_ctx.builder(font).size(size_px).hint(true).build();",
        "render.format(Format::Alpha).render(&mut scaler, glyph_id);",
        "Compiling wgpu-core v28.0.1",
        "warning: unused variable: `font_index`",
        "    --> crates/cell-renderer/src/shaper.rs:42:9",
        "@@ -1,7 +1,7 @@",
        " pub struct Shaper { font_data: Vec<u8> }",
        "thread 'main' panicked at 'no compatible wgpu adapter'",
        "GET /api/v1/orgs/2rami/repos HTTP/1.1 200 OK",
        "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s",
    ];
    for i in 0..600 {
        let sigil = sigils[i % sigils.len()];
        let snip = snippets[i % snippets.len()];
        out.push(format!("{:>3} {}{}", i, sigil, snip));
    }
    out
}

fn main() -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[derive(Default)]
struct App {
    state: Option<RenderState>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("cell-renderer Phase 1 — scroll demo")
            .with_inner_size(winit::dpi::LogicalSize::new(1200, 720));
        let window = Arc::new(el.create_window(attrs).expect("create_window"));
        self.state = Some(pollster::block_on(RenderState::new(window)).expect("RenderState"));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _wid: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(e) = state.render() {
                    eprintln!("render error: {e:?}");
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                match event.logical_key {
                    Key::Named(NamedKey::ArrowDown) => state.scroll_lines(1),
                    Key::Named(NamedKey::ArrowUp) => state.scroll_lines(-1),
                    Key::Named(NamedKey::PageDown) => state.scroll_lines(20),
                    Key::Named(NamedKey::PageUp) => state.scroll_lines(-20),
                    Key::Named(NamedKey::Home) => state.set_scroll(0),
                    Key::Named(NamedKey::End) => state.set_scroll(i32::MAX),
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y as i32,
                    MouseScrollDelta::PixelDelta(p) => -(p.y / 24.0) as i32,
                };
                if lines != 0 {
                    state.scroll_lines(lines);
                }
            }
            _ => {}
        }
    }
}

struct RenderState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: Pipeline,
    atlas: Atlas,
    shaper: Shaper,
    bind_group: wgpu::BindGroup,
    buffer: Vec<String>,
    scroll: i32,
    cell_w: f32,
    cell_h: f32,
    frame_count: u32,
    frame_t0: Instant,
}

impl RenderState {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let size = window.inner_size();
        let surface_target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: window.display_handle()?.as_raw(),
            raw_window_handle: window.window_handle()?.as_raw(),
        };
        let surface = unsafe { instance.create_surface_unsafe(surface_target)? };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no compatible wgpu adapter")?;
        let info = adapter.get_info();
        eprintln!(
            "[gpu] backend={:?} device={:?} type={:?} driver={:?}",
            info.backend, info.name, info.device_type, info.driver
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("cell-renderer device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                trace: wgpu::Trace::Off,
            })
            .await?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let font_path = std::env::var("KASATERM_GRID_FONT")
            .unwrap_or_else(|_| "/System/Library/Fonts/Menlo.ttc".to_string());
        let mut shaper = Shaper::from_path(&font_path, 0)
            .with_context(|| format!("load font {font_path}"))?;
        let cell_w = shaper.cell_advance(FONT_SIZE_PX).ceil();
        let cell_h = (FONT_SIZE_PX * 1.4).ceil();
        let mut atlas = Atlas::new(&device, &queue, 2048);
        // Pre-bake printable ASCII so the scroll never has to upload
        // mid-frame. This makes the perf number measure the
        // steady-state cost, not a one-off bake.
        for code in 0x20u32..0x7Fu32 {
            if let Some(ch) = char::from_u32(code) {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px: FONT_SIZE_PX as u32,
                };
                let _ = atlas.get_or_bake(&device, &queue, &mut shaper, key);
            }
        }
        let mut pipeline = Pipeline::new(&device, format, 16_384);
        pipeline.write_uniforms(&queue, [config.width as f32, config.height as f32]);
        let bind_group = pipeline.make_bind_group(&device, atlas.view(), atlas.sampler());

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            atlas,
            shaper,
            bind_group,
            buffer: build_buffer(),
            scroll: 0,
            cell_w,
            cell_h,
            frame_count: 0,
            frame_t0: Instant::now(),
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
        self.pipeline.write_uniforms(
            &self.queue,
            [self.config.width as f32, self.config.height as f32],
        );
        self.window.request_redraw();
    }

    fn scroll_lines(&mut self, delta: i32) {
        self.set_scroll(self.scroll.saturating_add(delta));
    }

    fn set_scroll(&mut self, val: i32) {
        let max = (self.buffer.len() as i32 - 1).max(0);
        self.scroll = val.clamp(0, max);
        self.window.request_redraw();
    }

    fn build_instances(&mut self) -> Vec<CellInstance> {
        let pad = 16.0;
        let visible_rows =
            ((self.config.height as f32 - pad * 2.0) / self.cell_h).floor() as usize;
        let visible_rows = visible_rows.max(1);
        let start = self.scroll as usize;
        let end = (start + visible_rows).min(self.buffer.len());
        let mut out = Vec::with_capacity(visible_rows * 100);
        for (vrow, line_idx) in (start..end).enumerate() {
            let line = &self.buffer[line_idx];
            for (col, ch) in line.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px: FONT_SIZE_PX as u32,
                };
                let Some(entry) =
                    self.atlas.get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
                else {
                    continue;
                };
                let cell_x = pad + col as f32 * self.cell_w;
                let cell_y = pad + vrow as f32 * self.cell_h;
                let baseline_y = cell_y + self.cell_h * 0.78;
                let x = cell_x + entry.bearing_x as f32;
                let y = baseline_y - entry.bearing_y as f32;
                out.push(CellInstance {
                    cell_px: [x, y, entry.px_w as f32, entry.px_h as f32],
                    uv_min: entry.uv_min,
                    uv_max: entry.uv_max,
                    fg_rgba: [0.92, 0.93, 0.95, 1.0],
                    ..Default::default()
                });
            }
        }
        out
    }

    fn render(&mut self) -> Result<()> {
        // Measure the CPU side only — instance build + buffer write +
        // encoder record. `frame.present()` blocks for vsync under
        // Fifo and would otherwise pollute the number, making 0.2ms
        // of real work look like 8ms of "CPU".
        let t0 = Instant::now();
        let instances = self.build_instances();
        self.pipeline
            .write_instances(&self.device, &self.queue, &instances);
        let cpu_us = t0.elapsed().as_micros();
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cell-renderer encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cell-renderer pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.06,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.pipeline
                .draw(&mut pass, &self.bind_group, instances.len() as u32);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        // 60-frame rolling average so the print isn't drowning out
        // the actual scroll smoothness in the terminal.
        self.frame_count += 1;
        if self.frame_count >= 60 {
            let elapsed = self.frame_t0.elapsed();
            let per_frame_us = elapsed.as_micros() as f32 / self.frame_count as f32;
            eprintln!(
                "[grid] cells={} avg_frame={:.0}us last_cpu={}us scroll={}",
                instances.len(),
                per_frame_us,
                cpu_us,
                self.scroll
            );
            self.frame_count = 0;
            self.frame_t0 = Instant::now();
        }
        // Keep the loop ticking so Poll mode gives us a steady stream
        // of frames to measure against; otherwise idle would gate
        // RedrawRequested and the perf log only prints on input.
        self.window.request_redraw();
        Ok(())
    }
}
