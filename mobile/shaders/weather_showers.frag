#include <flutter/runtime_effect.glsl>

uniform vec2 uOrigin;
uniform float uTime;
uniform float uAmount;
uniform float uWind;
uniform float uSeed;
uniform float uMoving;
uniform float uPass;
uniform float uDark;
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
  float shade = (0.10 + 0.22 * amount) * (0.23 + 0.77 * canopy) * smoothstep(0.11, 0.83, cloud);
  float rain = moving * (falling(p, t, wind, amount, 0.05) +
      falling(p, t, wind, amount, 0.48) + falling(p, t, wind, amount, 1.0));
  float gleam = min(rain, 0.63);
  vec3 sky = mix(vec3(0.08, 0.14, 0.21), vec3(0.48, 0.59, 0.70), cloud);
  return vec4(sky * shade * (1.0 - gleam) + ink * gleam, shade + gleam * (1.0 - shade));
}

void main() {
  vec2 screen = FlutterFragCoord().xy;
  vec2 p = screen - uOrigin;
  vec3 ink = mix(vec3(0.35, 0.48, 0.60), vec3(0.77, 0.87, 0.96), uDark);
  if (uPass > 1.5) {
    float a = impact(p, uTime, uAmount, uWind, uSeed) * 0.85 * uMoving;
    fragColor = vec4(ink * a, a);
  } else {
    vec4 sky = atmosphere(screen, uTime, uAmount, uWind, uMoving, ink);
    vec2 water = vec2(0.0);
    if (uPass > 0.5) water = bead(p, uAmount, uSeed);
    float a = water.x + water.y;
    fragColor = vec4(sky.rgb * (1.0 - a) + vec3(0.81, 0.90, 0.96) * water.y, sky.a * (1.0 - a) + a);
  }
}
