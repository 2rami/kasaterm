// Each pane composes its own drops, droplets, mist, bottom pool and wiper blade over the
// scene. Drop model from raindrop-fx (SardineFish, MIT): exclusion-combined normal maps,
// refraction = base + size * scale, Lambert with a shadow offset. A drop shows only its
// own pane (refraction measured in pane size, clamped inside it); the focused input row
// stays dry.

@group(0) @binding(0) var<uniform> g: G;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var scene: texture_2d<f32>;
@group(0) @binding(3) var blurred: texture_2d<f32>;
@group(0) @binding(4) var drops: texture_2d<f32>;
@group(0) @binding(5) var droplets: texture_2d<f32>;
@group(0) @binding(6) var mist: texture_2d<f32>;

const REFRACT_BASE: f32 = 0.4;
const REFRACT_SCALE: f32 = 0.6;

fn at(px: vec2f, r: vec4f) -> vec3f {
    let p = clamp(px, r.xy + 1.0, r.xy + r.zw - 1.0);
    return textureSampleLevel(scene, samp, p * g.res.zw, 0.0).rgb;
}

@fragment
fn fs_glass(in: VOut) -> @location(0) vec4f {
    let rd = textureSampleLevel(drops, samp, in.uv, 0.0);
    let dl = textureSampleLevel(droplets, samp, in.uv, 0.0);
    let comp = vec4f(rd.rgb + dl.rgb - 2.0 * rd.rgb * dl.rgb, max(rd.a, dl.a));
    let fw = max(fwidth(comp.a), 0.02);
    let px = in.uv * g.res.xy;
    let sharp = textureSampleLevel(scene, samp, in.uv, 0.0).rgb;
    let ri = region_of(px, 0.0);
    if (ri < 0 || guarded(px, ri)) {
        return vec4f(sharp, 1.0);
    }
    let r = rg.rect[ri];
    let a = rg.a[ri];
    let pt = g.t.z;

    let fog = clamp(textureSampleLevel(mist, samp, in.uv, 0.0).r, 0.0, 1.0) * a.y;
    let misted = textureSampleLevel(blurred, samp, in.uv, 0.0).rgb * 0.88 + vec3f(0.04, 0.045, 0.055);
    var col = mix(sharp, misted, fog);

    let mask = smoothstep(0.985 - fw, 0.985, comp.a);
    if (comp.a > 0.5) {
        let nxy = comp.xy / max(comp.a, 1e-3) - 0.5;
        let k = comp.z / max(comp.a, 1e-3) * REFRACT_SCALE + REFRACT_BASE;
        let n = normalize(vec3f(nxy * 2.0, 1.0));
        let l = normalize(vec3f(-1.0, -1.0, 2.0));
        let lambert = clamp(dot(l, n), 0.0, 1.0);
        let h = normalize(l + vec3f(0.0, 0.0, 1.0));
        let spec = pow(max(dot(n, h), 0.0), 70.0) * 0.35;
        var dc = at(px - nxy * k * r.w * 0.5, r) * 1.05 + vec3f(0.03, 0.036, 0.045);
        // Light from behind focuses into the lower rim; on a dark terminal this is what
        // makes a drop read as water rather than a hole.
        let edge = smoothstep(0.35, 0.95, length(nxy) * 2.0);
        dc += vec3f((lambert - 0.8) * 0.22 + spec + edge * clamp(nxy.y * 2.0, 0.0, 1.0) * 0.16);
        col = mix(col, dc, mask);
    }

    // Water pooled on the pane's bottom edge, its surface rocked by arrivals.
    let pool_h = a.z;
    if (pool_h > 0.3) {
        let amp = rg.b[ri].x;
        let wave = amp * (sin(px.x * 0.035 / pt + g.t.x * 3.1) * 0.7 + sin(px.x * 0.083 / pt - g.t.x * 4.7) * 0.3)
            + 0.35 * pt * sin(px.x * 0.021 / pt + g.t.x * 1.3);
        let surface = r.y + r.w - pool_h + wave;
        let depth = px.y - surface;
        if (depth > -1.5 * pt) {
            let wet = smoothstep(-1.0, 1.0, depth);
            let lift = min(depth * 0.3, REFRACT_CAP * pt);
            let src = vec2f(px.x + sin(px.y * 0.09 / pt + g.t.x * 2.0) * 1.2 * pt, px.y - lift);
            var water = at(src, r) * vec3f(0.8, 0.86, 0.93) + vec3f(0.025, 0.04, 0.055);
            let meniscus = 1.0 - smoothstep(0.0, 1.6 * pt, abs(depth - 0.4 * pt));
            water += vec3f(0.5, 0.58, 0.68) * meniscus * 0.55;
            water *= 1.0 - 0.18 * smoothstep(pool_h * 0.4, pool_h, depth);
            col = mix(col, water, wet);
        }
    }

    // Squeegee blade while a wipe runs.
    if (a.w >= 0.0) {
        let dy = px.y - a.w;
        let blade = 1.0 - smoothstep(1.0 * pt, 2.0 * pt, abs(dy));
        let lip = (1.0 - smoothstep(0.0, 1.2 * pt, abs(dy + 2.6 * pt))) * 0.22;
        col = mix(col, vec3f(0.07, 0.08, 0.09), blade * 0.85) + vec3f(lip);
    }
    return vec4f(col, 1.0);
}
