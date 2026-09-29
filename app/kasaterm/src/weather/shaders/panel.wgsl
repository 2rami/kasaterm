// Panels (sidebar, side columns) refract the scene through their own ripple field
// (jquery.ripples render: normal from height deltas, offset along it, specular), capped
// so text under a ring stays readable.

@group(0) @binding(0) var<uniform> g: G;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var scene: texture_2d<f32>;
@group(0) @binding(3) var ripple: texture_2d<f32>;

@fragment
fn fs_panel(in: VOut) -> @location(0) vec4f {
    let px = in.uv * g.res.xy;
    let base = textureSampleLevel(scene, samp, in.uv, 0.0).rgb;
    let ri = region_of(px, 1.0);
    if (ri < 0 || rg.b[ri].y <= 0.0) {
        return vec4f(base, 1.0);
    }
    let r = rg.rect[ri];
    // Sample inside the panel's own texels so the border reads as a wall, not a cliff.
    let tex = vec2f(1.0) / g.rip.zw;
    let lo = (r.xy / g.rip.xy + 1.0) / tex;
    let hi = ((r.xy + r.zw) / g.rip.xy - 1.0) / tex;
    let dl = g.rip.zw;
    let ruv = clamp(px / g.rip.xy / tex, lo, hi);
    let h = textureSampleLevel(ripple, samp, ruv, 0.0).r;
    let hx = textureSampleLevel(ripple, samp, min(ruv + vec2f(dl.x, 0.0), hi), 0.0).r;
    let hy = textureSampleLevel(ripple, samp, min(ruv + vec2f(0.0, dl.y), hi), 0.0).r;
    let dx = vec3f(dl.x, hx - h, 0.0);
    let dy = vec3f(0.0, hy - h, dl.y);
    let off = -normalize(cross(dy, dx)).xz;
    let spec = pow(max(0.0, dot(off, normalize(vec2f(-0.6, -1.0)))), 4.0);
    let o = cap(off * REFRACT_CAP * g.t.z, REFRACT_CAP * g.t.z);
    let refr = textureSampleLevel(scene, samp, in.uv + o * g.res.zw, 0.0).rgb;
    return vec4f(refr + vec3f(0.9, 0.95, 1.0) * spec * 0.3, 1.0);
}
