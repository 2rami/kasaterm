// Effect 3, pass 1: water maps for drops on each pane's glass.
// Follows raindrop-fx by SardineFish (MIT, (c) 2021): drops are splatted into a map as
// (normal.xy, refraction size, coverage); tiny droplets accumulate in a persistent map;
// a mist layer fogs in over time; sliding drops erase droplets and mist behind them.
// Here every instance carries its pane rect and is clipped to it, and drying panes and
// per-pane mist growth are drawn as rect instances. Drop shape is analytic.

struct GM {
    sim: vec4f, // canvas w, h (pt), 1/w, 1/h
};

@group(0) @binding(0) var<uniform> gm: GM;

struct Inst {
    @location(0) pos: vec2f,
    @location(1) size: vec2f,
    @location(2) extra: vec4f, // refraction size, or the value a rect writes
    @location(3) clip: vec4f,  // pane rect (pt)
};

struct DV {
    @builtin(position) pos: vec4f,
    @location(0) q: vec2f,
    @location(1) sn: f32,
    @location(2) w: vec2f,
    @location(3) clip: vec4f,
};

// Visible diameter as a fraction of the simulated size, so raindrop-fx's merge distance
// (0.16 * size per drop) matches what is on screen.
const VIS: f32 = 0.36;
const PAD: f32 = 1.2;

fn ndc(p: vec2f) -> vec4f {
    return vec4f(p.x * gm.sim.z * 2.0 - 1.0, 1.0 - p.y * gm.sim.w * 2.0, 0.0, 1.0);
}

fn corner(vi: u32) -> vec2f {
    return vec2f(f32(vi & 1u), f32((vi >> 1u) & 1u));
}

@vertex
fn vs_drop(@builtin(vertex_index) vi: u32, inst: Inst) -> DV {
    let c = corner(vi) * 2.0 - 1.0;
    let p = inst.pos + c * inst.size * 0.5 * VIS * PAD;
    var o: DV;
    o.pos = ndc(p);
    o.q = c * PAD;
    o.sn = inst.extra.x;
    o.w = p;
    o.clip = inst.clip;
    return o;
}

@vertex
fn vs_rect(@builtin(vertex_index) vi: u32, inst: Inst) -> DV {
    let p = inst.pos + corner(vi) * inst.size;
    var o: DV;
    o.pos = ndc(p);
    o.q = vec2f(0.0);
    o.sn = inst.extra.x;
    o.w = p;
    o.clip = inst.clip;
    return o;
}

fn clipped(in: DV) -> bool {
    return any(in.w < in.clip.xy) || any(in.w > in.clip.xy + in.clip.zw);
}

// Coverage ramps from 1 at the visible edge (r = 1) to 0 at r = PAD.
fn coverage(q: vec2f) -> f32 {
    return clamp((PAD - length(q)) / (PAD - 1.0), 0.0, 1.0);
}

fn normal_xy(q: vec2f) -> vec2f {
    return q / max(length(q), 1.0);
}

@fragment
fn fs_drop(in: DV) -> @location(0) vec4f {
    if (clipped(in)) {
        discard;
    }
    let a = coverage(in.q);
    return vec4f((normal_xy(in.q) * 0.5 + 0.5) * a, in.sn * a, a);
}

@fragment
fn fs_droplet(in: DV) -> @location(0) vec4f {
    if (clipped(in)) {
        discard;
    }
    let a = coverage(in.q);
    return vec4f((normal_xy(in.q) * 0.5 + 0.5) * a, 0.0, a);
}

// Blended as dst * (1 - a): wipes droplets and mist under a drop.
@fragment
fn fs_erase(in: DV) -> @location(0) vec4f {
    if (clipped(in)) {
        discard;
    }
    return vec4f(0.0, 0.0, 0.0, smoothstep(0.3, 1.0, coverage(in.q * 0.97)));
}

// A pane out of the rain: clears everything under the rect.
@fragment
fn fs_erase_rect(in: DV) -> @location(0) vec4f {
    return vec4f(0.0, 0.0, 0.0, 1.0);
}

// Blended as src * (1 - dst) + dst: mist approaches 1 at the rect's own pace.
@fragment
fn fs_mist_rect(in: DV) -> @location(0) vec4f {
    return vec4f(in.sn);
}
