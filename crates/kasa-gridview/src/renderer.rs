//! 격자 렌더러 — wgpu 표면 하나에 칸 셀·커서·조합 중 글자·인라인 그림·사각형을 한 인스턴스 목록으로
//! 쌓아 그린다. 호스트(본판·카사라이트)는 그 위에 자기 크롬을 같은 목록으로 얹는다: 쌓은 순서가 곧
//! 그려지는 순서라, 나중에 쌓은 판이 먼저 쌓은 것을 덮는다.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use kasa_cells::pipeline::CellInstance;
use kasa_cells::{Atlas, AtlasEntry, GlyphKey, Pipeline, Shaper};
use kasa_screen::Cell;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

use crate::fonts::{attach_fallback_chain, default_font_path, primary_bold_font_path, primary_italic_font_path};
#[cfg(target_os = "macos")]
use crate::macos::{apply_p3_via_hal, install_root_p3_layer, patch_metal_layer_gravity, promote_metal_layer_to_root, reapply_p3};
use crate::palette::Palette;

/// 호스트가 넘기는 글꼴. 엔진은 글꼴 파일을 싣지 않는다 — cargo git 의존이 LFS 를 안 풀기 때문이다.
pub struct GridFonts {
    /// 주 고정폭 글꼴 경로. 없거나 비면 플랫폼 기본(`fonts::default_font_path`).
    pub primary: Option<String>,
    /// 시스템 대체 글꼴 뒤에 붙일 내장 글꼴 바이트(Nerd 기호 등).
    pub bundled: Vec<&'static [u8]>,
}

pub struct GridRenderer {
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    /// When set, the next `render` reads the presented frame back into a PNG at
    /// this path — permission-free self-capture for headless verification.
    pub capture_next: Option<String>,
    /// 물리픽셀 크롭 `(x, y, w, h)`. `capture_next` 와 함께 소비된다. None = 창 전체.
    /// `surface.capture` 가 pane 한 칸만 잘라 내는 데 쓴다.
    pub capture_crop: Option<(u32, u32, u32, u32)>,
    /// 저장 전 가로 상한(0 = 원본). 넘을 때만 비율을 지켜 줄인다 — 캡처를 읽는
    /// 쪽은 보통 에이전트라, 원본 해상도 그대로면 컨텍스트를 크게 태운다.
    pub capture_max_w: u32,
    pub pipeline: Pipeline,
    /// 글리프 아틀라스 하나를 격자와 호스트 글꼴(마크다운·크롬)이 함께 쓴다 — `GlyphKey::font` 로 가른다.
    pub atlas: Atlas,
    /// 격자 글꼴(font=0).
    pub shaper: Shaper,
    pub bind_group: wgpu::BindGroup,
    pub font_size_px: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Per-frame chrome instances. Callers push via `rect()` / text draws
    /// between frames; `render()` drains.
    pub chrome: Vec<CellInstance>,
    /// Scale we cached on init. winit logical→physical conversion.
    pub scale: f32,
    /// True when KASATERM_P3_ROOT installed our own root metal layer and
    /// wgpu was given that layer via `SurfaceTargetUnsafe::CoreAnimationLayer`.
    /// In this mode the legacy per-frame P3 re-apply / re-promote calls must
    /// be skipped — they target wgpu's would-be sublayer, which doesn't
    /// exist on this path, and on macOS 26 they actively undo our root install.
    pub p3_root_owned: bool,
    /// Separate pipeline for image panes — built with linear filtering so a
    /// photo scaled to a pane reads smooth, not pixelated. Has its own
    /// instance buffer so the image quads don't collide with the chrome
    /// pass's buffer in the same render pass.
    pub image_pipeline: Pipeline,
    /// Linear, clamp-to-edge sampler shared by every image bind group.
    pub image_sampler: wgpu::Sampler,
    /// Uploaded image textures keyed by pane id. Populated lazily on the
    /// first frame a given image pane is drawn.
    pub images: HashMap<String, ImageEntry>,
    /// Per-frame image quads: (pane id, instance, chrome watermark). Drained
    /// in `render()` where each is drawn with that pane's texture bind group.
    ///
    /// The watermark is `chrome.len()` at queue time — how much chrome was
    /// already queued when this quad was asked for. `render` uses it to slice
    /// the chrome pass and drop the quad back into its place, so **the order
    /// you queue things in is the order they come out**. Without it images and
    /// icons live in their own passes, permanently below/above all chrome, and
    /// no panel or modal can cover them however late it is drawn.
    pub image_quads: Vec<(String, CellInstance, u32, Option<[u32; 4]>)>,
    /// Per-frame chrome icon quads: (texture key, instance, chrome watermark).
    /// Same texture path and same ordering rule as `image_quads`.
    pub icon_quads: Vec<(String, CellInstance, u32, Option<[u32; 4]>)>,
    /// 지금 유효한 클립 사각형들 — LOGICAL px `[x0, y0, x1, y1]`, 이미 교집합이
    /// 접혀 있어 `last()` 하나가 곧 현재 클립이다. 프레임마다 `clear_chrome` 이
    /// 비운다.
    ///
    /// 클로저(`with_clip(|g| …)`)가 아니라 스택인 건 호출부 사정이다. 칼럼 그리기는
    /// 렌더러를 이미 손에 쥔 자유 함수들이고 루프 안에서 `continue` 로 빠져나간다 —
    /// 클로저로 감싸면 그 `continue` 가 안 넘어간다.
    pub clip_stack: Vec<[f32; 4]>,
    /// 클립이 바뀐 지점들 — `(그 시점의 chrome.len(), 그 뒤로 유효한 클립)`.
    /// PHYSICAL px `[x, y, w, h]`(`set_scissor_rect` 가 받는 그대로), `None` 은
    /// 클립 없음. `render` 가 이걸로 chrome 패스를 세그먼트로 잘라 그린다.
    pub clip_runs: Vec<(u32, Option<[u32; 4]>)>,
    /// 셀 색을 푸는 팔레트. 호스트가 테마를 바꾸면 여기를 갈아 끼운다.
    pub palette: Palette,
    /// 실제로 실린 주 글꼴 경로 — 호스트가 같은 글꼴로 보조 shaper 를 세울 때 쓴다.
    pub font_path: String,
}

impl GridRenderer {
    /// 창에 wgpu 표면을 세우고 주 글꼴로 격자를 잰다. `fonts.primary` 가 없으면 플랫폼 기본 고정폭
    /// 글꼴을 찾고, `fonts.bundled` 는 시스템 대체 글꼴 뒤에 마지막 그물로 붙는다.
    pub fn new(window: Arc<Window>, font_size_logical: f32, fonts: GridFonts) -> Result<Self> {
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
        // ghostty uses (see feedback_kasaterm_rendering_pipeline).
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
            // 2 = 드로어블 3장(wgpu-hal metal 은 값+1 을 maximumDrawableCount 로
            // 건다). 1(드로어블 2장)이던 때는 앞 장을 컴포지터가 놓아 줄 때까지
            // `get_current_texture` 가 GUI 스레드를 세워, 스크롤 중 그 스레드의
            // 36~50%가 그 기다림이었다. 격리 release 리그(방 8·칸 16·claude 10)
            // A/B 4회: 스크롤 ~78→~100fps, 키→화면 p50 12.5→9.0ms·p95 27.7→17.0ms
            // (2026-10-06). Immediate 라 장이 늘어도 vsync 큐가 생기지 않는다.
            desired_maximum_frame_latency: 2,
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
        let font_path = fonts.primary.filter(|p| !p.is_empty()).unwrap_or_else(default_font_path);
        eprintln!("[font] primary={font_path}");
        let mut shaper = Shaper::from_path(&font_path, 0)
            .with_context(|| format!("load font {font_path}"))?;
        // Register a bold variant of the primary face. swash uses this when
        // a cell's BOLD flag is set; no variant → renderer can fall back to
        // double-draw synthesised bold (handled in draw_cells).
        if let Some((bold_path, bold_idx)) = primary_bold_font_path(&font_path) {
            shaper.set_bold_face_path(0, &bold_path, bold_idx);
        }
        // Real italic file (JetBrains Mono Italic etc). Without one, the
        // shaper synthesises italic via a 10° skew transform — works but
        // designed italic reads much cleaner. Same trick for the bold-
        // italic combo (renderer adds dilation on top of italic glyphs).
        if let Some((italic_path, italic_idx)) = primary_italic_font_path() {
            shaper.set_italic_face_path(0, &italic_path, italic_idx);
        }
        attach_fallback_chain(&mut shaper, &fonts.bundled);
        let cell_w = cell_w_for(&mut shaper, font_size_px as f32);
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
        atlas.set_oversample(oversample_for(scale));
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
            window,
            surface,
            device,
            queue,
            capture_next: None,
            capture_crop: None,
            capture_max_w: 0,
            config,
            pipeline,
            atlas,
            shaper,
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
            clip_stack: Vec::new(),
            clip_runs: Vec::new(),
            palette: Palette::default(),
            font_path,
        })
    }

    /// 이번 장에 쌓은 그리기를 비운다. 호스트가 장마다 맨 앞에서 부른다.
    pub fn clear_chrome(&mut self) {
        self.chrome.clear();
        self.image_quads.clear();
        self.icon_quads.clear();
        self.clip_stack.clear();
        self.clip_runs.clear();
    }

    /// 장식 띠 하나(논리 px). `flag` 는 `CellInstance::FLAG_BAND_*` — 셰이더가 `u.time` 으로 움직여 CPU 는
    /// 띠를 다시 짓지 않는다. uv 가 0..1 가로 위치를 나른다.
    pub fn band(&mut self, x: f32, y: f32, w: f32, h: f32, rgba_u8: [u8; 4], flag: u32) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            flags: flag,
            ..Default::default()
        });
    }

    /// 빛 조각 하나가 한 바퀴씩 도는 둥근 사각 윤곽(논리 px). 굵기·반지름은 논리 px 로 받아 장치 px 로 실어
    /// 보낸다 — 셰이더가 장치 px 거리로 그린다. 쿼드 하나라 네 변이 모서리에서 겹쳐 진해지지 않는다.
    pub fn edge_orbit(
        &mut self,
        rect: (f32, f32, f32, f32),
        rgba_u8: [u8; 4],
        (head, tail_end): (f32, f32),
        radius: f32,
        lap_s: f32,
        tail: f32,
    ) {
        let s = self.scale;
        self.edge(rect, rgba_u8, CellInstance::edge_orbit_flags(head * s, tail_end * s, radius * s, lap_s, tail));
    }

    /// 무늬가 천천히 흐르는 점선 둥근 사각 윤곽(논리 px). `cycle` 은 선+틈 한 칸 길이.
    pub fn edge_dash(
        &mut self,
        rect: (f32, f32, f32, f32),
        rgba_u8: [u8; 4],
        thick: f32,
        cycle: f32,
        radius: f32,
        step_s: f32,
        duty: f32,
    ) {
        let s = self.scale;
        self.edge(rect, rgba_u8, CellInstance::edge_dash_flags(thick * s, cycle * s, radius * s, step_s, duty));
    }

    fn edge(&mut self, (x, y, w, h): (f32, f32, f32, f32), rgba_u8: [u8; 4], flags: u32) {
        let s = self.scale;
        self.chrome.push(CellInstance {
            cell_px: [x * s, y * s, w * s, h * s],
            uv_min: [-1.0, -1.0],
            uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(rgba_u8),
            flags,
            ..Default::default()
        });
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
    ///
    /// The atlas has to follow. Its supersampling factor is chosen from the
    /// scale, so leaving it stale after a monitor move bakes 1x-resolution
    /// coverage masks for a 1x display — the "글씨 깨짐" on the external
    /// monitor. And every cached entry is keyed by a `size_px` derived from
    /// the old scale, so without a repack the dead set just sits there until
    /// the texture is full.
    pub fn set_scale(&mut self, scale: f32) {
        let scale = scale.max(0.1);
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        // set_oversample only requests a reset when the factor actually
        // changes (Retina↔Retina moves keep it), so ask explicitly — the
        // size_px keys are stale either way.
        self.atlas.set_oversample(oversample_for(scale));
        self.atlas.request_reset();
    }

    /// Current effective render scale the GPU side is drawing with. Used by
    /// the render loop to detect drift from the window's `effective_scale()`
    /// (a missed DPI change) and self-heal before painting a compressed frame.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Repack the glyph atlas if it asked to be repacked — because a bake
    /// found no room, or because a DPI / font-size change invalidated every
    /// cached size. **Frame boundary only**: quads already queued this frame
    /// hold UVs into the current packing, and a repack would leave them
    /// pointing at whatever lands in those texels next.
    ///
    /// Missing glyphs re-bake on the paint that follows, so a full atlas
    /// costs one frame with some blank cells instead of blanking those
    /// characters for the rest of the session.
    pub fn maintain_atlas(&mut self) {
        let before = self.atlas.len();
        if self.atlas.begin_frame() {
            eprintln!("[gpu] atlas repacked ({before} glyphs dropped, scale={})", self.scale);
        }
    }

    /// True when the frame just painted left blank cells the next one can
    /// fill. The caller must schedule that frame — nothing else will.
    pub fn atlas_needs_another_frame(&self) -> bool {
        self.atlas.needs_another_frame()
    }

    /// Unconditional repack — the manual "화면 새로고침" escape hatch for
    /// state we failed to invalidate on our own.
    pub fn force_atlas_reset(&mut self) {
        self.atlas.request_reset();
    }

    pub fn set_font_size(&mut self, font_size_logical: f32) -> (f32, f32) {
        let new_px = (font_size_logical * self.scale).round().max(8.0) as u32;
        // Only on a real change: this is called on every DPI event and every
        // reflow, usually with the value it already has, and an unconditional
        // repack would throw the atlas away several times per second.
        if new_px != self.font_size_px {
            self.atlas.request_reset();
        }
        self.font_size_px = new_px;
        let cell_w_px = cell_w_for(&mut self.shaper, new_px as f32);
        let cell_h_px = self.shaper.line_height(new_px as f32).ceil();
        self.cell_w = cell_w_px / self.scale;
        self.cell_h = cell_h_px / self.scale;
        eprintln!(
            "[gpu] font resized → size_px={} cell={}x{} (logical {}x{})",
            new_px, cell_w_px as u32, cell_h_px as u32, self.cell_w, self.cell_h
        );
        (self.cell_w, self.cell_h)
    }

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

    /// 둥근 사각형의 **테두리만** — `round_rect_fill` 과 같은 원호를 따라 `t` 두께로
    /// 두른다. 채움은 둥글게 그려 놓고 테두리를 네모난 `rect` 넉 줄로 두르면
    /// 모서리 밖으로 직각 선이 삐져나온다(2026-09-10 지적 「z-index 안 맞아서
    /// 선 튀어나옴」 — 설정 화면의 카드·세그먼트·입력칸 전부가 그랬다).
    ///
    /// 캡 구간은 `round_rect_fill` 과 같은 행 단위로 돌되, 바깥 원(반지름 `r`)과
    /// 안쪽 원(반지름 `r - t`) 사이만 칠한다. 바깥 경계 픽셀은 같은 부분 알파를
    /// 받아 채움 위에 얹었을 때 계단이 안 보인다.
    pub fn round_rect_stroke(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        t: f32,
        col: [u8; 4],
    ) {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let t = t.max(0.0).min(w / 2.0).min(h / 2.0);
        if t <= 0.0 {
            return;
        }
        if r <= t {
            self.rect(x, y, w, t, col);
            self.rect(x, y + h - t, w, t, col);
            self.rect(x, y + t, t, (h - 2.0 * t).max(0.0), col);
            self.rect(x + w - t, y + t, t, (h - 2.0 * t).max(0.0), col);
            return;
        }
        // 좌우 직선 변 — 캡 사이.
        let band = (h - 2.0 * r).max(0.0);
        self.rect(x, y + r, t, band, col);
        self.rect(x + w - t, y + r, t, band, col);
        let s = self.scale;
        let inv = 1.0 / s;
        let steps = (r * s).ceil() as i32;
        let ri = r - t;
        for k in 0..steps {
            let yy = k as f32 * inv;
            let yc = yy + 0.5 * inv;
            let d_out = (r * r - (r - yc) * (r - yc)).max(0.0).sqrt();
            let dx_out_dev = ((r - d_out) * s).max(0.0);
            let dx_floor = dx_out_dev.floor();
            let frac = dx_out_dev - dx_floor;
            let edge_col = [col[0], col[1], col[2], (col[3] as f32 * (1.0 - frac)).round() as u8];
            // 안쪽 원의 같은 행 — 아직 원이 시작되지 않은 위쪽 행(`yc < t`)은 통째로
            // 윗변이다.
            let inner_dx = if yc < t {
                None
            } else {
                let dy = r - yc;
                if dy.abs() >= ri {
                    None
                } else {
                    Some(r - (ri * ri - dy * dy).sqrt())
                }
            };
            let lx = x + dx_floor * inv;
            let rx = x + w - (dx_floor + 1.0) * inv;
            let solid_from = (dx_floor + 1.0) * inv;
            for ry in [y + yy, y + h - yy - inv] {
                self.rect(lx, ry, inv, inv, edge_col);
                self.rect(rx, ry, inv, inv, edge_col);
                match inner_dx {
                    None => {
                        let cw = (w - 2.0 * solid_from).max(0.0);
                        if cw > 0.0 {
                            self.rect(x + solid_from, ry, cw, inv, col);
                        }
                    }
                    Some(dx_in) => {
                        let span = (dx_in - solid_from).max(0.0);
                        if span > 0.0 {
                            self.rect(x + solid_from, ry, span, inv, col);
                            self.rect(x + w - solid_from - span, ry, span, inv, col);
                        }
                    }
                }
            }
        }
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
            fg_rgba: srgb_rgba_to_linear(self.palette.bg),
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
                        self.chrome.push(CellInstance {
                            // preedit / ghost text is drawn standalone, with no
                            // row to look sideways into — no room to lend.
                            cell_px: fit_cell_glyph(
                                &entry, cell_x, baseline_y, cell_w_px, 0.0, 0.0,
                            ),
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
        let fg = srgb_rgba_to_linear(crate::palette::GHOST_FG);
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
                        self.chrome.push(CellInstance {
                            // preedit / ghost text is drawn standalone, with no
                            // row to look sideways into — no room to lend.
                            cell_px: fit_cell_glyph(
                                &entry, cell_x, baseline_y, cell_w_px, 0.0, 0.0,
                            ),
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

    /// 지금 유효한 클립을 PHYSICAL px `[x, y, w, h]` 로. 클립이 없으면 `None`.
    /// 폭이나 높이가 0 이면 「아무것도 안 보이는 클립」이라 그리기 자체를 건너뛴다.
    pub fn cur_clip_phys(&self) -> Option<[u32; 4]> {
        let [x0, y0, x1, y1] = *self.clip_stack.last()?;
        let s = self.scale;
        let (fw, fh) = (self.config.width as f32, self.config.height as f32);
        // 바깥으로 나간 만큼은 잘라 낸다 — `set_scissor_rect` 는 어태치먼트를
        // 넘는 사각형에 패닉한다.
        let px0 = (x0 * s).floor().clamp(0.0, fw);
        let py0 = (y0 * s).floor().clamp(0.0, fh);
        let px1 = (x1 * s).ceil().clamp(px0, fw);
        let py1 = (y1 * s).ceil().clamp(py0, fh);
        Some([px0 as u32, py0 as u32, (px1 - px0) as u32, (py1 - py0) as u32])
    }

    /// 클립이 방금 바뀌었음을 기록한다. 같은 chrome 위치에 두 번 기록되면 뒤엣것만
    /// 살아남게 덮어쓴다 — `push_clip` 직후 `pop_clip` 처럼 사이에 아무것도 안 그린
    /// 경우 세그먼트가 빈 채로 쌓이는 것을 막는다.
    ///
    /// 그리고 **앞과 값이 같아진 run 은 지운다.** 세그먼트를 가르지 않으니 draw call
    /// 만 하나 늘 뿐이다. 이게 있어야 `draw_text_clipped` 를 줄마다 부르는 편집기가
    /// 세그먼트 N 개가 아니라 하나로 접힌다 — pop 이 남긴 run 을 다음 줄의 push 가
    /// 같은 인덱스에서 덮어써 앞 run 과 같은 값이 되기 때문이다.
    pub fn note_clip(&mut self) {
        let at = self.chrome.len() as u32;
        let cur = self.cur_clip_phys();
        match self.clip_runs.last_mut() {
            Some((i, c)) if *i == at => *c = cur,
            _ => self.clip_runs.push((at, cur)),
        }
        let n = self.clip_runs.len();
        if n >= 2 && self.clip_runs[n - 1].1 == self.clip_runs[n - 2].1 {
            self.clip_runs.pop();
        }
    }

    /// 이 뒤로 그리는 chrome 을 `(x, y, w, h)`(LOGICAL px) 안으로 가둔다. 이미 클립이
    /// 서 있으면 **교집합**이 된다 — 안쪽 클립이 바깥을 넓힐 수는 없다.
    ///
    /// ⚠️ **행 루프 밖에서 한 번만 불러라.** 안에서 부르면 행마다 run 이 두 개씩
    /// 쌓여 세그먼트가 행 수만큼 늘고, 그만큼 draw call 이 는다.
    ///
    /// ⚠️ **시저는 픽셀만 자르지 클릭은 안 자른다.** 그리기 스킵을 지운 자리마다
    /// 히트렉트를 [`clip_hit`](Self::clip_hit) 로 교집합 내지 않으면 화면은 멀쩡한데
    /// 안 보이는 행이 눌린다 — 스크린샷이 절대 못 잡는 부류다.
    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w.max(0.0), y + h.max(0.0));
        if let Some([px0, py0, px1, py1]) = self.clip_stack.last().copied() {
            x0 = x0.max(px0);
            y0 = y0.max(py0);
            x1 = x1.min(px1).max(x0);
            y1 = y1.min(py1).max(y0);
        }
        self.clip_stack.push([x0, y0, x1, y1]);
        self.note_clip();
    }

    /// 가장 안쪽 클립을 걷는다. 짝이 안 맞는 `pop` 은 무시하고 로그만 남긴다 —
    /// 여기서 패닉하면 그리기 한 곳의 실수가 앱을 죽인다.
    pub fn pop_clip(&mut self) {
        if self.clip_stack.pop().is_none() {
            eprintln!("[clip] pop_clip 이 push 보다 많다 — 이 프레임의 클립은 버려진다");
            // 스택을 음수로 만들 수는 없으니, 대신 `render` 의 fail-open 이 물도록
            // 균형을 깨 둔다.
            self.clip_stack.push([0.0, 0.0, 0.0, 0.0]);
            return;
        }
        self.note_clip();
    }

    /// 히트렉트를 지금 클립과 교집합 낸다 — 잘려 안 보이는 부분은 눌려도 안 되므로.
    /// 교집합이 비면 `None`(= 그 rect 는 등록하지 마라).
    ///
    /// 클립이 없으면 받은 그대로 돌려준다. LOGICAL px `(x, y, w, h)`.
    pub fn clip_hit(&self, r: (f32, f32, f32, f32)) -> Option<(f32, f32, f32, f32)> {
        let Some([cx0, cy0, cx1, cy1]) = self.clip_stack.last().copied() else {
            return (r.2 > 0.0 && r.3 > 0.0).then_some(r);
        };
        let x0 = r.0.max(cx0);
        let y0 = r.1.max(cy0);
        let x1 = (r.0 + r.2).min(cx1);
        let y1 = (r.1 + r.3).min(cy1);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
    }

    /// 이 사각형이 지금 클립에 조금이라도 걸치나 — **컬링용**이다.
    ///
    /// 완전히 밖이면 `false`, 즉 그리기를 통째로 건너뛰어도 되는 경우다. 반쯤
    /// 걸치면 `true` 이고, 그건 시저가 알아서 자르니 **그릴 것**이다. 목록이
    /// 5000행쯤 되면 이 컬링이 없을 때 인스턴스가 수만 개로 불어난다 —
    /// 시저는 컬링을 대체하지 않는다.
    pub fn clip_visible(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        let Some([cx0, cy0, cx1, cy1]) = self.clip_stack.last().copied() else {
            return true;
        };
        x + w > cx0 && x < cx1 && y + h > cy0 && y < cy1
    }

    /// Has this pane's image already been uploaded? Lets the caller skip
    /// re-handing us the pixel buffer every frame.
    pub fn has_image(&self, id: &str) -> bool {
        self.images.contains_key(id)
    }

    /// 이번 프레임에 실제로 올라간 이미지 드로우의 키들 — **두 목록 다**.
    ///
    /// 하네스가 "학생 스프라이트가 정말 그려졌나"를 그림 없이 판정하는 유일한
    /// 창구다. 스캐너(`find_statusline_face` 등)를 하네스가 직접 다시 돌리면
    /// **자리가 있는지**만 알 뿐, 렌더 패스가 그걸 부르는지는 못 본다 — 그게
    /// 정확히 #48 이었다(자리는 멀쩡했고 부르는 쪽이 없었다).
    ///
    /// `image_quads` 만 세면 안 된다: 같은 학생이라도 statusline 프사는 셀 위에
    /// 얹히는 `queue_image_above` → `icon_quads` 로 가고 배경/커버는 `image_quads`
    /// 로 간다. 한쪽만 보면 프사가 멀쩡히 떠 있는데 0 개로 읽힌다(실측).
    pub fn drawn_image_keys(&self) -> impl Iterator<Item = &str> {
        self.image_quads.iter().chain(self.icon_quads.iter()).map(|(k, ..)| k.as_str())
    }

    /// 이번 프레임에 그 키로 그린 quad 들의 자리 — LOGICAL px `(x, y, w, h)`.
    ///
    /// 키만으로는 못 가르는 게 있어서 필요하다: 프사 마우스오버 팝업은 작은
    /// 프사와 **같은 키**(`student:<slug>:profile`)를 크기만 키워 재사용한다.
    /// 그래서 「팝업이 떴나」는 키 존재가 아니라 **큰 quad 가 하나 더 붙었나**로
    /// 판정해야 한다.
    ///
    /// 덤으로 하네스가 프사 자리를 렌더에서 되읽을 수 있다 — 좌표 계산
    /// (`cell_left()`·`AUX_CELL_TOP`)을 복제하면 그 복제본이 틀려도 하네스는
    /// 통과해 버린다.
    pub fn drawn_image_rects(&self, key: &str) -> Vec<(f32, f32, f32, f32)> {
        let s = self.scale.max(f32::EPSILON);
        self.image_quads
            .iter()
            .chain(self.icon_quads.iter())
            .filter(|(k, ..)| k == key)
            .map(|(_, c, ..)| {
                let [x, y, w, h] = c.cell_px;
                (x / s, y / s, w / s, h / s)
            })
            .collect()
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

    /// Evict every cached texture whose id starts with `prefix`. Used to force a
    /// reload after the user swaps character images — `upload_image` no-ops on an
    /// existing key, so the stale texture must be dropped first.
    pub fn drop_images_with_prefix(&mut self, prefix: &str) {
        self.images.retain(|k, _| !k.starts_with(prefix));
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
        // Contain fit, but never upscale past native — a small icon stays
        // crisp at 1:1 instead of blowing up blurry to fill the pane.
        let (_, _, fitted_w, fitted_h) =
            fit_terminal_art((bx, by, bw, bh), (entry.w, entry.h), 1.0, false);
        let z = zoom.max(1.0);
        let raw_dw = fitted_w * z;
        let raw_dh = fitted_h * z;
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
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// 상자에 비율을 지켜 꽉 맞춘다 — 작은 그림은 키우고 가운데 놓는다. kitty 그림
    /// 프로토콜의 놓기 규칙이다(`c`×`r` 칸 상자). `queue_image` 는 원본 크기를 넘겨
    /// 키우지 않아 레티나 칸 상자를 못 채운다. LOGICAL px.
    pub fn queue_image_contain(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w.max(1) as f32, entry.h.max(1) as f32);
        let fit = (bw / iw).min(bh / ih);
        let (dw, dh) = (iw * fit, ih * fit);
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [bx + (bw - dw) * 0.5, by + (bh - dh) * 0.5, dw, dh],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_image` 의 cover-fit 바닥 배경 버전 — 박스를 꽉 채우고(fill) 넘치는
    /// 축은 UV 를 중앙 크롭한다. 이미지 패스(셀보다 먼저 그려짐)라 default-bg 셀
    /// 자리로 비친다 — agents/resume 피커의 교실 배경용. LOGICAL px.
    pub fn queue_image_cover(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (iw, ih) = (entry.w as f32, entry.h as f32);
        // cover: 박스를 덮는 최소 배율(둘 중 큰 쪽). no-upscale 캡을 두지 않는다 —
        // 배경은 살짝 확대돼 흐려도 빈틈 없이 채우는 게 맞다.
        let fit = (bw / iw).max(bh / ih);
        let (dw, dh) = (iw * fit, ih * fit);
        let uv_x0 = (1.0 - (bw / dw).min(1.0)) * 0.5;
        let uv_y0 = (1.0 - (bh / dh).min(1.0)) * 0.5;
        self.image_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [bx, by, bw, bh],
                uv_min: [uv_x0, uv_y0],
                uv_max: [1.0 - uv_x0, 1.0 - uv_y0],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// `queue_image` 의 전경(chrome 위) 버전 — icon 패스로 그려져 셀 글리프·
    /// rect 위에 뜬다(학생 걷기 도트 등 장식 스프라이트용). 박스 안 contain-fit
    /// 후 가로 중앙·**바닥 정렬**(발이 박스 바닥에 닿게). 좌표는 LOGICAL px.
    pub fn queue_image_above(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(entry) = self.images.get(id) else { return };
        let s = self.scale;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (dx, dy, dw, dh) =
            fit_terminal_art((bx, by, bw, bh), (entry.w, entry.h), 1.0, true);
        self.icon_quads.push((
            id.to_string(),
            CellInstance {
                cell_px: [dx, dy, dw, dh],
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                fg_rgba: [1.0, 1.0, 1.0, 1.0],
                flags: CellInstance::FLAG_COLOR,
                ..Default::default()
            },
            self.chrome.len() as u32,
            self.cur_clip_phys(),
        ));
    }

    /// 스왑체인이 지금 잡고 있는 물리 픽셀 크기. 창 크기와 어긋났는지 프레임마다
    /// 대조하는 자가치유용 — 어긋난 채로 두면 컴포지터가 그 작은 드로어블을 창
    /// 구석에 얹어 화면이 영구히 축소돼 처박힌다.
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
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

    /// 보이지 않는 pane 의 격자를 **화면 밖 텍스처**에 그려 PNG 로 굽는다 —
    /// 다른 방(비활성 window)의 pane 은 표면 프레임에 존재하지 않아 기존
    /// capture(프레임 크롭)가 원리적으로 못 찍는다(나쵸 알림 사진이 termshot
    /// 폴백으로 밀리던 원인, 2026-09-02). 본 프레임 경로는 안 건드린다:
    /// 누적 리스트를 통째로 스왑해 두고 셀 전용 미니 패스를 자기 텍스처에
    /// 돌린 뒤 원상복구한다. 커서·스프라이트 오버레이는 안 싣는다 — 알림
    /// 사진의 목적은 내용이고, 오버레이는 활성 방 실촬영의 몫이다. 칸 안 그림
    /// (kitty·OSC 1337)은 내용이라 `paint` 가 셀 뒤에 얹는다 — 빼면 학생 얼굴 같은
    /// 그림 자리가 빈칸으로 찍혀 「이 칸만 그림이 안 그려진다」로 오판한다(2026-10-06).
    pub fn render_cells_offscreen(
        &mut self,
        panes: &[PaneSlot<'_>],
        w: u32,
        h: u32,
        path: &str,
        max_w: u32,
        paint: impl FnOnce(&mut Self),
    ) -> Result<(u32, u32), String> {
        let w = w.max(1);
        let h = h.max(1);
        // 본 프레임 몫과 섞이지 않게 스왑 — 실패해도 아래 복구 블록이 돌도록
        // 이 지점 이후의 이른 return 을 만들지 않는다.
        let saved_chrome = std::mem::take(&mut self.chrome);
        let saved_imgq = std::mem::take(&mut self.image_quads);
        let saved_iconq = std::mem::take(&mut self.icon_quads);
        let saved_runs = std::mem::take(&mut self.clip_runs);
        let saved_stack = std::mem::take(&mut self.clip_stack);
        self.draw_cells(panes);
        paint(self);
        let dims = [w as f32, h as f32];
        let (gamma, contrast, sat) = text_render_knobs();
        self.pipeline
            .write_uniforms_full(&self.queue, dims, gamma, contrast, sat, self.p3_root_owned, 0.0);
        self.pipeline
            .write_instances(&self.device, &self.queue, &self.chrome);
        let n = self.chrome.len() as u32;
        // 본 프레임은 그림 버퍼를 매번 새로 올리므로(`render` 의 chrome_changed) 여기서
        // 덮어써도 다음 프레임이 되돌린다. 크기 uniform 만 아래에서 되돌린다.
        if !self.image_quads.is_empty() {
            self.image_pipeline
                .write_uniforms_full(&self.queue, dims, gamma, contrast, sat, self.p3_root_owned, 0.0);
            let quads: Vec<CellInstance> = self.image_quads.iter().map(|(_, inst, ..)| *inst).collect();
            self.image_pipeline.write_instances(&self.device, &self.queue, &quads);
        }
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kasaterm offscreen capture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kasaterm offscreen encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kasaterm offscreen pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear({
                            let b = self.palette.bg;
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
            pass.set_scissor_rect(0, 0, w, h);
            self.pipeline.draw_range(&mut pass, &self.bind_group, 0, n);
            // 그림은 전부 셀 뒤에 큐잉됐으니 셀 위에 한 번에 얹는다. 클립은 표면 크기로
            // 잘려 있어 이 텍스처 크기로 한 번 더 자른다 — 넘으면 시저가 패닉한다.
            for (i, (key, _, _, clip)) in self.image_quads.iter().enumerate() {
                let Some(entry) = self.images.get(key) else { continue };
                let [cx, cy, cw, ch] = clip.unwrap_or([0, 0, w, h]);
                let (x0, y0) = (cx.min(w), cy.min(h));
                let (x1, y1) = ((cx + cw).min(w), (cy + ch).min(h));
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                self.image_pipeline.draw_at(&mut pass, &entry.bind_group, i as u32);
            }
        }
        let bpr = w.div_ceil(64) * 256; // align(w*4, 256)
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
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
        self.queue.submit(Some(encoder.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let saved = {
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
            if max_w > 0 && w > max_w {
                let nh = ((h as u64 * max_w as u64) / w as u64).max(1) as u32;
                match image::RgbaImage::from_raw(w, h, rgba.clone()) {
                    Some(img) => {
                        let small = image::imageops::resize(
                            &img,
                            max_w,
                            nh,
                            image::imageops::FilterType::Lanczos3,
                        );
                        save_rgba_png(path, small.as_raw(), max_w, nh).map(|()| (max_w, nh))
                    }
                    None => save_rgba_png(path, &rgba, w, h).map(|()| (w, h)),
                }
            } else {
                save_rgba_png(path, &rgba, w, h).map(|()| (w, h))
            }
        };
        // 원상복구 — 누적 리스트와 uniforms(표면 크기)를 본 프레임 몫으로.
        self.chrome = saved_chrome;
        self.image_quads = saved_imgq;
        self.icon_quads = saved_iconq;
        self.clip_runs = saved_runs;
        self.clip_stack = saved_stack;
        let sdims = [self.config.width as f32, self.config.height as f32];
        self.pipeline
            .write_uniforms_full(&self.queue, sdims, gamma, contrast, sat, self.p3_root_owned, 0.0);
        self.image_pipeline
            .write_uniforms_full(&self.queue, sdims, gamma, contrast, sat, self.p3_root_owned, 0.0);
        saved.map_err(|e| format!("offscreen png 저장 실패: {e}"))
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
        DRAW_CELLS_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The URL currently under the mouse renders in this blue — both its
        // glyphs (Pass 2) and its underline (Pass 3) — so a hovered link reads
        // like a hyperlink. `pane.links` holds the 0..1 hovered range.
        const LINK_BLUE: [u8; 4] = [0x0a, 0x84, 0xff, 0xff];
        // Glyph alpha for unfocused panes (PaneSlot.dim). Backgrounds keep
        // full alpha — only the text fades, so the box doesn't darken.
        const DIM_TEXT_ALPHA: f32 = 0.70;
        let pal = self.palette;
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
                    let adapted = crate::palette::adapt_to_viewer(cell, pane.source.as_ref());
                    let cell = adapted.as_ref();
                    let want_bg = !matches!(cell.bg, kasa_screen::Color::Default)
                        || cell.inverse;
                    let bg = pal.cell_bg_with(cell, pane.default_fg);
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
                probe_cell_row(r, row, pane.dim, pane.font_scale);
                // Right edge of the last glyph painted on this row. A blank
                // cell an oversized neighbour already spilled into is not free
                // room any more, so `fit_cell_glyph` is told about it.
                let mut glyph_right = f32::NEG_INFINITY;
                for (col, cell) in row.iter().enumerate() {
                    let adapted = crate::palette::adapt_to_viewer(cell, pane.source.as_ref());
                    let cell = adapted.as_ref();
                    // Blanks contribute no glyph.
                    let ch = cell.ch;
                    if ch == ' ' || ch == '\0' {
                        continue;
                    }
                    // SGR 8(conceal) — 배경은 위 패스에서 칠했고 글리프만 생략.
                    // statusline 의 세션 id 마커가 이 플래그로 화면에서 숨는다.
                    if cell.hidden {
                        continue;
                    }
                    // Box Drawing(U+2500..257F)과 Block Elements(U+2580..259F)는
                    // 폰트 글리프 대신 GPU 사각형으로 — 글리프는 advance 폭까지만
                    // 그려서 칸이 그보다 넓으면 이웃 칸과 틈이 남는다(표 가로줄이
                    // 점선이 되고, 반칸 블록으로 짠 학생 도트가 찢어진다). 선은
                    // `box_line_rects`(px), 면은 `block_rects`(비율). 혼합 굵기
                    // 교차처럼 안 다루는 글자만 폰트로 떨어진다.
                    if ('\u{2500}'..='\u{259F}').contains(&ch) {
                        let mut fg = pal.cell_fg_with(cell, pane.default_fg);
                        if pane.dim {
                            fg[3] = (fg[3] as f32 * DIM_TEXT_ALPHA) as u8;
                        }
                        let lin = srgb_rgba_to_linear(fg);
                        let cx = pane.origin_px.0 + col as f32 * cell_w_px;
                        let cy = pane.origin_px.1 + r as f32 * cell_h_px;
                        let chrome = &mut self.chrome;
                        let mut put = |x0: f32, y0: f32, x1: f32, y1: f32, alpha: f32| {
                            let mut c = lin;
                            c[3] *= alpha;
                            chrome.push(CellInstance {
                                cell_px: [cx + x0, cy + y0, x1 - x0, y1 - y0],
                                uv_min: Atlas::SOLID_UV,
                                uv_max: Atlas::SOLID_UV,
                                fg_rgba: c,
                                ..Default::default()
                            });
                        };
                        if crate::geometry::box_line_rects(ch, cell_w_px, cell_h_px, &mut put) {
                            continue;
                        }
                        if let Some(rects) = crate::geometry::block_rects(ch) {
                            for &(x0, y0, x1, y1, alpha) in rects {
                                put(
                                    x0 * cell_w_px,
                                    y0 * cell_h_px,
                                    x1 * cell_w_px,
                                    y1 * cell_h_px,
                                    alpha,
                                );
                            }
                            continue;
                        }
                    }
                    let cell_x = pane.origin_px.0 + col as f32 * cell_w_px;
                    let cell_y = pane.origin_px.1 + r as f32 * cell_h_px;
                    let mut fg = pal.cell_fg_with(cell, pane.default_fg);
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
                        glyph_right = x + gw;
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
                        glyph_right = x + gw;
                        self.chrome.push(CellInstance {
                            cell_px: [x, y, gw, gh],
                            uv_min: entry.uv_min,
                            uv_max: entry.uv_max,
                            fg_rgba: srgb_rgba_to_linear(fg),
                            ..Default::default()
                        });
                    } else {
                        // An oversized ambiguous-width glyph is slid into the
                        // blank columns beside it. A column counts as free only
                        // when it really exists — past the row's end there is
                        // nothing to borrow and the spill would cross into the
                        // neighbouring pane — and only up to where the previous
                        // glyph already reaches, so `① ②잠금` cannot have the ②
                        // slide left onto the ①.
                        let room_right = match row.get(col + 1) {
                            Some(n) if matches!(n.ch, ' ' | '\0') => cell_w_px,
                            _ => 0.0,
                        };
                        let room_left = match col.checked_sub(1).and_then(|c| row.get(c)) {
                            Some(p) if matches!(p.ch, ' ' | '\0') => {
                                cell_w_px.min((cell_x - glyph_right).max(0.0))
                            }
                            _ => 0.0,
                        };
                        let rect = fit_cell_glyph(
                            &entry, cell_x, baseline_y, cell_w_px, room_left, room_right,
                        );
                        glyph_right = rect[0] + rect[2];
                        self.chrome.push(CellInstance {
                            cell_px: rect,
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

    /// 이번 장을 그려 낸다. `overlay` 는 같은 장에 호스트가 얹을 패스다(본판은 날씨) — 셀·크롬을 다
    /// 그린 뒤, 자기 캡처와 present 앞에 불린다.
    pub fn render(
        &mut self,
        time_secs: f32,
        chrome_changed: bool,
        overlay: impl FnOnce(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::Texture,
            &wgpu::TextureView,
            wgpu::TextureFormat,
        ),
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
                reapply_p3(self.window.as_ref());
                promote_metal_layer_to_root(self.window.as_ref(), &self.surface);
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
            // appended) into one buffer. Buffer position is just storage —
            // the draw order comes from the watermarks below.
            if !self.image_quads.is_empty() || !self.icon_quads.is_empty() {
                let all_instances: Vec<CellInstance> = self
                    .image_quads
                    .iter()
                    .chain(self.icon_quads.iter())
                    .map(|(_, inst, ..)| *inst)
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
                            let b = self.palette.bg;
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
            // Chrome is one big instance buffer, but images and icons each
            // need their own texture bound, so they can't ride in it. Instead
            // of giving them fixed layers under/over the whole chrome pass, we
            // cut the chrome at each quad's watermark and drop the quad in
            // there — an icon queued before a panel ends up under that panel,
            // and one queued after ends up on top, which is what every caller
            // already assumes when it draws a cover over something.
            let mut ordered: Vec<(u32, u32, &String, Option<[u32; 4]>)> = self
                .image_quads
                .iter()
                .enumerate()
                .map(|(i, (id, _, wm, clip))| (*wm, i as u32, id, *clip))
                .chain(
                    self.icon_quads
                        .iter()
                        .enumerate()
                        .map(|(j, (id, _, wm, clip))| (*wm, (n_img + j) as u32, id, *clip)),
                )
                .collect();
            // Stable so quads with the same watermark keep queue order — that
            // is the case for an icon drawn right on top of an image.
            ordered.sort_by_key(|(wm, ..)| *wm);
            // 클립 세그먼트. `clip_runs` 는 `(chrome 인덱스, 그 뒤로 유효한 클립)` 이
            // 오름차순으로 들어 있고, 여기서 그 경계마다 chrome 패스를 끊어
            // `set_scissor_rect` 를 갈아 끼운다.
            //
            // ⚠️ fail-open: 스택이 안 닫힌 채로 프레임이 끝났으면 run 을 통째로
            // 버린다. 최악이 「오늘과 똑같은 그림」이어야지 「화면 절반 실종」이면
            // 안 된다 — 클립 하나 안 닫은 실수가 앱을 못 쓰게 만들면 안 된다.
            let runs: &[(u32, Option<[u32; 4]>)] = if self.clip_stack.is_empty() {
                &self.clip_runs
            } else {
                eprintln!(
                    "[clip] 프레임이 끝났는데 클립 {}개가 안 닫혔다 — 이 프레임은 클립 없이 그린다",
                    self.clip_stack.len()
                );
                &[]
            };
            let (full_w, full_h) = (self.config.width, self.config.height);
            let mut drawn = 0u32;
            let mut run_i = 0usize;
            let mut cur: Option<[u32; 4]> = None;
            // 한 세그먼트를 그리기 직전마다 시저를 **매번** 정한다. 「바뀔 때만
            // 세운다」로 하면 한 번 세운 사각형이 되돌려지지 않아 그 뒤 전부가
            // 거기 갇히는 사고가 난다.
            macro_rules! scissor {
                ($c:expr) => {
                    match $c {
                        Some([x, y, w, h]) => pass.set_scissor_rect(x, y, w, h),
                        None => pass.set_scissor_rect(0, 0, full_w, full_h),
                    }
                };
            }
            macro_rules! flush_chrome {
                ($upto:expr) => {{
                    let upto: u32 = $upto;
                    while drawn < upto {
                        while run_i < runs.len() && runs[run_i].0 <= drawn {
                            cur = runs[run_i].1;
                            run_i += 1;
                        }
                        // 다음 경계까지가 이번 세그먼트. 위에서 `<= drawn` 을 다
                        // 소비했으므로 남은 run 의 인덱스는 반드시 `drawn` 보다 커
                        // 세그먼트가 비지 않는다(무한루프 방지).
                        let seg_end =
                            runs.get(run_i).map(|(i, _)| (*i).min(upto)).unwrap_or(upto);
                        // 빈 클립은 그리기 자체를 건너뛴다 — 0 크기 시저를 세우느니
                        // draw call 을 안 내는 편이 싸고 검증도 명확하다.
                        if !matches!(cur, Some([_, _, 0, _]) | Some([_, _, _, 0])) {
                            scissor!(cur);
                            self.pipeline
                                .draw_range(&mut pass, &self.bind_group, drawn, seg_end);
                        }
                        drawn = seg_end;
                    }
                }};
            }
            for (wm, buf_idx, id, qclip) in ordered {
                flush_chrome!(wm.min(instance_count as u32));
                if let Some(entry) = self.images.get(id) {
                    // 이미지·아이콘은 자기가 큐잉될 때의 클립을 들고 다닌다.
                    // chrome 인덱스로 되짚으면 같은 워터마크에 여러 run 이 붙었을 때
                    // 어느 쪽인지 못 가른다.
                    if !matches!(qclip, Some([_, _, 0, _]) | Some([_, _, _, 0])) {
                        scissor!(qclip);
                        self.image_pipeline
                            .draw_at(&mut pass, &entry.bind_group, buf_idx);
                    }
                }
            }
            flush_chrome!(instance_count as u32);
            if std::env::var_os("KASATERM_CLIP_DEBUG").is_some() {
                eprintln!(
                    "[clip] 인스턴스 {instance_count} · 세그먼트 {} · runs {:?}",
                    runs.len() + 1,
                    runs
                );
            }
        }
        overlay(&self.device, &self.queue, &mut encoder, &frame.texture, &view, self.config.format);
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
            // 크롭은 GPU 가 아니라 여기서 한다. copy_texture_to_buffer 의 origin 을
            // 옮기면 bytes_per_row 256 정렬을 잘린 폭 기준으로 다시 맞춰야 하는데,
            // 캡처는 드문 연산이라 전체를 읽고 잘라 내는 편이 훨씬 단순하다.
            let (cx, cy, cw, chh) = match self.capture_crop.take() {
                Some((x, y, cw, ch)) => (
                    x.min(w.saturating_sub(1)),
                    y.min(h.saturating_sub(1)),
                    cw.min(w.saturating_sub(x)).max(1),
                    ch.min(h.saturating_sub(y)).max(1),
                ),
                None => (0, 0, w, h),
            };
            let max_w = std::mem::take(&mut self.capture_max_w);
            {
                let data = buf.slice(..).get_mapped_range();
                let mut rgba = Vec::with_capacity((cw * chh * 4) as usize);
                for row in cy..cy + chh {
                    let s = (row * bpr + cx * 4) as usize;
                    let line = &data[s..s + (cw * 4) as usize];
                    for px in line.chunks_exact(4) {
                        if bgra {
                            rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
                        } else {
                            rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
                        }
                    }
                }
                let saved = if max_w > 0 && cw > max_w {
                    let nh = ((chh as u64 * max_w as u64) / cw as u64).max(1) as u32;
                    match image::RgbaImage::from_raw(cw, chh, rgba.clone()) {
                        Some(img) => {
                            let small = image::imageops::resize(
                                &img,
                                max_w,
                                nh,
                                image::imageops::FilterType::Lanczos3,
                            );
                            save_rgba_png(&path, small.as_raw(), max_w, nh).map(|()| (max_w, nh))
                        }
                        None => save_rgba_png(&path, &rgba, cw, chh).map(|()| (cw, chh)),
                    }
                } else {
                    save_rgba_png(&path, &rgba, cw, chh).map(|()| (cw, chh))
                };
                match saved {
                    Ok((ow, oh)) => eprintln!("[capture] gpu readback → {path} ({ow}x{oh})"),
                    Err(e) => eprintln!("[capture] gpu png failed: {e}"),
                }
            }
            buf.unmap();
        }
        Ok(instance_count)
    }
}

pub const ATLAS_SIZE: u32 = 2048;

/// Glyph supersampling factor for a render scale. Below Retina the logical
/// pixel size is too small to resolve a coverage mask cleanly, so bake at 2x
/// and let the Linear sampler downsample; Retina already has the pixels.
/// Must be re-evaluated on every DPI change, not just at startup — see
/// `Renderer::set_scale`.
pub fn oversample_for(scale: f32) -> u32 {
    if scale < 2.0 { 2 } else { 1 }
}

/// 칸 폭을 주 폰트의 자연 advance 보다 좁히는 비율. `KASATERM_CELL_TIGHTEN`.
///
/// 자간은 "칸 폭 − 글자 폭"이라, 글자를 안 건드리고 좁히려면 여기밖에 없다.
/// 한글은 두 칸을 쓰므로 칸을 1px 줄이면 한글 사이는 2px, 라틴은 1px 줄어
/// **한글이 두 배 속도로** 좁아진다 — 라틴 자간이 지나치게 붙기 전에 한글이
/// 먼저 제자리를 찾는다는 뜻이다. 칸이 글자보다 좁아지면 글자가 옆 칸을
/// 침범하므로 0.85 아래로는 못 내려간다.
pub fn cell_tighten() -> f32 {
    use std::sync::OnceLock;
    static T: OnceLock<f32> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("KASATERM_CELL_TIGHTEN")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_CELL_TIGHTEN)
            .clamp(0.85, 1.0)
    })
}

/// 0.87 = JetBrains Mono 의 자연 칸(0.6em)에서 12% 조임. 눈으로 골랐다 —
/// 16px 칸에선 한글이 글자 단위로 흩어져 읽히고, 14px 에서 비로소 단어로
/// 뭉친다(한글 사이 8px→4px). 라틴은 4px→2px 인데 JetBrains 가 넓은 얼굴이라
/// 여전히 답답하지 않다. 글리프 잉크는 24px 로 셋 다 같다 — 칸만 좁아진다.
pub const DEFAULT_CELL_TIGHTEN: f32 = 0.87;

/// 칸 폭 계산의 **단 하나의 자리**. 부팅과 폰트 크기 변경이 서로 다른 식을
/// 쓰면 크기를 바꾸는 순간 조임이 풀린다(실제로 그랬다).
pub fn cell_w_for(shaper: &mut Shaper, size_px: f32) -> f32 {
    (shaper.cell_advance(size_px) * cell_tighten()).ceil().max(1.0)
}

/// A decoded image uploaded to its own wgpu texture. Kept alive (texture +
/// view) for as long as the pane shows it, since the bind group borrows the
/// view. Keyed by pane id in `GpuRenderer::images`.
pub struct ImageEntry {
    pub _texture: wgpu::Texture,
    pub _view: wgpu::TextureView,
    pub bind_group: wgpu::BindGroup,
    pub w: u32,
    pub h: u32,
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
    pub links: Vec<crate::CellSpan>,
    /// 이 pane 의 "기본 전경색" — tmux `window-style fg=<색>` 등가 pane 틴트.
    /// 테마 default fg 를 쓰는 셀만 이 색으로 풀리고 명시 색(ANSI/truecolor)은
    /// 그대로다. 무틴트 pane 은 `cells::default_fg()` 를 넣는다(셀당 추가 분기 0).
    pub default_fg: [u8; 4],
    /// 거울 pane 이면 원본 기기의 팔레트. 원본 배경·글자색과 같은 명시색을 보는 쪽
    /// 기본색으로 되돌린다(`cells::adapt_to_viewer`). 로컬 pane 은 None.
    pub source: Option<crate::palette::SourcePalette>,
}

/// `KASATERM_CELL_PROBE=<문자>` — 그 문자를 담은 행이 **글리프 패스에 도달할 때**
/// 그 행의 셀 속성을 한 번 찍는다.
///
/// 왜 여기인가: "셀에 써넣었는데 화면에 없다"를 진단할 자리는 쓴 쪽이 아니라 **받는
/// 쪽**이다. 쓴 쪽 로그는 데이터가 들어갔다는 것만 말하고, 그 뒤 어디서 떨어졌는지는
/// 침묵한다(2026-08-05: 인레이가 자기 결과 문자열을 뱉는데도 픽셀엔 없었고, 그
/// 사이 구간이 통째로 안 보였다). 이 프로브는 `hidden`(SGR 8 — 글리프만 생략하고
/// 텍스트 추출엔 남아 하네스가 "그렸다"로 읽는다)·`bold`·`fg`·`bg`·`dim` 을 나란히
/// 찍으므로, 안 보이는 셀과 보이는 이웃을 **같은 줄에서** 대조할 수 있다.
///
/// 행당 한 번만(프레임마다 반복하면 로그가 흐른다).
pub fn probe_cell_row(r: usize, row: &[kasa_screen::Cell], dim: bool, font_scale: f32) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static WANT: OnceLock<Option<String>> = OnceLock::new();
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let Some(want) = WANT.get_or_init(|| std::env::var("KASATERM_CELL_PROBE").ok()).as_deref()
    else {
        return;
    };
    if want.is_empty() || !row.iter().any(|c| want.contains(c.ch)) {
        return;
    }
    let text: String = row.iter().map(|c| c.ch).collect();
    // 호출 순번을 키와 출력에 함께 싣는다 — 한 프레임에 `draw_cells` 가 두 번
    // 불리고 나중 것이 앞의 것을 덮으면, 순번 없이는 그 사실이 로그에서 안 보인다.
    let pass = DRAW_CELLS_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    let key = format!("{pass}:{r}:{}", text.trim_end());
    if !SEEN.get_or_init(Default::default).lock().is_ok_and(|mut s| s.insert(key)) {
        return;
    }
    eprintln!(
        "[cell-probe] call#{pass} row {r} dim={dim} font_scale={font_scale} → {:?}",
        text.trim_end()
    );
    for (col, c) in row.iter().enumerate() {
        if matches!(c.ch, ' ' | '\0') {
            continue;
        }
        eprintln!(
            "  col {col:>3} {:?} bold={} hidden={} dim={} inverse={} fg={:?} bg={:?}",
            c.ch, c.bold, c.hidden, c.dim, c.inverse, c.fg, c.bg
        );
    }
}

/// `draw_cells` 진입 횟수 — `probe_cell_row` 가 "몇 번째 호출의 그리드인가"를 찍는다.
pub static DRAW_CELLS_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Encode RGBA8 pixels to a PNG file. Used by the GPU self-capture path. Uses
/// the `image` crate (available on every target; `png` is Windows-only here).
pub fn save_rgba_png(path: &str, rgba: &[u8], w: u32, h: u32) -> std::io::Result<()> {
    image::save_buffer(path, rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Place a **single-column** glyph, keeping its natural size whenever the
/// columns beside it leave room. `room_left`/`room_right` are how many px of
/// blank the caller is willing to lend. Returns `[x, y, w, h]` in physical px.
///
/// East Asian **Ambiguous** codepoints (①②③, Ⅰ Ⅱ, ⓐ …) are counted as one
/// column by the grid — `unicode_width` says 1 and alacritty agrees — but no
/// monospace face in the chain carries them, so the fallback that does is a CJK
/// font that rasters them **full-width** (measured: D2Coding gives `①` exactly
/// 2× the advance of `A`). Drawn unguarded they ran over their neighbour:
/// `⑤브릿지` had the ⑤ sitting on top of the 브.
///
/// Widening the grid instead (adding the block to `is_wide_char`) is the wrong
/// half of the fix — the parser still advances one column, so every glyph after
/// it would sit a cell off. Shrinking to one cell is the other wrong fix: a lone
/// stunted ⑤ beside full-size ①②③④ reads worse than the overlap did.
///
/// So the glyph is *slid* rather than resized. It holds its natural position
/// until it would spill onto a real character, then backs off into whatever
/// blank the caller lent — `판정 ⑤브릿지` borrows the space to its left, `① ② ③`
/// the one to its right. Only a glyph boxed in on both sides scales, and it
/// keeps its aspect ratio. Glyphs that fit their own cell take the untouched
/// bearing path, so ASCII and every well-behaved monospace glyph render exactly
/// as before.
pub fn fit_cell_glyph(
    entry: &AtlasEntry,
    cell_x: f32,
    baseline_y: f32,
    cell_w: f32,
    room_left: f32,
    room_right: f32,
) -> [f32; 4] {
    let gw = entry.px_w as f32;
    let y = baseline_y - entry.bearing_y as f32;
    if gw <= cell_w {
        return [cell_x + entry.bearing_x as f32, y, gw, entry.px_h as f32];
    }
    let lo = cell_x - room_left;
    let hi = cell_x + cell_w + room_right;
    if gw <= hi - lo {
        let x = (cell_x + entry.bearing_x as f32).min(hi - gw).max(lo);
        return [x, y, gw, entry.px_h as f32];
    }
    // 양쪽이 막혀도 절반까지 쪼그라들게 두지는 않는다. 자리에 정확히 맞춰
    // 줄이면 `①②잠금` 의 ② 가 아래첨자처럼 혼자 작아지는데, 그 모습은
    // 겹침보다 못생겼다는 판정이 이미 났다("크기비율그대로 수정안돼?").
    // 하한에서 남는 넘침은 빌린 자리 한가운데를 기준으로 좌우 반씩 흘린다 —
    // monospace 이웃은 잉크가 칸을 꽉 채우지 않아 그 여백이 대부분을 먹고,
    // 한쪽으로 몰아줄 때처럼 이웃 글자를 파고들지 않는다.
    // 상한을 비율이 아니라 **넘침 폭**으로 건다: 비율 하한(예: 0.78배)을
    // 두면 3칸짜리 폴백 글리프가 2.3칸으로 앉아 이웃을 통째로 덮는다.
    // 실측 — 원문자 잉크는 1.79칸이라 이 상한에서 0.78배로 앉는다(절반보다
    // 훨씬 크다). 병적으로 넓은 글리프만 계속 크게 줄어든다.
    let gwf = gw.min(hi - lo + cell_w * 0.4);
    let fit = gwf / gw;
    [
        (lo + hi - gwf) * 0.5,
        baseline_y - entry.bearing_y as f32 * fit,
        gwf,
        entry.px_h as f32 * fit,
    ]
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
/// 편집기 격자에서 이 텍스트가 차지하는 칸 수. 캐럿 x·선택 밴드·괄호 강조·
/// 들여쓰기 가이드가 전부 이 하나로 좌표를 얻으므로, 그리는 쪽
/// (`draw_editor_cells`)과 칸 세는 규칙이 갈릴 수 없다.
pub fn cell_cols(text: &str) -> usize {
    text.chars()
        .map(|c| 1 + usize::from(is_wide_char(c)))
        .sum()
}

pub fn is_wide_char(ch: char) -> bool {
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

pub fn is_icon_codepoint(cp: u32) -> bool {
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
pub fn text_render_knobs() -> (f32, f32, f32) {
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

/// The native fit corresponds to the cell-slot fit/anchor contract sent to viewers.
pub fn fit_terminal_art(
    rect: (f32, f32, f32, f32),
    image: (u32, u32),
    scale: f32,
    foreground: bool,
) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = rect;
    let (iw, ih) = (image.0.max(1) as f32, image.1.max(1) as f32);
    let fit = (w / iw).min(h / ih);
    let fit = if foreground {
        fit
    } else {
        fit.min(1.0 / scale.max(0.01))
    };
    let (dw, dh) = (iw * fit, ih * fit);
    (
        x + (w - dw) * 0.5,
        y + (h - dh) * if foreground { 1.0 } else { 0.5 },
        dw,
        dh,
    )
}
