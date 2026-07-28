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
//! lower.
//!
//! There is no eviction — the packer only ever moves forward. That is
//! fine as long as the live glyph set is bounded, but `GlyphKey` carries
//! `size_px`, so every DPI change, ui-zoom step and font-size tweak
//! admits a whole second copy of the working set while the old one stays
//! resident. Hangul alone is thousands of glyphs, so a few monitor moves
//! used to exhaust the texture — and an exhausted atlas is *permanent*
//! damage, because the miss got memoized as `None` and that character
//! stayed blank for the life of the process ("글자가 하나씩 사라진다").
//!
//! So exhaustion is now a recoverable condition, not a cached answer:
//! `try_place` failure raises `wants_reset` instead of poisoning the
//! cache, and the renderer drains that flag at a frame boundary via
//! `take_wants_reset` + `reset`. Resetting mid-frame would invalidate
//! the UVs of quads already in the draw list, so the flag must never be
//! consumed while a frame is being built.

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
    /// Supersampling factor. Glyphs are rasterized at `size_px * oversample`
    /// and stored at that resolution, while `AtlasEntry` geometry is divided
    /// back down to logical pixels. With a Linear sampler this gives Retina-
    /// class sharpness on 1x (100% DPI) displays where the logical pixel size
    /// is too small to resolve a coverage mask cleanly. 1 = no supersampling.
    oversample: u32,
    /// A caller invalidated every cached size (DPI / font-size change, manual
    /// refresh). Always honoured — unlike running out of room, this says
    /// nothing about whether the live set fits.
    wants_reset: bool,
    /// A bake found no room during the frame being built. Settled into
    /// `consecutive_full` at the next frame boundary.
    full_this_frame: bool,
    /// How many frames in a row ended out of room. One or two means the atlas
    /// was clogged with dead entries and repacking clears it. More than that
    /// means the glyphs actually on screen outnumber the texture, and every
    /// further repack would refill and overflow at the same place — an endless
    /// repaint loop, which is worse than the blanks it is trying to fix. So
    /// past the limit we stop asking and let the frame settle.
    consecutive_full: u32,
    /// True once we've given up above, so the diagnostic prints once.
    futility_logged: bool,
}

/// Consecutive out-of-room frames tolerated before concluding the live glyph
/// set simply doesn't fit. Two, because the first full frame is the one that
/// discovers the problem and the second proves a repack didn't help.
const MAX_CONSECUTIVE_FULL: u32 = 2;

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
            // Linear so the supersampled (oversample>1) atlas glyphs
            // downsample smoothly to the logical quad size — that's what
            // produces the Retina-class sharpness on 1x displays. At
            // oversample=1 the glyph maps ~1:1 so Linear stays crisp too.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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
            oversample: 1,
            wants_reset: false,
            full_this_frame: false,
            consecutive_full: 0,
            futility_logged: false,
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

    /// Set the supersampling factor (>=1). Every cached entry was baked at
    /// the old factor, so a change invalidates the whole atlas. Called at
    /// startup from the display scale (2 on a 1x display, 1 on Retina) and
    /// again on every DPI change — moving to a non-Retina monitor with a
    /// stale factor is what made text look blurry-then-broken.
    pub fn set_oversample(&mut self, factor: u32) {
        let factor = factor.max(1);
        if factor != self.oversample {
            self.oversample = factor;
            self.request_reset();
        }
    }

    /// Ask for a repack before the next frame. Clearing the cache alone would
    /// leak the space those glyphs held (the shelf cursor only moves forward),
    /// so invalidation and repacking have to happen together — that is `reset`.
    ///
    /// This also forgives an earlier "doesn't fit" verdict: the scale or font
    /// size just changed, so the live set is a different set now.
    pub fn request_reset(&mut self) {
        self.wants_reset = true;
        self.consecutive_full = 0;
        self.futility_logged = false;
    }

    /// Frame-boundary tick. Settles the previous frame's out-of-room state and
    /// repacks if that would help. Returns whether a repack happened (the
    /// caller only needs it for logging).
    ///
    /// Must run before the frame emits any quad — see the module docs.
    pub fn begin_frame(&mut self) -> bool {
        // try_place can fail many times within one frame; the streak only
        // advances here so it counts frames, not failures.
        let was_full = std::mem::take(&mut self.full_this_frame);
        self.consecutive_full = if was_full { self.consecutive_full + 1 } else { 0 };
        let futile = self.consecutive_full > MAX_CONSECUTIVE_FULL;
        if futile && !self.futility_logged {
            self.futility_logged = true;
            eprintln!(
                "[atlas] {}×{} 텍스처에 이 화면의 글리프가 다 안 들어감 \
                 ({} 개까지 담김) — 일부 셀이 빈칸으로 남는다. 반복 repack 은 중단.",
                self.width,
                self.height,
                self.cache.len()
            );
        }
        let repack = self.wants_reset || (was_full && !futile);
        if repack {
            self.reset();
        }
        repack
    }

    /// Whether the frame just painted needs a follow-up. An out-of-room bake
    /// left blank cells on screen, and only the next frame — which repacks
    /// first — can fill them. An idle app paints no next frame on its own.
    pub fn needs_another_frame(&self) -> bool {
        self.wants_reset
            || (self.full_this_frame && self.consecutive_full <= MAX_CONSECUTIVE_FULL)
    }

    /// Drop every cached glyph and rewind the shelf packer. Glyphs re-bake
    /// lazily on the next draw, so the cost is one frame of raster work for
    /// whatever is actually on screen — not the thousands of dead entries a
    /// few monitor moves had accumulated.
    ///
    /// The 2×2 white block at the origin is left in place: nothing ever
    /// overwrites it because the cursor rewinds to x=2, exactly as `new`
    /// leaves it. That is why this needs no `queue`.
    ///
    /// Deliberately does not touch `consecutive_full` — that streak exists to
    /// notice that repacking isn't helping, so it has to survive the repack.
    pub fn reset(&mut self) {
        self.cache.clear();
        self.cursor_x = 2;
        self.cursor_y = 0;
        self.shelf_h = 2;
        self.wants_reset = false;
    }

    /// Live glyph count — diagnostics for the atlas-pressure log.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Look up a glyph; bake it if it isn't in the cache yet. `None` means
    /// the cell pipeline emits a blank quad.
    ///
    /// The two ways to get `None` are memoized very differently. "This font
    /// has no glyph for `ch`" is a stable fact about the font, so it is cached
    /// — re-asking would re-run the shaper every frame for nothing. "The atlas
    /// is out of room" is a transient fact about *this* texture state, so it
    /// must NOT be cached: doing so blanked the character permanently, long
    /// after a repack had freed the space. It raises `wants_reset` instead.
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
        // Rasterize at the supersampled resolution; `upload` divides the
        // geometry back to logical pixels so the quad stays logical-sized.
        let render_px = (key.size_px * self.oversample.max(1)) as f32;
        let Some(raster) = shaper.rasterize_styled(key.ch, render_px, key.bold, key.italic) else {
            self.cache.insert(key, None);
            return None;
        };
        match self.upload(device, queue, &raster) {
            Some(entry) => {
                self.cache.insert(key, Some(entry));
                Some(entry)
            }
            None => {
                self.full_this_frame = true;
                None
            }
        }
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
        // Texture region (uv) stays at the supersampled resolution; quad
        // geometry is divided back to logical pixels so layout is unchanged
        // and the Linear sampler downsamples the hi-res glyph into it.
        let os = self.oversample.max(1);
        Some(AtlasEntry {
            uv_min: [x as f32 / aw, y as f32 / ah],
            uv_max: [(x + r.width) as f32 / aw, (y + r.height) as f32 / ah],
            px_w: r.width / os,
            px_h: r.height / os,
            bearing_x: r.bearing_x / os as i32,
            bearing_y: r.bearing_y / os as i32,
            advance: r.advance / os as f32,
            is_color: r.is_color,
        })
    }
}

