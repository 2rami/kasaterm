// Phase 1 cell pipeline. One quad per glyph instance, indexed via
// `vertex_index % 6` so we don't ship a vertex buffer at all. The
// instance carries the cell's pixel rect, the glyph's atlas UV rect,
// and the foreground colour. R8 atlas sampled as alpha; B&W path
// outputs (fg.rgb, fg.a * alpha).

struct Uniforms {
    // Screen size in physical pixels — used to project pixel-space
    // cell rects into clip space without a CPU-side matrix multiply.
    screen_px: vec2<f32>,
    // text_gamma: WezTerm-style alpha curve on the glyph coverage mask.
    //   >1.0 boosts mid-tones (crisper, "darker" antialiased edges)
    //   1.0  passthrough (legacy behavior)
    //   <1.0 lifts mids (softer, foggy)
    // 1.3 = WezTerm's default. We apply pow(alpha, 1/gamma).
    // text_contrast: extra multiplier on the post-gamma alpha. 1.0 = no
    // change; small bumps (1.05) sharpen further without crushing mids.
    text_gamma: f32,
    text_contrast: f32,
    // color_sat: HSL-style saturation multiplier applied to fg.rgb (and
    // the colored-glyph texel.rgb). 1.0 = passthrough. >1 punches up
    // colours toward their primaries (sRGB green that lands at the same
    // chromaticity as P3 green after the layer tag, etc). Cells whose
    // bg/fg are pure white / black stay neutral because they have zero
    // chroma to scale.
    color_sat: f32,
    _pad: f32,
};

// Push fg toward its primary chromaticity by `sat`. 1.0 = identity. We
// move the chroma component (rgb - luma) outward and re-add luma so
// brightness is preserved. Numerically stable for sat in [0, ~3].
fn boost_saturation(rgb: vec3<f32>, sat: f32) -> vec3<f32> {
    // BT.709 luma weights — terminal cells are dominantly mono / pastel
    // so this is "good enough" perceptual luma.
    let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mono = vec3<f32>(luma);
    return clamp(mix(mono, rgb, sat), vec3<f32>(0.0), vec3<f32>(1.0));
}


@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) cell_px: vec4<f32>,     // x, y, w, h (physical pixels)
    @location(1) uv_min: vec2<f32>,
    @location(2) uv_max: vec2<f32>,
    @location(3) fg: vec4<f32>,
    @location(4) flags: u32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

@vertex
fn vs_main(in: VsIn, @builtin(vertex_index) vi: u32) -> VsOut {
    // Quad expansion via vertex index. CCW triangles, top-left origin
    // matches winit's surface convention so y grows downward in pixel
    // space and we just flip once at the end.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];
    let raw_px = vec2<f32>(in.cell_px.x + c.x * in.cell_px.z,
                           in.cell_px.y + c.y * in.cell_px.w);
    // pixel_perfect_quad — round quad corners to integer physical
    // pixels so glyph edges align with the pixel grid instead of
    // bleeding sub-pixel coverage into the neighbouring column. Box
    // drawings and ASCII rules suddenly read razor-sharp; colour
    // chips stop dithering against the body bg.
    let px = vec2<f32>(round(raw_px.x), round(raw_px.y));
    // px / screen → 0..1, then *2 - 1 → -1..1 clip space. Y flip
    // because clip space is bottom-up.
    let ndc = vec2<f32>(px.x / u.screen_px.x * 2.0 - 1.0,
                        1.0 - px.y / u.screen_px.y * 2.0);
    let uv = mix(in.uv_min, in.uv_max, c);

    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    out.fg = in.fg;
    out.flags = in.flags;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_tex, atlas_sampler, in.uv);
    // Color glyphs (emoji) are baked as full RGBA — draw them verbatim,
    // letting fg.a act as a global opacity. Coverage masks are baked as
    // white×alpha, so fg.rgb × tex.a reproduces the monochrome path.
    if ((in.flags & 1u) != 0u) {
        let sat_rgb = boost_saturation(texel.rgb, u.color_sat);
        return vec4<f32>(sat_rgb, texel.a * in.fg.a);
    }
    let alpha_raw = clamp(texel.a, 0.0, 1.0);
    let alpha_gamma = pow(alpha_raw, 1.0 / max(u.text_gamma, 0.001));
    let alpha = clamp(alpha_gamma * u.text_contrast, 0.0, 1.0);
    // Raw passthrough: this path is only entered when the user opts
    // into `KASATERM_RENDERER=gpu`. macOS 26 silently drops the
    // CAMetalLayer P3 tag for wgpu's sublayer pattern, so the gpu
    // path's colour reproduction stays at plain sRGB — sugarloaf is
    // the colour-correct default. Knobs (sat / gamma / contrast) let
    // the user dial in a closer match if they really want gpu speed.
    let fg_sat = boost_saturation(in.fg.rgb, u.color_sat);
    return vec4<f32>(fg_sat, in.fg.a * alpha);
}
