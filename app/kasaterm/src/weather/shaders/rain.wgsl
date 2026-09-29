// Falling rain in parallax layers over whichever places are rained on, and the pass that
// lays it over the app frame. Layering idea after Brian Smith's "Rain and Snow with
// Parallax" Godot shader (MIT): depth layers of scrolling hashed cells, near layers
// bigger/faster/brighter. Written from scratch.

@group(0) @binding(0) var<uniform> g: G;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var frame: texture_2d<f32>;
@group(0) @binding(3) var rain: texture_2d<f32>;

const LAYERS: i32 = 6;

// Streak density and brightness where this pixel is.
fn weather_at(px: vec2f) -> vec2f {
    let ri = region_any(px);
    if (ri < 0) {
        return rg.n.yz;
    }
    if (guarded(px, ri)) {
        return vec2f(0.0);
    }
    return rg.b[ri].zw;
}

// x: added light, y: sideways bend in px (a streak is a thin water cylinder).
fn streaks(px: vec2f, density: f32, alpha: f32) -> vec2f {
    let pt = g.t.z;
    let time = g.t.x;
    let speed_k = g.rain.x;
    var light = 0.0;
    var bend = 0.0;
    for (var i = 0; i < LAYERS; i++) {
        let d = f32(i) / f32(LAYERS - 1);
        let cell = vec2f(mix(7.0, 33.0, d), mix(48.0, 240.0, d)) * pt;
        let speed = mix(390.0, 1300.0, d) * pt * speed_k;
        let len = mix(9.0, 90.0, d) * pt * (0.55 + 0.45 * speed_k);
        let half_w = mix(0.25, 1.0, d) * pt;
        // Shear so the streaks lean with the wind; in sheared space the fall is straight down.
        var p = vec2f(px.x - px.y * g.rain.y + f32(i) * 37.0 * pt, px.y);
        p.y -= time * speed;
        let c = floor(p / cell);
        let f = p - c * cell;
        let r = rand3(vec3i(i32(c.x), i32(c.y), i + 11));
        if (r.z > min(mix(0.5, 0.26, d) * density, 0.92)) {
            continue;
        }
        let l = len * mix(0.55, 1.0, fract(r.x * 13.7 + r.y * 5.3));
        let x0 = mix(0.2, 0.8, r.x) * cell.x;
        let y0 = r.y * max(cell.y - l, 0.0);
        let yy = clamp(f.y, y0, y0 + l);
        let q = vec2f(f.x - x0, f.y - yy);
        let along = (yy - y0) / l;
        let m = (1.0 - smoothstep(half_w, half_w + 1.0, length(q))) * (0.15 + 0.85 * along * along);
        light += m * mix(0.08, 0.34, d) * alpha;
        bend += clamp(q.x / max(half_w, 0.5), -1.0, 1.0) * m * mix(0.6, 3.0, d) * pt;
    }
    return vec2f(light, bend);
}

// Half resolution: streaks are motion-blurred anyway. Out: light, sideways offset px, density.
@fragment
fn fs_rain(in: VOut) -> @location(0) vec4f {
    let px = in.uv * g.res.xy;
    let w = weather_at(px);
    if (w.x <= 0.001) {
        return vec4f(0.0);
    }
    let s = streaks(px, w.x, w.y);
    return vec4f(s.x, s.y, w.x, 0.0);
}

@fragment
fn fs_scene(in: VOut) -> @location(0) vec4f {
    let r = textureSampleLevel(rain, samp, in.uv, 0.0);
    let mood = clamp(r.z / 2.2, 0.0, 1.0);
    let bend = cap(vec2f(r.y, 0.0), REFRACT_CAP * g.t.z);
    var col = textureSampleLevel(frame, samp, in.uv + bend * g.res.zw, 0.0).rgb;
    col *= mix(vec3f(1.0), vec3f(0.8, 0.84, 0.92), mood);
    col += vec3f(0.72, 0.8, 0.9) * r.x;
    return vec4f(col, 1.0);
}

@fragment
fn fs_copy(in: VOut) -> @location(0) vec4f {
    return vec4f(textureSampleLevel(frame, samp, in.uv, 0.0).rgb, 1.0);
}
