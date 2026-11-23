#include <flutter/runtime_effect.glsl>

uniform vec2 uOrigin;
uniform float uTime;
uniform float uAmount;
uniform float uWind;
uniform float uSeed;
uniform float uMoving;
uniform float uPass;
uniform float uDark;
uniform float uOpacity;
uniform float uMaterial;
uniform float uVariant;
uniform vec2 uSize;
out vec4 fragColor;

float grain(vec2 p) {
  vec3 q = fract(p.xyx * vec3(0.1031, 0.1030, 0.0973));
  q += dot(q, q.yzx + 33.33);
  return fract((q.x + q.y) * q.z);
}

float haze(vec2 p) {
  vec2 cell = floor(p);
  vec2 u = fract(p);
  u = u * u * (3.0 - 2.0 * u);
  return mix(mix(grain(cell), grain(cell + vec2(1.0, 0.0)), u.x),
             mix(grain(cell + vec2(0.0, 1.0)), grain(cell + 1.0), u.x), u.y);
}

float falling(vec2 p, float time, float wind, float amount, float depth) {
  float pitch = mix(11.0, 41.0, depth);
  float speed = mix(180.0, 560.0, depth) * (0.75 + amount * 0.4);
  float spacing = mix(72.0, 181.0, depth);
  float laneX = (p.x - wind * p.y) / pitch;
  vec2 grid = vec2(laneX, (p.y - time * speed) / spacing + grain(vec2(floor(laneX), depth * 191.0)));
  vec2 id = floor(grid) + depth * 231.0;
  float r = grain(id);
  if (r > 0.16 + 0.67 * amount) return 0.0;
  vec2 delta = (fract(grid) - vec2(0.12 + 0.76 * grain(id + 12.0), 0.33 + 0.58 * grain(id + 17.0))) * vec2(pitch, spacing);
  float len = mix(5.0, 33.0, depth) * (0.6 + grain(id + 6.0) * 0.8);
  float body = smoothstep(-len, 0.0, delta.y) * (1.0 - smoothstep(0.0, 1.2, delta.y));
  return body * (1.0 - smoothstep(0.0, 0.65 + depth * 0.8, abs(delta.x))) * (0.17 + depth * 0.32);
}

// The approaching drop and its spray share a landing position and a clock.
float impact(vec2 p, float t, float amount, float wind, float seed) {
  if (p.y < -32.0 || p.y > 2.0) return 0.0;
  float pitch = 24.0;
  float lane = floor(p.x / pitch);
  float light = 0.0;
  for (int i = -1; i <= 1; i++) {
    float id = lane + float(i);
    float phase = grain(vec2(id, seed));
    float clock = t * (0.45 + 1.8 * amount) * (0.8 + 0.5 * phase) + phase;
    vec2 key = vec2(id + seed, floor(clock));
    float land = (id + 0.12 + 0.76 * grain(key)) * pitch;
    float age = (fract(clock) - 0.48) / ((0.45 + 1.8 * amount) * (0.8 + 0.5 * phase));
    float dx = p.x - land;
    if (age < 0.0) {
      float head = age * 455.0;
      float tail = p.y - head;
      light += (1.0 - smoothstep(0.05, 1.15, abs(dx - wind * p.y))) *
          smoothstep(-16.0, 0.0, tail) * (1.0 - smoothstep(0.0, 1.0, tail));
    } else if (age < 0.26) {
      light += (1.0 - smoothstep(0.0, 1.0, length(vec2(dx * 0.28, p.y)))) *
          (1.0 - smoothstep(0.0, 0.09, age));
      for (int j = 0; j < 3; j++) {
        float r = grain(key + float(j) * 9.7 + 3.3);
        float u = age / (0.13 + r * 0.13);
        if (u >= 1.0) continue;
        float vx = (float(j) - 1.0) * (4.0 + 7.0 * r) + wind * 7.0;
        vec2 center = vec2(vx * u, -4.0 * u * (1.0 - u) * (2.0 + r * 8.0));
        light += (1.0 - smoothstep(0.15, 0.85 + r * 0.7, length(p - vec2(land, 0.0) - center))) *
            (1.0 - u * u);
      }
    }
  }
  return min(light, 1.0);
}

// A bright lower rim and a dark interior keep the beads legible over both themes.
vec2 bead(vec2 p, float amount, float seed) {
  vec2 grid = p / vec2(17.0, 21.0);
  vec2 cell = floor(grid) + seed;
  float r = grain(cell);
  if (r > 0.10 + 0.38 * amount) return vec2(0.0);
  vec2 q = (fract(grid) - vec2(0.2 + 0.6 * grain(cell + 2.0), 0.2 + 0.6 * grain(cell + 4.0)))
      * vec2(17.0, 21.0) / (0.9 + 2.0 * grain(cell + 8.0));
  q.y *= 0.76;
  float mask = 1.0 - smoothstep(0.58, 1.10, length(q));
  float rim = smoothstep(0.2, 0.86, q.y - 0.28 * q.x);
  float glint = 1.0 - smoothstep(0.0, 0.40, length(q + vec2(0.23, 0.36)));
  return mask * vec2(0.025 + 0.19 * smoothstep(0.0, 0.83, -q.y + 0.22 * q.x), rim * 0.52 + glint * 0.32);
}

vec4 atmosphere(vec2 p, float t, float amount, float wind, float moving, vec3 ink) {
  vec2 q = p / vec2(148.0, 73.0) - vec2(t * (0.06 + 0.11 * wind), t * 0.012);
  float cloud = 0.58 * haze(q) + 0.29 * haze(q * 2.13 + 11.0) + 0.13 * haze(q * 4.31);
  float canopy = 1.0 - smoothstep(24.0, 330.0, p.y);
  float shade = (0.10 + 0.22 * amount) * (0.23 + 0.77 * canopy) * smoothstep(0.11, 0.83, cloud) * uOpacity;
  float rain = moving * smoothstep(0.28, 1.0, uOpacity) * (falling(p, t, wind, amount, 0.05) +
      falling(p, t, wind, amount, 0.48) + falling(p, t, wind, amount, 1.0));
  float gleam = min(rain, 0.63);
  vec3 sky = mix(vec3(0.08, 0.14, 0.21), vec3(0.48, 0.59, 0.70), cloud);
  return vec4(sky * shade * (1.0 - gleam) + ink * gleam, shade + gleam * (1.0 - shade));
}

// Premultiplied layers keep the underlying terminal and card text visible.
vec4 wash(vec3 color, float alpha) {
    return vec4(color * alpha, alpha);
}

vec4 overLayer(vec4 under, vec4 top) {
    return under * (1.0 - top.a) + top;
}

float cloudField(vec2 p) {
    float warp = haze(p * 0.63);
    vec2 q = p + vec2(0.74 * warp, 0.32 * warp);
    return 0.56 * haze(q) + 0.28 * haze(q * 2.03 + 9.1) + 0.16 * haze(q * 4.19 + 3.7);
}

vec4 cloudBank(vec2 uv, float clock, float cover, float wind) {
    vec2 q = uv * vec2(3.0, 3.8) - vec2(clock * (0.047 + 0.09 * wind), clock * 0.009);
    float field = cloudField(q) - uv.y * 0.16;
    float threshold = mix(0.67, 0.23, cover);
    float mass = smoothstep(threshold - 0.085, threshold + 0.13, field);
    float relief = cloudField(q + vec2(0.11, 0.18)) - field - uv.y * 0.16;
    float light = clamp(0.58 + 1.7 * relief + 0.28 * (1.0 - uv.y), 0.0, 1.0);
    vec3 color = mix(vec3(0.24, 0.31, 0.41), vec3(0.83, 0.87, 0.91), light);
    return wash(color, mass * (0.18 + 0.21 * cover));
}

vec4 skyScene(vec2 p, vec2 size, float clock, float variant, float wind) {
    vec2 uv = p / size;
    bool night = variant >= 5.0 && variant <= 7.0;
    bool horizon = variant >= 8.0;
    float cover = min(1.0, variant * 0.25);
    if (night) { cover = (variant - 5.0) * 0.23; }
    if (horizon) { cover = 0.24; }
    float height = 0.23 + 0.018 * sin(clock * 0.08);
    if (horizon) { height = 0.74 + 0.065 * sin(clock * 0.09); }
    vec2 sun = (p - vec2(0.76, height) * size) / min(size.x, size.y);
    float distanceToSun = length(sun);
    float glow = exp(-distanceToSun * 3.8);
    float disk = 1.0 - smoothstep(0.069, 0.080, distanceToSun);
    vec4 sky = wash(vec3(0.32, 0.67, 0.91), 0.17 * (1.0 - uv.y * 0.65));
    if (night) {
        sky = wash(vec3(0.095, 0.14, 0.36), 0.34 - 0.13 * uv.y);
        vec2 grid = p / 18.0;
        vec2 cell = floor(grid);
        float star = grain(cell + 61.0);
        vec2 offset = fract(grid) - vec2(0.18 + 0.64 * star, 0.18 + 0.64 * grain(cell + 3.0));
        float spark = (1.0 - smoothstep(0.016, 0.059, length(offset))) * smoothstep(0.72, 0.85, star);
        sky = overLayer(sky, wash(vec3(0.76, 0.84, 1.0), spark * (0.54 + 0.17 * sin(clock * 0.8 + star * 97.0))));
        float crescent = disk * smoothstep(0.064, 0.073, length(sun - vec2(0.033, -0.023)));
        sky = overLayer(sky, wash(vec3(0.88, 0.90, 1.0), crescent * 0.70 + 0.13 * glow));
    } else {
        if (horizon) {
            vec3 band = mix(vec3(0.51, 0.34, 0.62), vec3(1.0, 0.50, 0.20), smoothstep(0.0, 0.82, uv.y));
            if (variant == 8.0) { band = mix(vec3(0.43, 0.59, 0.77), vec3(1.0, 0.74, 0.47), uv.y); }
            sky = wash(band, 0.36);
        }
        float angle = atan(sun.y, sun.x + 0.00001);
        float rays = 0.5 + 0.5 * sin(9.0 * angle + clock * 0.12 + sin(4.0 * angle - clock * 0.09));
        float sunlight = (0.49 * disk + glow * (0.25 + 0.10 * rays)) * (1.0 - cover * cover);
        sky = overLayer(sky, wash(vec3(1.0, 0.86, 0.55), sunlight));
    }
    if (cover > 0.0) { sky = overLayer(sky, cloudBank(uv, clock, cover, wind)); }
    return sky;
}

// A flake stays in one moving cell throughout its fall. The two depths never share a lattice.
float flakes(vec2 p, float clock, float wind, float pitch, float speed, float density, float sizePx, bool ice) {
    vec2 q = p - vec2(wind * speed * clock, speed * clock);
    if (!ice) { q.x += sin(p.y * 0.017 + clock * 1.3 + pitch) * pitch * 0.20; }
    vec2 grid = q / pitch;
    vec2 cell = floor(grid);
    float noise = grain(cell + pitch);
    vec2 d = (fract(grid) - vec2(0.22 + 0.56 * grain(cell + 3.0), 0.22 + 0.56 * grain(cell + 8.0))) * pitch;
    float radius = sizePx * (0.48 + noise * 0.80);
    float dist = length(d);
    if (ice) { dist = abs(d.x) + abs(d.y) * 0.76; }
    return (1.0 - smoothstep(radius * 0.3, radius + 0.55, dist)) * step(noise, density);
}

vec4 snowScene(vec2 p, vec2 size, float clock, float variant, float wind, float amount, vec3 ink) {
    vec2 uv = p / size;
    bool drift = variant == 4.0 || variant == 5.0;
    bool ice = variant >= 6.0;
    float sweep = wind;
    float speed = 52.0;
    float density = 0.5;
    if (variant == 0.0) { density = 0.22; }
    if (variant == 2.0 || variant == 5.0) { density = 0.93; }
    if (drift) { sweep += 1.45; speed = 83.0; }
    if (ice) { speed = 196.0; }
    vec4 sky = cloudBank(uv, clock, 0.60, sweep);
    if (variant == 3.0) { sky = skyScene(p, size, clock, 1.0, wind); }
    float farSnow = flakes(p, clock, sweep, 16.0, speed * 0.56, density, 1.10, ice);
    float nearSnow = flakes(p + 19.0, clock, sweep, 32.0, speed, density, 2.6, ice);
    if (variant == 8.0) { nearSnow = flakes(p, clock, wind, 23.0, 330.0, 0.57, 3.7, true); }
    if (variant == 7.0) { nearSnow = nearSnow * 0.55 + falling(p, clock, wind, amount, 0.7); }
    if (variant == 5.0) {
        float veil = cloudField(p / vec2(110.0, 17.0) - vec2(clock * 1.3, 0.0));
        sky = overLayer(sky, wash(vec3(0.78, 0.87, 0.94), 0.31 * veil));
    }
    return overLayer(sky, wash(ink, min(0.86, 0.29 * farSnow + 0.65 * nearSnow)));
}

vec4 visibilityScene(vec2 p, vec2 size, float clock, float variant, float wind) {
    vec2 uv = p / size;
    float drift = clock * (0.05 + 0.14 * wind);
    vec2 q = vec2(uv.x * 2.7 - drift, uv.y * 5.0);
    float bank = cloudField(q);
    vec3 color = vec3(0.76, 0.85, 0.88);
    float alpha = 0.10 + 0.39 * smoothstep(0.27, 0.70, bank + 0.12 * uv.y);
    if (variant == 1.0) { color = vec3(0.83, 0.71, 0.53); alpha = 0.13 + 0.20 * bank; }
    if (variant == 2.0) {
        float plume = cloudField(vec2(q.x + 0.44 * sin(q.y * 1.5 - clock * 0.21), q.y + clock * 0.18));
        color = mix(vec3(0.27, 0.23, 0.25), vec3(0.77, 0.67, 0.61), plume);
        alpha = 0.10 + 0.37 * smoothstep(0.23, 0.70, plume);
    }
    if (variant == 3.0) {
        float dust = flakes(p.yx, clock, 0.08, 13.0, 147.0, 0.43, 1.0, false);
        color = vec3(0.88, 0.61, 0.31);
        alpha = 0.14 + 0.21 * cloudField(q - vec2(clock * 0.45, 0.0)) + dust * 0.45;
    }
    return wash(color, alpha);
}

vec4 windScene(vec2 p, float clock, float variant, float wind, vec3 ink) {
    float direction = 1.0;
    if (wind < 0.0) { direction = -1.0; }
    float travel = direction * p.x - clock * (73.0 + variant * 105.0 + abs(wind) * 60.0);
    float lift = 6.0 * sin(p.x * 0.029 + clock * 0.54) + 3.4 * sin(p.x * 0.043 - clock * 0.22);
    vec2 grid = vec2(travel / 116.0, (p.y + lift) / 28.0);
    vec2 cell = floor(grid);
    float y = 0.20 + 0.60 * grain(cell);
    float x = fract(grid.x);
    float arc = (1.0 - smoothstep(0.005, 0.047, abs(fract(grid.y) - y))) * sin(x * 3.14159);
    float fade = smoothstep(0.16, 0.59, grain(cell + 4.0) + variant * 0.25);
    return wash(ink, arc * fade * (0.39 + 0.21 * variant));
}

vec4 stormScene(vec2 p, vec2 size, float clock, float variant, float wind, vec3 ink, float motion) {
    vec2 uv = p / size;
    vec4 sky = cloudBank(uv, clock, 0.92, wind + 0.3);
    float cell = cloudField(uv * vec2(2.4, 1.4) + vec2(clock * 0.09, 0.0));
    float coverage = smoothstep(0.56 - 0.09 * variant, 0.72 - 0.08 * variant, cell);
    float drops = falling(p, clock, wind + 0.11, 0.7, 0.38) + falling(p, clock, wind + 0.16, 1.0, 0.90);
    sky = overLayer(sky, wash(ink, min(0.70, drops * (0.27 + coverage * 0.9))));
    float cycle = clock / (7.5 - variant * 0.72) + 0.14;
    float pulse = exp(-22.0 * fract(cycle)) * smoothstep(0.0, 0.022, fract(cycle)) * motion;
    float origin = 0.14 + 0.72 * grain(vec2(floor(cycle), 49.0));
    float bend = 0.095 * (haze(vec2(uv.y * 23.0, floor(cycle))) - 0.5) + 0.12 * uv.y;
    float bolt = (1.0 - smoothstep(0.001, 0.009, abs(uv.x - origin - bend))) * (1.0 - smoothstep(0.05, 0.68, uv.y));
    float glow = exp(-length((uv - vec2(origin, 0.16)) * vec2(2.2, 1.5)) * 5.0);
    return overLayer(sky, wash(vec3(0.83, 0.85, 1.0), pulse * (0.53 * bolt + 0.19 * glow)));
}

vec4 cyclone(vec2 p, vec2 size, float clock, float variant, float wind, vec3 ink) {
    vec2 uv = p / size;
    vec2 rel = (uv - vec2(0.56, 0.41)) * vec2(size.x / size.y, 1.0);
    float radius = length(rel);
    float a = atan(rel.y, rel.x + 0.00001);
    float turbulence = cloudField(uv * 8.0 + vec2(clock * 0.035, -clock * 0.021));
    float bands = 0.5 + 0.5 * sin(2.0 * a + 15.0 * radius + 5.0 * turbulence - clock * (0.28 + 0.13 * variant));
    float textureCloud = cloudField(uv * 4.9 + vec2(bands * 0.6, clock * 0.045));
    float mass = smoothstep(0.13, 0.89, 0.30 * bands + 0.73 * textureCloud + 0.14 * turbulence);
    if (variant == 1.0) { mass *= smoothstep(0.038, 0.095, radius); }
    if (variant == 2.0) {
        float height = clamp(uv.y, 0.0, 1.0);
        float axis = 0.46 + 0.06 * sin(height * 7.0 + clock * 0.45);
        float width = mix(0.31, 0.026, height);
        float curl = 0.5 + 0.5 * sin(height * 65.0 + (uv.x - axis) * 20.0 - clock * 3.3);
        mass = (1.0 - smoothstep(width * 0.62, width, abs(uv.x - axis))) * (0.28 + 0.22 * curl + 0.58 * turbulence);
    }
    vec3 color = mix(vec3(0.20, 0.25, 0.37), vec3(0.73, 0.79, 0.85), mass);
    vec4 sky = wash(color, 0.055 + mass * 0.48);
    float spray = falling(p, clock, wind + 0.72, 0.9, 0.6);
    return overLayer(sky, wash(ink, spray * 0.56));
}

vec4 conditionLayer(vec2 p, vec2 size, float clock, float material, float variant, float wind, float amount, float opacity, vec3 ink, float motion) {
    vec4 layer = vec4(0.0);
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
            float shimmer = cloudField(p / vec2(72.0, 13.0) + vec2(0.0, clock * 0.37));
            layer = overLayer(layer, wash(vec3(1.0, 0.56, 0.20), shimmer * 0.31 * (p.y / size.y)));
        } else {
            float frost = flakes(p, clock, wind, 31.0, 12.0, 0.60, 2.3, true);
            vec2 edge = min(p, size - p);
            float rim = exp(-max(0.0, min(edge.x, edge.y)) * 0.07);
            layer = wash(vec3(0.63, 0.84, 0.96), 0.065 + 0.28 * rim * cloudField(p / 15.0) + 0.52 * frost);
        }
    } else {
        layer = cyclone(p, size, clock, variant, wind, ink);
    }
    return layer * (opacity * (0.58 + 0.42 * amount));
}

float rainRate(float amount, float variant) {
    if (variant == 1.0 || variant == 4.0 || variant == 6.0) { return amount * 0.38; }
    if (variant == 2.0) { return 0.48 + 0.80 * amount; }
    return amount;
}

vec4 rainSky(vec2 p, vec2 size, float clock, float variant, float wind, float opacity) {
    if (variant == 3.0) { return skyScene(p, size, clock, 0.0, wind) * opacity; }
    if (variant == 6.0) { return skyScene(p, size, clock, 6.0, wind) * opacity; }
    if (variant == 4.0 || variant == 5.0) {
        vec2 edge = min(p, size - p);
        float rim = exp(-max(0.0, min(edge.x, edge.y)) * 0.085);
        float ice = cloudField(p / 9.0);
        return wash(vec3(0.63, 0.85, 1.0), rim * ice * opacity * 0.54);
    }
    return vec4(0.0);
}

void main() {
  vec2 screen = FlutterFragCoord().xy;
  vec2 p = screen - uOrigin;
  vec3 ink = mix(vec3(0.35, 0.48, 0.60), vec3(0.77, 0.87, 0.96), uDark);
  vec2 size = max(uSize, vec2(1.0));
  if (uMaterial != 0.0) {
    fragColor = conditionLayer(p, size, uTime, uMaterial, uVariant, uWind, uAmount, uOpacity, ink, uMoving);
    return;
  }
  float rate = rainRate(uAmount, uVariant);
  if (uPass > 1.5) {
    float a = impact(p, uTime, rate, uWind, uSeed) * 0.85 * uMoving * smoothstep(0.28, 1.0, uOpacity);
    fragColor = vec4(ink * a, a);
  } else {
    vec4 sky = overLayer(rainSky(p, size, uTime, uVariant, uWind, uOpacity), atmosphere(screen, uTime, rate, uWind, uMoving, ink));
    vec2 water = vec2(0.0);
    if (uPass > 0.5) water = bead(p, rate, uSeed) * uOpacity;
    float a = water.x + water.y;
    fragColor = vec4(sky.rgb * (1.0 - a) + vec3(0.81, 0.90, 0.96) * water.y, sky.a * (1.0 - a) + a);
  }
}
