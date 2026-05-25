//! KASATERM_RENDERER=gpu path. Owns its own wgpu Surface + cell
//! pipeline, parallel to (and mutually exclusive with) the existing
//! sugarloaf path. Phase 2a renders the cell grid only; chrome
//! (sidebar, tabs, headers) is intentionally absent until the
//! rect/text facade lands in Phase 2b+.
//!
//! Two surfaces on one window are not portable — we only init this
//! module when `KASATERM_RENDERER=gpu` is set, and in that case we
//! skip sugarloaf init entirely so the swapchain has a single
//! owner.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use cell_renderer::pipeline::CellInstance;
use cell_renderer::{Atlas, AtlasEntry, GlyphKey, Pipeline, Shaper};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use tmux_bridge::screen::Cell;
use winit::window::Window;

const ATLAS_SIZE: u32 = 2048;

/// A decoded image uploaded to its own wgpu texture. Kept alive (texture +
/// view) for as long as the pane shows it, since the bind group borrows the
/// view. Keyed by pane id in `GpuRenderer::images`.
struct ImageEntry {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    w: u32,
    h: u32,
}

pub struct GpuRenderer {
    _window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: Pipeline,
    atlas: Atlas,
    shaper: Shaper,
    /// Secondary shaper for markdown body/heading text — a proportional gothic
    /// (Noto Sans KR if installed, else Apple SD Gothic Neo) so documents read
    /// like prose, not code. Glyphs go into the SAME atlas keyed by font=1.
    md_shaper: Shaper,
    bind_group: wgpu::BindGroup,
    font_size_px: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Per-frame chrome instances. main.rs's chrome code pushes via
    /// `rect()` / `draw_text()` between frames; `render()` drains.
    chrome: Vec<CellInstance>,
    /// Scale we cached on init. winit logical→physical conversion.
    scale: f32,
    /// Separate pipeline for image panes — built with linear filtering so a
    /// photo scaled to a pane reads smooth, not pixelated. Has its own
    /// instance buffer so the image quads don't collide with the chrome
    /// pass's buffer in the same render pass.
    image_pipeline: Pipeline,
    /// Linear, clamp-to-edge sampler shared by every image bind group.
    image_sampler: wgpu::Sampler,
    /// Uploaded image textures keyed by pane id. Populated lazily on the
    /// first frame a given image pane is drawn.
    images: HashMap<String, ImageEntry>,
    /// Per-frame image quads: (pane id, instance). Drained in `render()`
    /// where each is drawn with that pane's texture bind group.
    image_quads: Vec<(String, CellInstance)>,
}

/// One pane's slot in `render_frame`. Mirrors the data the existing
/// sugarloaf renderer carries through `PaneFrame` but trimmed to
/// what Phase 2a needs (background fills, fg color, and the wide
/// markers come back in 2b).
pub struct PaneSlot<'a> {
    pub rows: &'a [Vec<Cell>],
    /// Pane top-left in physical pixels.
    pub origin_px: (f32, f32),
}

/// Pending chrome instances accumulated between `clear()` and the
/// next `render()`. Mirrors sugarloaf's immediate-mode API surface
/// (`rect`, `text_mut().draw`) but flushes through our retained
/// pipeline. Caller order is preserved so the rect-then-text painters
/// in main.rs paint in the same z-order as before.
#[derive(Default)]
pub struct ChromeBuffer {
    pub instances: Vec<CellInstance>,
}

#[derive(Debug, Clone, Copy)]
pub struct DrawOpts {
    pub font_size: f32,
    pub color: [u8; 4],
    pub bold: bool,
    pub italic: bool,
}

impl GpuRenderer {
    pub fn new(window: Arc<Window>, font_size_logical: f32) -> Result<Self> {
        let scale = window.scale_factor() as f32;
        let font_size_px = (font_size_logical * scale).round() as u32;
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface_target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: window.display_handle()?.as_raw(),
            raw_window_handle: window.window_handle()?.as_raw(),
        };
        let surface = unsafe { instance.create_surface_unsafe(surface_target)? };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("no compatible wgpu adapter")?;
        let info = adapter.get_info();
        eprintln!(
            "[gpu] backend={:?} device={:?} type={:?}",
            info.backend, info.name, info.device_type
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kasaterm gpu device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        }))?;
        let caps = surface.get_capabilities(&adapter);
        // Pick a NON-sRGB (linear-storage) framebuffer and feed it
        // already-sRGB-encoded colours directly. Why not an sRGB
        // target? Alpha blending. An sRGB target makes the hardware
        // blend glyph coverage in *linear* space, which lightens
        // anti-aliased edges and makes body text read thin/grey. A
        // plain Unorm target blends in gamma (sRGB) space — the same
        // gamma-incorrect-but-bolder blend sugarloaf / Terminal.app
        // use — so text matches. We hand it sRGB bytes and clear with
        // sRGB bytes, so the stored values are correct on screen too.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or_else(|| caps.formats[0].remove_srgb_suffix());
        eprintln!("[gpu] surface format = {:?} srgb={}", format, format.is_srgb());
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Fifo (vsync) queues 2-3 frames, adding 33-50ms of
            // input-to-screen latency — typing felt laggy vs Ghostty /
            // iTerm. AutoNoVsync picks the lowest-latency mode the
            // surface supports (Immediate or Mailbox), falling back to
            // Fifo only if neither exists. Tearing is irrelevant for a
            // text grid, and the damage gate already bounds how often we
            // actually present.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            // 1, not the wgpu default of 2: a 2-deep frame queue holds a
            // freshly-rendered frame for an extra vblank before it's
            // scanned out, so a keystroke that paints "now" only appears
            // ~1 frame later. The terminal renders tiny diffs, so a depth
            // of 1 is plenty and shaves that frame of input latency.
            desired_maximum_frame_latency: 1,
        };
        eprintln!(
            "[gpu] present_modes={:?} chosen={:?} frame_latency={}",
            caps.present_modes, config.present_mode, config.desired_maximum_frame_latency
        );
        surface.configure(&device, &config);

        // Phase 2a font path: macOS Menlo for now, mirrors the
        // grid_bw example. The real fallback chain (D2Coding →
        // Nerd Font → Segoe UI Symbol) reattaches in Phase 2c when
        // chrome text comes back.
        let font_path = std::env::var("KASATERM_GRID_FONT")
            .unwrap_or_else(|_| default_font_path());
        eprintln!("[font] primary={font_path}");
        let mut shaper = Shaper::from_path(&font_path, 0)
            .with_context(|| format!("load font {font_path}"))?;
        for (path, idx) in fallback_font_paths() {
            shaper.add_fallback_path(&path, idx);
        }
        // Bundled fallbacks (always present in the binary) — sit at
        // the end of the chain so a user-installed Nerd Font still
        // wins, but blank-outline gaps in the primary fall through
        // here for a guaranteed glyph.
        shaper.add_fallback_bytes(cell_renderer::CASCADIA_CODE_NF, 0);
        shaper.add_fallback_bytes(cell_renderer::SYMBOLS_NERD_FONT_MONO, 0);
        // Markdown body font — a proportional gothic. Falls back to the primary
        // mono if the gothic can't load so the renderer never panics.
        let (md_font, md_idx) = md_font_path();
        let mut md_shaper = Shaper::from_path(&md_font, md_idx)
            .or_else(|_| Shaper::from_path(&font_path, 0))
            .with_context(|| format!("load markdown font {md_font}"))?;
        eprintln!("[font] markdown={md_font}");
        // Same bundled symbol/icon fallbacks so glyphs the gothic lacks still
        // resolve (and CJK falls through to the gothic's own coverage first).
        for (path, idx) in fallback_font_paths() {
            md_shaper.add_fallback_path(&path, idx);
        }
        md_shaper.add_fallback_bytes(cell_renderer::CASCADIA_CODE_NF, 0);
        md_shaper.add_fallback_bytes(cell_renderer::SYMBOLS_NERD_FONT_MONO, 0);
        let cell_w = shaper.cell_advance(font_size_px as f32).ceil();
        // Use the font's natural line metric (ascent+descent+leading)
        // for cell height instead of an arbitrary multiplier. Lines
        // pack at the same density sugarloaf produces with
        // `line_height=1.0` (which itself reads the same metrics
        // under the hood via cosmic-text).
        let cell_h = shaper.line_height(font_size_px as f32).ceil();
        let mut atlas = Atlas::new(&device, &queue, ATLAS_SIZE);
        for code in 0x20u32..0x7Fu32 {
            if let Some(ch) = char::from_u32(code) {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px: font_size_px,
                    font: 0,
                };
                let _ = atlas.get_or_bake(&device, &queue, &mut shaper, key);
            }
        }
        let pipeline = Pipeline::new(&device, format, 32_768);
        pipeline.write_uniforms(&queue, [config.width as f32, config.height as f32]);
        let bind_group = pipeline.make_bind_group(&device, atlas.view(), atlas.sampler());

        // Image pass: own buffer (a few quads), linear filtering for smooth
        // scaling. Shares the same screen-size uniform projection.
        let image_pipeline = Pipeline::with_filtering(&device, format, 64, true);
        image_pipeline.write_uniforms(&queue, [config.width as f32, config.height as f32]);
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kasaterm image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        Ok(Self {
            _window: window,
            surface,
            device,
            queue,
            config,
            pipeline,
            atlas,
            shaper,
            md_shaper,
            bind_group,
            font_size_px,
            cell_w: cell_w / scale,
            cell_h: cell_h / scale,
            chrome: Vec::with_capacity(1024),
            scale,
            image_pipeline,
            image_sampler,
            images: HashMap::new(),
            image_quads: Vec::new(),
        })
    }

    /// Logical-pixel solid rect (sugarloaf.rect drop-in). Caller
    /// passes the same logical coordinates main.rs has been using;
    /// we promote to physical pixels here to stay consistent with
    /// the cell pass. `rgba_f` is 0..1.
    /// Logical-pixel solid rect (sugarloaf.rect drop-in). Caller
    /// passes the same u8 RGBA they would have handed sugarloaf —
    /// we sRGB-decode here so the framebuffer's sRGB encode round-
    /// trips back to the same on-screen bytes.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: Atlas::SOLID_UV,
            uv_max: Atlas::SOLID_UV,
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            ..Default::default()
        });
    }

    /// Draw a text label using glyphs baked into the atlas at the
    /// requested size. Returns the pen-x after the last glyph
    /// (mirrors sugarloaf's `text.draw` return behaviour for callers
    /// that want it). Coordinates are logical pixels; `y` is the
    /// label's top edge — we approximate baseline via cell_h * 0.78
    /// matching the cell-grid path.
    pub fn draw_text(&mut self, x: f32, y: f32, text: &str, opts: DrawOpts) -> f32 {
        let s = self.scale;
        let size_px = (opts.font_size * s).round() as u32;
        let baseline_px = y * s + (size_px as f32 * 0.78);
        let fg = srgb_rgba_to_linear(opts.color);
        let mut pen = x * s;
        for ch in text.chars() {
            if ch == ' ' {
                let adv = self.shaper.cell_advance(size_px as f32);
                pen += adv;
                continue;
            }
            let key = GlyphKey {
                ch,
                bold: opts.bold,
                italic: opts.italic,
                size_px,
                font: 0,
            };
            let Some(entry) = self.atlas.get_or_bake(
                &self.device,
                &self.queue,
                &mut self.shaper,
                key,
            ) else {
                continue;
            };
            let glyph_x = pen + entry.bearing_x as f32;
            let glyph_y = baseline_px - entry.bearing_y as f32;
            self.chrome.push(CellInstance {
                cell_px: [glyph_x, glyph_y, entry.px_w as f32, entry.px_h as f32],
                uv_min: entry.uv_min,
                uv_max: entry.uv_max,
                fg_rgba: fg,
                ..Default::default()
            });
            // Header text is proportional, not a mono grid. A wide (CJK)
            // glyph carries a ~2-cell mono advance, which leaves big gaps
            // between Hangul in a small label ("탭이름 테스트" reads spaced
            // out). Tighten wide glyphs to their ink width + a little
            // tracking so the label reads evenly.
            pen += if is_wide_char(ch) {
                entry.px_w as f32 + size_px as f32 * 0.18
            } else {
                entry.advance
            };
        }
        pen / s
    }

    /// Draw the IME preedit (composing Hangul) the SAME way the cell
    /// grid draws committed text. `draw_text` used a `size_px * 0.78`
    /// baseline, but the grid uses `cell_h_px * 0.78`; since the line
    /// height is taller than the font size the composing syllable
    /// floated above the row ("조합 중 글자가 올라간다"). It also walked
    /// the pen by glyph advance, which drifts wide chars. Here we pin to
    /// the cell grid: cell-grid baseline + per-glyph 2-cell fit, exactly
    /// like draw_cells, mirroring the sugarloaf fix that routes preedit
    /// through render_row. `origin` is logical px (top-left of the
    /// anchor cell); colors are the accent (text + underline).
    pub fn draw_preedit(&mut self, origin_x: f32, origin_y: f32, text: &str, accent: [u8; 4]) {
        let cell_w_px = self.cell_w * self.scale;
        let cell_h_px = self.cell_h * self.scale;
        let ox = origin_x * self.scale;
        let oy = origin_y * self.scale;
        // Cell span: wide (CJK/Hangul) chars take two columns.
        let span_cells: u32 = text
            .chars()
            .map(|c| if is_wide_char(c) { 2 } else { 1 })
            .sum();
        let span_px = span_cells.max(1) as f32 * cell_w_px;
        // Opaque background so the composing glyph isn't muddied by the
        // grid cells underneath, plus an accent underline.
        self.chrome.push(CellInstance {
            cell_px: [ox, oy, span_px, cell_h_px],
            uv_min: Atlas::SOLID_UV,
            uv_max: Atlas::SOLID_UV,
            fg_rgba: srgb_rgba_to_linear(crate::cells::DEFAULT_BG),
            ..Default::default()
        });
        let acc = srgb_rgba_to_linear(accent);
        self.chrome.push(CellInstance {
            cell_px: [ox, oy + cell_h_px - 2.0 * self.scale, span_px, 2.0 * self.scale],
            uv_min: Atlas::SOLID_UV,
            uv_max: Atlas::SOLID_UV,
            fg_rgba: acc,
            ..Default::default()
        });
        // Glyphs — identical placement math to draw_cells.
        let baseline_y = oy + cell_h_px * 0.78;
        let mut col = 0u32;
        for ch in text.chars() {
            let wide = is_wide_char(ch);
            if ch != ' ' {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px: self.font_size_px,
                    font: 0,
                };
                if let Some(entry) = self.atlas.get_or_bake(
                    &self.device,
                    &self.queue,
                    &mut self.shaper,
                    key,
                ) {
                    let cell_x = ox + col as f32 * cell_w_px;
                    if wide {
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let scale_fit = if gw0 > span_w { span_w / gw0 } else { 1.0 };
                        let gw = gw0 * scale_fit;
                        let gh = entry.px_h as f32 * scale_fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = baseline_y - entry.bearing_y as f32 * scale_fit;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: acc,
                            ..Default::default()
                        });
                    } else {
                        let x = cell_x + entry.bearing_x as f32;
                        let y = baseline_y - entry.bearing_y as f32;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, entry.px_w as f32, entry.px_h as f32],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: acc,
                            ..Default::default()
                        });
                    }
                }
            }
            col += if wide { 2 } else { 1 };
        }
    }

    /// Draw inline-autosuggestion ghost text. Same cell-grid placement
    /// math as `draw_preedit` / `draw_cells`, but with NO background fill
    /// or underline and a dim foreground, so it reads as a hint sitting
    /// behind where the user would type. `max_cells` clips it to the
    /// remaining columns on the row (no wrapping). `origin` is logical px
    /// at the top-left of the first ghost cell.
    pub fn draw_ghost(&mut self, origin_x: f32, origin_y: f32, text: &str, max_cells: u32) {
        let cell_w_px = self.cell_w * self.scale;
        let cell_h_px = self.cell_h * self.scale;
        let ox = origin_x * self.scale;
        let oy = origin_y * self.scale;
        let fg = srgb_rgba_to_linear(crate::cells::GHOST_FG);
        let baseline_y = oy + cell_h_px * 0.78;
        let mut col = 0u32;
        for ch in text.chars() {
            let wide = is_wide_char(ch);
            let span = if wide { 2 } else { 1 };
            if col + span > max_cells {
                break;
            }
            if ch != ' ' {
                let key = GlyphKey {
                    ch,
                    bold: false,
                    italic: false,
                    size_px: self.font_size_px,
                    font: 0,
                };
                if let Some(entry) =
                    self.atlas
                        .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
                {
                    let cell_x = ox + col as f32 * cell_w_px;
                    if wide {
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let scale_fit = if gw0 > span_w { span_w / gw0 } else { 1.0 };
                        let gw = gw0 * scale_fit;
                        let gh = entry.px_h as f32 * scale_fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = baseline_y - entry.bearing_y as f32 * scale_fit;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: fg,
                            ..Default::default()
                        });
                    } else {
                        let x = cell_x + entry.bearing_x as f32;
                        let y = baseline_y - entry.bearing_y as f32;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, entry.px_w as f32, entry.px_h as f32],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: fg,
                            ..Default::default()
                        });
                    }
                }
            }
            col += span;
        }
    }

    /// Bake (or fetch cached) a glyph from the requested font (0 = primary
    /// mono, 1 = markdown gothic) into the shared atlas. Centralizes the
    /// shaper choice so every caller stays consistent.
    fn bake_glyph(
        &mut self,
        ch: char,
        bold: bool,
        italic: bool,
        size_px: u32,
        font: u8,
    ) -> Option<AtlasEntry> {
        let key = GlyphKey { ch, bold, italic, size_px, font };
        if font == 1 {
            self.atlas
                .get_or_bake(&self.device, &self.queue, &mut self.md_shaper, key)
        } else {
            self.atlas
                .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
        }
    }

    /// Space/cell advance for the requested font at `size_px`.
    fn font_cell_advance(&mut self, size_px: u32, font: u8) -> f32 {
        if font == 1 {
            self.md_shaper.cell_advance(size_px as f32)
        } else {
            self.shaper.cell_advance(size_px as f32)
        }
    }

    /// Draw a single word (no internal wrapping) at logical (x, y) using the
    /// given font. Mirrors draw_text's glyph placement but lets the markdown
    /// renderer pick the gothic (font=1) for prose and mono (font=0) for code.
    fn md_draw_word(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: [u8; 4],
        bold: bool,
        italic: bool,
        font: u8,
    ) {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let baseline = y * s + size_px as f32 * 0.78;
        let fg = srgb_rgba_to_linear(color);
        let mut pen = x * s;
        for ch in text.chars() {
            if ch == ' ' {
                pen += self.font_cell_advance(size_px, font);
                continue;
            }
            if let Some(e) = self.bake_glyph(ch, bold, italic, size_px, font) {
                let gx = pen + e.bearing_x as f32;
                let gy = baseline - e.bearing_y as f32;
                let (col, flags) = if e.is_color {
                    ([1.0, 1.0, 1.0, 1.0], CellInstance::FLAG_COLOR)
                } else {
                    (fg, 0)
                };
                let inst = CellInstance {
                    cell_px: [gx, gy, e.px_w as f32, e.px_h as f32],
                    uv_min: e.uv_min,
                    uv_max: e.uv_max,
                    fg_rgba: col,
                    flags,
                    ..Default::default()
                };
                self.chrome.push(inst);
                // Faux bold: the rasterizer has no weighted face, so smear the
                // glyph a fraction of an em to the right for a heavier stroke.
                if bold && !e.is_color {
                    let mut b = inst;
                    b.cell_px[0] += size_px as f32 * 0.04;
                    self.chrome.push(b);
                }
                pen += if is_wide_char(ch) {
                    e.px_w as f32 + size_px as f32 * 0.18
                } else {
                    e.advance
                };
            }
        }
    }

    /// Width (logical px) a styled run occupies, matching `md_draw_word`'s
    /// advance so word-wrap measurement equals what gets drawn. `code` selects
    /// the mono font (0); prose uses the gothic (1).
    fn measure_run(&mut self, text: &str, size: f32, bold: bool, italic: bool, code: bool) -> f32 {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let font: u8 = if code { 0 } else { 1 };
        let mut w = 0.0;
        for ch in text.chars() {
            if ch == ' ' {
                w += self.font_cell_advance(size_px, font);
                continue;
            }
            if let Some(e) = self.bake_glyph(ch, bold, italic, size_px, font) {
                w += if is_wide_char(ch) {
                    e.px_w as f32 + size_px as f32 * 0.18
                } else {
                    e.advance
                };
            }
        }
        w / s
    }

    /// Lay styled spans into `max_w` at logical (x_start, y_start), wrapping on
    /// word boundaries. Returns pen_y after the last line. Lines fully outside
    /// [clip_top, clip_bot) are skipped — that's the scroll clip for markdown.
    fn md_runs(
        &mut self,
        spans: &[crate::MdSpan],
        x_start: f32,
        y_start: f32,
        max_w: f32,
        size: f32,
        force_bold: bool,
        color: [u8; 4],
        clip_top: f32,
        clip_bot: f32,
    ) -> f32 {
        // Line metrics from the gothic (markdown body font), even when a run
        // is inline code — keeps the baseline steady across a mixed line.
        let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
        let space_w = self.md_shaper.cell_advance(size * self.scale) / self.scale;
        let mut pen_x = x_start;
        let mut pen_y = y_start;
        for span in spans {
            let bold = span.bold || force_bold;
            for word in span.text.split_inclusive(' ') {
                let trailing_space = word.ends_with(' ');
                let trimmed = word.trim_end_matches(' ');
                if !trimmed.is_empty() {
                    let ww = self.measure_run(trimmed, size, bold, span.italic, span.code);
                    if pen_x + ww > x_start + max_w && pen_x > x_start {
                        pen_x = x_start;
                        pen_y += lh;
                    }
                    if pen_y + lh > clip_top && pen_y < clip_bot {
                        if span.code {
                            self.rect(
                                pen_x - space_w * 0.15,
                                pen_y,
                                ww + space_w * 0.3,
                                lh,
                                crate::theme::SURFACE_ACTIVE,
                            );
                        }
                        let col = if span.code { crate::theme::ACCENT } else { color };
                        let font: u8 = if span.code { 0 } else { 1 };
                        self.md_draw_word(trimmed, pen_x, pen_y, size, col, bold, span.italic, font);
                    }
                    pen_x += ww;
                }
                if trailing_space {
                    pen_x += space_w;
                }
            }
        }
        pen_y + lh
    }

    /// Lay out + draw a markdown document into the pane box (all logical px).
    /// Glyphs/rects go into the chrome buffer (drawn over the empty cell pass,
    /// under pane headers). Returns total content height (logical) so the
    /// caller can clamp the scroll offset.
    pub fn draw_markdown(
        &mut self,
        blocks: &[crate::MdBlock],
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scroll: f32,
    ) -> f32 {
        use crate::MdBlock;
        let base = self.font_size_px as f32 / self.scale;
        let clip_top = y;
        let clip_bot = y + h;
        let top0 = y - scroll;
        let mut pen_y = top0;
        for block in blocks {
            match block {
                MdBlock::Heading { level, spans } => {
                    let scale_f = match level {
                        1 => 1.7,
                        2 => 1.45,
                        3 => 1.25,
                        4 => 1.1,
                        _ => 1.0,
                    };
                    let size = base * scale_f;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    pen_y += lh * 0.5;
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, true, crate::theme::TEXT, clip_top, clip_bot,
                    );
                    pen_y += lh * 0.25;
                }
                MdBlock::Para { spans } => {
                    let size = base;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, false, crate::theme::TEXT, clip_top, clip_bot,
                    );
                    pen_y += lh * 0.45;
                }
                MdBlock::Code { code } => {
                    let size = base * 0.92;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    let pad = base * 0.4;
                    let lines: Vec<&str> = code.trim_end_matches('\n').split('\n').collect();
                    let block_h = lines.len() as f32 * lh + pad * 2.0;
                    if pen_y + block_h > clip_top && pen_y < clip_bot {
                        self.rect(x, pen_y, w, block_h, crate::theme::SURFACE);
                    }
                    let mut ly = pen_y + pad;
                    for line in &lines {
                        if ly + lh > clip_top && ly < clip_bot {
                            self.draw_text(
                                x + pad,
                                ly,
                                line,
                                DrawOpts {
                                    font_size: size,
                                    color: crate::theme::TEXT_DIM,
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                        ly += lh;
                    }
                    pen_y += block_h + lh * 0.4;
                }
                MdBlock::ListItem { depth, marker, spans } => {
                    let size = base;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    let indent = (*depth as f32 + 1.0) * base * 1.3;
                    if pen_y + lh > clip_top && pen_y < clip_bot {
                        self.draw_text(
                            x + indent - base * 1.1,
                            pen_y,
                            marker,
                            DrawOpts {
                                font_size: size,
                                color: crate::theme::ACCENT,
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    pen_y = self.md_runs(
                        spans,
                        x + indent,
                        pen_y,
                        (w - indent).max(1.0),
                        size,
                        false,
                        crate::theme::TEXT,
                        clip_top,
                        clip_bot,
                    );
                    pen_y += lh * 0.25;
                }
                MdBlock::Quote { spans } => {
                    let size = base;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    let indent = base * 1.2;
                    let start_y = pen_y;
                    pen_y = self.md_runs(
                        spans,
                        x + indent,
                        pen_y,
                        (w - indent).max(1.0),
                        size,
                        false,
                        crate::theme::TEXT_DIM,
                        clip_top,
                        clip_bot,
                    );
                    let bar_h = pen_y - start_y;
                    if start_y + bar_h > clip_top && start_y < clip_bot {
                        self.rect(x, start_y, base * 0.18, bar_h, crate::theme::ACCENT);
                    }
                    pen_y += lh * 0.45;
                }
                MdBlock::Rule => {
                    let lh = self.md_shaper.line_height(base * self.scale).ceil() / self.scale;
                    pen_y += lh * 0.4;
                    if pen_y > clip_top && pen_y < clip_bot {
                        self.rect(x, pen_y, w, 1.5, crate::theme::BORDER);
                    }
                    pen_y += lh * 0.5;
                }
            }
        }
        (pen_y - top0).max(0.0)
    }

    /// Drop all pending chrome instances. main.rs calls this at the
    /// top of each frame so stale rects/labels from the previous
    /// frame don't pile up.
    pub fn clear_chrome(&mut self) {
        self.chrome.clear();
        self.image_quads.clear();
    }

    /// Has this pane's image already been uploaded? Lets the caller skip
    /// re-handing us the pixel buffer every frame.
    pub fn has_image(&self, id: &str) -> bool {
        self.images.contains_key(id)
    }

    /// Upload an image pane's RGBA8 pixels into a texture + bind group keyed
    /// by pane id. No-op if already present. `rgba` must be `w * h * 4` bytes.
    pub fn upload_image(&mut self, id: &str, rgba: &[u8], w: u32, h: u32) {
        if self.images.contains_key(id) || w == 0 || h == 0 {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kasaterm image"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Non-srgb to match the surface: image crate yields sRGB bytes
            // and our framebuffer shows them verbatim (same reasoning as the
            // glyph atlas), so colours land correct without a colour-space hop.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        let bind_group =
            self.image_pipeline
                .make_bind_group(&self.device, &view, &self.image_sampler);
        self.images.insert(
            id.to_string(),
            ImageEntry {
                _texture: texture,
                _view: view,
                bind_group,
                w,
                h,
            },
        );
    }

    /// Free a pane's image texture when the pane closes.
    pub fn drop_image(&mut self, id: &str) {
        self.images.remove(id);
    }

    /// Queue an image pane for this frame. `(x, y, w, h)` is the pane's body
    /// box in LOGICAL px; the image is contain-fit (aspect preserved,
    /// centered) inside it. Must have been `upload_image`d first.
    pub fn queue_image(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w as f32, entry.h as f32);
        // Contain fit, but never upscale past native — a small icon stays
        // crisp at 1:1 instead of blowing up blurry to fill the pane.
        let fit = (bw / iw).min(bh / ih).min(1.0);
        let dw = iw * fit;
        let dh = ih * fit;
        let dx = bx + (bw - dw) * 0.5;
        let dy = by + (bh - dh) * 0.5;
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [dx, dy, dw, dh],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
        ));
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
        self.pipeline.write_uniforms(
            &self.queue,
            [self.config.width as f32, self.config.height as f32],
        );
        self.image_pipeline.write_uniforms(
            &self.queue,
            [self.config.width as f32, self.config.height as f32],
        );
    }

    /// Render one frame. `panes` covers every pane the caller wants
    /// drawn this frame, each carrying its grid + pixel origin. The
    /// pipeline gathers all instances into one draw call regardless
    /// of pane count.
    /// Push cells from each pane onto the chrome instance list at
    /// the *current* z-order. Caller pushes background rects before
    /// this and overlays (cursor, selection, preedit) after. The
    /// pipeline draws everything in insertion order, so painting
    /// layers fall out naturally from the call sequence.
    pub fn draw_cells(&mut self, panes: &[PaneSlot<'_>]) {
        let cell_w_px = self.cell_w * self.scale;
        let cell_h_px = self.cell_h * self.scale;
        // Pass 1: backgrounds only. A tall CJK glyph bleeds a little
        // into the row below; emitting EVERY background first stops the
        // next row's bg fill from painting over the previous glyph's
        // bottom half. That over-paint was clipping Hangul in claude's
        // input-echo row (a run of reverse/bg cells); claude's normal
        // output rows have no bg below them, so they rendered fine.
        // (Reverse-video spaces still fill here — claude's cursor is an
        // inverse space, "띄어쓰기 커서".)
        for pane in panes {
            for (r, row) in pane.rows.iter().enumerate() {
                for (col, cell) in row.iter().enumerate() {
                    let want_bg = !matches!(cell.bg, tmux_bridge::screen::Color::Default)
                        || cell.inverse;
                    let bg = cell_bg_rgba(cell);
                    if want_bg && bg[3] > 0 {
                        let cx = pane.origin_px.0 + col as f32 * cell_w_px;
                        let cy = pane.origin_px.1 + r as f32 * cell_h_px;
                        self.chrome.push(CellInstance {
                            cell_px: [cx, cy, cell_w_px, cell_h_px],
                            uv_min: Atlas::SOLID_UV,
                            uv_max: Atlas::SOLID_UV,
                            fg_rgba: srgb_rgba_to_linear(bg),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        // Pass 2: glyphs, drawn over every background.
        for pane in panes {
            for (r, row) in pane.rows.iter().enumerate() {
                for (col, cell) in row.iter().enumerate() {
                    // Blanks contribute no glyph.
                    let Some(ch) = cell.ch.chars().next() else { continue };
                    if ch == ' ' || ch == '\0' || cell.ch.is_empty() {
                        continue;
                    }
                    // Block Elements (U+2580..259F) — paint as GPU
                    // quads instead of font glyphs. Monospace fonts
                    // render these with seams/gaps, so claude code's
                    // pixel-art character (built from half/quadrant
                    // blocks) tears when shaped as glyphs. The
                    // sub-cell rects from cells::block_rects fill the
                    // exact regions seamlessly.
                    if cell.ch.chars().count() == 1 {
                        if let Some(rects) = crate::cells::block_rects(ch) {
                            let fg = cell_fg_rgba(cell);
                            let lin = srgb_rgba_to_linear(fg);
                            let cx = pane.origin_px.0 + col as f32 * cell_w_px;
                            let cy = pane.origin_px.1 + r as f32 * cell_h_px;
                            for &(x0, y0, x1, y1, alpha) in rects {
                                let mut c = lin;
                                c[3] *= alpha;
                                self.chrome.push(CellInstance {
                                    cell_px: [
                                        cx + x0 * cell_w_px,
                                        cy + y0 * cell_h_px,
                                        (x1 - x0) * cell_w_px,
                                        (y1 - y0) * cell_h_px,
                                    ],
                                    uv_min: Atlas::SOLID_UV,
                                    uv_max: Atlas::SOLID_UV,
                                    fg_rgba: c,
                                    ..Default::default()
                                });
                            }
                            continue;
                        }
                    }
                    let cell_x = pane.origin_px.0 + col as f32 * cell_w_px;
                    let cell_y = pane.origin_px.1 + r as f32 * cell_h_px;
                    let fg = cell_fg_rgba(cell);
                    let icon = is_icon_codepoint(ch as u32);
                    if icon {
                        // Ghostty-style fit-to-cell, done CRISP: scale
                        // happens at raster time, never on a finished
                        // bitmap. Two-pass — probe-bake at cell height
                        // to read the glyph's natural bbox, compute the
                        // size that lands the bbox at ~0.82 of the cell
                        // height, then re-bake natively at that size.
                        // Both bakes are atlas-cached so it's one-time
                        // per glyph. The final bitmap is sharp because
                        // swash rasterized the outline at the target
                        // size directly.
                        let target_h = cell_h_px * 0.82;
                        let probe_size = cell_h_px.round().max(1.0) as u32;
                        let probe = self.atlas.get_or_bake(
                            &self.device,
                            &self.queue,
                            &mut self.shaper,
                            GlyphKey { ch, bold: cell.bold, italic: cell.italic, size_px: probe_size, font: 0 },
                        );
                        if let Some(p) = probe {
                            if p.px_h > 0 {
                                let mut final_size =
                                    (probe_size as f32 * target_h / p.px_h as f32).round();
                                // Guard the width: scale down if the
                                // glyph would exceed ~1.9 cells.
                                let projected_w = p.px_w as f32
                                    * (final_size / probe_size as f32);
                                let max_w = cell_w_px * 1.9;
                                if projected_w > max_w {
                                    final_size *= max_w / projected_w;
                                }
                                let final_size = (final_size.round() as u32).max(1);
                                let entry = self.atlas.get_or_bake(
                                    &self.device,
                                    &self.queue,
                                    &mut self.shaper,
                                    GlyphKey { ch, bold: cell.bold, italic: cell.italic, size_px: final_size, font: 0 },
                                );
                                if let Some(e) = entry {
                                    let x = cell_x + (cell_w_px - e.px_w as f32) * 0.5;
                                    let y = cell_y + (cell_h_px - e.px_h as f32) * 0.5;
                                    self.chrome.push(CellInstance {
                                        cell_px: [x, y, e.px_w as f32, e.px_h as f32],
                                        uv_min: e.uv_min,
                                        uv_max: e.uv_max,
                                        fg_rgba: srgb_rgba_to_linear(fg),
                                        ..Default::default()
                                    });
                                }
                            }
                        }
                        continue;
                    }
                    let key = GlyphKey {
                        ch,
                        bold: cell.bold,
                        italic: cell.italic,
                        size_px: self.font_size_px,
                        font: 0,
                    };
                    let Some(entry) = self.atlas.get_or_bake(
                        &self.device,
                        &self.queue,
                        &mut self.shaper,
                        key,
                    ) else {
                        continue;
                    };
                    let baseline_y = cell_y + cell_h_px * 0.78;
                    if entry.is_color {
                        // Color emoji: the atlas holds a verbatim RGBA
                        // bitmap. Fit it into a 2-cell box (emoji read as
                        // full-width) keeping aspect, never upscaling past
                        // native, and center it in the row. FLAG_COLOR
                        // tells the shader to sample the texture directly
                        // instead of fg × coverage.
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let gh0 = entry.px_h as f32;
                        let fit = (span_w / gw0).min(cell_h_px / gh0).min(1.0);
                        let gw = gw0 * fit;
                        let gh = gh0 * fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = cell_y + (cell_h_px - gh) * 0.5;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            flags: CellInstance::FLAG_COLOR,
                            ..Default::default()
                        });
                        continue;
                    }
                    if is_wide_char(ch) {
                        // Fit the glyph into its 2-cell box. Scale down
                        // (keeping aspect) only if it overflows, then
                        // center horizontally so syllables sit on the
                        // grid instead of bleeding into the next cell.
                        let span_w = cell_w_px * 2.0;
                        let gw0 = entry.px_w as f32;
                        let scale_fit = if gw0 > span_w { span_w / gw0 } else { 1.0 };
                        let gw = gw0 * scale_fit;
                        let gh = entry.px_h as f32 * scale_fit;
                        let x = cell_x + (span_w - gw) * 0.5;
                        let y = baseline_y - entry.bearing_y as f32 * scale_fit;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            ..Default::default()
                        });
                    } else {
                        let x = cell_x + entry.bearing_x as f32;
                        let y = baseline_y - entry.bearing_y as f32;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, entry.px_w as f32, entry.px_h as f32],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }

    pub fn render(&mut self, _panes: &[PaneSlot<'_>], _scale: f32) -> Result<usize> {
        let instance_count = self.chrome.len();
        self.pipeline
            .write_instances(&self.device, &self.queue, &self.chrome);
        // Upload this frame's image quads into the image pipeline's own
        // buffer; each is drawn individually below so it can bind its texture.
        if !self.image_quads.is_empty() {
            let img_instances: Vec<CellInstance> =
                self.image_quads.iter().map(|(_, inst)| *inst).collect();
            self.image_pipeline
                .write_instances(&self.device, &self.queue, &img_instances);
        }
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kasaterm gpu encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kasaterm gpu pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear({
                            // Raw sRGB bytes → non-sRGB target = shown
                            // verbatim, matching cells::DEFAULT_BG.
                            let b = crate::cells::DEFAULT_BG;
                            wgpu::Color {
                                r: b[0] as f64 / 255.0,
                                g: b[1] as f64 / 255.0,
                                b: b[2] as f64 / 255.0,
                                a: 1.0,
                            }
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Images first: they sit at the bottom of the pane box so the
            // chrome pass (pane headers, focus ring, inactive-dim overlay)
            // paints over them. Each image binds its own texture.
            for (i, (id, _)) in self.image_quads.iter().enumerate() {
                if let Some(entry) = self.images.get(id) {
                    self.image_pipeline
                        .draw_at(&mut pass, &entry.bind_group, i as u32);
                }
            }
            self.pipeline
                .draw(&mut pass, &self.bind_group, instance_count as u32);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(instance_count)
    }
}

/// Nerd Font / symbol icon codepoint ranges that should be scaled to
/// fill the cell rather than rendered at the text font size. Mirrors
/// the ranges Ghostty constrains: BMP Private Use Area (where most
/// Nerd Font icons live), both supplementary PUA planes, and the
/// Misc-Technical block that carries powerline-adjacent symbols.
/// East Asian Wide / Fullwidth — these occupy two terminal cells.
/// alacritty fills the right half with an empty cell (skipped in the
/// glyph pass), so the glyph itself has to be fit into a 2-cell box.
/// The bundled Hangul fallback font rasters at its own natural advance,
/// which does not match the primary monospace font's `cell_w`; without
/// this the syllable drifts into / overlaps its neighbour ("출력 한글
/// 깨짐"). sugarloaf gets this for free because cosmic_text shapes onto
/// the monospace grid.
pub(crate) fn is_wide_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp,
        0x1100..=0x115F        // Hangul Jamo
        | 0x2E80..=0x303E      // CJK Radicals, Kangxi, CJK Symbols
        | 0x3041..=0x33FF      // Kana, CJK enclosed/compat
        | 0x3400..=0x4DBF      // CJK Ext A
        | 0x4E00..=0x9FFF      // CJK Unified
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul Syllables
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F      // CJK Compatibility Forms
        | 0xFF00..=0xFF60      // Fullwidth Forms
        | 0xFFE0..=0xFFE6      // Fullwidth signs
    ) || cp >= 0x20000          // CJK Ext B and beyond
}

fn is_icon_codepoint(cp: u32) -> bool {
    // Only Private-Use-Area Nerd Font icons get the fit-to-cell
    // enlargement — these are the statusline glyphs (server, git
    // branch, folder, gauge, …) that D2Coding designs small. Other
    // symbol blocks are left alone on purpose:
    //   - box drawing (2500..257F) must align to cell edges
    //   - Misc-Technical (2300..23FF, the bypass ▶ chevron) and
    //     misc arrows (2B00..2BFF) already read at the right size;
    //     enlarging them made bypass look oversized (user feedback).
    (0xE000..=0xF8FF).contains(&cp)        // BMP PUA — Nerd icons
        || (0xF0000..=0xFFFFD).contains(&cp)   // Supplementary PUA-A
        || (0x100000..=0x10FFFD).contains(&cp) // Supplementary PUA-B
}

fn cell_fg_rgba(cell: &Cell) -> [u8; 4] {
    crate::cells::cell_fg(cell)
}

fn cell_bg_rgba(cell: &Cell) -> [u8; 4] {
    crate::cells::cell_bg(cell)
}

/// Normalize u8 RGBA to 0..1 with NO colour-space conversion. The
/// framebuffer is a plain (non-sRGB) Unorm target, so the bytes we
/// write are displayed verbatim — our source colours are already
/// authored as sRGB (cells::DEFAULT_*, ITERM_*, ANSI palette), so
/// passing them straight through is correct, and it gives us
/// gamma-space alpha blending (bolder text) for free.
#[inline]
pub fn srgb_rgba_to_linear(rgba: [u8; 4]) -> [f32; 4] {
    [
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    ]
}

/// Order matches the sugarloaf SugarloafFonts config we previously
/// shipped (`fonts.family = "D2CodingLigature Nerd Font Mono"`,
/// `symbol_map` for Misc-Tech / PUA / Supplementary PUA). Each entry
/// is `(path, face_index_inside_TTC)`; swash skips a face whose
/// charmap doesn't cover the codepoint, so the chain falls through
/// gracefully.
fn fallback_font_paths() -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let push_if = |out: &mut Vec<(String, u32)>, p: String, i: u32| {
            if std::path::Path::new(&p).exists() {
                out.push((p, i));
            }
        };
        // JetBrainsMono Nerd Font Mono — broader nerd icon coverage
        // when D2Coding skips a PUA codepoint.
        push_if(
            &mut out,
            format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
            0,
        );
        // D2Coding non-Mono variant catches a few glyphs the Mono
        // patch trims (the Mono variant force-fits everything into
        // a cell width — anything that didn't fit was dropped).
        push_if(
            &mut out,
            format!("{home}/Library/Fonts/D2CodingLigatureNerdFont-Regular.ttf"),
            0,
        );
        // STIX Two Math — the only macOS default with U+23F5 ⏵
        // (Black Medium Right-Pointing Triangle) baked in. Without
        // this, claude code's BYPASS prompt row shows a blank where
        // the chevron should sit.
        push_if(
            &mut out,
            "/System/Library/Fonts/Supplemental/STIXTwoMath.otf".into(),
            0,
        );
        // Menlo — generous BMP coverage for symbols D2Coding skips.
        push_if(&mut out, "/System/Library/Fonts/Menlo.ttc".into(), 0);
        // Hangul fallback — D2Coding has Hangul, but Apple SD Gothic
        // Neo catches anything D2 skips (very rare jamo cluster).
        push_if(
            &mut out,
            "/System/Library/Fonts/AppleSDGothicNeo.ttc".into(),
            0,
        );
        // Japanese / Chinese.
        push_if(
            &mut out,
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc".into(),
            0,
        );
        // Apple Symbols catches dingbats etc. when Menlo also misses.
        push_if(&mut out, "/System/Library/Fonts/Apple Symbols.ttf".into(), 0);
        // Color emoji last.
        push_if(
            &mut out,
            "/System/Library/Fonts/Apple Color Emoji.ttc".into(),
            0,
        );
    }
    out
}

/// Markdown body font: a proportional gothic. Prefer Noto Sans KR if the user
/// installed it, else fall back to Apple SD Gothic Neo (always present on
/// macOS). Returns (path, face_index).
fn md_font_path() -> (String, u32) {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/Library/Fonts/NotoSansKR-Regular.otf"),
            format!("{home}/Library/Fonts/NotoSansKR-Regular.ttf"),
            "/Library/Fonts/NotoSansKR-Regular.otf".to_string(),
        ];
        for c in candidates {
            if std::path::Path::new(&c).exists() {
                return (c, 0);
            }
        }
        return ("/System/Library/Fonts/AppleSDGothicNeo.ttc".to_string(), 0);
    }
    #[cfg(target_os = "windows")]
    {
        return (r"C:\Windows\Fonts\malgun.ttf".to_string(), 0);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return (
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".to_string(),
            0,
        );
    }
}

fn default_font_path() -> String {
    #[cfg(target_os = "macos")]
    {
        // D2CodingLigature Nerd Font Mono — same primary the
        // sugarloaf path used. Falls back to Menlo when D2Coding
        // isn't installed (fresh Mac) so the renderer never panics
        // on a missing-font path.
        let home = std::env::var("HOME").unwrap_or_default();
        let d2 = format!("{home}/Library/Fonts/D2CodingLigatureNerdFontMono-Regular.ttf");
        if std::path::Path::new(&d2).exists() {
            return d2;
        }
        return "/System/Library/Fonts/Menlo.ttc".into();
    }
    #[cfg(target_os = "windows")]
    {
        return r"C:\Windows\Fonts\consola.ttf".into();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into();
    }
}
