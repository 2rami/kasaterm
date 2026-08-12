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
    // 0.0 = passthrough (sRGB stays sRGB).
    // 1.0 = sRGB→DisplayP3 Bradford matrix in linear light, re-encode.
    //   Only meaningful when the host's CAMetalLayer is actually tagged
    //   DisplayP3 (KASATERM_P3_ROOT path). Without the layer tag, this
    //   washes colours out — the bytes become P3-encoded but the layer
    //   is still treated as sRGB by macOS, so they display dim.
    p3_convert: f32,
    // Monotonic seconds for GPU-driven animation (the working-bar sweep). The
    // CPU rewrites only this each present, so a busy pane animates without
    // re-emitting any chrome instances — idle stays at 0 CPU rebuild work.
    time: f32,
    _pad: f32,
};

// sRGB ↔ linear-light conversions (the IEC 61966-2-1 piecewise curve).
// We split scalar / vector forms so the matrix dot products below stay
// readable.
fn srgb_to_linear_s(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_to_linear_s(c.r), srgb_to_linear_s(c.g), srgb_to_linear_s(c.b));
}
fn linear_to_srgb_s(c: f32) -> f32 {
    let cc = max(c, 0.0);
    if (cc <= 0.0031308) { return cc * 12.92; }
    return 1.055 * pow(cc, 1.0 / 2.4) - 0.055;
}
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_to_srgb_s(c.r), linear_to_srgb_s(c.g), linear_to_srgb_s(c.b));
}

// Bradford-adapted sRGB D65 primaries → DisplayP3 D65 primaries, in
// linear light. Lifted directly from sugarloaf's renderer.metal so the
// byte-level output matches what kasaterm's sugarloaf opt-in path
// produces — same numbers, same gamut.
fn srgb_to_p3(linear_srgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(linear_srgb, vec3<f32>(0.82246197, 0.17753803, 0.0)),
        dot(linear_srgb, vec3<f32>(0.03319420, 0.96680580, 0.0)),
        dot(linear_srgb, vec3<f32>(0.01708263, 0.07239744, 0.91051993))
    );
}

// One-shot wrapper. When `u.p3_convert > 0.5`, walk the colour through
// the matrix; otherwise pass through. Branching cost is negligible
// (uniform predicate, fully predicated by the driver).
fn prepare_output(srgb: vec3<f32>) -> vec3<f32> {
    if (u.p3_convert > 0.5) {
        let lin = srgb_to_linear(srgb);
        let p3_lin = srgb_to_p3(lin);
        return linear_to_srgb(p3_lin);
    }
    return srgb;
}

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
    // Working-bar (flags & 4): an indeterminate ~32% segment sweeps a faint
    // track on a 1.2s loop, driven entirely by `u.time` — the CPU emits the
    // bar quad once and the GPU animates the sweep, so a busy pane costs no
    // per-frame chrome rebuild. `in.uv.x` is the 0..1 horizontal position.
    if ((in.flags & 4u) != 0u) {
        let seg = 0.32;
        let head = -seg + (1.0 + seg) * fract(u.time / 1.2);
        let inseg = step(head, in.uv.x) * step(in.uv.x, head + seg);
        let a = in.fg.a * mix(0.22, 1.0, inseg);
        let rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(rgb), a);
    }
    // Compact-bar (flags & 16): fills from the left on a 2.4s loop, then
    // restarts. compact 는 몇 초에서 수십 초 걸리는 **끝이 있는** 작업이라,
    // 「칸이 차는」 모양이 「쓸고 지나가는」 working bar 보다 상태를 옳게 읽힌다.
    // 진행률 자체는 claude 가 화면에만 내놓고 우리에게 주지 않으므로 시간으로
    // 채운다(indeterminate) — 그래서 채운 칸이 실제 퍼센트는 아니다.
    if ((in.flags & 16u) != 0u) {
        let fill = fract(u.time / 2.4);
        let infill = step(in.uv.x, fill);
        let a = in.fg.a * mix(0.18, 1.0, infill);
        let rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(rgb), a);
    }
    // Pulse-bar (flags & 8): a full-width rail whose alpha breathes on a slow
    // 3s sine — a background/Monitor job is running with no on-screen spinner.
    // The gentler, slower rhythm keeps it distinct from the working-bar sweep.
    if ((in.flags & 8u) != 0u) {
        let breath = 0.30 + 0.45 * (0.5 + 0.5 * sin(u.time * 2.0943951)); // 2π/3 → 3s period
        let rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(rgb), in.fg.a * breath);
    }
    // Color glyphs (emoji) are baked as full RGBA — draw them verbatim,
    // letting fg.a act as a global opacity. Coverage masks are baked as
    // white×alpha, so fg.rgb × tex.a reproduces the monochrome path.
    if ((in.flags & 1u) != 0u) {
        let sat_rgb = boost_saturation(texel.rgb, u.color_sat);
        return vec4<f32>(prepare_output(sat_rgb), texel.a * in.fg.a);
    }
    // SVG icon mask: tint through coverage but keep the raster's own linear
    // anti-aliasing — the text gamma/contrast curve below jaggies thin strokes.
    if ((in.flags & 2u) != 0u) {
        let icon_rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(icon_rgb), in.fg.a * clamp(texel.a, 0.0, 1.0));
    }
    let alpha_raw = clamp(texel.a, 0.0, 1.0);
    let alpha_gamma = pow(alpha_raw, 1.0 / max(u.text_gamma, 0.001));
    let alpha = clamp(alpha_gamma * u.text_contrast, 0.0, 1.0);
    // `prepare_output` is the sRGB→Display P3 hop when KASATERM_P3_ROOT
    // is on, identity otherwise. Same pattern sugarloaf uses — the
    // byte we write is gamma-encoded P3, the CAMetalLayer is tagged
    // DisplayP3, and macOS scans out the P3 chromaticity.
    let fg_sat = boost_saturation(in.fg.rgb, u.color_sat);
    return vec4<f32>(prepare_output(fg_sat), in.fg.a * alpha);
}
