//! kasaterm — embeddable terminal cell renderer.
//!
//! Caller supplies `wgpu::Device`, `wgpu::Queue`, and a scoped
//! `wgpu::RenderPass`; kasaterm draws terminal cells, box-drawing quads,
//! and glyphs into that pass. No window, no event loop, no UI framework
//! lock-in — same shape as libghostty for cmux but written for Rust.
//!
//! Pipeline diagram:
//!
//! ```text
//!     tmux-bridge ──► (cells, layout)
//!                              │
//!                              ▼
//!                   Frame (per-frame snapshot)
//!                              │  caller calls prepare()/draw()
//!                              ▼
//!                   Pipeline (wgpu state)
//!                              ├── glyph atlas (rgba8, cosmic-text SwashCache)
//!                              ├── unified instance pipeline
//!                              │    one draw call covers:
//!                              │      • cell backgrounds
//!                              │      • box-drawing quads (block_rects)
//!                              │      • glyph quads (atlas mask × color)
//!                              ▼
//!                       wgpu::RenderPass (caller-owned)
//! ```
//!
//! cosmic-text 0.18 does shaping + SwashCache glyph rasterisation.
//! 2048×2048 RGBA atlas covers a long Claude session without overflow.

#[cfg(feature = "iced")]
mod iced_glue;

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashImage,
};
use wgpu::util::DeviceExt;

/// Plain rect — caller-friendly (no UI-framework type dependency).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Internal shim — the prepare body was written against an iced::Rectangle-
/// shaped struct (width/height). We keep the field shape so the merge from
/// the host crate stayed a one-liner; future cleanup can inline this.
struct Bounds {
    width: f32,
    height: f32,
}

// We bind directly to tmux-bridge's cell/color types — no need for an
// adapter layer for the spike. If tmux-bridge ever moves out-of-tree
// (different transport), re-introduce a Grid*-style indirection here.
use tmux_bridge::{Cell as GridCell, Color as GridColor};

// === Constants ============================================================

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 18.0;
/// 2048² gives ~4M pixels — enough to hold every distinct (glyph, size,
/// scale) tuple a long-running Claude session has thrown at it so far.
/// 1024² ran out within minutes once Retina baking doubled each glyph's
/// pixel footprint; statusline emoji then started flickering between
/// sizes as the alloc cursor wrapped onto smaller tiles.
const ATLAS_DIM: u32 = 2048;

/// Pane border colour for inactive panes — slightly brighter than the
/// terminal bg so the seam reads as chrome rather than glitch.
const PANE_BORDER_DIM: [f32; 4] = [0.235, 0.255, 0.290, 1.0];
/// Active pane gets the app accent (matches `main::ACCENT`).
const PANE_BORDER_ACTIVE: [f32; 4] = [0x5a as f32 / 255.0, 0x82 as f32 / 255.0, 0xf3 as f32 / 255.0, 1.0];
/// Some safety margin between glyphs in the atlas so bilinear bleed never
/// pulls in a neighbour. Cosmic returns tight bitmaps already, but a row of
/// transparent pixels is cheap.
const ATLAS_PAD: u32 = 1;

// === Primitive instance =================================================

/// One drawable rectangle. Either a flat-coloured quad (bg cell, box rect)
/// or a glyph sample (atlas mask × fg colour).
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    /// Widget-local pixel rect (x, y, w, h). Origin is widget top-left.
    pub rect: [f32; 4],
    /// Linear RGBA (0..1). For glyphs this is the foreground colour; the
    /// atlas mask supplies alpha.
    pub color: [f32; 4],
    /// Atlas UV (u0, v0, u1, v1). Ignored when mode == 0.
    pub uv: [f32; 4],
    /// 0 = solid fill, 1 = glyph (atlas .r as alpha).
    pub mode: u32,
    pub _pad: [u32; 3],
}

unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

// === Primitive (per-frame snapshot) ====================================

/// One pane's data inside a `TerminalPrimitive`. `rect` is widget-local
/// pixel space (origin = widget top-left). `is_active` raises an accent
/// 1px border so the user can see which pane has keyboard focus.
#[derive(Debug, Clone)]
pub struct PaneRender {
    pub rect: [f32; 4],
    pub cells: Arc<Vec<Vec<GridCell>>>,
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub is_active: bool,
}

/// Pane snapshot passed from app state into the shader program; used to
/// build `PaneRender`s after the layout walk picks rects.
#[derive(Debug, Clone)]
pub struct PaneSnapshot {
    pub cells: Arc<Vec<Vec<GridCell>>>,
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
}

/// The data shape iced hands to its renderer once per frame. Owns enough
/// to rebuild instance buffers; the heavy state (atlas texture, swash
/// cache, etc.) lives in `TerminalPipeline`.
#[derive(Debug, Clone)]
pub struct TerminalPrimitive {
    pub panes: Vec<PaneRender>,
    pub bg_color: [f32; 4],
    pub fg_color: [f32; 4],
    /// Cell dimensions of the single layout-grid unit (one tmux "cell").
    /// Each pane scales its own cols/rows to match these.
    pub cell_w: f32,
    pub cell_h: f32,
    pub font_size: f32,
    /// Widget bounds (used as the shader viewport, since the iced render
    /// pass is already scoped to this rect).
    pub widget_bounds: [f32; 2],
    /// Live IME composition string. Painted in the active pane's cursor
    /// cell with an accent underline so the user sees what's being
    /// composed (e.g. "ㅇ → 아 → 안" for Korean Hangul).
    pub preedit: String,
}

// iced glue lives in the host crate (see tmuxify::cell_shader). kasaterm
// itself is framework-agnostic — callers invoke TerminalPipeline::prepare
// and TerminalPipeline::draw directly with their own wgpu state.

// === Atlas =============================================================

/// Tracks where each rasterised glyph lives in the RGBA atlas. Keyed by
/// cosmic-text's `CacheKey` which already folds (font_id, glyph_id, size,
/// subpixel) into a single struct.
struct AtlasEntry {
    uv: [f32; 4],
    /// Logical placement of the glyph relative to its baseline, in pixels.
    placement: cosmic_text::Placement,
    /// Whether the cosmic bitmap was a Color (subpixel) bitmap. If so the
    /// fg colour is ignored at draw time (we tint via vertex colour only
    /// for mask glyphs).
    is_color: bool,
}

struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Row-allocator state. We pack glyphs left-to-right into a strip,
    /// bumping `row_y` once `cursor_x` overflows.
    cursor_x: u32,
    row_y: u32,
    row_h: u32,
    entries: HashMap<CacheKey, AtlasEntry>,
}

impl Atlas {
    fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tmuxify glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_DIM,
                height: ATLAS_DIM,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            cursor_x: 0,
            row_y: 0,
            row_h: 0,
            entries: HashMap::new(),
        }
    }

    /// Reserve a (w × h) tile and return its UV box. None if the atlas is
    /// full — in that case the caller skips this glyph for the frame.
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32, [f32; 4])> {
        if w == 0 || h == 0 {
            return None;
        }
        if w > ATLAS_DIM || h > ATLAS_DIM {
            return None;
        }
        if self.cursor_x + w + ATLAS_PAD > ATLAS_DIM {
            self.row_y += self.row_h + ATLAS_PAD;
            self.cursor_x = 0;
            self.row_h = 0;
        }
        if self.row_y + h > ATLAS_DIM {
            return None;
        }
        let x = self.cursor_x;
        let y = self.row_y;
        self.cursor_x += w + ATLAS_PAD;
        self.row_h = self.row_h.max(h);
        let dim = ATLAS_DIM as f32;
        Some((
            x,
            y,
            [
                x as f32 / dim,
                y as f32 / dim,
                (x + w) as f32 / dim,
                (y + h) as f32 / dim,
            ],
        ))
    }
}

// === Pipeline (lives across frames, owns all wgpu state) ================

pub struct TerminalPipeline {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,
    instance_buf: wgpu::Buffer,
    instance_capacity: u64,
    instance_count: u32,

    atlas: Atlas,
    atlas_bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,

    font_system: FontSystem,
    swash: SwashCache,

    /// Memo key. We rebuild instance buffers only when something
    /// observable changed — cell grid pointer, cursor pos, or
    /// widget bounds.
    last_cells_ptr: usize,
    last_cursor: (u16, u16, bool),
    /// [width, height, scale_factor] — scale flips on monitor changes
    /// (Retina ↔ external) so it has to be part of the cache key, or
    /// the atlas keeps serving the wrong-resolution glyphs.
    last_bounds: [f32; 3],
}

impl std::fmt::Debug for TerminalPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPipeline")
            .field("instance_count", &self.instance_count)
            .finish()
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// Widget bounds size in pixels (the iced render pass viewport).
    viewport: [f32; 2],
    _pad: [f32; 2],
}

impl TerminalPipeline {
    /// Build the pipeline against the caller's wgpu device and the target
    /// `format` of the render pass kasaterm will eventually draw into.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> Self {
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tmuxify cell shader"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tmuxify uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let atlas_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tmuxify atlas layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tmuxify pipeline layout"),
            bind_group_layouts: &[&uniform_layout, &atlas_bind_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tmuxify cell pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4,  // rect
                        1 => Float32x4,  // color
                        2 => Float32x4,  // uv
                        3 => Uint32,     // mode
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tmuxify uniform buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tmuxify uniform bind"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tmuxify atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let atlas = Atlas::new(device);
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tmuxify atlas bind"),
            layout: &atlas_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let initial_cap: u64 = 1024;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tmuxify instance buffer"),
            size: initial_cap * std::mem::size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            uniform_buf,
            uniform_bind_group,
            atlas_bind_group,
            instance_buf,
            instance_capacity: initial_cap,
            instance_count: 0,
            atlas,
            atlas_bind_layout,
            sampler,
            font_system: {
                let mut fs = FontSystem::new();
                // D2Coding (NAVER, OFL) — base monospace face that
                // matches the user's preferred terminal font. Bundled
                // so Windows / Linux builds don't need the user to
                // install it. The Nerd Font variant comes after as a
                // fallback for icon ranges D2Coding doesn't cover.
                fs.db_mut().load_font_source(cosmic_text::fontdb::Source::Binary(
                    std::sync::Arc::new(include_bytes!(
                        "../assets/D2Coding.ttc"
                    )),
                ));
                fs.db_mut().load_font_source(cosmic_text::fontdb::Source::Binary(
                    std::sync::Arc::new(include_bytes!(
                        "../assets/D2CodingLigatureNerdFontMono-Regular.ttf"
                    )),
                ));
                fs
            },
            swash: SwashCache::new(),
            last_cells_ptr: 0,
            last_cursor: (u16::MAX, u16::MAX, false),
            last_bounds: [0.0, 0.0, 0.0],
        }
    }
}

impl TerminalPipeline {
    /// Build the instance buffer for `frame` against the caller-supplied
    /// viewport `bounds` (in logical pixels) at the given `scale_factor`.
    ///
    /// Memoised: if the frame's pane snapshots, cursor, preedit, scale,
    /// and bounds all match the previous call, the GPU buffer is left
    /// untouched.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: Rect,
        prim: &TerminalPrimitive,
        scale: f32,
    ) {
        // Bridge from the public Rect type to the internal logic that
        // was originally written against iced::Rectangle. Keep the
        // shape identical so the body below doesn't need to change.
        let bounds = &Bounds { width: bounds.width, height: bounds.height };
        // Memoise — sum every pane's Arc identity into a single hash so a
        // single-pane resize, a cursor blink, or any pane's cells getting
        // a fresh Arc all invalidate the buffer. Mix scale so DPI changes
        // (window dragged to / from external monitor) invalidate too.
        let mut hash: u64 = 0xcbf29ce484222325;
        for p in &prim.panes {
            for w in [
                Arc::as_ptr(&p.cells) as u64,
                ((p.cursor_row as u64) << 32) | (p.cursor_col as u64),
                p.cursor_visible as u64,
                p.is_active as u64,
                p.rect[0].to_bits() as u64
                    ^ (p.rect[1].to_bits() as u64).rotate_left(16)
                    ^ (p.rect[2].to_bits() as u64).rotate_left(32)
                    ^ (p.rect[3].to_bits() as u64).rotate_left(48),
            ] {
                hash ^= w;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        hash ^= scale.to_bits() as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        // Mix preedit so live IME composition triggers a redraw on
        // every jamo change.
        for b in prim.preedit.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let new_bounds = [bounds.width, bounds.height, scale];
        if hash == self.last_cells_ptr as u64
            && new_bounds == self.last_bounds
            && self.instance_count > 0
        {
            return;
        }
        self.last_cells_ptr = hash as usize;
        self.last_bounds = new_bounds;

        // Uniform: viewport size (= widget bounds).
        let uniforms = Uniforms {
            viewport: [bounds.width.max(1.0), bounds.height.max(1.0)],
            _pad: [0.0, 0.0],
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let mut instances: Vec<Instance> = Vec::new();

        // 1. Full-widget background fill. Each pane paints its own bg
        //    on top, so this is just so any gutter pixel (between panes
        //    or off the layout grid) stays the terminal bg colour.
        instances.push(Instance {
            rect: [0.0, 0.0, bounds.width, bounds.height],
            color: prim.bg_color,
            uv: [0.0, 0.0, 0.0, 0.0],
            mode: 0,
            _pad: [0; 3],
        });

        // 2. Per-pane rendering. Each pane scales its own cells to its
        //    rect, baking glyphs at physical resolution via `scale`.
        for pane in &prim.panes {
            self.emit_pane(&mut instances, queue, prim, pane, scale);
        }

        // 3. Pane borders + active highlight. Drawn after cells so they
        //    sit on top of any cell content at the seam.
        if prim.panes.len() > 1 {
            for pane in &prim.panes {
                let border = if pane.is_active {
                    PANE_BORDER_ACTIVE
                } else {
                    PANE_BORDER_DIM
                };
                let width = if pane.is_active { 1.5 } else { 1.0 };
                push_pane_border(&mut instances, pane.rect, border, width);
            }
        } else if let Some(pane) = prim.panes.first() {
            // Single pane: draw the active highlight only when there's
            // a real layout (i.e. user has interacted). Without one,
            // skip the border to keep the chrome clean.
            if pane.is_active && self.last_bounds[0] > 0.0 {
                // intentionally skipped — single pane needs no chrome
                let _ = PANE_BORDER_ACTIVE;
            }
        }

        self.instance_count = instances.len() as u32;
        if instances.is_empty() {
            return;
        }

        let needed = instances.len() as u64;
        if needed > self.instance_capacity {
            // Double until it fits — amortised constant growth.
            let mut cap = self.instance_capacity.max(1);
            while cap < needed {
                cap *= 2;
            }
            self.instance_capacity = cap;
            self.instance_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tmuxify instance buffer (grown)"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
        } else {
            queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&instances));
        }
    }

    /// Emit instances for one pane. The pane's `rect` is widget-local
    /// pixel coordinates; cells are scaled to fit, so an individual
    /// pane's cell_w/h is `rect.w / pane.cols`. Box-drawing chars and
    /// glyph quads use those local metrics — pane sizes can be smaller
    /// than the global cell_w (a vertical split halves the column
    /// pitch) without distorting glyphs.
    fn emit_pane(
        &mut self,
        instances: &mut Vec<Instance>,
        queue: &wgpu::Queue,
        prim: &TerminalPrimitive,
        pane: &PaneRender,
        scale: f32,
    ) {
        let [px, py, pw, ph] = pane.rect;
        if pw <= 0.0 || ph <= 0.0 {
            return;
        }
        // Pane bg fill — keeps the gutter bg from bleeding through
        // when a pane has its own theme colour.
        instances.push(Instance {
            rect: [px, py, pw, ph],
            color: prim.bg_color,
            uv: [0.0, 0.0, 0.0, 0.0],
            mode: 0,
            _pad: [0; 3],
        });

        let cell_w = if pane.cols > 0 { pw / pane.cols as f32 } else { prim.cell_w };
        let cell_h = if pane.rows > 0 { ph / pane.rows as f32 } else { prim.cell_h };
        let font_size = (cell_h * 0.78).max(8.0);

        for (row_idx, row) in pane.cells.iter().enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let x = px + col_idx as f32 * cell_w;
                let y = py + row_idx as f32 * cell_h;

                let mut fg = palette(&cell.fg, prim.fg_color);
                let mut bg = palette(&cell.bg, prim.bg_color);
                if cell.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let is_cursor = pane.cursor_visible
                    && row_idx as u16 == pane.cursor_row
                    && col_idx as u16 == pane.cursor_col;
                if is_cursor {
                    std::mem::swap(&mut fg, &mut bg);
                }

                if bg != prim.bg_color {
                    instances.push(Instance {
                        rect: [x, y, cell_w, cell_h],
                        color: bg,
                        uv: [0.0, 0.0, 0.0, 0.0],
                        mode: 0,
                        _pad: [0; 3],
                    });
                }

                let first_char = cell.ch.chars().next();
                let rects = first_char.map(block_rects).unwrap_or(&[]);
                if !rects.is_empty() {
                    for r in rects {
                        instances.push(Instance {
                            rect: [
                                x + r[0] * cell_w,
                                y + r[1] * cell_h,
                                r[2] * cell_w,
                                r[3] * cell_h,
                            ],
                            color: fg,
                            uv: [0.0, 0.0, 0.0, 0.0],
                            mode: 0,
                            _pad: [0; 3],
                        });
                    }
                    continue;
                }

                if cell.ch == " " || cell.ch.is_empty() {
                    continue;
                }

                let glyph_data = shape_one_glyph(
                    &mut self.font_system,
                    &cell.ch,
                    font_size,
                    cell.bold,
                    cell.italic,
                    scale,
                );

                for (cache_key, x_off, y_off) in glyph_data {
                    let entry = ensure_glyph(
                        &mut self.atlas,
                        &mut self.font_system,
                        &mut self.swash,
                        queue,
                        cache_key,
                    );
                    let Some(entry) = entry else { continue };
                    // Atlas glyph baked at physical resolution
                    // (font_size * scale). Divide placement back to
                    // logical for iced's logical→physical projection.
                    let baseline = y + cell_h * 0.78;
                    let gx = x + x_off + (entry.placement.left as f32) / scale;
                    let gy = baseline - (entry.placement.top as f32) / scale + y_off;
                    let gw = entry.placement.width as f32 / scale;
                    let gh = entry.placement.height as f32 / scale;
                    if gw <= 0.0 || gh <= 0.0 {
                        continue;
                    }
                    instances.push(Instance {
                        rect: [gx, gy, gw, gh],
                        color: if entry.is_color { [1.0, 1.0, 1.0, 1.0] } else { fg },
                        uv: entry.uv,
                        mode: 1,
                        _pad: [0; 3],
                    });
                }
            }
        }

        // Live IME preedit overlay — only on the active pane. Painted
        // on top of cells starting at the cursor cell, with an accent
        // underline so the user sees mid-composition (Korean jamo).
        if pane.is_active && !prim.preedit.is_empty() {
            let cur_x = px + (pane.cursor_col as f32) * cell_w;
            let cur_y = py + (pane.cursor_row as f32) * cell_h;
            // Solid bg under preedit so we don't mix with terminal text.
            let preedit_bg = [
                prim.bg_color[0] * 1.4 + 0.05,
                prim.bg_color[1] * 1.4 + 0.05,
                prim.bg_color[2] * 1.4 + 0.05,
                1.0,
            ];
            let preedit_w = cell_w
                * (prim.preedit.chars().count().max(1)) as f32
                * 1.6; // CJK is wide — give it room
            instances.push(Instance {
                rect: [cur_x, cur_y, preedit_w, cell_h],
                color: preedit_bg,
                uv: [0.0, 0.0, 0.0, 0.0],
                mode: 0,
                _pad: [0; 3],
            });
            // Shape the preedit text as one run and emit each glyph.
            let glyphs = shape_one_glyph(
                &mut self.font_system,
                &prim.preedit,
                font_size,
                false,
                false,
                scale,
            );
            let mut pen_x = cur_x;
            for (cache_key, x_off, y_off) in glyphs {
                let entry = ensure_glyph(
                    &mut self.atlas,
                    &mut self.font_system,
                    &mut self.swash,
                    queue,
                    cache_key,
                );
                let Some(entry) = entry else { continue };
                let baseline = cur_y + cell_h * 0.78;
                let gx = pen_x + x_off + (entry.placement.left as f32) / scale;
                let gy = baseline - (entry.placement.top as f32) / scale + y_off;
                let gw = entry.placement.width as f32 / scale;
                let gh = entry.placement.height as f32 / scale;
                if gw > 0.0 && gh > 0.0 {
                    instances.push(Instance {
                        rect: [gx, gy, gw, gh],
                        color: if entry.is_color {
                            [1.0, 1.0, 1.0, 1.0]
                        } else {
                            prim.fg_color
                        },
                        uv: entry.uv,
                        mode: 1,
                        _pad: [0; 3],
                    });
                }
                pen_x += gw.max(cell_w * 0.6);
            }
            // 2px accent underline below the preedit run.
            instances.push(Instance {
                rect: [cur_x, cur_y + cell_h - 2.0, preedit_w, 2.0],
                color: PANE_BORDER_ACTIVE,
                uv: [0.0, 0.0, 0.0, 0.0],
                mode: 0,
                _pad: [0; 3],
            });
        }
    }

    /// Issue the prepared instance batch into the caller's render pass.
    /// The pass's viewport / scissor must already be scoped to the
    /// `bounds` rect that was passed to `prepare`, otherwise the cells
    /// will draw at the wrong position.
    pub fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        if self.instance_count == 0 {
            return;
        }
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        render_pass.set_bind_group(1, &self.atlas_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.instance_buf.slice(..));
        render_pass.draw(0..6, 0..self.instance_count);
    }
}

// === Pane chrome =========================================================

/// Push four 1-instance border strips around `rect`. Used by both the
/// inactive-pane gutter (dim grey) and the active-pane highlight (accent).
fn push_pane_border(instances: &mut Vec<Instance>, rect: [f32; 4], color: [f32; 4], w: f32) {
    let [x, y, pw, ph] = rect;
    // top
    instances.push(Instance {
        rect: [x, y, pw, w],
        color,
        uv: [0.0; 4],
        mode: 0,
        _pad: [0; 3],
    });
    // bottom
    instances.push(Instance {
        rect: [x, y + ph - w, pw, w],
        color,
        uv: [0.0; 4],
        mode: 0,
        _pad: [0; 3],
    });
    // left
    instances.push(Instance {
        rect: [x, y, w, ph],
        color,
        uv: [0.0; 4],
        mode: 0,
        _pad: [0; 3],
    });
    // right
    instances.push(Instance {
        rect: [x + pw - w, y, w, ph],
        color,
        uv: [0.0; 4],
        mode: 0,
        _pad: [0; 3],
    });
}

// === Glyph rasterisation ================================================

/// Shape a single terminal cell's text. Returns (cache_key, x_offset, y_offset).
/// For ascii/CJK that's exactly one glyph; ligatures or emoji may emit several.
fn shape_one_glyph(
    fs: &mut FontSystem,
    text: &str,
    font_size: f32,
    bold: bool,
    italic: bool,
    scale: f32,
) -> Vec<(CacheKey, f32, f32)> {
    let mut buf = Buffer::new(fs, Metrics::new(font_size, font_size * 1.25));
    buf.set_size(fs, Some(font_size * 4.0), Some(font_size * 2.0));
    // Font fallback like alacritty / iTerm: D2Coding is the everyday face
    // but doesn't carry Nerd Font's Private Use Area icons (powerline /
    // git / file-type glyphs that claude / starship / lsd / etc. emit).
    // Pick the bundled Nerd Font for PUA codepoints, D2Coding otherwise.
    // Single-cell granularity is enough — terminal cells are atomic so we
    // never need per-char fallback mid-string.
    let needs_nerd = text
        .chars()
        .any(|c| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD));
    let family = if needs_nerd {
        Family::Name("D2CodingLigature Nerd Font Mono")
    } else {
        Family::Name("D2Coding")
    };
    let mut attrs = Attrs::new()
        .family(family)
        .stretch(cosmic_text::Stretch::Normal);
    let _ = Family::Monospace;
    if bold {
        attrs = attrs.weight(cosmic_text::Weight::BOLD);
    }
    if italic {
        attrs = attrs.style(cosmic_text::Style::Italic);
    }
    buf.set_text(fs, text, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);

    let mut out = Vec::new();
    if let Some(run) = buf.layout_runs().next() {
        for g in run.glyphs {
            // Bake at physical resolution so the atlas holds 2× detail
            // on Retina; pipeline divides placement back to logical for
            // iced's projection. CacheKey already folds the physical
            // font size, so two monitors with different scales coexist.
            let physical = g.physical((0.0, 0.0), scale);
            out.push((physical.cache_key, g.x, g.y));
        }
    }
    out
}

fn ensure_glyph<'a>(
    atlas: &'a mut Atlas,
    fs: &mut FontSystem,
    swash: &mut SwashCache,
    queue: &wgpu::Queue,
    key: CacheKey,
) -> Option<&'a AtlasEntry> {
    if atlas.entries.contains_key(&key) {
        return atlas.entries.get(&key);
    }
    let img: SwashImage = swash.get_image_uncached(fs, key)?;
    let w = img.placement.width;
    let h = img.placement.height;
    if w == 0 || h == 0 {
        // Whitespace-shaped run; nothing to draw, but cache an empty entry
        // so we don't keep re-rasterising.
        atlas.entries.insert(
            key,
            AtlasEntry {
                uv: [0.0, 0.0, 0.0, 0.0],
                placement: img.placement,
                is_color: false,
            },
        );
        return atlas.entries.get(&key);
    }
    let Some((x, y, uv)) = atlas.alloc(w, h) else {
        // Atlas full. Cache a sentinel entry so we don't re-rasterise
        // this glyph (via SwashCache) on every frame — that's the real
        // CPU killer on overflow, not the missing draw itself. A future
        // round can swap in an LRU shelf-bin packer; for the spike, a
        // bigger atlas + this sentinel are enough.
        eprintln!(
            "[tmuxify] glyph atlas full (key={:?} size={}x{}) — skipping",
            key, w, h
        );
        atlas.entries.insert(
            key,
            AtlasEntry {
                uv: [0.0, 0.0, 0.0, 0.0],
                placement: img.placement,
                is_color: false,
            },
        );
        return atlas.entries.get(&key);
    };

    // Expand to RGBA8. Cosmic's `Content::Mask` is 8-bit alpha — we store
    // (255,255,255,a) so the fragment shader can tint by colour. Colour
    // bitmaps (emoji) come pre-RGBA.
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    match img.content {
        cosmic_text::SwashContent::Mask => {
            for i in 0..(w * h) as usize {
                let a = img.data[i];
                rgba[i * 4] = 255;
                rgba[i * 4 + 1] = 255;
                rgba[i * 4 + 2] = 255;
                rgba[i * 4 + 3] = a;
            }
        }
        cosmic_text::SwashContent::Color => {
            rgba.copy_from_slice(&img.data);
        }
        cosmic_text::SwashContent::SubpixelMask => {
            // Treat the same as Mask — cosmic's subpixel masks come back
            // 3 bytes per pixel; we collapse to luma so the glyph still
            // shows up readable.
            for i in 0..(w * h) as usize {
                let a = img.data[i.min(img.data.len() - 1)];
                rgba[i * 4] = 255;
                rgba[i * 4 + 1] = 255;
                rgba[i * 4 + 2] = 255;
                rgba[i * 4 + 3] = a;
            }
        }
    }

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &atlas.texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
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

    atlas.entries.insert(
        key,
        AtlasEntry {
            uv,
            placement: img.placement,
            is_color: matches!(img.content, cosmic_text::SwashContent::Color),
        },
    );
    atlas.entries.get(&key)
}

// === Palette ============================================================

fn palette(c: &GridColor, default: [f32; 4]) -> [f32; 4] {
    match c {
        GridColor::Default => default,
        GridColor::Rgb(r, g, b) => [
            *r as f32 / 255.0,
            *g as f32 / 255.0,
            *b as f32 / 255.0,
            1.0,
        ],
        GridColor::Idx(i) => xterm_palette(*i),
    }
}

/// Standard xterm 256-colour table. ANSI 0..15 are the base palette
/// (Solarized-leaning to match the native build), 16..231 are the 6×6×6
/// colour cube, 232..255 are the grayscale ramp.
fn xterm_palette(i: u8) -> [f32; 4] {
    const BASE: [[u8; 3]; 16] = [
        [0x1c, 0x20, 0x26], // 0 black (matches BG)
        [0xe0, 0x6c, 0x75], // 1 red
        [0x98, 0xc3, 0x79], // 2 green
        [0xe5, 0xc0, 0x7b], // 3 yellow
        [0x61, 0xaf, 0xef], // 4 blue
        [0xc6, 0x78, 0xdd], // 5 magenta
        [0x56, 0xb6, 0xc2], // 6 cyan
        [0xab, 0xb2, 0xbf], // 7 white
        [0x5c, 0x63, 0x70], // 8 bright black
        [0xff, 0x8c, 0x95], // 9 bright red
        [0xb8, 0xd3, 0x99], // 10
        [0xff, 0xd0, 0x8b], // 11
        [0x81, 0xcf, 0xff], // 12
        [0xe6, 0x98, 0xfd], // 13
        [0x76, 0xd6, 0xe2], // 14
        [0xea, 0xee, 0xf4], // 15
    ];
    let rgb = if i < 16 {
        BASE[i as usize]
    } else if i < 232 {
        let v = i - 16;
        let r = v / 36;
        let g = (v % 36) / 6;
        let b = v % 6;
        let step = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
        [step(r), step(g), step(b)]
    } else {
        let v = 8 + (i - 232) * 10;
        [v, v, v]
    };
    [rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, 1.0]
}

// === Box drawing ========================================================

/// Cell-relative fill rects for Unicode block-drawing chars. Lifted from
/// the native `render.rs::block_rects` — keep them in sync. Each entry is
/// `(x, y, w, h)` normalised 0..1 inside the cell.
fn block_rects(ch: char) -> &'static [[f32; 4]] {
    match ch {
        '\u{2580}' => &[[0.0, 0.0, 1.0, 0.5]],
        '\u{2581}' => &[[0.0, 0.875, 1.0, 0.125]],
        '\u{2582}' => &[[0.0, 0.75, 1.0, 0.25]],
        '\u{2583}' => &[[0.0, 0.625, 1.0, 0.375]],
        '\u{2584}' => &[[0.0, 0.5, 1.0, 0.5]],
        '\u{2585}' => &[[0.0, 0.375, 1.0, 0.625]],
        '\u{2586}' => &[[0.0, 0.25, 1.0, 0.75]],
        '\u{2587}' => &[[0.0, 0.125, 1.0, 0.875]],
        '\u{2588}' => &[[0.0, 0.0, 1.0, 1.0]],
        '\u{2589}' => &[[0.0, 0.0, 0.875, 1.0]],
        '\u{258A}' => &[[0.0, 0.0, 0.75, 1.0]],
        '\u{258B}' => &[[0.0, 0.0, 0.625, 1.0]],
        '\u{258C}' => &[[0.0, 0.0, 0.5, 1.0]],
        '\u{258D}' => &[[0.0, 0.0, 0.375, 1.0]],
        '\u{258E}' => &[[0.0, 0.0, 0.25, 1.0]],
        '\u{258F}' => &[[0.0, 0.0, 0.125, 1.0]],
        '\u{2590}' => &[[0.5, 0.0, 0.5, 1.0]],
        '\u{2594}' => &[[0.0, 0.0, 1.0, 0.125]],
        '\u{2595}' => &[[0.875, 0.0, 0.125, 1.0]],
        '\u{2596}' => &[[0.0, 0.5, 0.5, 0.5]],
        '\u{2597}' => &[[0.5, 0.5, 0.5, 0.5]],
        '\u{2598}' => &[[0.0, 0.0, 0.5, 0.5]],
        '\u{2599}' => &[[0.0, 0.0, 0.5, 0.5], [0.0, 0.5, 1.0, 0.5]],
        '\u{259A}' => &[[0.0, 0.0, 0.5, 0.5], [0.5, 0.5, 0.5, 0.5]],
        '\u{259B}' => &[[0.0, 0.0, 1.0, 0.5], [0.0, 0.5, 0.5, 0.5]],
        '\u{259C}' => &[[0.0, 0.0, 1.0, 0.5], [0.5, 0.5, 0.5, 0.5]],
        '\u{259D}' => &[[0.5, 0.0, 0.5, 0.5]],
        '\u{259E}' => &[[0.5, 0.0, 0.5, 0.5], [0.0, 0.5, 0.5, 0.5]],
        '\u{259F}' => &[[0.5, 0.0, 0.5, 0.5], [0.0, 0.5, 1.0, 0.5]],
        '\u{2500}' => &[[0.0, 0.46, 1.0, 0.08]],
        '\u{2502}' => &[[0.46, 0.0, 0.08, 1.0]],
        '\u{250C}' => &[[0.46, 0.46, 0.54, 0.08], [0.46, 0.46, 0.08, 0.54]],
        '\u{2510}' => &[[0.0, 0.46, 0.54, 0.08], [0.46, 0.46, 0.08, 0.54]],
        '\u{2514}' => &[[0.46, 0.46, 0.54, 0.08], [0.46, 0.0, 0.08, 0.54]],
        '\u{2518}' => &[[0.0, 0.46, 0.54, 0.08], [0.46, 0.0, 0.08, 0.54]],
        '\u{251C}' => &[[0.46, 0.0, 0.08, 1.0], [0.46, 0.46, 0.54, 0.08]],
        '\u{2524}' => &[[0.46, 0.0, 0.08, 1.0], [0.0, 0.46, 0.54, 0.08]],
        '\u{252C}' => &[[0.0, 0.46, 1.0, 0.08], [0.46, 0.46, 0.08, 0.54]],
        '\u{2534}' => &[[0.0, 0.46, 1.0, 0.08], [0.46, 0.0, 0.08, 0.54]],
        '\u{253C}' => &[[0.0, 0.46, 1.0, 0.08], [0.46, 0.0, 0.08, 1.0]],
        '\u{256D}' => &[[0.46, 0.46, 0.54, 0.08], [0.46, 0.46, 0.08, 0.54]],
        '\u{256E}' => &[[0.0, 0.46, 0.54, 0.08], [0.46, 0.46, 0.08, 0.54]],
        '\u{256F}' => &[[0.0, 0.46, 0.54, 0.08], [0.46, 0.0, 0.08, 0.54]],
        '\u{2570}' => &[[0.46, 0.46, 0.54, 0.08], [0.46, 0.0, 0.08, 0.54]],
        '\u{2501}' => &[[0.0, 0.42, 1.0, 0.16]],
        '\u{2503}' => &[[0.42, 0.0, 0.16, 1.0]],
        _ => &[],
    }
}

// === WGSL ===============================================================

const WGSL: &str = r#"
struct Uniforms {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) mode: u32,
};

struct Inst {
    rect: vec4<f32>,
    color: vec4<f32>,
    uv: vec4<f32>,
    mode: u32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @location(0) in_rect: vec4<f32>,
    @location(1) in_color: vec4<f32>,
    @location(2) in_uv: vec4<f32>,
    @location(3) in_mode: u32,
) -> VsOut {
    // Two triangles, six vertices, CCW (winding doesn't matter — no cull).
    // Verts: 0=TL 1=BL 2=BR  3=TL 4=BR 5=TR
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );
    let c = corners[vid];
    let px = in_rect.x + c.x * in_rect.z;
    let py = in_rect.y + c.y * in_rect.w;
    // Convert widget-local pixel coords to clip space (origin top-left).
    let nx = px / u.viewport.x * 2.0 - 1.0;
    let ny = 1.0 - py / u.viewport.y * 2.0;

    // Sample uv inside (uv.x..uv.z, uv.y..uv.w)
    let uv = vec2<f32>(
        mix(in_uv.x, in_uv.z, c.x),
        mix(in_uv.y, in_uv.w, c.y),
    );

    var out: VsOut;
    out.clip = vec4<f32>(nx, ny, 0.0, 1.0);
    out.color = in_color;
    out.uv = uv;
    out.mode = in_mode;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.mode == 0u) {
        return in.color;
    }
    let s = textureSample(atlas_tex, atlas_samp, in.uv);
    // Glyph: alpha mask tinted by colour. (Colour bitmaps come through
    // with color == 1,1,1,1, so the tint is a no-op.)
    return vec4<f32>(in.color.rgb * s.rgb, s.a * in.color.a);
}
"#;

// Suppress unused-field warnings on items we keep for future use.
#[allow(dead_code)]
fn _unused(a: &Atlas) -> &wgpu::Sampler {
    let _ = a.texture.size();
    panic!("never called")
}
