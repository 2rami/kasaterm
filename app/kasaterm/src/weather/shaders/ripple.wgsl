// Effect 2: wave-equation height field on a ping-pong float texture covering the window,
// updated only inside panels; a panel's edges are walls, so rings stay in their own panel.
// Algorithm follows jquery.ripples by Pim Schreurs (MIT, (c) 2017): drop = cosine bump added
// to height, update = neighbour average drives velocity with damping. Reimplemented in WGSL.

struct Drops {
    count: vec4f,
    d: array<vec4f, 64>, // x, y (texel), radius (texel), strength
};

@group(0) @binding(0) var<uniform> g: G;
@group(0) @binding(1) var<uniform> drops: Drops;
@group(0) @binding(2) var src: texture_2d<f32>;

const PANEL: f32 = 1.0;
// Not in jquery.ripples: a touch of viscosity so grid-scale chop dies out and text
// under the panel stays readable between rings.
const VISCOSITY: f32 = 0.04;

@fragment
fn fs_drop(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    if (region_of(fc.xy * g.rip.xy, PANEL) < 0) {
        return vec4f(0.0);
    }
    var info = textureLoad(src, vec2i(fc.xy), 0);
    let n = i32(drops.count.x);
    for (var k = 0; k < n; k++) {
        let d = drops.d[k];
        var x = max(0.0, 1.0 - length(fc.xy - d.xy) / d.z);
        x = 0.5 - cos(x * PI) * 0.5;
        info.r += x * d.w;
    }
    return info;
}

@fragment
fn fs_update(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    let ri = region_of(fc.xy * g.rip.xy, PANEL);
    if (ri < 0) {
        return vec4f(0.0);
    }
    let r = rg.rect[ri];
    let lo = vec2i(ceil(r.xy / g.rip.xy - 0.5));
    let hi = vec2i(ceil((r.xy + r.zw) / g.rip.xy - 0.5)) - vec2i(1);
    let c = vec2i(fc.xy);
    var info = textureLoad(src, c, 0);
    // Clamping to the panel's own texels reflects waves at its border.
    let avg = (textureLoad(src, clamp(c - vec2i(1, 0), lo, hi), 0).r
        + textureLoad(src, clamp(c + vec2i(1, 0), lo, hi), 0).r
        + textureLoad(src, clamp(c - vec2i(0, 1), lo, hi), 0).r
        + textureLoad(src, clamp(c + vec2i(0, 1), lo, hi), 0).r) * 0.25;
    info.g += (avg - info.r) * 2.0;
    info.g *= rg.b[ri].y;
    info.r += info.g;
    info.r = mix(info.r, avg, VISCOSITY);
    return info;
}
