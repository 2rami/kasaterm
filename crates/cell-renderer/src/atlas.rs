//! Glyph atlas: one wgpu `Rgba8Unorm` texture, shelf packer, glyph
//! cache. Each unique (codepoint, weight, style, size) gets baked
//! once into a free spot and reused for every subsequent frame —
//! that's the cache miss vs. hit split that sugarloaf's
//! per-frame `text.draw` was paying.
//!
//! Packing strategy: shelf (row) packer. Monospace cells are roughly
//! a fixed height per font size, so shelves waste almost no space
//! and the bookkeeping fits in three integers. When the active shelf
//! runs out of horizontal room we open a new one one shelf-height
//! lower. Atlas-full surfaces as `None` from `get_or_bake` — Phase 1
//! falls back to a blank cell; Phase 2 will grow / repack.

use std::collections::HashMap;

use crate::shaper::Shaper;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct GlyphKey {
    pub ch: char,
    pub bold: bool,
    pub italic: bool,
    /// Pixel size, rounded to integer so `1.0 px` jitter doesn't
    /// double-cache the same glyph.
    pub size_px: u32,
    /// Which font shaper baked this glyph. The renderer keys distinct fonts
    /// (0 = primary monospace, 1 = markdown gothic) so the same codepoint at
    /// the same size from two fonts doesn't collide in the shared atlas.
    pub font: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasEntry {
    /// UV rectangle in the atlas, 0..1.
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Glyph bitmap size in pixels — needed to size the quad the cell
    /// pipeline emits for this glyph.
    pub px_w: u32,
    pub px_h: u32,
    /// Offset from the cell's pen position to the bitmap's top-left,
    /// in pixels (positive y = below baseline).
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: f32,
    /// True when the baked texels are a full-color RGBA bitmap (Apple
    /// Color Emoji / COLR / sbix) rather than a coverage mask. The
    /// pipeline samples color entries verbatim instead of multiplying
    /// the cell's foreground colour through them.
    pub is_color: bool,
}

pub struct Atlas {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    shelf_h: u32,
    cache: HashMap<GlyphKey, Option<AtlasEntry>>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl Atlas {
    /// UV for the dedicated solid-white pixel baked at atlas origin
    /// during `new`. Pipeline callers use this when they want to
    /// draw a flat rectangle (chrome backgrounds, cursor block,
    /// selection overlay) — same instance format as glyphs, only
    /// the uv differs. Keeping it the very first pack slot means
    /// later glyph bakes can't accidentally overwrite it.
    pub const SOLID_UV: [f32; 2] = [0.5 / 2048.0, 0.5 / 2048.0];

    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cell-renderer atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Rgba8Unorm (linear-storage, NOT srgb): coverage masks are
            // baked as white×alpha so fg-multiply in the shader matches
            // the old R8 path byte-for-byte, while color glyphs (Apple
            // Color Emoji etc.) keep their own RGBA. Non-srgb because the
            // surface is non-srgb and we want bytes shown verbatim.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cell-renderer atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // Nearest matches the bitmap-perfect look terminal users
            // expect on Retina; linear would mush edges of small
            // monospace glyphs.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let mut me = Self {
            width: size,
            height: size,
            cursor_x: 0,
            cursor_y: 0,
            shelf_h: 0,
            cache: HashMap::new(),
            texture,
            view,
            sampler,
        };
        // Reserve a 2×2 solid-white block at (0, 0). 2×2 (not 1×1)
        // keeps the bilinear sampler honest in case a caller picks
        // wgpu::FilterMode::Linear instead of Nearest — Nearest is
        // our default but we want the SOLID_UV constant to keep
        // working without a footgun.
        let white = [255u8; 16]; // 2×2 RGBA texels, all opaque white.
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &me.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &white,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(2 * 4),
                rows_per_image: Some(2),
            },
            wgpu::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
        );
        me.cursor_x = 2;
        me.shelf_h = 2;
        me
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    pub fn extent(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Look up a glyph; bake it if it isn't in the cache yet. Returns
    /// `None` when the font has no glyph for `ch` *or* the atlas is
    /// out of space — both cases are handled identically by the cell
    /// pipeline (emit a blank quad).
    pub fn get_or_bake(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shaper: &mut Shaper,
        key: GlyphKey,
    ) -> Option<AtlasEntry> {
        if let Some(slot) = self.cache.get(&key) {
            return *slot;
        }
        let raster = shaper.rasterize(key.ch, key.size_px as f32);
        let entry = raster.and_then(|r| self.upload(device, queue, &r));
        self.cache.insert(key, entry);
        entry
    }

    fn try_place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        // Empty glyphs (e.g. a font's blank for U+0020) still occupy
        // a logical cell but contribute no pixels — skip the pack +
        // upload so they don't burn a slot of width 0 either.
        if w == 0 || h == 0 {
            return Some((0, 0));
        }
        if self.cursor_x + w > self.width {
            self.cursor_y += self.shelf_h;
            self.cursor_x = 0;
            self.shelf_h = 0;
        }
        if self.cursor_y + h > self.height {
            return None;
        }
        let (x, y) = (self.cursor_x, self.cursor_y);
        self.cursor_x += w;
        if h > self.shelf_h {
            self.shelf_h = h;
        }
        Some((x, y))
    }

    fn upload(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        r: &crate::shaper::Rasterized,
    ) -> Option<AtlasEntry> {
        let (x, y) = self.try_place(r.width, r.height)?;
        if r.width > 0 && r.height > 0 {
            // The atlas is always RGBA8. A color glyph already carries
            // RGBA texels; a coverage mask arrives as 1 byte/texel and
            // is expanded here to opaque white with the coverage in the
            // alpha channel, so the shader's `fg.rgb × tex.a` reproduces
            // the old R8 result exactly.
            let rgba: std::borrow::Cow<[u8]> = if r.is_color {
                std::borrow::Cow::Borrowed(&r.data)
            } else {
                let mut buf = Vec::with_capacity(r.data.len() * 4);
                for &a in &r.data {
                    buf.extend_from_slice(&[255, 255, 255, a]);
                }
                std::borrow::Cow::Owned(buf)
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // Rgba8Unorm = 4 bytes per texel.
                    bytes_per_row: Some(r.width * 4),
                    rows_per_image: Some(r.height),
                },
                wgpu::Extent3d {
                    width: r.width,
                    height: r.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let (aw, ah) = (self.width as f32, self.height as f32);
        Some(AtlasEntry {
            uv_min: [x as f32 / aw, y as f32 / ah],
            uv_max: [(x + r.width) as f32 / aw, (y + r.height) as f32 / ah],
            px_w: r.width,
            px_h: r.height,
            bearing_x: r.bearing_x,
            bearing_y: r.bearing_y,
            advance: r.advance,
            is_color: r.is_color,
        })
    }
}

