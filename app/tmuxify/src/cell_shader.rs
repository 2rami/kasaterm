//! Native terminal cell renderer as an iced `Shader` primitive.
//!
//! Architecture (R2):
//!
//! ```text
//!     tmux-bridge ──► ScreenUpdate (cells)
//!                              │
//!                              ▼
//!                   TerminalPrimitive (per-frame data)
//!                              │  iced calls prepare()/render()
//!                              ▼
//!                   TerminalPipeline (wgpu state)
//!                              │
//!                              ├── glyph atlas (rgba8, cosmic-text SwashCache)
//!                              ├── unified instance pipeline
//!                              │    one draw call covers:
//!                              │      • cell backgrounds
//!                              │      • box-drawing quads (block_rects)
//!                              │      • glyph quads (atlas mask × color)
//!                              ▼
//!                       wgpu::RenderPass
//! ```
//!
//! There is no glyphon — glyphon 0.11 pulls wgpu 29 which conflicts with
//! iced 0.14's wgpu 27 pin. We rasterise glyphs ourselves via cosmic-text
//! 0.18's `SwashCache` (CPU mask), pack them into a single 1024×1024 RGBA
//! atlas, and reference them through a per-instance `(uv0, uv1, mode)`
//! tuple. One pipeline, one draw call per frame.

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashImage,
};
use iced::Rectangle;
use iced::widget::shader;
use iced::wgpu::{self, util::DeviceExt};

// We bind directly to tmux-bridge's cell/color types — no need for an
// adapter layer for the spike. If tmux-bridge ever moves out-of-tree
// (different transport), re-introduce a Grid*-style indirection here.
use tmux_bridge::{Cell as GridCell, Color as GridColor};

// === Constants ============================================================

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 18.0;
const ATLAS_DIM: u32 = 1024;
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

/// The data shape iced hands to its renderer once per frame. Owns enough
/// to rebuild instance buffers; the heavy state (atlas texture, swash
/// cache, etc.) lives in `TerminalPipeline`.
#[derive(Debug, Clone)]
pub struct TerminalPrimitive {
    pub cells: Arc<Vec<Vec<GridCell>>>,
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub bg_color: [f32; 4],
    pub fg_color: [f32; 4],
    pub cell_w: f32,
    pub cell_h: f32,
    pub font_size: f32,
}

impl shader::Primitive for TerminalPrimitive {
    type Pipeline = TerminalPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &shader::Viewport,
    ) {
        pipeline.prepare(device, queue, bounds, self);
    }

    fn draw(
        &self,
        pipeline: &Self::Pipeline,
        render_pass: &mut wgpu::RenderPass<'_>,
    ) -> bool {
        pipeline.draw(render_pass);
        true
    }
}

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
    last_bounds: [f32; 2],
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

impl shader::Pipeline for TerminalPipeline {
    fn new(
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
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
            last_bounds: [0.0, 0.0],
        }
    }
}

impl TerminalPipeline {
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        prim: &TerminalPrimitive,
    ) {
        // Memoise — if nothing observable changed since the last frame,
        // the instance buffer is still valid and we can reuse it. iced
        // re-issues a fresh Primitive every frame even when nothing
        // dirty, so this is the cheapest place to gate work.
        let cells_ptr = Arc::as_ptr(&prim.cells) as usize;
        let cursor = (prim.cursor_row, prim.cursor_col, prim.cursor_visible);
        let new_bounds = [bounds.width, bounds.height];
        if cells_ptr == self.last_cells_ptr
            && cursor == self.last_cursor
            && new_bounds == self.last_bounds
            && self.instance_count > 0
        {
            return;
        }
        self.last_cells_ptr = cells_ptr;
        self.last_cursor = cursor;
        self.last_bounds = new_bounds;

        // Uniform: viewport size (= widget bounds).
        let uniforms = Uniforms {
            viewport: [bounds.width.max(1.0), bounds.height.max(1.0)],
            _pad: [0.0, 0.0],
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        // Build instances.
        let mut instances: Vec<Instance> = Vec::with_capacity(
            (prim.rows as usize) * (prim.cols as usize) * 2,
        );

        // 1. Full background fill so any pixel the grid doesn't cover gets
        //    the terminal bg colour (the iced pass viewport is scoped to
        //    our bounds, so this is a single quad).
        instances.push(Instance {
            rect: [0.0, 0.0, bounds.width, bounds.height],
            color: prim.bg_color,
            uv: [0.0, 0.0, 0.0, 0.0],
            mode: 0,
            _pad: [0; 3],
        });

        let cell_w = prim.cell_w;
        let cell_h = prim.cell_h;

        // 2. Walk every cell. Bg quads first, then box-rects or glyph.
        for (row_idx, row) in prim.cells.iter().enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let x = col_idx as f32 * cell_w;
                let y = row_idx as f32 * cell_h;

                // Resolve fg/bg from cell attrs, honouring `inverse`.
                let mut fg = palette(&cell.fg, prim.fg_color);
                let mut bg = palette(&cell.bg, prim.bg_color);
                if cell.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }

                // Cursor: paint cursor cell as inverted block underneath.
                let is_cursor = prim.cursor_visible
                    && row_idx as u16 == prim.cursor_row
                    && col_idx as u16 == prim.cursor_col;
                if is_cursor {
                    std::mem::swap(&mut fg, &mut bg);
                }

                // Background quad (skip if it would draw the default bg —
                // already covered by the full-bounds fill).
                if bg != prim.bg_color {
                    instances.push(Instance {
                        rect: [x, y, cell_w, cell_h],
                        color: bg,
                        uv: [0.0, 0.0, 0.0, 0.0],
                        mode: 0,
                        _pad: [0; 3],
                    });
                }

                // Now the glyph. Two paths:
                //   a. Char is in `block_rects` → draw quads directly.
                //   b. Otherwise rasterise via cosmic-text + swash.
                let first_char = cell.ch.chars().next();
                let rects = first_char
                    .map(block_rects)
                    .unwrap_or(&[]);
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

                // Shape this single cell glyph. Cosmic shapes whole lines
                // normally — for a terminal each cell is one column, so
                // shaping the single character is the right granularity
                // (and matches the cell grid exactly).
                let glyph_data = shape_one_glyph(
                    &mut self.font_system,
                    &cell.ch,
                    prim.font_size,
                    cell.bold,
                    cell.italic,
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
                    // Place the glyph: baseline is at line_height * 0.78
                    // below the row top (matches the native renderer's
                    // measurement, roughly the ascent fraction for the
                    // D2Coding family at this size).
                    let baseline = y + cell_h * 0.78;
                    let gx = x + x_off + entry.placement.left as f32;
                    let gy = baseline - entry.placement.top as f32 + y_off;
                    let gw = entry.placement.width as f32;
                    let gh = entry.placement.height as f32;
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

        // Cursor outline (when cell under cursor is empty — we already
        // inverted bg above, so just ensure visibility on blanks).
        // Skipped for now; the bg invert is enough.

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

    fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>) {
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

// === Glyph rasterisation ================================================

/// Shape a single terminal cell's text. Returns (cache_key, x_offset, y_offset).
/// For ascii/CJK that's exactly one glyph; ligatures or emoji may emit several.
fn shape_one_glyph(
    fs: &mut FontSystem,
    text: &str,
    font_size: f32,
    bold: bool,
    italic: bool,
) -> Vec<(CacheKey, f32, f32)> {
    let mut buf = Buffer::new(fs, Metrics::new(font_size, font_size * 1.25));
    buf.set_size(fs, Some(font_size * 4.0), Some(font_size * 2.0));
    // Match the native build's preferred family. cosmic-text walks
    // installed fonts via fontdb; on macOS the Nerd Font is found by name
    // when it's been `brew install`'d into ~/Library/Fonts. If it's not
    // present, cosmic falls back through Family::Monospace which is what
    // the previous Family::Monospace request did anyway.
    let mut attrs = Attrs::new()
        .family(Family::Name("D2CodingLigature Nerd Font Mono"))
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
            let physical = g.physical((0.0, 0.0), 1.0);
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
    let (x, y, uv) = atlas.alloc(w, h)?;

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
