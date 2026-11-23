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

// Premultiplied layers keep the underlying terminal and card text visible.
fn wash(color: vec3f, alpha: f32) -> vec4f {
    return vec4f(color * alpha, alpha);
}

fn overLayer(under: vec4f, top: vec4f) -> vec4f {
    return under * (1.0 - top.a) + top;
}

fn cloudField(p: vec2f) -> f32 {
    let warp = haze(p * 0.63);
    let q = p + vec2f(0.74 * warp, 0.32 * warp);
    return 0.56 * haze(q) + 0.28 * haze(q * 2.03 + 9.1) + 0.16 * haze(q * 4.19 + 3.7);
}

fn cloudBank(uv: vec2f, clock: f32, cover: f32, wind: f32) -> vec4f {
    let q = uv * vec2f(3.0, 3.8) - vec2f(clock * (0.047 + 0.09 * wind), clock * 0.009);
    let field = cloudField(q) - uv.y * 0.16;
    let threshold = mix(0.67, 0.23, cover);
    let mass = smoothstep(threshold - 0.085, threshold + 0.13, field);
    let relief = cloudField(q + vec2f(0.11, 0.18)) - field - uv.y * 0.16;
    let light = clamp(0.58 + 1.7 * relief + 0.28 * (1.0 - uv.y), 0.0, 1.0);
    let color = mix(vec3f(0.24, 0.31, 0.41), vec3f(0.83, 0.87, 0.91), light);
    return wash(color, mass * (0.18 + 0.21 * cover));
}

fn skyScene(p: vec2f, size: vec2f, clock: f32, variant: f32, wind: f32) -> vec4f {
    let uv = p / size;
    let night = variant >= 5.0 && variant <= 7.0;
    let horizon = variant >= 8.0;
    var cover = min(1.0, variant * 0.25);
    if (night) { cover = (variant - 5.0) * 0.23; }
    if (horizon) { cover = 0.24; }
    var height = 0.23 + 0.018 * sin(clock * 0.08);
    if (horizon) { height = 0.74 + 0.065 * sin(clock * 0.09); }
    let sun = (p - vec2f(0.76, height) * size) / min(size.x, size.y);
    let distanceToSun = length(sun);
    let glow = exp(-distanceToSun * 3.8);
    let disk = 1.0 - smoothstep(0.069, 0.080, distanceToSun);
    var sky = wash(vec3f(0.32, 0.67, 0.91), 0.17 * (1.0 - uv.y * 0.65));
    if (night) {
        sky = wash(vec3f(0.095, 0.14, 0.36), 0.34 - 0.13 * uv.y);
        let grid = p / 18.0;
        let cell = floor(grid);
        let star = grain(cell + 61.0);
        let offset = fract(grid) - vec2f(0.18 + 0.64 * star, 0.18 + 0.64 * grain(cell + 3.0));
        let spark = (1.0 - smoothstep(0.016, 0.059, length(offset))) * smoothstep(0.72, 0.85, star);
        sky = overLayer(sky, wash(vec3f(0.76, 0.84, 1.0), spark * (0.54 + 0.17 * sin(clock * 0.8 + star * 97.0))));
        let crescent = disk * smoothstep(0.064, 0.073, length(sun - vec2f(0.033, -0.023)));
        sky = overLayer(sky, wash(vec3f(0.88, 0.90, 1.0), crescent * 0.70 + 0.13 * glow));
    } else {
        if (horizon) {
            var band = mix(vec3f(0.51, 0.34, 0.62), vec3f(1.0, 0.50, 0.20), smoothstep(0.0, 0.82, uv.y));
            if (variant == 8.0) { band = mix(vec3f(0.43, 0.59, 0.77), vec3f(1.0, 0.74, 0.47), uv.y); }
            sky = wash(band, 0.36);
        }
        let angle = atan2(sun.y, sun.x + 0.00001);
        let rays = 0.5 + 0.5 * sin(9.0 * angle + clock * 0.12 + sin(4.0 * angle - clock * 0.09));
        let sunlight = (0.49 * disk + glow * (0.25 + 0.10 * rays)) * (1.0 - cover * cover);
        sky = overLayer(sky, wash(vec3f(1.0, 0.86, 0.55), sunlight));
    }
    if (cover > 0.0) { sky = overLayer(sky, cloudBank(uv, clock, cover, wind)); }
    return sky;
}

// A flake stays in one moving cell throughout its fall. The two depths never share a lattice.
fn flakes(p: vec2f, clock: f32, wind: f32, pitch: f32, speed: f32, density: f32, sizePx: f32, ice: bool) -> f32 {
    var q = p - vec2f(wind * speed * clock, speed * clock);
    if (!ice) { q.x += sin(p.y * 0.017 + clock * 1.3 + pitch) * pitch * 0.20; }
    let grid = q / pitch;
    let cell = floor(grid);
    let noise = grain(cell + pitch);
    let d = (fract(grid) - vec2f(0.22 + 0.56 * grain(cell + 3.0), 0.22 + 0.56 * grain(cell + 8.0))) * pitch;
    let radius = sizePx * (0.48 + noise * 0.80);
    var dist = length(d);
    if (ice) { dist = abs(d.x) + abs(d.y) * 0.76; }
    return (1.0 - smoothstep(radius * 0.3, radius + 0.55, dist)) * step(noise, density);
}

fn snowScene(p: vec2f, size: vec2f, clock: f32, variant: f32, wind: f32, amount: f32, ink: vec3f) -> vec4f {
    let uv = p / size;
    let drift = variant == 4.0 || variant == 5.0;
    let ice = variant >= 6.0;
    var sweep = wind;
    var speed = 52.0;
    var density = 0.5;
    if (variant == 0.0) { density = 0.22; }
    if (variant == 2.0 || variant == 5.0) { density = 0.93; }
    if (drift) { sweep += 1.45; speed = 83.0; }
    if (ice) { speed = 196.0; }
    var sky = cloudBank(uv, clock, 0.60, sweep);
    if (variant == 3.0) { sky = skyScene(p, size, clock, 1.0, wind); }
    let farSnow = flakes(p, clock, sweep, 16.0, speed * 0.56, density, 1.10, ice);
    var nearSnow = flakes(p + 19.0, clock, sweep, 32.0, speed, density, 2.6, ice);
    if (variant == 8.0) { nearSnow = flakes(p, clock, wind, 23.0, 330.0, 0.57, 3.7, true); }
    if (variant == 7.0) { nearSnow = nearSnow * 0.55 + falling(p, clock, wind, amount, 0.7); }
    if (variant == 5.0) {
        let veil = cloudField(p / vec2f(110.0, 17.0) - vec2f(clock * 1.3, 0.0));
        sky = overLayer(sky, wash(vec3f(0.78, 0.87, 0.94), 0.31 * veil));
    }
    return overLayer(sky, wash(ink, min(0.86, 0.29 * farSnow + 0.65 * nearSnow)));
}

fn visibilityScene(p: vec2f, size: vec2f, clock: f32, variant: f32, wind: f32) -> vec4f {
    let uv = p / size;
    let drift = clock * (0.05 + 0.14 * wind);
    let q = vec2f(uv.x * 2.7 - drift, uv.y * 5.0);
    let bank = cloudField(q);
    var color = vec3f(0.76, 0.85, 0.88);
    var alpha = 0.10 + 0.39 * smoothstep(0.27, 0.70, bank + 0.12 * uv.y);
    if (variant == 1.0) { color = vec3f(0.83, 0.71, 0.53); alpha = 0.13 + 0.20 * bank; }
    if (variant == 2.0) {
        let plume = cloudField(vec2f(q.x + 0.44 * sin(q.y * 1.5 - clock * 0.21), q.y + clock * 0.18));
        color = mix(vec3f(0.27, 0.23, 0.25), vec3f(0.77, 0.67, 0.61), plume);
        alpha = 0.10 + 0.37 * smoothstep(0.23, 0.70, plume);
    }
    if (variant == 3.0) {
        let dust = flakes(p.yx, clock, 0.08, 13.0, 147.0, 0.43, 1.0, false);
        color = vec3f(0.88, 0.61, 0.31);
        alpha = 0.14 + 0.21 * cloudField(q - vec2f(clock * 0.45, 0.0)) + dust * 0.45;
    }
    return wash(color, alpha);
}

fn windScene(p: vec2f, clock: f32, variant: f32, wind: f32, ink: vec3f) -> vec4f {
    var direction = 1.0;
    if (wind < 0.0) { direction = -1.0; }
    let travel = direction * p.x - clock * (73.0 + variant * 105.0 + abs(wind) * 60.0);
    let lift = 6.0 * sin(p.x * 0.029 + clock * 0.54) + 3.4 * sin(p.x * 0.043 - clock * 0.22);
    let grid = vec2f(travel / 116.0, (p.y + lift) / 28.0);
    let cell = floor(grid);
    let y = 0.20 + 0.60 * grain(cell);
    let x = fract(grid.x);
    let arc = (1.0 - smoothstep(0.005, 0.047, abs(fract(grid.y) - y))) * sin(x * 3.14159);
    let fade = smoothstep(0.16, 0.59, grain(cell + 4.0) + variant * 0.25);
    return wash(ink, arc * fade * (0.39 + 0.21 * variant));
}

fn stormScene(p: vec2f, size: vec2f, clock: f32, variant: f32, wind: f32, ink: vec3f, motion: f32) -> vec4f {
    let uv = p / size;
    var sky = cloudBank(uv, clock, 0.92, wind + 0.3);
    let cell = cloudField(uv * vec2f(2.4, 1.4) + vec2f(clock * 0.09, 0.0));
    let coverage = smoothstep(0.56 - 0.09 * variant, 0.72 - 0.08 * variant, cell);
    let drops = falling(p, clock, wind + 0.11, 0.7, 0.38) + falling(p, clock, wind + 0.16, 1.0, 0.90);
    sky = overLayer(sky, wash(ink, min(0.70, drops * (0.27 + coverage * 0.9))));
    let cycle = clock / (7.5 - variant * 0.72) + 0.14;
    let pulse = exp(-22.0 * fract(cycle)) * smoothstep(0.0, 0.022, fract(cycle)) * motion;
    let origin = 0.14 + 0.72 * grain(vec2f(floor(cycle), 49.0));
    let bend = 0.095 * (haze(vec2f(uv.y * 23.0, floor(cycle))) - 0.5) + 0.12 * uv.y;
    let bolt = (1.0 - smoothstep(0.001, 0.009, abs(uv.x - origin - bend))) * (1.0 - smoothstep(0.05, 0.68, uv.y));
    let glow = exp(-length((uv - vec2f(origin, 0.16)) * vec2f(2.2, 1.5)) * 5.0);
    return overLayer(sky, wash(vec3f(0.83, 0.85, 1.0), pulse * (0.53 * bolt + 0.19 * glow)));
}

fn cyclone(p: vec2f, size: vec2f, clock: f32, variant: f32, wind: f32, ink: vec3f) -> vec4f {
    let uv = p / size;
    let rel = (uv - vec2f(0.56, 0.41)) * vec2f(size.x / size.y, 1.0);
    let radius = length(rel);
    let a = atan2(rel.y, rel.x + 0.00001);
    let turbulence = cloudField(uv * 8.0 + vec2f(clock * 0.035, -clock * 0.021));
    let bands = 0.5 + 0.5 * sin(2.0 * a + 15.0 * radius + 5.0 * turbulence - clock * (0.28 + 0.13 * variant));
    let textureCloud = cloudField(uv * 4.9 + vec2f(bands * 0.6, clock * 0.045));
    var mass = smoothstep(0.13, 0.89, 0.30 * bands + 0.73 * textureCloud + 0.14 * turbulence);
    if (variant == 1.0) { mass *= smoothstep(0.038, 0.095, radius); }
    if (variant == 2.0) {
        let height = clamp(uv.y, 0.0, 1.0);
        let axis = 0.46 + 0.06 * sin(height * 7.0 + clock * 0.45);
        let width = mix(0.31, 0.026, height);
        let curl = 0.5 + 0.5 * sin(height * 65.0 + (uv.x - axis) * 20.0 - clock * 3.3);
        mass = (1.0 - smoothstep(width * 0.62, width, abs(uv.x - axis))) * (0.28 + 0.22 * curl + 0.58 * turbulence);
    }
    let color = mix(vec3f(0.20, 0.25, 0.37), vec3f(0.73, 0.79, 0.85), mass);
    let sky = wash(color, 0.055 + mass * 0.48);
    let spray = falling(p, clock, wind + 0.72, 0.9, 0.6);
    return overLayer(sky, wash(ink, spray * 0.56));
}

fn conditionLayer(p: vec2f, size: vec2f, clock: f32, material: f32, variant: f32, wind: f32, amount: f32, opacity: f32, ink: vec3f, motion: f32) -> vec4f {
    var layer = vec4f(0.0);
    if (material == 1.0) {
        layer = skyScene(p, size, clock, variant, wind);
    } else if (material == 2.0) {
        layer = visibilityScene(p, size, clock, variant, wind);
    } else if (material == 3.0) {
        layer = windScene(p, clock, variant, wind, ink);
    } else if (material == 4.0) {
        layer = snowScene(p, size, clock, variant, wind, amount, ink);
    } else if (material == 5.0) {
        layer = stormScene(p, size, clock, variant, wind, ink, motion);
    } else if (material == 6.0) {
        if (variant == 0.0) {
            layer = skyScene(p, size, clock, 0.0, wind);
            let shimmer = cloudField(p / vec2f(72.0, 13.0) + vec2f(0.0, clock * 0.37));
            layer = overLayer(layer, wash(vec3f(1.0, 0.56, 0.20), shimmer * 0.31 * (p.y / size.y)));
        } else {
            let frost = flakes(p, clock, wind, 31.0, 12.0, 0.60, 2.3, true);
            let edge = min(p, size - p);
            let rim = exp(-max(0.0, min(edge.x, edge.y)) * 0.07);
            layer = wash(vec3f(0.63, 0.84, 0.96), 0.065 + 0.28 * rim * cloudField(p / 15.0) + 0.52 * frost);
        }
    } else {
        layer = cyclone(p, size, clock, variant, wind, ink);
    }
    return layer * (opacity * (0.58 + 0.42 * amount));
}

fn rainRate(amount: f32, variant: f32) -> f32 {
    if (variant == 1.0 || variant == 4.0 || variant == 6.0) { return amount * 0.38; }
    if (variant == 2.0) { return 0.48 + 0.80 * amount; }
    return amount;
}

fn rainSky(p: vec2f, size: vec2f, clock: f32, variant: f32, wind: f32, opacity: f32) -> vec4f {
    if (variant == 3.0) { return skyScene(p, size, clock, 0.0, wind) * opacity; }
    if (variant == 6.0) { return skyScene(p, size, clock, 6.0, wind) * opacity; }
    if (variant == 4.0 || variant == 5.0) {
        let edge = min(p, size - p);
        let rim = exp(-max(0.0, min(edge.x, edge.y)) * 0.085);
        let ice = cloudField(p / 9.0);
        return wash(vec3f(0.63, 0.85, 1.0), rim * ice * opacity * 0.54);
    }
    return vec4f(0.0);
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
            let local = (px - rg.rect[owner].xy) / g.t.z;
            let size = max(rg.rect[owner].zw / g.t.z, vec2f(1.0));
            if (g.rain.x == 0.0) {
                let rate = rainRate(amount, g.rain.z);
                let water = bead(local, rate, rg.a[owner].w) * opacity;
                let backdrop = rainSky(local, size, g.t.x, g.rain.z, g.rain.y, opacity);
                let sky = overLayer(backdrop, atmosphere(px / g.t.z, g.t.x, rate, g.rain.y, g.rain.w, opacity, ink));
                col = base * (1.0 - sky.a) + sky.rgb;
                col = col * (1.0 - water.x - water.y) + vec3f(0.81, 0.90, 0.96) * water.y;
            } else {
                let sky = conditionLayer(local, size, g.t.x, g.rain.x, g.rain.z, g.rain.y, amount, opacity, ink, g.rain.w);
                col = base * (1.0 - sky.a) + sky.rgb;
            }
        }
    }
    if (g.rain.x == 0.0 && g.rain.w > 0.0) {
        for (var i = 0; i < i32(rg.n.x); i++) {
            let dropAlpha = smoothstep(0.28, 1.0, rg.b[i].w);
            let r = rg.rect[i];
            let local = (px - r.xy) / g.t.z;
            if (dropAlpha <= 0.0 || local.y < -32.0 || local.y > 2.0 ||
                local.x < 8.0 || local.x > r.z / g.t.z - 8.0) { continue; }
            let hit = impact(local, g.t.x, rainRate(rg.b[i].z, g.rain.z), g.rain.y, rg.a[i].w);
            col = mix(col, ink, hit * 0.85 * dropAlpha);
        }
    }
    return vec4f(col, 1.0);
}
