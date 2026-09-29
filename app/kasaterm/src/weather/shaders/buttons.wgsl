// A water drop on every control the app drew this frame. Hover swells it, press squashes
// it and sends a ring through the place it sits in, a disabled control is a flat film.
// Neighbours on one row are joined by a smooth-min distance field whose blend width rises
// while the row is hovered (polynomial smooth-min as described by Inigo Quilez).
// Shaded only inside per-row boxes; the frame itself is copied by fs_final.

struct B {
    n: vec4f,               // count
    b: array<vec4f, 64>,    // centre x, y, half width, half height (px)
    s: array<vec4f, 64>,    // hover, press, inflate (1 enabled, 0 disabled), ring age s (<0 none)
    c: array<vec4f, 64>,    // rect the ring stays in (px)
    m: array<vec4f, 64>,    // row, 0, 0, 0
    k: array<vec4f, 32>,    // blend width per row (px)
};

@group(0) @binding(0) var<uniform> g: G;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var scene: texture_2d<f32>;
@group(0) @binding(3) var<uniform> bt: B;

fn smin(a: f32, c: f32, k: f32) -> f32 {
    let h = max(k - abs(a - c), 0.0) / k;
    return min(a, c) - h * h * k * 0.25;
}

fn sd_btn(p: vec2f, i: i32) -> f32 {
    let b = bt.b[i];
    let s = bt.s[i];
    let off = 1.0 - clamp(s.z, 0.0, 1.2);
    let sx = 1.0 + 0.16 * s.y + 0.1 * off;
    let sy = max(1.0 - 0.24 * s.y - 0.3 * off, 0.3);
    let half = b.zw * (1.0 + 0.1 * s.x);
    let q = (p - b.xy) / vec2f(sx, sy);
    return sd_round_box(q, half, min(half.x, half.y)) * min(sx, sy);
}

fn field(p: vec2f) -> f32 {
    let n = i32(bt.n.x);
    var total = 1e9;
    var acc = 1e9;
    var row = -1.0;
    for (var i = 0; i < n; i++) {
        let d = sd_btn(p, i);
        let ri = bt.m[i].x;
        if (ri != row) {
            total = min(total, acc);
            acc = d;
            row = ri;
        } else {
            acc = smin(acc, d, max(bt.k[i32(ri)].x, 0.5));
        }
    }
    return min(total, acc);
}

@fragment
fn fs_final(in: VOut) -> @location(0) vec4f {
    return vec4f(textureSampleLevel(scene, samp, in.uv, 0.0).rgb, 1.0);
}

struct GI {
    @location(0) pos: vec2f,
    @location(1) size: vec2f,
    @location(2) extra: vec4f,
    @location(3) clip: vec4f,
};

@vertex
fn vs_group(@builtin(vertex_index) vi: u32, gi: GI) -> VOut {
    let c = vec2f(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = gi.pos + c * gi.size;
    var o: VOut;
    o.pos = vec4f(p.x * g.res.z * 2.0 - 1.0, 1.0 - p.y * g.res.w * 2.0, 0.0, 1.0);
    o.uv = p * g.res.zw;
    return o;
}

@fragment
fn fs_group(in: VOut) -> @location(0) vec4f {
    let px = in.pos.xy;
    let pt = g.t.z;
    var col = textureSampleLevel(scene, samp, px * g.res.zw, 0.0).rgb;
    var touched = false;

    let n = i32(bt.n.x);
    var off = vec2f(0.0);
    var ring_hl = 0.0;
    for (var i = 0; i < n; i++) {
        let age = bt.s[i].w;
        let clip = bt.c[i];
        if (age < 0.0 || age > 0.7 || px.x < clip.x || px.y < clip.y || px.x >= clip.x + clip.z || px.y >= clip.y + clip.w) {
            continue;
        }
        let k = age / 0.7;
        let v = px - bt.b[i].xy;
        let dd = length(v);
        let w = dd - min(bt.b[i].z, bt.b[i].w) - sqrt(k) * 40.0 * pt;
        let sigma = 3.5 * pt;
        let env = exp(-w * w / (sigma * sigma)) * (1.0 - k) * (1.0 - k);
        let freq = 1.3 / pt;
        off += v / max(dd, 1e-3) * (freq * cos(w * freq)) * env * 2.2 * pt;
        ring_hl += max(0.0, sin(w * freq)) * env * 0.14;
    }
    if (ring_hl > 0.0 || any(off != vec2f(0.0))) {
        off = cap(off, REFRACT_CAP * pt);
        col = textureSampleLevel(scene, samp, (px + off) * g.res.zw, 0.0).rgb + vec3f(ring_hl);
        touched = true;
    }

    let f = field(px);
    if (f > 10.0 * pt) {
        if (!touched) {
            discard;
        }
        return vec4f(col, 1.0);
    }
    // Merged drops blend their centre and state smoothly; taking the nearest one leaves a
    // seam where the nearest switches inside a bridge.
    var wsum = 0.0;
    var bn = vec4f(0.0);
    var sn = vec4f(0.0);
    for (var i = 0; i < n; i++) {
        let w = exp(-max(sd_btn(px, i) - f, 0.0) / (3.0 * pt));
        wsum += w;
        bn += bt.b[i] * w;
        sn += bt.s[i] * w;
    }
    bn /= wsum;
    sn /= wsum;
    let inflate = clamp(sn.z, 0.0, 1.2);
    let fs = field(px - vec2f(0.0, 2.0 * pt));
    col *= 1.0 - 0.22 * inflate * (1.0 - smoothstep(-2.0 * pt, 5.0 * pt, fs));
    col *= 1.0 - 0.12 * (1.0 - smoothstep(0.0, 0.8 * pt, abs(f - 0.4 * pt)));
    if (f < 1.0) {
        let e = 1.0;
        let grad = vec2f(field(px + vec2f(e, 0.0)) - field(px - vec2f(e, 0.0)),
                         field(px + vec2f(0.0, e)) - field(px - vec2f(0.0, e)));
        let n2 = grad / max(length(grad), 1e-4);
        let depth = max(-f, 0.0);
        let band = min(bn.z, bn.w) * 0.75;
        let m = pow(1.0 - clamp(depth / band, 0.0, 1.0), 2.2);
        let lens = 0.25 + 0.75 * inflate;
        // Magnify toward the centre so the icon under the drop reads larger, not bent.
        let o = cap(-n2 * m * 5.0 * pt - (px - bn.xy) * 0.18 * (1.0 - m), REFRACT_CAP * pt) * lens;
        let ca = n2 * m * 0.6 * pt * lens;
        var lc = vec3f(
            textureSampleLevel(scene, samp, (px + o + ca) * g.res.zw, 0.0).r,
            textureSampleLevel(scene, samp, (px + o) * g.res.zw, 0.0).g,
            textureSampleLevel(scene, samp, (px + o - ca) * g.res.zw, 0.0).b);
        lc = lc * (1.03 + 0.05 * sn.x) + vec3f(0.03, 0.036, 0.046) * lens;
        let nn = normalize(vec3f(n2 * m * 1.3 * lens, 1.0));
        let l = normalize(vec3f(-0.35, -0.9, 0.75));
        let h = normalize(l + vec3f(0.0, 0.0, 1.0));
        let nh = max(dot(nn, h), 0.0);
        let spec = (pow(nh, 90.0) * 1.1 + pow(nh, 12.0) * 0.08) * (0.3 + 0.7 * inflate);
        let caustic = smoothstep(0.2, 1.0, dot(n2, vec2f(0.25, 0.97))) * m * 0.2 * inflate;
        let rim = (1.0 - smoothstep(0.0, 1.2 * pt, depth)) * mix(0.10, 0.22, inflate);
        lc += vec3f(spec + caustic + rim);
        col = mix(col, lc, 1.0 - smoothstep(-0.75, 0.75, f));
    }
    return vec4f(col, 1.0);
}
