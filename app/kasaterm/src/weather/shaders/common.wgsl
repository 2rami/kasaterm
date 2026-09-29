// Prepended to every weather shader.

struct G {
    res: vec4f,  // w, h, 1/w, 1/h (physical px)
    t: vec4f,    // time, dt, physical px per logical px, 0
    rain: vec4f, // fall speed, slant, 0, 0
    rip: vec4f,  // ripple texel size in px (x, y), 1/ripple w, 1/ripple h
};

struct VOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

const PI: f32 = 3.14159265;
// Refraction over text stops here (logical px): the first prototype garbled terminal text
// until its ripple offset was halved, and that halved value is the cap.
const REFRACT_CAP: f32 = 7.0;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> VOut {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4f(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = p;
    return o;
}

// pcg3d — Jarzynski & Olano, "Hash Functions for GPU Rendering" (JCGT 2020).
fn pcg3d(v0: vec3u) -> vec3u {
    var v = v0 * 1664525u + 1013904223u;
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3u(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return v;
}

fn rand3(c: vec3i) -> vec3f {
    return vec3f(pcg3d(bitcast<vec3u>(c))) * (1.0 / 4294967295.0);
}

fn sd_round_box(p: vec2f, b: vec2f, r: f32) -> f32 {
    let q = abs(p) - b + vec2f(r);
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn cap(v: vec2f, limit: f32) -> vec2f {
    let l = length(v);
    return select(v, v * (limit / l), l > limit);
}
