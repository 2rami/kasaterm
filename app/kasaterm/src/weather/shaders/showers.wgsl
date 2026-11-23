@group(0) @binding(0) var<uniform> g: G;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var frame: texture_2d<f32>;

fn grain(p: vec2f) -> f32 {
  var q = fract(p.xyx * vec3f(0.1031, 0.1030, 0.0973));
  q += vec3f(dot(q, q.yzx + 33.33));
  return fract((q.x + q.y) * q.z);
}

fn haze(p: vec2f) -> f32 {
  let cell = floor(p);
  var u = fract(p);
  u = u * u * (3.0 - 2.0 * u);
  return mix(mix(grain(cell), grain(cell + vec2f(1.0, 0.0)), u.x),
             mix(grain(cell + vec2f(0.0, 1.0)), grain(cell + 1.0), u.x), u.y);
}

fn falling(p: vec2f, time: f32, wind: f32, amount: f32, depth: f32) -> f32 {
  let pitch = mix(11.0, 41.0, depth);
  let speed = mix(180.0, 560.0, depth) * (0.75 + amount * 0.4);
  let spacing = mix(72.0, 181.0, depth);
  let laneX = (p.x - wind * p.y) / pitch;
  let grid = vec2f(laneX, (p.y - time * speed) / spacing + grain(vec2f(floor(laneX), depth * 191.0)));
  let id = floor(grid) + depth * 231.0;
  let r = grain(id);
  if (r > 0.16 + 0.67 * amount) { return 0.0; }
  let delta = (fract(grid) - vec2f(0.12 + 0.76 * grain(id + 12.0), 0.33 + 0.58 * grain(id + 17.0))) * vec2f(pitch, spacing);
  let len = mix(5.0, 33.0, depth) * (0.6 + grain(id + 6.0) * 0.8);
  let body = smoothstep(-len, 0.0, delta.y) * (1.0 - smoothstep(0.0, 1.2, delta.y));
  return body * (1.0 - smoothstep(0.0, 0.65 + depth * 0.8, abs(delta.x))) * (0.17 + depth * 0.32);
}

// The approaching drop and its spray share a landing position and a clock.
fn impact(p: vec2f, t: f32, amount: f32, wind: f32, seed: f32) -> f32 {
  if (p.y < -32.0 || p.y > 2.0) { return 0.0; }
  let pitch = 24.0;
  let lane = floor(p.x / pitch);
  var light = 0.0;
  for (var i = -1; i <= 1; i++) {
    let id = lane + f32(i);
    let phase = grain(vec2f(id, seed));
    let clock = t * (0.45 + 1.8 * amount) * (0.8 + 0.5 * phase) + phase;
    let key = vec2f(id + seed, floor(clock));
    let land = (id + 0.12 + 0.76 * grain(key)) * pitch;
    let age = (fract(clock) - 0.48) / ((0.45 + 1.8 * amount) * (0.8 + 0.5 * phase));
    let dx = p.x - land;
    if (age < 0.0) {
      let head = age * 455.0;
      let tail = p.y - head;
      light += (1.0 - smoothstep(0.05, 1.15, abs(dx - wind * p.y))) *
          smoothstep(-16.0, 0.0, tail) * (1.0 - smoothstep(0.0, 1.0, tail));
    } else if (age < 0.26) {
      light += (1.0 - smoothstep(0.0, 1.0, length(vec2f(dx * 0.28, p.y)))) *
          (1.0 - smoothstep(0.0, 0.09, age));
      for (var j = 0; j < 3; j++) {
        let r = grain(key + f32(j) * 9.7 + 3.3);
        let u = age / (0.13 + r * 0.13);
        if (u >= 1.0) { continue; }
        let vx = (f32(j) - 1.0) * (4.0 + 7.0 * r) + wind * 7.0;
        let center = vec2f(vx * u, -4.0 * u * (1.0 - u) * (2.0 + r * 8.0));
        light += (1.0 - smoothstep(0.15, 0.85 + r * 0.7, length(p - vec2f(land, 0.0) - center))) *
            (1.0 - u * u);
      }
    }
  }
  return min(light, 1.0);
}

// A bright lower rim and a dark interior keep the beads legible over both themes.
fn bead(p: vec2f, amount: f32, seed: f32) -> vec2f {
  let grid = p / vec2f(17.0, 21.0);
  let cell = floor(grid) + seed;
  let r = grain(cell);
  if (r > 0.10 + 0.38 * amount) { return vec2f(0.0); }
  var q = (fract(grid) - vec2f(0.2 + 0.6 * grain(cell + 2.0), 0.2 + 0.6 * grain(cell + 4.0)))
      * vec2f(17.0, 21.0) / (0.9 + 2.0 * grain(cell + 8.0));
  q.y = q.y * 0.76;
  let mask = 1.0 - smoothstep(0.58, 1.10, length(q));
  let rim = smoothstep(0.2, 0.86, q.y - 0.28 * q.x);
  let glint = 1.0 - smoothstep(0.0, 0.40, length(q + vec2f(0.23, 0.36)));
  return mask * vec2f(0.025 + 0.19 * smoothstep(0.0, 0.83, -q.y + 0.22 * q.x), rim * 0.52 + glint * 0.32);
}

fn atmosphere(p: vec2f, t: f32, amount: f32, wind: f32, moving: f32, opacity: f32, ink: vec3f) -> vec4f {
  let q = p / vec2f(148.0, 73.0) - vec2f(t * (0.06 + 0.11 * wind), t * 0.012);
  let cloud = 0.58 * haze(q) + 0.29 * haze(q * 2.13 + 11.0) + 0.13 * haze(q * 4.31);
  let canopy = 1.0 - smoothstep(24.0, 330.0, p.y);
  let shade = (0.10 + 0.22 * amount) * (0.23 + 0.77 * canopy) * smoothstep(0.11, 0.83, cloud) * opacity;
  let rain = moving * smoothstep(0.28, 1.0, opacity) * (falling(p, t, wind, amount, 0.05) +
      falling(p, t, wind, amount, 0.48) + falling(p, t, wind, amount, 1.0));
  let gleam = min(rain, 0.63);
  let sky = mix(vec3f(0.08, 0.14, 0.21), vec3f(0.48, 0.59, 0.70), cloud);
  return vec4f(sky * shade * (1.0 - gleam) + ink * gleam, shade + gleam * (1.0 - shade));
}

@fragment
fn fs_showers(in: VOut) -> @location(0) vec4f {
    let px = in.uv * g.res.xy;
    let base = textureSampleLevel(frame, samp, in.uv, 0.0).rgb;
    let luminance = dot(base, vec3f(0.21, 0.72, 0.07));
    let ink = mix(vec3f(0.77, 0.87, 0.96), vec3f(0.35, 0.48, 0.60), smoothstep(0.38, 0.85, luminance));
    let owner = region_any(px);
    var col = base;
    if (owner >= 0) {
        if (guarded(px, owner)) { return vec4f(base, 1.0); }
        let amount = rg.b[owner].z;
        let opacity = rg.b[owner].w;
        if (opacity > 0.0) {
            let water = bead((px - rg.rect[owner].xy) / g.t.z, amount, rg.a[owner].w) * opacity;
            let sky = atmosphere(px / g.t.z, g.t.x, amount, g.rain.y, g.rain.w, opacity, ink);
            col = base * (1.0 - sky.a) + sky.rgb;
            col = col * (1.0 - water.x - water.y) + vec3f(0.81, 0.90, 0.96) * water.y;
        }
    }
    if (g.rain.w > 0.0) {
        for (var i = 0; i < i32(rg.n.x); i++) {
            let dropAlpha = smoothstep(0.28, 1.0, rg.b[i].w);
            let r = rg.rect[i];
            let local = (px - r.xy) / g.t.z;
            if (dropAlpha <= 0.0 || local.y < -32.0 || local.y > 2.0 ||
                local.x < 8.0 || local.x > r.z / g.t.z - 8.0) { continue; }
            let hit = impact(local, g.t.x, rg.b[i].z, g.rain.y, rg.a[i].w);
            col = mix(col, ink, hit * 0.85 * dropAlpha);
        }
    }
    return vec4f(col, 1.0);
}
