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
use kasa_cells::pipeline::CellInstance;
use kasa_cells::{Atlas, AtlasEntry, GlyphKey, Pipeline, Shaper};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use kasa_bridge::screen::Cell;
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
    /// When set, the next `render` reads the presented frame back into a PNG at
    /// this path — permission-free self-capture for headless verification.
    pub capture_next: Option<String>,
    pipeline: Pipeline,
    atlas: Atlas,
    shaper: Shaper,
    /// Secondary shaper for markdown body/heading text — a proportional gothic
    /// (Noto Sans KR if installed, else Apple SD Gothic Neo) so documents read
    /// like prose, not code. Glyphs go into the SAME atlas keyed by font=1.
    md_shaper: Shaper,
    /// Bold weight of the markdown gothic (font=2). A real heavy face reads far
    /// cleaner than smearing the regular glyph, so headings / **bold** use this.
    md_bold_shaper: Shaper,
    bind_group: wgpu::BindGroup,
    font_size_px: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Per-frame chrome instances. main.rs's chrome code pushes via
    /// `rect()` / `draw_text()` between frames; `render()` drains.
    chrome: Vec<CellInstance>,
    /// Scale we cached on init. winit logical→physical conversion.
    scale: f32,
    /// True when KASATERM_P3_ROOT installed our own root metal layer and
    /// wgpu was given that layer via `SurfaceTargetUnsafe::CoreAnimationLayer`.
    /// In this mode the legacy per-frame P3 re-apply / re-promote calls must
    /// be skipped — they target wgpu's would-be sublayer, which doesn't
    /// exist on this path, and on macOS 26 they actively undo our root install.
    p3_root_owned: bool,
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
    /// Per-frame chrome icon quads: (texture key, instance). Same texture
    /// path as `image_quads`, but drawn AFTER the chrome pass so the icons
    /// sit on top of the title bar / pane headers instead of under them.
    icon_quads: Vec<(String, CellInstance)>,
    /// Logical-px rects of link spans drawn in the most recent markdown
    /// frame: (x, y, w, h, dest). main.rs hit-tests a click against these to
    /// open a file (Finder) or URL (browser). Cleared at the start of every
    /// `draw_markdown` so it always reflects the current scroll position.
    pub md_link_rects: Vec<(f32, f32, f32, f32, String)>,
    /// Logical-px rects of markdown code-block copy buttons: (x, y, w, h,
    /// code). main.rs hit-tests a click and copies `code`. Rebuilt each
    /// `draw_markdown` like `md_link_rects`.
    pub md_copy_rects: Vec<(f32, f32, f32, f32, String)>,
}

/// One pane's slot in `render_frame`. Mirrors the data the existing
/// sugarloaf renderer carries through `PaneFrame` but trimmed to
/// what Phase 2a needs (background fills, fg color, and the wide
/// markers come back in 2b).
pub struct PaneSlot<'a> {
    pub rows: &'a [Vec<Cell>],
    /// Pane top-left in physical pixels.
    pub origin_px: (f32, f32),
    /// Per-pane font multiplier. The shared cell metric (`cell_w`/`cell_h`)
    /// and font size are multiplied by this so one pane can render bigger/
    /// smaller than its neighbours without touching the BSP layout (which
    /// stays on the base cell). 1.0 = same as the rest of the UI.
    pub font_scale: f32,
    /// Unfocused pane: glyphs render at reduced alpha (text-only dim) so
    /// the active pane stands out without darkening the whole box.
    pub dim: bool,
    /// Clickable URL ranges in this pane's visible rows. Drawn as accent
    /// underlines (always-on hyperlink affordance) after the glyph pass.
    pub links: Vec<crate::links::LinkSpan>,
}

/// Pending chrome instances accumulated between `clear()` and the
/// next `render()`. Mirrors sugarloaf's immediate-mode API surface
/// (`rect`, `text_mut().draw`) but flushes through our retained
/// pipeline. Caller order is preserved so the rect-then-text painters
/// in main.rs paint in the same z-order as before.
#[derive(Default)]
#[allow(dead_code)]
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
        // P3 color reproduction is the DEFAULT path. Despite ghostty's
        // `+show-config --default` advertising `window-colorspace = srgb`,
        // empirical measurement against ghostty's actual output shows it
        // applies the sRGB→Display P3 matrix in practice (e.g. emitting
        // sRGB byte (202,58,50) makes Digital Color Meter read (186,70,58)
        // on the same display, which matches the matrix-converted value).
        // To match ghostty byte-for-byte we have to run the same matrix.
        // Set `KASATERM_P3_ROOT=0` to fall back to the legacy
        // RawHandle/sublayer path (byte passthrough — useful only when
        // comparing against a non-P3 reference).
        #[cfg(target_os = "macos")]
        let p3_root = std::env::var("KASATERM_P3_ROOT")
            .ok()
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        #[cfg(not(target_os = "macos"))]
        let p3_root = false;
        let surface = if p3_root {
            #[cfg(target_os = "macos")]
            unsafe {
                let layer_ptr = install_root_p3_layer(&window, scale)
                    .context("install_root_p3_layer failed")?;
                let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer_ptr);
                instance.create_surface_unsafe(target)?
            }
            #[cfg(not(target_os = "macos"))]
            unreachable!()
        } else {
            let surface_target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display_handle()?.as_raw(),
                raw_window_handle: window.window_handle()?.as_raw(),
            };
            unsafe { instance.create_surface_unsafe(surface_target)? }
        };
        // Live-resize would otherwise show the layer's stale pixels stretched
        // into the new bounds until our next frame lands. Pinning the layer's
        // contentsGravity to top-left keeps content anchored — same trick
        // ghostty uses (see feedback_tmuxify_rendering_pipeline).
        #[cfg(target_os = "macos")]
        unsafe {
            patch_metal_layer_gravity(&window);
            if !p3_root {
                // Legacy path: promote wgpu's observer CAMetalLayer to root
                // (try 5 — recorded as ineffective on macOS 26 because the
                // observer reattaches its own layer as a child). Kept for
                // the default `RawHandle` branch only.
                promote_metal_layer_to_root(&window, &surface);
            } else {
                eprintln!("[gpu] P3 root layer path active (KASATERM_P3_ROOT)");
            }
        }
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
        // Non-sRGB Unorm + raw sRGB bytes + CAMetalLayer P3 tag = the
        // simplest path to "punchier" colours. The bytes the GPU stores
        // get reinterpreted as P3-encoded at scan-out → sRGB pure red
        // (byte 255) displays at P3 pure red chromaticity, which is the
        // wider-gamut "look". Switching to an sRGB-tagged framebuffer +
        // shader decode introduced round-trip precision loss that
        // visibly dimmed Claude Code's saturated bgs.
        // CAMetalLayer.colorspace = P3 is honored more reliably by macOS
        // when the surface pixel format has wider precision than plain
        // 8-bit Unorm. Try in order:
        //   1. Rgba16Float (HDR-capable, P3 always honored)
        //   2. Bgra8Unorm (legacy, works in sugarloaf but flaky on
        //      macOS 26 sublayer setups)
        // Env override KASATERM_PIXEL_FORMAT for diagnostics.
        let prefer = std::env::var("KASATERM_PIXEL_FORMAT").unwrap_or_default();
        let format = if prefer == "float" {
            caps.formats
                .iter()
                .copied()
                .find(|f| matches!(f, wgpu::TextureFormat::Rgba16Float))
                .unwrap_or(wgpu::TextureFormat::Bgra8Unorm)
        } else if prefer == "srgb" {
            caps.formats
                .iter()
                .copied()
                .find(|f| f.is_srgb())
                .unwrap_or_else(|| caps.formats[0].add_srgb_suffix())
        } else {
            caps.formats
                .iter()
                .copied()
                .find(|f| !f.is_srgb())
                .unwrap_or_else(|| caps.formats[0].remove_srgb_suffix())
        };
        eprintln!("[gpu] surface format = {:?} srgb={}", format, format.is_srgb());
        let config = wgpu::SurfaceConfiguration {
            // COPY_SRC lets us read the presented frame back into a buffer for
            // headless self-capture (no screen-recording permission needed).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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
        // Register a bold variant of the primary face. swash uses this when
        // a cell's BOLD flag is set; no variant → renderer can fall back to
        // double-draw synthesised bold (handled in draw_cells).
        if let Some((bold_path, bold_idx)) = primary_bold_font_path() {
            shaper.set_bold_face_path(0, &bold_path, bold_idx);
        }
        // Real italic file (JetBrains Mono Italic etc). Without one, the
        // shaper synthesises italic via a 10° skew transform — works but
        // designed italic reads much cleaner. Same trick for the bold-
        // italic combo (renderer adds dilation on top of italic glyphs).
        if let Some((italic_path, italic_idx)) = primary_italic_font_path() {
            shaper.set_italic_face_path(0, &italic_path, italic_idx);
        }
        for (path, idx) in fallback_font_paths() {
            shaper.add_fallback_path(&path, idx);
        }
        // Bundled fallbacks (always present in the binary) — sit at
        // the end of the chain so a user-installed Nerd Font still
        // wins, but blank-outline gaps in the primary fall through
        // here for a guaranteed glyph.
        shaper.add_fallback_bytes(kasa_cells::CASCADIA_CODE_NF, 0);
        shaper.add_fallback_bytes(kasa_cells::SYMBOLS_NERD_FONT_MONO, 0);
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
        md_shaper.add_fallback_bytes(kasa_cells::CASCADIA_CODE_NF, 0);
        md_shaper.add_fallback_bytes(kasa_cells::SYMBOLS_NERD_FONT_MONO, 0);
        // Bold weight of the markdown gothic.
        let (md_bold_font, md_bold_idx) = md_bold_font_path();
        let mut md_bold_shaper = Shaper::from_path(&md_bold_font, md_bold_idx)
            .or_else(|_| Shaper::from_path(&md_font, md_idx))
            .with_context(|| format!("load markdown bold font {md_bold_font}"))?;
        for (path, idx) in fallback_font_paths() {
            md_bold_shaper.add_fallback_path(&path, idx);
        }
        md_bold_shaper.add_fallback_bytes(kasa_cells::CASCADIA_CODE_NF, 0);
        md_bold_shaper.add_fallback_bytes(kasa_cells::SYMBOLS_NERD_FONT_MONO, 0);
        let cell_w = shaper.cell_advance(font_size_px as f32).ceil();
        // Use the font's natural line metric (ascent+descent+leading)
        // for cell height instead of an arbitrary multiplier. Lines
        // pack at the same density sugarloaf produces with
        // `line_height=1.0` (which itself reads the same metrics
        // under the hood via cosmic-text).
        let cell_h = shaper.line_height(font_size_px as f32).ceil();
        let mut atlas = Atlas::new(&device, &queue, ATLAS_SIZE);
        // Supersample glyphs on sub-Retina displays (scale < 2): at 100% DPI
        // the logical pixel size (e.g. 13px) is too small to resolve a crisp
        // coverage mask, so bake at 2x and let the Linear sampler downsample
        // — Retina-class sharpness without changing layout. Retina (scale>=2)
        // already has the pixels, so keep it 1:1.
        atlas.set_oversample(if scale < 2.0 { 2 } else { 1 });
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
        // filterable=true: the glyph atlas now uses a Linear sampler so the
        // supersampled glyphs downsample smoothly (see Atlas::set_oversample).
        let pipeline = Pipeline::with_filtering(&device, format, 32_768, true);
        let init_dims = [config.width as f32, config.height as f32];
        let (init_gamma, init_contrast, init_sat) = text_render_knobs();
        pipeline.write_uniforms_full(
            &queue,
            init_dims,
            init_gamma,
            init_contrast,
            init_sat,
            p3_root,
            0.0,
        );
        let bind_group = pipeline.make_bind_group(&device, atlas.view(), atlas.sampler());

        // Image pass: own buffer (a few quads), linear filtering for smooth
        // scaling. Shares the same screen-size uniform projection.
        let image_pipeline = Pipeline::with_filtering(&device, format, 64, true);
        image_pipeline.write_uniforms_full(
            &queue,
            init_dims,
            init_gamma,
            init_contrast,
            init_sat,
            p3_root,
            0.0,
        );
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
            capture_next: None,
            config,
            pipeline,
            atlas,
            shaper,
            md_shaper,
            md_bold_shaper,
            bind_group,
            font_size_px,
            cell_w: cell_w / scale,
            cell_h: cell_h / scale,
            chrome: Vec::with_capacity(1024),
            scale,
            p3_root_owned: p3_root,
            image_pipeline,
            image_sampler,
            images: HashMap::new(),
            image_quads: Vec::new(),
            icon_quads: Vec::new(),
            md_link_rects: Vec::new(),
            md_copy_rects: Vec::new(),
        })
    }

    /// Logical-pixel solid rect (sugarloaf.rect drop-in). Caller
    /// passes the same logical coordinates main.rs has been using;
    /// we promote to physical pixels here to stay consistent with
    /// Resize the cell grid to a new logical font size (Cmd+= zoom).
    /// Atlas glyphs are keyed by size internally, so a re-bake happens
    /// lazily on the next draw — we just refresh the cached cell
    /// metrics so chrome/layout code sees the new geometry on the
    /// very next frame. Returns the new (cell_w, cell_h) in logical px.
    /// Update the effective render scale (DPI × ui_zoom). All chrome/cell
    /// draws multiply logical coords by `self.scale`, so changing it here and
    /// re-running `set_font_size` rescales the whole UI. Caller reflows layout.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.1);
    }

    /// Current effective render scale the GPU side is drawing with. Used by
    /// the render loop to detect drift from the window's `effective_scale()`
    /// (a missed DPI change) and self-heal before painting a compressed frame.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn set_font_size(&mut self, font_size_logical: f32) -> (f32, f32) {
        let new_px = (font_size_logical * self.scale).round().max(8.0) as u32;
        self.font_size_px = new_px;
        let cell_w_px = self.shaper.cell_advance(new_px as f32).ceil();
        let cell_h_px = self.shaper.line_height(new_px as f32).ceil();
        self.cell_w = cell_w_px / self.scale;
        self.cell_h = cell_h_px / self.scale;
        eprintln!(
            "[gpu] font resized → size_px={} cell={}x{} (logical {}x{})",
            new_px, cell_w_px as u32, cell_h_px as u32, self.cell_w, self.cell_h
        );
        (self.cell_w, self.cell_h)
    }

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

    /// Working-indicator rail (logical px). Pushes ONE `FLAG_WORKING_BAR`
    /// instance; the shader sweeps an indeterminate ~32% segment over a faint
    /// track from `u.time`, so a busy pane's loading bar animates on the GPU
    /// and the CPU never re-emits the bar per frame — the key to idle-0 CPU
    /// while any pane is working. uv carries the 0..1 horizontal sweep coord.
    pub fn working_bar(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4]) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            flags: CellInstance::FLAG_WORKING_BAR,
            ..Default::default()
        });
    }

    /// Filled rounded rectangle (logical px) — circle-traced caps, same as
    /// main.rs's `round_rect` but a method so the markdown renderer can round
    /// code blocks / inline-code chips.
    pub fn round_rect_fill(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: [u8; 4]) {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        // Straight middle band — no rounding needed between the two caps.
        self.rect(x, y + r, w, (h - 2.0 * r).max(0.0), col);
        if r <= 0.0 {
            return;
        }
        // Trace the caps at DEVICE-pixel resolution with a fractional-alpha
        // edge column so the corner reads smooth instead of stair-stepped.
        // The old version stepped one LOGICAL px (= 2 device px on retina)
        // with no anti-aliasing, which is the "hover 사각형 모서리 픽셀" the
        // user saw. One logical row here = `inv` device px; each row gets a
        // partial-coverage pixel on each side plus a solid middle span.
        let s = self.scale;
        let inv = 1.0 / s;
        let steps = (r * s).ceil() as i32;
        for k in 0..steps {
            let yy = k as f32 * inv; // logical distance inward from the cap edge
            let yc = yy + 0.5 * inv; // sample the row center for the circle test
            let d = (r * r - (r - yc) * (r - yc)).max(0.0).sqrt();
            let dx_dev = ((r - d) * s).max(0.0); // horizontal inset, device px
            let dx_floor = dx_dev.floor();
            let frac = dx_dev - dx_floor; // uncovered fraction of the boundary px
            let edge_col = [col[0], col[1], col[2], (col[3] as f32 * (1.0 - frac)).round() as u8];
            let lx = x + dx_floor * inv;
            let rx = x + w - (dx_floor + 1.0) * inv;
            let cx = x + (dx_floor + 1.0) * inv;
            let cw = (w - 2.0 * (dx_floor + 1.0) * inv).max(0.0);
            for ry in [y + yy, y + h - yy - inv] {
                self.rect(lx, ry, inv, inv, edge_col);
                self.rect(rx, ry, inv, inv, edge_col);
                if cw > 0.0 {
                    self.rect(cx, ry, cw, inv, col);
                }
            }
        }
    }

    /// Draw a text label using glyphs baked into the atlas at the
    /// requested size. Returns the pen-x after the last glyph
    /// (mirrors sugarloaf's `text.draw` return behaviour for callers
    /// that want it). Coordinates are logical pixels; `y` is the
    /// label's top edge — we approximate baseline via cell_h * 0.78
    /// matching the cell-grid path.
    /// Logical width `draw_text` would advance for `text` at `font_size`,
    /// without drawing. Same per-glyph stepping (wide-char tightening
    /// included) so tab backgrounds size to the exact drawn run.
    pub fn measure_chrome_text(&mut self, text: &str, font_size: f32, bold: bool) -> f32 {
        let s = self.scale;
        let size_px = (font_size * s).round() as u32;
        let mut pen = 0.0_f32;
        for ch in text.chars() {
            if ch == ' ' {
                pen += self.shaper.cell_advance(size_px as f32);
                continue;
            }
            let key = GlyphKey {
                ch,
                bold,
                italic: false,
                size_px,
                font: 0,
            };
            if let Some(entry) =
                self.atlas
                    .get_or_bake(&self.device, &self.queue, &mut self.shaper, key)
            {
                pen += if is_wide_char(ch) {
                    entry.px_w as f32 + size_px as f32 * 0.18
                } else {
                    entry.advance
                };
            }
        }
        pen / s
    }

    pub fn draw_text(&mut self, x: f32, y: f32, text: &str, opts: DrawOpts) -> f32 {
        self.draw_text_clipped(x, y, text, opts, f32::NEG_INFINITY, f32::INFINITY)
    }

    /// `draw_text` with hard left/right edges (logical px): a glyph that would
    /// cross either edge is skipped, but the pen keeps advancing so the returned
    /// width stays accurate. This renderer has no scissor (see render loop), so
    /// a Raw-editor pane's long code line would otherwise bleed past the pane
    /// (right) or, once panned by horizontal scroll, into the line-number gutter
    /// (left). Pass the pane's right edge and the gutter's right edge to fence
    /// the text in on both sides.
    pub fn draw_text_clipped(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        opts: DrawOpts,
        clip_left: f32,
        clip_right: f32,
    ) -> f32 {
        let s = self.scale;
        let size_px = (opts.font_size * s).round() as u32;
        let baseline_px = y * s + (size_px as f32 * 0.78);
        let fg = srgb_rgba_to_linear(opts.color);
        let clip_l = clip_left * s;
        let clip_px = clip_right * s;
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
            if glyph_x < clip_l || glyph_x + entry.px_w as f32 > clip_px {
                pen += if is_wide_char(ch) {
                    entry.px_w as f32 + size_px as f32 * 0.18
                } else {
                    entry.advance
                };
                continue;
            }
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
    pub fn draw_preedit(
        &mut self,
        origin_x: f32,
        origin_y: f32,
        text: &str,
        accent: [u8; 4],
        font_scale: f32,
    ) {
        let cell_w_px = self.cell_w * self.scale * font_scale;
        let cell_h_px = self.cell_h * self.scale * font_scale;
        // Glyph atlas size follows the pane zoom too — same rounding as
        // draw_cells so the composing syllable matches committed text.
        let size_px = ((self.font_size_px as f32 * font_scale).round() as u32).max(8);
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
            fg_rgba: srgb_rgba_to_linear(crate::cells::default_bg()),
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
                    size_px,
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
    pub fn draw_ghost(
        &mut self,
        origin_x: f32,
        origin_y: f32,
        text: &str,
        max_cells: u32,
        font_scale: f32,
    ) {
        let cell_w_px = self.cell_w * self.scale * font_scale;
        let cell_h_px = self.cell_h * self.scale * font_scale;
        let size_px = ((self.font_size_px as f32 * font_scale).round() as u32).max(8);
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
                    size_px,
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
        match font {
            2 => self
                .atlas
                .get_or_bake(&self.device, &self.queue, &mut self.md_bold_shaper, key),
            1 => self
                .atlas
                .get_or_bake(&self.device, &self.queue, &mut self.md_shaper, key),
            _ => self
                .atlas
                .get_or_bake(&self.device, &self.queue, &mut self.shaper, key),
        }
    }

    /// Space/cell advance for the requested font at `size_px`.
    #[allow(dead_code)]
    fn font_cell_advance(&mut self, size_px: u32, font: u8) -> f32 {
        match font {
            2 => self.md_bold_shaper.cell_advance(size_px as f32),
            1 => self.md_shaper.cell_advance(size_px as f32),
            _ => self.shaper.cell_advance(size_px as f32),
        }
    }

    /// True space advance for the requested font (metrics, not the 'M' cell
    /// width) — markdown word spacing.
    fn font_space_advance(&self, size_px: u32, font: u8) -> f32 {
        let sz = size_px as f32;
        match font {
            2 => self.md_bold_shaper.advance(' ', sz),
            1 => self.md_shaper.advance(' ', sz),
            _ => self.shaper.advance(' ', sz),
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
        // Inline code only: route CJK glyphs to the gothic body font. The mono
        // code face's Hangul advance is narrower than its raster, so a mono
        // syllable overlaps the next; the gothic face has matching metrics.
        cjk_gothic: bool,
    ) {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let baseline = y * s + size_px as f32 * 0.78;
        let fg = srgb_rgba_to_linear(color);
        let mut pen = x * s;
        // Proportional layout: each glyph advances by its own font metric. No
        // mono-grid wide-char fudge (terminal-only; made Hangul read loose).
        // Space has no raster, so its advance comes from metrics.
        for ch in text.chars() {
            let gfont = if cjk_gothic && is_wide_char(ch) {
                if bold { 2 } else { 1 }
            } else {
                font
            };
            if ch == ' ' {
                pen += self.font_space_advance(size_px, gfont);
                continue;
            }
            if let Some(e) = self.bake_glyph(ch, bold, italic, size_px, gfont) {
                {
                    let gx = pen + e.bearing_x as f32;
                    let gy = baseline - e.bearing_y as f32;
                    let (col, flags) = if e.is_color {
                        ([1.0, 1.0, 1.0, 1.0], CellInstance::FLAG_COLOR)
                    } else {
                        (fg, 0)
                    };
                    self.chrome.push(CellInstance {
                        cell_px: [gx, gy, e.px_w as f32, e.px_h as f32],
                        uv_min: e.uv_min,
                        uv_max: e.uv_max,
                        fg_rgba: col,
                        flags,
                        ..Default::default()
                    });
                }
                pen += e.advance;
            }
        }
    }

    /// Width (logical px) a styled run occupies, matching `md_draw_word`'s
    /// advance so word-wrap measurement equals what gets drawn. `code` selects
    /// the mono font (0); prose uses the gothic (1).
    fn measure_run(
        &mut self,
        text: &str,
        size: f32,
        bold: bool,
        italic: bool,
        code: bool,
        cjk_gothic: bool,
    ) -> f32 {
        let s = self.scale;
        let size_px = (size * s).round().max(1.0) as u32;
        let base_font: u8 = if code {
            0
        } else if bold {
            2
        } else {
            1
        };
        let mut w = 0.0;
        for ch in text.chars() {
            // Match md_draw_word: inline-code CJK measures on the gothic face.
            let font = if cjk_gothic && is_wide_char(ch) {
                if bold { 2 } else { 1 }
            } else {
                base_font
            };
            if ch == ' ' {
                w += self.font_space_advance(size_px, font);
                continue;
            }
            if let Some(e) = self.bake_glyph(ch, bold, italic, size_px, font) {
                w += e.advance;
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
        // 1.5× the natural line height for Notion-like airy paragraphs.
        let lh = (self.md_shaper.line_height(size * self.scale).ceil() / self.scale) * 1.5;
        // Real space advance, not the 'M' cell width (that over-spaced words).
        let space_w = self.measure_run(" ", size, false, false, false, false);
        let mut pen_x = x_start;
        let mut pen_y = y_start;
        for span in spans {
            let bold = span.bold || force_bold;
            for word in span.text.split_inclusive(' ') {
                let trailing_space = word.ends_with(' ');
                let trimmed = word.trim_end_matches(' ');
                if !trimmed.is_empty() {
                    let ww = self.measure_run(trimmed, size, bold, span.italic, span.code, span.code);
                    if pen_x + ww > x_start + max_w && pen_x > x_start {
                        pen_x = x_start;
                        pen_y += lh;
                    }
                    if pen_y + lh > clip_top && pen_y < clip_bot {
                        if span.code {
                            // Notion-style chip: a hair *lighter* than the body
                            // (SURFACE_ACTIVE > BG) so the code reads as a raised
                            // pill, not a black hole. (BORDER was near-black and
                            // swallowed the glyphs.) Size off the glyph metrics
                            // (not the 1.5× line height) so it hugs the text, and
                            // span the trailing space so a multi-word `inline
                            // code` is one chip, not one box per word.
                            let chip_w = ww
                                + space_w * 0.4
                                + if trailing_space { space_w } else { 0.0 };
                            self.round_rect_fill(
                                pen_x - space_w * 0.2,
                                pen_y + size * 0.06,
                                chip_w,
                                size * 1.04,
                                size * 0.28,
                                crate::theme::surface_active(),
                            );
                        }
                        if span.code {
                            // Inline code: syntax-highlight the word token by
                            // token (same lexer as code blocks; language is
                            // unknown inline so the generic keyword set applies),
                            // chaining pen-x with measure_run. Hangul still routes
                            // to the gothic via cjk_gothic=true so it never
                            // overlaps inside the chip.
                            let mut tpx = pen_x;
                            for (tok, tcol) in highlight_code_line(trimmed, "", crate::theme::text()) {
                                self.md_draw_word(
                                    &tok, tpx, pen_y, size, tcol, bold, span.italic, 0, true,
                                );
                                tpx += self.measure_run(&tok, size, bold, span.italic, true, true);
                            }
                        } else {
                            // Link → tint by destination kind; otherwise the
                            // block's own color.
                            let col = match &span.link {
                                Some(d) => link_color(d),
                                None => color,
                            };
                            let font: u8 = if bold { 2 } else { 1 };
                            self.md_draw_word(
                                trimmed, pen_x, pen_y, size, col, bold, span.italic, font, false,
                            );
                        }
                        if let Some(dest) = &span.link {
                            // Underline just below the glyph baseline (size-based,
                            // not the inflated line height) so it tracks the text.
                            // Span the trailing space so a multi-word link reads
                            // as one continuous underline, not one per word.
                            let uy = pen_y + size * 0.92;
                            let uw = ww + if trailing_space { space_w } else { 0.0 };
                            self.rect(pen_x, uy, uw, (size * 0.06).max(1.0), link_color(dest));
                            self.md_link_rects
                                .push((pen_x, pen_y, uw, lh, dest.clone()));
                        }
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

    /// Copy-button: rounded chip background (chrome layer) + Lucide copy SVG
    /// (icon layer, on top), sized to ICON_SIZE so it matches every other
    /// chrome icon. All logical px.
    fn draw_copy_icon(&mut self, bx: f32, by: f32, bw: f32, bh: f32) {
        let bg = crate::theme::with_alpha(crate::theme::surface_active(), 0xE0);
        self.round_rect_fill(bx, by, bw, bh, crate::theme::RADIUS_SM, bg);
        let isz = crate::theme::ICON_SIZE;
        self.queue_icon(
            "copy",
            bx + (bw - isz) / 2.0,
            by + (bh - isz) / 2.0,
            isz,
            crate::theme::text_dim(),
        );
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
        // Link / copy-button rects are rebuilt from scratch each frame so
        // they track the current scroll offset; main.rs hit-tests clicks.
        self.md_link_rects.clear();
        self.md_copy_rects.clear();
        let base = self.font_size_px as f32 / self.scale;
        // Notion-style reading column: generous side padding, capped content
        // width, centered in the pane. Shadow x/w so the block code below lays
        // out into the column without per-line changes. Clipping still uses the
        // full pane box (y/h).
        let side_pad = base * 1.7;
        let avail = (w - side_pad * 2.0).max(1.0);
        let cw = avail.min(base * 46.0);
        let x = x + side_pad + (avail - cw) * 0.5;
        let w = cw;
        let clip_top = y;
        let clip_bot = y + h;
        let top0 = y - scroll;
        let mut pen_y = top0 + base * 1.1;
        for block in blocks {
            match block {
                MdBlock::Heading { level, spans } => {
                    let scale_f = match level {
                        1 => 1.9,
                        2 => 1.5,
                        3 => 1.25,
                        4 => 1.1,
                        _ => 1.0,
                    };
                    let size = base * scale_f;
                    // Notion: big space above a heading, tight below so it binds
                    // to the text it introduces.
                    pen_y += if *level <= 1 { base * 1.6 } else { base * 1.2 };
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, true, crate::theme::text(), clip_top, clip_bot,
                    );
                    pen_y += base * 0.35;
                }
                MdBlock::Para { spans } => {
                    let size = base;
                    pen_y = self.md_runs(
                        spans, x, pen_y, w, size, false, crate::theme::text(), clip_top, clip_bot,
                    );
                    pen_y += base * 0.85;
                }
                MdBlock::Code { code, lang } => {
                    let size = base * 0.9;
                    let lh =
                        (self.md_shaper.line_height(size * self.scale).ceil() / self.scale) * 1.35;
                    let pad = base * 0.85;
                    let lines: Vec<&str> = code.trim_end_matches('\n').split('\n').collect();
                    let block_h = lines.len() as f32 * lh + pad * 2.0;
                    let block_top = pen_y;
                    let visible = pen_y + block_h > clip_top && pen_y < clip_bot;
                    if visible {
                        self.round_rect_fill(x, pen_y, w, block_h, base * 0.5, crate::theme::surface());
                    }
                    let mut ly = pen_y + pad;
                    for line in &lines {
                        if ly + lh > clip_top && ly < clip_bot {
                            // Syntax-highlight: draw each token in its color,
                            // chaining pen-x from draw_text's return value.
                            let mut tx = x + pad;
                            for (tok, col) in highlight_code_line(line, lang, crate::theme::text_dim()) {
                                tx = self.draw_text(
                                    tx,
                                    ly,
                                    &tok,
                                    DrawOpts {
                                        font_size: size,
                                        color: col,
                                        bold: false,
                                        italic: false,
                                    },
                                );
                            }
                        }
                        ly += lh;
                    }
                    if visible {
                        // Copy button, top-right; language label to its left.
                        let btn = base * 1.5;
                        let by = block_top + base * 0.35;
                        let bx = x + w - btn - base * 0.35;
                        self.draw_copy_icon(bx, by, btn, btn * 0.78);
                        self.md_copy_rects
                            .push((bx, by, btn, btn * 0.78, code.clone()));
                        if !lang.is_empty() {
                            let lw = self.measure_run(lang, size * 0.82, false, false, false, false);
                            self.draw_text(
                                bx - lw - base * 0.5,
                                by + base * 0.05,
                                lang,
                                DrawOpts {
                                    font_size: size * 0.82,
                                    color: crate::theme::text_mute(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                    }
                    pen_y += block_h + base * 0.85;
                }
                MdBlock::ListItem { depth, marker, spans } => {
                    let size = base;
                    let lh = self.md_shaper.line_height(size * self.scale).ceil() / self.scale;
                    let indent = (*depth as f32 + 1.0) * base * 1.5;
                    if pen_y + lh > clip_top && pen_y < clip_bot {
                        self.draw_text(
                            x + indent - base * 1.1,
                            pen_y,
                            marker,
                            DrawOpts {
                                font_size: size,
                                color: crate::theme::accent(),
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
                        crate::theme::text(),
                        clip_top,
                        clip_bot,
                    );
                    pen_y += base * 0.4;
                }
                MdBlock::Quote { spans } => {
                    let size = base;
                    let indent = base * 1.3;
                    let start_y = pen_y;
                    pen_y = self.md_runs(
                        spans,
                        x + indent,
                        pen_y,
                        (w - indent).max(1.0),
                        size,
                        false,
                        crate::theme::text_dim(),
                        clip_top,
                        clip_bot,
                    );
                    let bar_h = pen_y - start_y;
                    if start_y + bar_h > clip_top && start_y < clip_bot {
                        self.rect(x, start_y, base * 0.22, bar_h, crate::theme::accent());
                    }
                    pen_y += base * 0.8;
                }
                MdBlock::Rule => {
                    pen_y += base * 0.9;
                    if pen_y > clip_top && pen_y < clip_bot {
                        self.rect(x, pen_y, w, 1.0, crate::theme::border());
                    }
                    pen_y += base * 0.9;
                }
                MdBlock::Image { key, alt, w: iw_px, h: ih_px, .. } => {
                    if *iw_px > 0 && *ih_px > 0 && !key.is_empty() {
                        let iw = *iw_px as f32;
                        let ih = *ih_px as f32;
                        // Fit to the content column width, never upscaling past
                        // the image's own logical size. Keep aspect.
                        let disp_w = w.min(iw / self.scale);
                        let disp_h = disp_w * ih / iw;
                        if pen_y + disp_h > clip_top && pen_y < clip_bot {
                            self.queue_image(key, x, pen_y, disp_w, disp_h, 1.0, 0.0, 0.0);
                        }
                        pen_y += disp_h + base * 0.7;
                    } else {
                        // Decode failed / remote URL — show the alt text dimmed.
                        let lh = (self.md_shaper.line_height(base * self.scale).ceil()
                            / self.scale)
                            * 1.4;
                        if pen_y + lh > clip_top && pen_y < clip_bot {
                            self.md_draw_word(
                                &format!("[이미지: {alt}]"),
                                x,
                                pen_y,
                                base,
                                crate::theme::text_mute(),
                                false,
                                true,
                                1,
                                false,
                            );
                        }
                        pen_y += lh + base * 0.4;
                    }
                }
            }
        }
        (pen_y - top0).max(0.0)
    }

    /// Draw the Raw markdown editor: source lines in the mono font + a cursor
    /// bar. All logical px; returns total content height for scroll clamping.
    /// Hit-test a click (logical px) inside a raw-editor body box to a caret
    /// (line, col). Mirrors `draw_raw_editor`'s metrics so the caret lands where
    /// the glyph the user clicked actually sits. `x`/`y` are the body box origin,
    /// `scroll`/`h_scroll` the editor's pan.
    pub fn raw_editor_caret_at(
        &mut self,
        lines: &[String],
        x: f32,
        y: f32,
        scroll: f32,
        h_scroll: f32,
        click_x: f32,
        click_y: f32,
    ) -> (usize, usize) {
        let base = self.font_size_px as f32 / self.scale;
        let pad = base * 0.6;
        let lh = (self.shaper.line_height(base * self.scale).ceil() / self.scale) * 1.25;
        let digits = ((lines.len().max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        let cx0 = x + pad + gutter_w;
        let tx0 = cx0 - h_scroll;
        let top0 = (y - scroll) + pad;
        let line = (((click_y - top0) / lh).floor().max(0.0) as usize)
            .min(lines.len().saturating_sub(1));
        let chars: Vec<char> = lines.get(line).map(|l| l.chars().collect()).unwrap_or_default();
        // Walk the prefixes and pick the column whose pen-x is nearest the click.
        let mut best_col = 0;
        let mut best_d = f32::MAX;
        for col in 0..=chars.len() {
            let prefix: String = chars[..col].iter().collect();
            let px = tx0 + self.measure_run(&prefix, base, false, false, true, false);
            let d = (px - click_x).abs();
            if d < best_d {
                best_d = d;
                best_col = col;
            }
        }
        (line, best_col)
    }

    pub fn draw_raw_editor(
        &mut self,
        lines: &[String],
        cursor: (usize, usize),
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scroll: f32,
        h_scroll: f32,
        lang: &str,
        preedit: &str,
        cursor_on: bool,
    ) -> f32 {
        let clip_right = x + w;
        let base = self.font_size_px as f32 / self.scale;
        let pad = base * 0.6;
        let lh = (self.shaper.line_height(base * self.scale).ceil() / self.scale) * 1.25;
        // The line box (lh) is 1.25× the glyph height for breathing room, so the
        // text/number/cursor must drop by half the slack to sit centered in the
        // row — otherwise they cling to the top and the current-line highlight
        // band (which fills the whole box) looks misaligned.
        let glyph_voff = (lh - base) * 0.5;
        // Line-number gutter, sized to the digit count, right-aligned numbers.
        let digits = ((lines.len().max(1)) as f32).log10().floor() as usize + 1;
        let gutter_w = base * 0.62 * digits as f32 + base * 1.0;
        let cx0 = x + pad + gutter_w;
        // Text origin pans left with the horizontal scroll; the fixed gutter is
        // overpainted after each line so panned-left text never bleeds into it.
        let tx0 = cx0 - h_scroll;
        let clip_top = y;
        let clip_bot = y + h;
        let top0 = (y - scroll) + pad;
        let mut pen_y = top0;
        for (li, line) in lines.iter().enumerate() {
            if pen_y + lh > clip_top && pen_y < clip_bot {
                // Current-line highlight: a faint band across the pane behind
                // the cursor's row (drawn first so code paints on top). Must be
                // brighter than BG — SURFACE is *darker*, so it reads invisible.
                if li == cursor.0 {
                    self.rect(x, pen_y, w, lh, crate::theme::surface_hover());
                }
                // Code line, syntax-highlighted (single TEXT color when `lang`
                // is empty, e.g. plain text), panned by h_scroll.
                let mut tx = tx0;
                for (tok, col) in highlight_code_line(line, lang, crate::theme::text()) {
                    tx = self.draw_text_clipped(
                        tx,
                        pen_y + glyph_voff,
                        &tok,
                        DrawOpts { font_size: base, color: col, bold: false, italic: false },
                        cx0,
                        clip_right,
                    );
                }
                // Cursor (drawn before the gutter mask so one panned under the
                // gutter gets clipped away cleanly).
                if li == cursor.0 {
                    let prefix: String = line.chars().take(cursor.1).collect();
                    let cw = self.measure_run(&prefix, base, false, false, true, false);
                    let mut cur_x = tx0 + cw;
                    // Composing Hangul: draw the preedit at the cursor with an
                    // accent underline, cursor sits after it.
                    if !preedit.is_empty() {
                        let pw = self.measure_run(preedit, base, false, false, true, false);
                        self.rect(cur_x, pen_y + glyph_voff + base - 2.0, pw, 2.0, crate::theme::accent());
                        self.draw_text_clipped(
                            cur_x,
                            pen_y + glyph_voff,
                            preedit,
                            DrawOpts {
                                font_size: base,
                                color: crate::theme::accent(),
                                bold: false,
                                italic: false,
                            },
                            cx0,
                            clip_right,
                        );
                        cur_x += pw;
                    }
                    if cursor_on && cur_x >= cx0 {
                        // Cursor bar matches the glyph box (same voff + height as
                        // the text) so it lines up with the characters, not the
                        // padded line box.
                        self.rect(cur_x, pen_y + glyph_voff, 2.0, base, crate::theme::accent());
                    }
                }
                // Gutter mask: repaint the column over any text that scrolled
                // under it, then the right-aligned line number on top. The
                // current row keeps its highlight tint so the band reads as full
                // width (line number included).
                let gutter_bg = if li == cursor.0 {
                    crate::theme::surface_hover()
                } else {
                    crate::theme::bg()
                };
                self.rect(x, pen_y, cx0 - x, lh, gutter_bg);
                let num = format!("{}", li + 1);
                let num_w = self.measure_run(&num, base, false, false, true, false);
                self.draw_text(
                    x + pad + (gutter_w - base * 0.5 - num_w).max(0.0),
                    pen_y + glyph_voff,
                    &num,
                    DrawOpts {
                        font_size: base,
                        color: crate::theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            pen_y += lh;
        }
        (pen_y - top0 + pad).max(0.0)
    }

    /// Drop all pending chrome instances. main.rs calls this at the
    /// top of each frame so stale rects/labels from the previous
    /// frame don't pile up.
    pub fn clear_chrome(&mut self) {
        self.chrome.clear();
        self.image_quads.clear();
        self.icon_quads.clear();
    }

    /// Drop pending chrome icons only. Icons draw in their own pass *after*
    /// the chrome pass (see `icon_quads`), so a full-screen modal scrim — a
    /// plain chrome rect — can't cover them; the split/action glyphs bleed
    /// through. A modal that owns no icons of its own calls this so every
    /// icon queued below it disappears under the scrim.
    pub fn clear_icons(&mut self) {
        self.icon_quads.clear();
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

    /// Bundled Lucide SVG source for a chrome icon name. Compiled in so the
    /// .app needs no external asset dir.
    fn icon_svg(name: &str) -> Option<&'static str> {
        Some(match name {
            "folder" => include_str!("../assets/icons/folder.svg"),
            "x" => include_str!("../assets/icons/x.svg"),
            "plus" => include_str!("../assets/icons/plus.svg"),
            "minus" => include_str!("../assets/icons/minus.svg"),
            "panel-left" => include_str!("../assets/icons/panel-left.svg"),
            "folder-tree" => include_str!("../assets/icons/folder-tree.svg"),
            "folder-open" => include_str!("../assets/icons/folder-open.svg"),
            "folder-plus" => include_str!("../assets/icons/folder-plus.svg"),
            "file-plus" => include_str!("../assets/icons/file-plus.svg"),
            "chevron-right" => include_str!("../assets/icons/chevron-right.svg"),
            "chevron-down" => include_str!("../assets/icons/chevron-down.svg"),
            "file" => include_str!("../assets/icons/file.svg"),
            "file-code" => include_str!("../assets/icons/file-code.svg"),
            "image" => include_str!("../assets/icons/image.svg"),
            "braces" => include_str!("../assets/icons/braces.svg"),
            "settings-2" => include_str!("../assets/icons/settings-2.svg"),
            "columns-2" => include_str!("../assets/icons/columns-2.svg"),
            "rows-2" => include_str!("../assets/icons/rows-2.svg"),
            "copy" => include_str!("../assets/icons/copy.svg"),
            "terminal" => include_str!("../assets/icons/terminal.svg"),
            "sparkles" => include_str!("../assets/icons/sparkles.svg"),
            "rotate-cw" => include_str!("../assets/icons/rotate-cw.svg"),
            "maximize" => include_str!("../assets/icons/maximize.svg"),
            "file-text" => include_str!("../assets/icons/file-text.svg"),
            "git-branch" => include_str!("../assets/icons/git-branch.svg"),
            "chevrons-down-up" => include_str!("../assets/icons/chevrons-down-up.svg"),
            "panel-bottom" => include_str!("../assets/icons/panel-bottom.svg"),
            "panel-bottom-dashed" => include_str!("../assets/icons/panel-bottom-dashed.svg"),
            "git-commit-horizontal" => include_str!("../assets/icons/git-commit-horizontal.svg"),
            "ellipsis-vertical" => include_str!("../assets/icons/ellipsis-vertical.svg"),
            "arrow-up" => include_str!("../assets/icons/arrow-up.svg"),
            "arrow-down" => include_str!("../assets/icons/arrow-down.svg"),
            "github" => include_str!("../assets/icons/github.svg"),
            "undo-2" => include_str!("../assets/icons/undo-2.svg"),
            "external-link" => include_str!("../assets/icons/external-link.svg"),
            _ => return None,
        })
    }

    /// Rasterize an SVG into a square `px`-side RGBA8 buffer. `currentColor`
    /// is forced white: only the alpha channel matters because icons draw
    /// through the glyph tint path (texel.a × fg.rgb), so the theme color is
    /// applied at draw time, not bake time.
    fn rasterize_icon(svg: &str, px: u32) -> Option<Vec<u8>> {
        let svg = svg.replace("currentColor", "#ffffff");
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_str(&svg, &opt).ok()?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
        let size = tree.size();
        let scale = px as f32 / size.width().max(size.height());
        let tf = resvg::tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, tf, &mut pixmap.as_mut());
        Some(pixmap.data().to_vec())
    }

    /// Queue a chrome icon at `(x, y)` (logical px), `size`-side square, tinted
    /// `color`. Lazily rasterizes + caches the white alpha mask at the exact
    /// device-pixel resolution, then draws it through the monochrome tint path
    /// (`flags = 0` → shader does texel.a × fg.rgb) so it picks up hover /
    /// active colors exactly like a glyph would.
    pub fn queue_icon(&mut self, name: &str, x: f32, y: f32, size: f32, color: [u8; 4]) {
        let px = (size * self.scale).round() as u32;
        if px == 0 {
            return;
        }
        let key = format!("__icon:{name}:{px}");
        if !self.images.contains_key(&key) {
            let Some(svg) = Self::icon_svg(name) else { return };
            let Some(rgba) = Self::rasterize_icon(svg, px) else { return };
            self.upload_image(&key, &rgba, px, px);
        }
        if !self.images.contains_key(&key) {
            return;
        }
        // Snap to whole device pixels: the texture is rasterized 1:1 at `px`,
        // so a fractional dest makes the linear sampler blur / fringe the
        // edges ("마우스오버 픽셀 보임"). Integer dest = crisp 1:1 blit.
        let (dx, dy) = ((x * self.scale).round(), (y * self.scale).round());
        let dpx = px as f32;
        self.icon_quads.push((
            key,
            CellInstance {
                cell_px: [dx, dy, dpx, dpx],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: srgb_rgba_to_linear(color),
                flags: CellInstance::FLAG_ICON,
                ..Default::default()
            },
        ));
    }

    /// Queue an image pane for this frame. `(x, y, w, h)` is the pane's body
    /// box in LOGICAL px; the image is contain-fit (aspect preserved,
    /// centered) inside it. `zoom >= 1.0` scales past the fit size — when
    /// it would overflow the pane box we clip the dest rect AND adjust UVs
    /// so the image stays inside the pane (cropped to its center, never
    /// leaking into adjacent panes). `(pan_x, pan_y)` shift the crop window
    /// (logical px, image-center offset) so a drag pans a zoomed image;
    /// clamped here so the window never leaves the texture.
    pub fn queue_image(
        &mut self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        zoom: f32,
        pan_x: f32,
        pan_y: f32,
    ) {
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
        let z = zoom.max(1.0);
        let raw_dw = iw * fit * z;
        let raw_dh = ih * fit * z;
        // Per-axis: if the zoomed image fits, center it (pan has no room to
        // act); if it overflows, clip the dest to the pane edge and crop the
        // UV — shifted by the clamped pan so the visible window slides over
        // the texture instead of staying centered.
        let (dx, dw, uv_x0, uv_x1) = if raw_dw <= bw {
            (bx + (bw - raw_dw) * 0.5, raw_dw, 0.0_f32, 1.0_f32)
        } else {
            let max_off = (raw_dw - bw) * 0.5;
            let off = (pan_x * s).clamp(-max_off, max_off);
            let frac = (raw_dw - bw) / (2.0 * raw_dw);
            let d = off / raw_dw;
            (bx, bw, frac - d, 1.0 - frac - d)
        };
        let (dy, dh, uv_y0, uv_y1) = if raw_dh <= bh {
            (by + (bh - raw_dh) * 0.5, raw_dh, 0.0_f32, 1.0_f32)
        } else {
            let max_off = (raw_dh - bh) * 0.5;
            let off = (pan_y * s).clamp(-max_off, max_off);
            let frac = (raw_dh - bh) / (2.0 * raw_dh);
            let d = off / raw_dh;
            (by, bh, frac - d, 1.0 - frac - d)
        };
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [dx, dy, dw, dh],
                uv_min: [uv_x0, uv_y0],
                uv_max: [uv_x1, uv_y1],
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
        let dims = [self.config.width as f32, self.config.height as f32];
        let (gamma, contrast, sat) = text_render_knobs();
        self.pipeline.write_uniforms_full(
            &self.queue,
            dims,
            gamma,
            contrast,
            sat,
            self.p3_root_owned,
            0.0,
        );
        self.image_pipeline.write_uniforms_full(
            &self.queue,
            dims,
            gamma,
            contrast,
            sat,
            self.p3_root_owned,
            0.0,
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
        // The URL currently under the mouse renders in this blue — both its
        // glyphs (Pass 2) and its underline (Pass 3) — so a hovered link reads
        // like a hyperlink. `pane.links` holds the 0..1 hovered range.
        const LINK_BLUE: [u8; 4] = [0x0a, 0x84, 0xff, 0xff];
        // Glyph alpha for unfocused panes (PaneSlot.dim). Backgrounds keep
        // full alpha — only the text fades, so the box doesn't darken.
        const DIM_TEXT_ALPHA: f32 = 0.70;
        // Pass 1: backgrounds only. A tall CJK glyph bleeds a little
        // into the row below; emitting EVERY background first stops the
        // next row's bg fill from painting over the previous glyph's
        // bottom half. That over-paint was clipping Hangul in claude's
        // input-echo row (a run of reverse/bg cells); claude's normal
        // output rows have no bg below them, so they rendered fine.
        // (Reverse-video spaces still fill here — claude's cursor is an
        // inverse space, "띄어쓰기 커서".)
        for pane in panes {
            // Per-pane cell size: base metric × this pane's font multiplier.
            let cell_w_px = self.cell_w * self.scale * pane.font_scale;
            let cell_h_px = self.cell_h * self.scale * pane.font_scale;
            for (r, row) in pane.rows.iter().enumerate() {
                for (col, cell) in row.iter().enumerate() {
                    let want_bg = !matches!(cell.bg, kasa_bridge::screen::Color::Default)
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
            let cell_w_px = self.cell_w * self.scale * pane.font_scale;
            let cell_h_px = self.cell_h * self.scale * pane.font_scale;
            let pane_size_px = ((self.font_size_px as f32 * pane.font_scale).round() as u32).max(8);
            for (r, row) in pane.rows.iter().enumerate() {
                for (col, cell) in row.iter().enumerate() {
                    // Blanks contribute no glyph.
                    let ch = cell.ch;
                    if ch == ' ' || ch == '\0' {
                        continue;
                    }
                    // Block Elements (U+2580..259F) — paint as GPU
                    // quads instead of font glyphs. Monospace fonts
                    // render these with seams/gaps, so claude code's
                    // pixel-art character (built from half/quadrant
                    // blocks) tears when shaped as glyphs. The
                    // sub-cell rects from cells::block_rects fill the
                    // exact regions seamlessly.
                    {
                        if let Some(rects) = crate::cells::block_rects(ch) {
                            let mut fg = cell_fg_rgba(cell);
                            if pane.dim {
                                fg[3] = (fg[3] as f32 * DIM_TEXT_ALPHA) as u8;
                            }
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
                    let mut fg = cell_fg_rgba(cell);
                    if pane.links.iter().any(|l| {
                        l.row as usize == r
                            && (col as u16) >= l.col_start
                            && (col as u16) < l.col_end
                    }) {
                        fg = LINK_BLUE;
                    }
                    if pane.dim {
                        fg[3] = (fg[3] as f32 * DIM_TEXT_ALPHA) as u8;
                    }
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
                        size_px: pane_size_px,
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
        // Pass 3: blue underline beneath the URL currently under the mouse —
        // the hover hyperlink affordance (links are bare text until hovered).
        // The event handler flips the cursor to a pointer over the same range
        // and opens it on click. `pane.links` holds 0..1 ranges (the hovered
        // one), filled in render_frame_gpu from the live cursor position.
        let link_rgba = LINK_BLUE;
        for pane in panes {
            if pane.links.is_empty() {
                continue;
            }
            let cell_w_px = self.cell_w * self.scale * pane.font_scale;
            let cell_h_px = self.cell_h * self.scale * pane.font_scale;
            let thick = (cell_h_px * 0.06).max(1.0);
            let mut col = link_rgba;
            if pane.dim {
                col[3] = (col[3] as f32 * DIM_TEXT_ALPHA) as u8;
            }
            let lin = srgb_rgba_to_linear(col);
            for link in &pane.links {
                let x = pane.origin_px.0 + link.col_start as f32 * cell_w_px;
                let y = pane.origin_px.1 + (link.row as f32 + 1.0) * cell_h_px - thick - 1.0;
                let w = (link.col_end - link.col_start) as f32 * cell_w_px;
                self.chrome.push(CellInstance {
                    cell_px: [x, y, w, thick],
                    uv_min: Atlas::SOLID_UV,
                    uv_max: Atlas::SOLID_UV,
                    fg_rgba: lin,
                    ..Default::default()
                });
            }
        }
    }

    pub fn render(
        &mut self,
        _panes: &[PaneSlot<'_>],
        _scale: f32,
        time_secs: f32,
        chrome_changed: bool,
    ) -> Result<usize> {
        // Re-apply P3 colorspace before every drawable. wgpu's Metal HAL
        // doesn't touch this, but in practice the byte we read off the
        // panel ended up matching plain sRGB (255,0,0 measured as
        // 255,0,0 not the P3-wider 234,51,35 ghostty produces). Setting
        // it once at init wasn't enough on macOS 26.3 — possibly because
        // the layer's pixelFormat / drawableSize reconfig drops the tag.
        // Setting it every frame is cheap (one selector call) and keeps
        // the wider gamut sticky frame-to-frame.
        #[cfg(target_os = "macos")]
        if !self.p3_root_owned {
            // Legacy `RawHandle` path: wgpu owns the layer and creates it as
            // a sublayer that macOS won't color-manage. Re-apply / re-promote
            // every frame as a (mostly ineffective) workaround.
            apply_p3_via_hal(&self.surface);
            unsafe {
                reapply_p3(self._window.as_ref());
                promote_metal_layer_to_root(self._window.as_ref(), &self.surface);
            }
        } else {
            // P3_ROOT mode: we own the metal layer, but wgpu's
            // `surface.configure()` calls `setPixelFormat` / `setDevice` on
            // it which can quietly drop the Display P3 tag. Re-apply via the
            // hal handle every frame — same cheap setColorspace selector as
            // `apply_p3_via_hal`, just targeting the layer wgpu now reports
            // (which IS our root layer in this mode).
            apply_p3_via_hal(&self.surface);
        }
        // Advance the working-bar sweep on the GPU every present (cheap
        // offset write). When chrome is unchanged — a bar-only frame while a
        // pane is busy — skip re-uploading the instance buffers entirely, so a
        // working pane costs one uniform write + the draw, not a full chrome
        // rebuild. The cached instance buffer redraws as-is.
        self.pipeline.write_time(&self.queue, time_secs);
        let instance_count = self.chrome.len();
        let n_img = self.image_quads.len();
        if chrome_changed {
            self.pipeline
                .write_instances(&self.device, &self.queue, &self.chrome);
            // Upload this frame's image + icon quads (images first, icons
            // appended). Images draw under the chrome pass; icons over it.
            if !self.image_quads.is_empty() || !self.icon_quads.is_empty() {
                let all_instances: Vec<CellInstance> = self
                    .image_quads
                    .iter()
                    .chain(self.icon_quads.iter())
                    .map(|(_, inst)| *inst)
                    .collect();
                self.image_pipeline
                    .write_instances(&self.device, &self.queue, &all_instances);
            }
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
                            // verbatim, matching cells::default_bg().
                            let b = crate::cells::default_bg();
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
            // Chrome icons on top of the title bar / pane headers. Indices
            // continue past the image quads in the shared instance buffer.
            for (j, (id, _)) in self.icon_quads.iter().enumerate() {
                if let Some(entry) = self.images.get(id) {
                    self.image_pipeline
                        .draw_at(&mut pass, &entry.bind_group, (n_img + j) as u32);
                }
            }
        }
        // Self-capture: copy the just-rendered frame into a buffer before
        // present, then read it back to a PNG. No OS screen-record permission
        // needed (screencapture is blocked in headless runs).
        let capture = self.capture_next.take();
        let cap = if capture.is_some() {
            let w = self.config.width;
            let h = self.config.height;
            let bpr = w.div_ceil(64) * 256; // align(w*4, 256)
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture readback"),
                size: (bpr * h) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &frame.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bpr),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            Some((buf, w, h, bpr))
        } else {
            None
        };
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        if let (Some(path), Some((buf, w, h, bpr))) = (capture, cap) {
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
            let bgra = matches!(
                self.config.format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            );
            {
                let data = buf.slice(..).get_mapped_range();
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for row in 0..h {
                    let s = (row * bpr) as usize;
                    let line = &data[s..s + (w * 4) as usize];
                    for px in line.chunks_exact(4) {
                        if bgra {
                            rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
                        } else {
                            rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
                        }
                    }
                }
                match save_rgba_png(&path, &rgba, w, h) {
                    Ok(()) => eprintln!("[autocapture] gpu readback → {path} ({w}x{h})"),
                    Err(e) => eprintln!("[autocapture] gpu png failed: {e}"),
                }
            }
            buf.unmap();
        }
        Ok(instance_count)
    }
}

/// Encode RGBA8 pixels to a PNG file. Used by the GPU self-capture path. Uses
/// the `image` crate (available on every target; `png` is Windows-only here).
pub(crate) fn save_rgba_png(path: &str, rgba: &[u8], w: u32, h: u32) -> std::io::Result<()> {
    image::save_buffer(path, rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| std::io::Error::other(e.to_string()))
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

/// Link tint by destination kind, so links read as varied rather than one
/// flat blue: web=accent blue, local file=green, mailto=purple, anchor=cyan.
fn link_color(dest: &str) -> [u8; 4] {
    if dest.starts_with("http://") || dest.starts_with("https://") {
        crate::theme::accent()
    } else if dest.starts_with("mailto:") {
        crate::theme::syn_keyword()
    } else if dest.starts_with('#') {
        crate::theme::syn_function()
    } else {
        crate::theme::syn_string()
    }
}

/// Language keyword set for code-block syntax highlighting. Coarse on purpose
/// — a lightweight lexer, not a full grammar; the goal is colorful, readable
/// code, not perfect parsing.
fn syn_keywords(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => &[
            "fn", "let", "mut", "if", "else", "match", "for", "while", "loop", "return",
            "struct", "enum", "impl", "trait", "pub", "use", "mod", "self", "Self", "as",
            "const", "static", "ref", "move", "dyn", "where", "async", "await", "break",
            "continue", "in", "type", "unsafe", "crate", "super", "true", "false",
        ],
        "bash" | "sh" | "shell" | "zsh" | "fish" => &[
            "if", "then", "else", "elif", "fi", "for", "do", "done", "while", "case", "esac",
            "function", "echo", "export", "local", "return", "in", "set", "unset", "source",
            "alias", "cd", "exit", "read", "select", "until",
        ],
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => &[
            "function", "const", "let", "var", "if", "else", "for", "while", "return",
            "class", "new", "import", "export", "from", "async", "await", "try", "catch",
            "finally", "throw", "typeof", "instanceof", "this", "super", "extends", "switch",
            "case", "break", "continue", "default", "null", "undefined", "true", "false",
            "void", "yield", "interface", "type", "enum",
        ],
        "py" | "python" => &[
            "def", "class", "if", "elif", "else", "for", "while", "return", "import", "from",
            "as", "with", "try", "except", "finally", "raise", "lambda", "yield", "pass",
            "break", "continue", "in", "is", "not", "and", "or", "None", "True", "False",
            "global", "nonlocal", "async", "await",
        ],
        "go" | "golang" => &[
            "func", "var", "const", "if", "else", "for", "range", "return", "struct",
            "interface", "type", "package", "import", "go", "defer", "chan", "map", "select",
            "switch", "case", "break", "continue", "default", "nil", "true", "false",
        ],
        "c" | "cpp" | "c++" | "h" | "hpp" => &[
            "int", "char", "float", "double", "void", "if", "else", "for", "while", "return",
            "struct", "enum", "union", "typedef", "const", "static", "sizeof", "switch",
            "case", "break", "continue", "default", "unsigned", "signed", "long", "short",
            "class", "public", "private", "protected", "new", "delete", "true", "false",
            "nullptr", "namespace", "template", "auto",
        ],
        "json" => &["true", "false", "null"],
        _ => &[
            "if", "else", "for", "while", "return", "function", "fn", "def", "class",
            "import", "const", "let", "var", "true", "false", "null",
        ],
    }
}

/// Line-comment prefix(es) for a language.
fn syn_line_comment(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "bash" | "sh" | "shell" | "zsh" | "fish" | "py" | "python" | "yaml" | "yml" | "toml"
        | "rb" | "ruby" | "r" => &["#"],
        "lua" | "sql" | "hs" | "haskell" => &["--"],
        _ => &["//"],
    }
}

/// Tokenize one code line into (text, color) runs for syntax highlighting.
/// A small hand-rolled lexer: comments, strings, numbers, keywords, type-ish
/// (Capitalized) and call-ish (`name(`) identifiers; everything else uses
/// `base` — code blocks pass TEXT_DIM (light SURFACE bg), inline code passes
/// the brighter TEXT (its chip is darker, so dim plain text reads as black).
pub(crate) fn highlight_code_line(line: &str, lang: &str, base: [u8; 4]) -> Vec<(String, [u8; 4])> {
    use crate::theme;
    let kws = syn_keywords(lang);
    let comments = syn_line_comment(lang);
    let ch: Vec<char> = line.chars().collect();
    let n = ch.len();
    let mut out: Vec<(String, [u8; 4])> = Vec::new();
    let starts_comment = |i: usize| -> bool {
        comments
            .iter()
            .any(|cm| ch[i..].iter().take(cm.chars().count()).collect::<String>() == **cm)
    };
    let mut i = 0;
    while i < n {
        let c = ch[i];
        if starts_comment(i) {
            out.push((ch[i..].iter().collect(), theme::syn_comment()));
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            let q = c;
            let mut j = i + 1;
            while j < n {
                if ch[j] == '\\' {
                    j += 2;
                    continue;
                }
                if ch[j] == q {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let j = j.min(n);
            out.push((ch[i..j].iter().collect(), theme::syn_string()));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (ch[j].is_ascii_alphanumeric() || ch[j] == '.' || ch[j] == '_') {
                j += 1;
            }
            out.push((ch[i..j].iter().collect(), theme::syn_number()));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < n && (ch[j].is_alphanumeric() || ch[j] == '_') {
                j += 1;
            }
            let word: String = ch[i..j].iter().collect();
            let col = if kws.contains(&word.as_str()) {
                theme::syn_keyword()
            } else if word.chars().next().is_some_and(|c0| c0.is_uppercase()) {
                theme::syn_type()
            } else if j < n && ch[j] == '(' {
                theme::syn_function()
            } else {
                base
            };
            out.push((word, col));
            i = j;
            continue;
        }
        // Run of punctuation / whitespace up to the next interesting char.
        let mut j = i;
        while j < n {
            let cj = ch[j];
            if cj == '"'
                || cj == '\''
                || cj == '`'
                || cj.is_ascii_digit()
                || cj.is_alphabetic()
                || cj == '_'
                || starts_comment(j)
            {
                break;
            }
            j += 1;
        }
        out.push((ch[i..j].iter().collect(), base));
        i = j;
    }
    out
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
/// Source colours are authored in sRGB. The CAMetalLayer is tagged with
/// the Display P3 colorspace (`patch_p3_colorspace_safe`), so the bytes
/// we write are interpreted as P3-encoded by macOS at scan-out. To
/// actually USE the wider gamut we have to remap sRGB → linear sRGB →
/// linear P3 → P3-encoded here (chromaticity-preserving primary
/// transform) — without this remap an sRGB-pure-red byte stays at its
/// sRGB chromaticity inside the P3 container ("same look as before").
/// With the remap, sRGB primaries get pushed out toward the P3 gamut
/// edge for the punchier reds / greens ghostty / Rio default to.
///
/// Alpha is left untouched. The framebuffer is non-sRGB Unorm, so the
/// hardware alpha blend happens in encoded P3 space — slightly bolder
/// text, matching the previous "gamma-space blending" we shipped.
/// Source colours are authored in sRGB byte triples (ANSI palette,
/// truecolor SGR, theme tokens). CAMetalLayer is tagged Display P3, so
/// the EXACT bytes we write get reinterpreted by macOS as P3-encoded —
/// which means sRGB pure red (255,0,0) renders at the WIDER P3 pure red
/// chromaticity. That's the free saturation boost ghostty / Rio rely on:
/// "no transform, just tag the layer". Doing the matrix sRGB→P3 here
/// would CANCEL the boost (it would map sRGB pure red to its sRGB-inside-
/// P3 chromaticity, i.e. same visual as before). Alpha is byte-divided.
#[inline]
pub fn srgb_rgba_to_linear(rgba: [u8; 4]) -> [f32; 4] {
    [
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    ]
}

/// Text rendering knobs (text_gamma, text_contrast). WezTerm-style
/// `text_gamma>1.0` bends the glyph alpha mask so antialiased mid-tones
/// land more opaque — crisper text without changing the source colour.
/// `text_contrast` is an extra multiplier on top. Both readable from env
/// at startup so a user can tune without rebuilding.
fn text_render_knobs() -> (f32, f32, f32) {
    // gamma 1.0 = legacy linear alpha mask. Anything above sharpens but
    // also makes text feel "lifted / airy"; 1.0 stays grounded.
    let gamma = std::env::var("KASATERM_TEXT_GAMMA")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0_f32)
        .max(0.1);
    // 1.0 baseline — let the rendering pipeline pass alpha through
    // unchanged. The user wants knobs flat by default so the only
    // colour-shaping layer is the palette + P3 matrix.
    let contrast = std::env::var("KASATERM_TEXT_CONTRAST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0_f32)
        .max(0.1);
    // Saturation 1.0 default = passthrough. Bumping shifts perceived
    // hue slightly even with luma preservation (claude code's # comment
    // green drifted to chartreuse at 1.5). Source bytes go through
    // unchanged unless user dials this up.
    let sat = std::env::var("KASATERM_COLOR_SAT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0_f32)
        .max(0.0);
    (gamma, contrast, sat)
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
        // JetBrains Mono as the first fallback — covers Latin /
        // ASCII variants D2Coding's Korean designers left thinner,
        // plus its full Nerd Font icon table.
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
    #[cfg(target_os = "windows")]
    {
        let push_if = |out: &mut Vec<(String, u32)>, p: &str, i: u32| {
            if std::path::Path::new(p).exists() {
                out.push((p.to_string(), i));
            }
        };
        // Hangul — 맑은 고딕. The primary (consola) and bundled Cascadia
        // fallbacks carry no Hangul glyphs, so without this the entire
        // Korean output renders as blank cells.
        push_if(&mut out, r"C:\Windows\Fonts\malgun.ttf", 0);
        // CJK — Microsoft YaHei (Simplified Chinese) and Meiryo (Japanese).
        push_if(&mut out, r"C:\Windows\Fonts\msyh.ttc", 0);
        push_if(&mut out, r"C:\Windows\Fonts\meiryo.ttc", 0);
        // Symbols and color emoji.
        push_if(&mut out, r"C:\Windows\Fonts\seguisym.ttf", 0);
        push_if(&mut out, r"C:\Windows\Fonts\seguiemj.ttf", 0);
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

/// Bold weight of the markdown gothic. Apple SD Gothic Neo packs its Bold face
/// at TTC index 6; Noto Sans KR Bold ships as a separate file.
fn md_bold_font_path() -> (String, u32) {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/Library/Fonts/NotoSansKR-Bold.otf"),
            format!("{home}/Library/Fonts/NotoSansKR-Bold.ttf"),
            "/Library/Fonts/NotoSansKR-Bold.otf".to_string(),
        ];
        for c in candidates {
            if std::path::Path::new(&c).exists() {
                return (c, 0);
            }
        }
        return ("/System/Library/Fonts/AppleSDGothicNeo.ttc".to_string(), 6);
    }
    #[cfg(target_os = "windows")]
    {
        return (r"C:\Windows\Fonts\malgunbd.ttf".to_string(), 0);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return (
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Bold.ttc".to_string(),
            0,
        );
    }
}

/// Real italic variant of the primary mono face. JetBrains Mono ships
/// `JetBrainsMonoNerdFontMono-Italic.ttf` — D2Coding has none, so when
/// D2Coding is the primary we fall through to None (skew synthesis).
fn primary_italic_font_path() -> Option<(String, u32)> {
    if let Ok(p) = std::env::var("KASATERM_GRID_FONT_ITALIC") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let p = format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Italic.ttf");
        if std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    None
}

/// Bold variant of the primary mono face. Returns None on platforms where
/// we can't find one — the renderer falls back to synthesised double-draw
/// bold in that case. Honours `KASATERM_GRID_FONT_BOLD` for overrides.
fn primary_bold_font_path() -> Option<(String, u32)> {
    if let Ok(p) = std::env::var("KASATERM_GRID_FONT_BOLD") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let jb = format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Bold.ttf");
        if std::path::Path::new(&jb).exists() {
            return Some((jb, 0));
        }
        let p = format!("{home}/Library/Fonts/D2CodingLigatureNerdFontMono-Bold.ttf");
        if std::path::Path::new(&p).exists() {
            return Some((p, 0));
        }
        let menlo_bold = "/System/Library/Fonts/Menlo.ttc".to_string();
        if std::path::Path::new(&menlo_bold).exists() {
            return Some((menlo_bold, 1));
        }
    }
    #[cfg(target_os = "windows")]
    {
        // Designed bold matching the D2Coding primary; falls back to
        // Consolas Bold. Without a real bold face the shaper synthesises
        // bold via horizontal ink dilation, which spills past the glyph
        // advance and overlaps neighbours in bold chrome labels (active
        // tab title). The designed bold face fits its own advance.
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let d2b = format!(
            r"{local}\Microsoft\Windows\Fonts\D2CodingLigatureNerdFontMono-Bold.ttf"
        );
        if std::path::Path::new(&d2b).exists() {
            return Some((d2b, 0));
        }
        let p = r"C:\Windows\Fonts\consolab.ttf";
        if std::path::Path::new(p).exists() {
            return Some((p.to_string(), 0));
        }
    }
    None
}

fn default_font_path() -> String {
    #[cfg(target_os = "macos")]
    {
        // D2CodingLigature Nerd Font Mono. Has designed Hangul that
        // lines up cleanly on the monospace grid (자간 issues we saw
        // came from JetBrains-as-primary routing Hangul through a
        // fallback face with different sidebearings). Box-drawing
        // chars are rendered as GPU quads via `block_rects` so the
        // font choice doesn't affect line continuity — ghostty does
        // the same thing in `src/font/sprite/draw/box.zig`.
        let home = std::env::var("HOME").unwrap_or_default();
        let d2 = format!("{home}/Library/Fonts/D2CodingLigatureNerdFontMono-Regular.ttf");
        if std::path::Path::new(&d2).exists() {
            return d2;
        }
        return "/System/Library/Fonts/Menlo.ttc".into();
    }
    #[cfg(target_os = "windows")]
    {
        // Match the user's Windows Terminal font (D2CodingLigature Nerd
        // Font) so kasaterm reads consistently alongside their other
        // terminal. D2Coding's designed Hangul also lines up cleanly on
        // the mono grid — the same reason it's the macOS primary.
        // Per-user Nerd Font installs live under LocalAppData; fall back
        // to Consolas when it isn't installed.
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let d2 = format!(
            r"{local}\Microsoft\Windows\Fonts\D2CodingLigatureNerdFontMono-Regular.ttf"
        );
        if std::path::Path::new(&d2).exists() {
            return d2;
        }
        return r"C:\Windows\Fonts\consola.ttf".into();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into();
    }
}

#[cfg(target_os = "macos")]
unsafe fn patch_metal_layer_gravity(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = window.window_handle() else { return; };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return; };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;

    let root_layer: *mut AnyObject = msg_send![ns_view, layer];
    if root_layer.is_null() { return; }

    // wgpu attaches its drawing layer (WgpuObserverLayer wrapping a
    // CAMetalLayer) as a sublayer of the NSView's backing layer — so the
    // contents we need to anchor with gravity live on the SUBLAYER, not on
    // the NSView's root layer. Walk the tree and pin gravity on every
    // descendant we find.
    let gravity = NSString::from_str("topLeft");
    fn patch_recursive(layer: *mut objc2::runtime::AnyObject, gravity: &objc2_foundation::NSString) {
        use objc2::msg_send;
        unsafe {
            let _: () = msg_send![layer, setContentsGravity: gravity];
            let subs: *mut objc2::runtime::AnyObject = msg_send![layer, sublayers];
            if subs.is_null() { return; }
            let n: usize = msg_send![subs, count];
            for i in 0..n {
                let s: *mut objc2::runtime::AnyObject = msg_send![subs, objectAtIndex: i];
                if !s.is_null() {
                    patch_recursive(s, gravity);
                }
            }
        }
    }
    patch_recursive(root_layer, &gravity);
    eprintln!("[live-resize-probe] patched gravity recursively from root layer");

    // NSWindow-level colorspace. wgpu attaches its CAMetalLayer as a
    // SUBLAYER (sugarloaf replaces the view's layer entirely — that's
    // why their P3 tag stuck and ours didn't). For sublayer-based
    // setups the window's `colorSpace` is what macOS color-manages
    // against; setting it propagates Display P3 to everything inside.
    let ns_window: *mut AnyObject = msg_send![ns_view, window];
    if !ns_window.is_null() {
        if let Some(ns_cs_cls) = objc2::runtime::AnyClass::get(c"NSColorSpace") {
            let p3: *mut AnyObject = msg_send![ns_cs_cls, displayP3ColorSpace];
            if !p3.is_null() {
                let _: () = msg_send![ns_window, setColorSpace: p3];
                eprintln!("[gpu] NSWindow colorSpace → Display P3");
            }
        }
    }

    // Display P3 on CAMetalLayer. Doesn't modify source colours — just
    // tells macOS to interpret the same sRGB-encoded bytes as P3 at
    // scan-out. On Retina P3 panels the green (and red, blue) primaries
    // reach the wider P3 gamut → noticeably punchier diff bg highlights
    // / Claude Code colour chips. We had this once, removed it for fear
    // of "altering the terminal", but it's the layer-level setting
    // ghostty / iTerm2 use by default; the byte values stay untouched.
    patch_p3_colorspace_safe(root_layer);

    // NSViewLayerContentsRedrawPolicy: 2 = .duringViewResize. Default
    // (.onSetNeedsDisplay) lets AppKit skip paint during the live-resize
    // tracking loop, which is what makes the grid lag behind the frame.
    let _: () = msg_send![ns_view, setLayerContentsRedrawPolicy: 2_isize];
    // NSViewLayerContentsPlacement: 9 = .topLeft — mirrors the layer gravity
    // so AppKit's own resize-time scaling doesn't stretch contents either.
    let _: () = msg_send![ns_view, setLayerContentsPlacement: 9_isize];
}

/// Create a fresh CAMetalLayer, install it as the NSView's root layer,
/// tag it Display P3, and return the raw pointer. Used by the
/// `KASATERM_P3_ROOT=1` opt-in path: feeding this pointer to
/// `SurfaceTargetUnsafe::CoreAnimationLayer` makes wgpu reuse our layer
/// rather than create a sublayer-attached one (the macOS-color-management
/// blocker described in reference_kasaterm_color_pipeline).
///
/// Returns the layer pointer cast to `*mut c_void` — what wgpu wants.
#[cfg(target_os = "macos")]
unsafe fn install_root_p3_layer(
    window: &winit::window::Window,
    scale: f32,
) -> Result<*mut std::ffi::c_void> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    use std::sync::OnceLock;

    unsafe {
        let handle = window.window_handle().context("no window handle")?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            anyhow::bail!("not an AppKit handle");
        };
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;

        // Fresh CAMetalLayer instance — `[[CAMetalLayer alloc] init]`.
        let metal_cls = objc2::runtime::AnyClass::get(c"CAMetalLayer")
            .context("CAMetalLayer class missing")?;
        let layer_obj: *mut AnyObject = msg_send![metal_cls, alloc];
        let layer_ptr: *mut AnyObject = msg_send![layer_obj, init];
        if layer_ptr.is_null() {
            anyhow::bail!("CAMetalLayer init returned nil");
        }

        // setFrame on the layer requires the NSRect encode trait we
        // don't bring in here — and wgpu's `surface.configure()` calls
        // `setDrawableSize` later anyway, so skipping the initial frame
        // is harmless. Just pin the backing scale.
        let _: () = msg_send![layer_ptr, setContentsScale: scale as f64];
        // Anchor content to top-left during live resize (same as
        // patch_metal_layer_gravity for the legacy path).
        let topleft = objc2_foundation::NSString::from_str("topLeft");
        let _: () = msg_send![layer_ptr, setContentsGravity: &*topleft];

        // P3 colorspace tag — cached because CGColorSpace is expensive.
        static CS: OnceLock<usize> = OnceLock::new();
        let cs = *CS.get_or_init(|| {
            #[link(name = "CoreGraphics", kind = "framework")]
            unsafe extern "C" {
                fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
                static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            }
            let p = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
            p as usize
        });
        if cs != 0 {
            let _: () = msg_send![layer_ptr, setColorspace: cs as *mut std::ffi::c_void];
        }

        // Install as the NSView's root layer (layer-hosting view).
        let _: () = msg_send![ns_view, setLayer: layer_ptr];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        // Match the legacy patch_metal_layer_gravity: redraw on resize,
        // keep contents top-left during live drag.
        let _: () = msg_send![ns_view, setLayerContentsRedrawPolicy: 2_isize];
        let _: () = msg_send![ns_view, setLayerContentsPlacement: 9_isize];

        eprintln!(
            "[gpu] installed root P3 metal layer {:p} on NSView {:p}",
            layer_ptr, ns_view
        );
        Ok(layer_ptr as *mut std::ffi::c_void)
    }
}

/// Promote wgpu's CAMetalLayer (created as a sublayer by `layer_observer`)
/// to be the NSView's root layer. Without this, macOS color-manages
/// the parent root and silently ignores the sublayer's `colorspace`
/// tag, so Display P3 never takes effect (Color Meter reads pure sRGB).
/// Sugarloaf does this directly because it owns the layer creation.
#[cfg(target_os = "macos")]
unsafe fn promote_metal_layer_to_root(
    window: &winit::window::Window,
    surface: &wgpu::Surface<'static>,
) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    unsafe {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
        let hal_surface_opt = surface.as_hal::<wgpu_hal::api::Metal>();
        let Some(hal_surface) = hal_surface_opt else { return };
        let layer_lock = hal_surface.render_layer().lock();
        let layer_ref = layer_lock.as_ref();
        let layer_ptr: *mut AnyObject = layer_ref as *const _ as *mut AnyObject;
        // setLayer: requires the view to want a layer.
        let _: () = msg_send![ns_view, setLayer: layer_ptr];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        // P3 colorspace stays sticky only when EDR is enabled — on Apple
        // Silicon Mini-LED panels macOS color-manages SDR content to the
        // sRGB primary subspace of the display unless wantsEDR is on.
        // Use respondsToSelector to avoid the abort we hit earlier on
        // macOS 26 when calling it via the wrong object.
        let edr_sel = objc2::sel!(setWantsExtendedDynamicRangeContent:);
        let responds: bool = msg_send![layer_ptr, respondsToSelector: edr_sel];
        if responds {
            let _: () = msg_send![layer_ptr, setWantsExtendedDynamicRangeContent: true];
            eprintln!("[gpu] EDR enabled on render layer");
        }
        eprintln!("[gpu] promoted wgpu CAMetalLayer to NSView root layer");
    }
}

/// Apply P3 colorspace through wgpu-hal directly — the actual render
/// layer wgpu owns, not whatever sublayer we walked the NSView tree
/// looking for. Without this, the layer-walk approach silently fails
/// (Color Meter still reads 255,0,0 for a pure-red printf).
#[cfg(target_os = "macos")]
fn apply_p3_via_hal(surface: &wgpu::Surface<'static>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use std::sync::OnceLock;
    static CS: OnceLock<usize> = OnceLock::new();
    let cs = *CS.get_or_init(|| unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
        }
        let p = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
        p as usize
    });
    if cs == 0 { return; }
    unsafe {
        let hal_surface_opt = surface.as_hal::<wgpu_hal::api::Metal>();
        let Some(hal_surface) = hal_surface_opt else { return };
        let layer_lock = hal_surface.render_layer().lock();
        // metal::MetalLayerRef IS the CAMetalLayer Obj-C object — its
        // `&Ref` IS the pointer. Cast through *const () to drop the
        // type info safely.
        let layer_ref = layer_lock.as_ref();
        let layer_ptr: *mut AnyObject = layer_ref as *const _ as *mut AnyObject;
        let _: () = msg_send![layer_ptr, setColorspace: cs as *mut std::ffi::c_void];
        if std::env::var_os("KASATERM_COLORSPACE_DEBUG").is_some() {
            let applied: *mut AnyObject = msg_send![layer_ptr, colorspace];
            eprintln!(
                "[gpu] HAL P3 set on render_layer={:p} applied={}",
                layer_ptr,
                !applied.is_null()
            );
        }
    }
}

/// Per-frame P3 colorspace re-application via NSView layer walk. Kept as
/// a belt-and-braces — wgpu-hal path is the real fix.
#[cfg(target_os = "macos")]
unsafe fn reapply_p3(window: &winit::window::Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    use std::sync::OnceLock;
    static CACHED: OnceLock<(usize, usize)> = OnceLock::new(); // (layer_ptr, cs_ptr)
    let entry = CACHED.get_or_init(|| {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
        }
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
            let Ok(handle) = window.window_handle() else { return (0, 0) };
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return (0, 0) };
            let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
            let root_layer: *mut AnyObject = msg_send![ns_view, layer];
            // Walk to find the first CAMetalLayer-subclass descendant.
            let Some(metal_cls) = objc2::runtime::AnyClass::get(c"CAMetalLayer") else {
                return (0, 0);
            };
            fn find(l: *mut AnyObject, cls: &objc2::runtime::AnyClass) -> *mut AnyObject {
                unsafe {
                    let is_metal: bool = msg_send![l, isKindOfClass: cls];
                    if is_metal { return l; }
                    let subs: *mut AnyObject = msg_send![l, sublayers];
                    if !subs.is_null() {
                        let n: usize = msg_send![subs, count];
                        for i in 0..n {
                            let s: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                            if !s.is_null() {
                                let r = find(s, cls);
                                if !r.is_null() { return r; }
                            }
                        }
                    }
                    std::ptr::null_mut()
                }
            }
            let metal_layer = find(root_layer, metal_cls);
            (metal_layer as usize, cs as usize)
        }
    });
    let (layer_ptr, cs_ptr) = *entry;
    if layer_ptr == 0 || cs_ptr == 0 { return; }
    let layer = layer_ptr as *mut AnyObject;
    let cs = cs_ptr as *mut std::ffi::c_void;
    let _: () = unsafe { msg_send![layer, setColorspace: cs] };
}

/// Walks the layer tree and sets every CAMetalLayer descendant's
/// colorspace to Display P3 via direct CoreGraphics FFI. Skips any
/// non-CAMetalLayer (CALayer doesn't respond to `setColorspace:` on
/// older OS versions and the previous "patch every layer" version
/// aborted there). Returns silently on any failure — colours stay
/// sRGB rather than crashing the process.
#[cfg(target_os = "macos")]
fn patch_p3_colorspace_safe(root_layer: *mut objc2::runtime::AnyObject) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        // ExtendedDisplayP3 (vs plain DisplayP3): the "extended" variant
        // accepts encoded values outside [0,1] mapping to HDR-bright
        // colours. Even on a Bgra8Unorm framebuffer (which clamps), the
        // layer's intent telegraphs to the macOS compositor that we want
        // the panel's widest available gamut. Ghostty / iTerm2 both
        // settle on this when an EDR display is detected.
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CGColorSpaceRelease(cs: *mut std::ffi::c_void);
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            static kCGColorSpaceExtendedDisplayP3: *const std::ffi::c_void;
        }
        // env override KASATERM_COLORSPACE=p3|extended-p3|disabled
        let cs_name = std::env::var("KASATERM_COLORSPACE")
            .unwrap_or_else(|_| "p3".to_string());
        let cs_ref: *const std::ffi::c_void = match cs_name.as_str() {
            "disabled" => return,
            "extended-p3" => kCGColorSpaceExtendedDisplayP3,
            _ => kCGColorSpaceDisplayP3,
        };
        let cs = CGColorSpaceCreateWithName(cs_ref);
        if cs.is_null() {
            return;
        }
        let Some(metal_class) = AnyClass::get(c"CAMetalLayer") else {
            CGColorSpaceRelease(cs);
            return;
        };
        fn walk(
            layer: *mut objc2::runtime::AnyObject,
            cs: *mut std::ffi::c_void,
            metal_class: &AnyClass,
        ) -> usize {
            unsafe {
                let mut hits = 0usize;
                let is_metal: bool = msg_send![layer, isKindOfClass: metal_class];
                if is_metal {
                    let _: () = msg_send![layer, setColorspace: cs];
                    hits += 1;
                }
                let subs: *mut objc2::runtime::AnyObject = msg_send![layer, sublayers];
                if subs.is_null() {
                    return hits;
                }
                let n: usize = msg_send![subs, count];
                for i in 0..n {
                    let s: *mut objc2::runtime::AnyObject = msg_send![subs, objectAtIndex: i];
                    if !s.is_null() {
                        hits += walk(s, cs, metal_class);
                    }
                }
                hits
            }
        }
        let hits = walk(root_layer, cs, metal_class);
        // Sugarloaf's defensive pattern: never release the colorspace
        // we just handed to the layer. The CA property is documented to
        // retain on set, but if Apple ever changes that semantics our
        // colorspace would silently drop and the layer falls back to
        // sRGB — exactly the "set returned ok but colours look wrong"
        // symptom. We create one per process, so the leak is fine.
        // (See sugarloaf-0.4.4/src/context/metal.rs.)
        // `cs` is a *mut c_void (Copy) — `mem::forget` on it is a no-op;
        // we just want to suppress unused-result warnings. The actual
        // retain happens at the setColorspace: msg_send above.
        let _ = cs;
        eprintln!("[gpu] CAMetalLayer colorspace → {cs_name} ({hits} layer(s) tagged)");
    }
}

/// True while AppKit's live-resize tracking loop owns the window — the user
/// is dragging an edge. ghostty's resize trick depends on knowing this:
/// during live resize we leave the CAMetalLayer's drawableSize alone (no
/// surface.configure, no render) so the layer keeps its last painted
/// contents, and gravity=topLeft anchors that to the top-left while AppKit
/// stretches the bounds. The newly revealed area shows the clear colour
/// instead of stretched stale pixels.
#[cfg(target_os = "macos")]
pub fn is_in_live_resize(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let r: bool = msg_send![ns_view, inLiveResize];
        r
    }
}

#[cfg(not(target_os = "macos"))]
pub fn is_in_live_resize(_window: &Window) -> bool {
    false
}

/// Run `f` inside a CATransaction with implicit animations disabled. AppKit
/// hangs a layer animation on bounds jumps (zoom / maximize is the worst
/// case) and lets stale contents interpolate to the new bounds — gravity
/// alone can't fix that mid-animation. Wrapping the resize + render kills
/// the animation so the new frame is what AppKit composites.
#[cfg(target_os = "macos")]
pub fn with_disabled_layer_actions<F: FnOnce()>(f: F) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        let Some(class) = AnyClass::get(c"CATransaction") else {
            f();
            return;
        };
        let _: () = msg_send![class, begin];
        let _: () = msg_send![class, setDisableActions: true];
        f();
        let _: () = msg_send![class, commit];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn with_disabled_layer_actions<F: FnOnce()>(f: F) {
    f();
}

/// Toggle window maximize ("zoom") with NO frame animation. winit's
/// `set_maximized` routes through `[NSWindow zoom:]`, which animates the frame
/// over `animationResizeTime:` — that's the slow "이상한 애니메이션으로 늦게
/// 커짐" the user sees on a title-strip double-click. We drive the frame swap
/// ourselves with `animate:NO` so it snaps instantly. `saved` holds the
/// pre-zoom frame (Cocoa screen coords) so the next toggle can restore it;
/// `None` means currently un-maximized.
#[cfg(target_os = "macos")]
pub fn toggle_maximize_no_anim(window: &Window, saved: &mut Option<(f64, f64, f64, f64)>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        // isZoomed reflects the real frame regardless of how it got there
        // (our path, the green button, a live-resize drag), so it's a safer
        // truth than tracking our own bool.
        let is_zoomed: bool = msg_send![ns_window, isZoomed];
        if is_zoomed {
            if let Some((x, y, w, ht)) = saved.take() {
                let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, ht));
                let _: () = msg_send![ns_window, setFrame: frame, display: true, animate: false];
            }
            // saved == None here means we never recorded a restore frame
            // (e.g. the window was already zoomed by some other path). Leave
            // it maximized rather than guessing a frame.
        } else {
            let cur: NSRect = msg_send![ns_window, frame];
            *saved = Some((cur.origin.x, cur.origin.y, cur.size.width, cur.size.height));
            let mut screen: *mut AnyObject = msg_send![ns_window, screen];
            if screen.is_null() {
                if let Some(cls) = AnyClass::get(c"NSScreen") {
                    screen = msg_send![cls, mainScreen];
                }
            }
            if screen.is_null() {
                return;
            }
            // visibleFrame excludes the menu bar + Dock — same target AppKit
            // zoom uses, so this matches the old maximize bounds exactly.
            let vf: NSRect = msg_send![screen, visibleFrame];
            let _: () = msg_send![ns_window, setFrame: vf, display: true, animate: false];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn toggle_maximize_no_anim(window: &Window, _saved: &mut Option<(f64, f64, f64, f64)>) {
    window.set_maximized(!window.is_maximized());
}
