#include <flutter/runtime_effect.glsl>

// 떨어지는 빗줄기 여러 겹과 바닥에 닿은 자리의 파문. 투명 바탕 위에 그린다(날씨 「빗줄기」).
// 빗줄기는 「Rain and Snow with Parallax Effect」(Brian Smith, godotshaders.com, MIT)의
// 기법을 옮겼다 — 비스듬히 자른 세로 칸마다 난수 하나를 뽑아 그 값으로 길이·굵기·투명도·
// 속도를 함께 정하면, 난수가 곧 깊이가 되어 겹이 생긴다. 겹 셋, 칸마다 있을지 없을지,
// 기기 픽셀 기준 가장자리 부드럽게, 바닥 파문은 카사텀 자체 구현(MIT).

uniform vec2 uSize;
uniform float uTime;
uniform float uDpr;
uniform float uAmount;
uniform float uSlant;
uniform float uGround;
uniform float uSpeed;
uniform float uLength;
uniform vec3 uColor;

out vec4 fragColor;

float hash(float n) {
  return fract(sin(n * 91.3458) * 47453.5453);
}

float hash2(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

// 겹 하나. cols = 화면 폭에 드는 칸 수, per = 한 칸에 세로로 드는 빗방울 수.
float layer(vec2 px, float cols, float per, float speed, float len, float width, float alpha, float seed) {
  float slantPx = px.x - px.y * uSlant;
  float colW = uSize.x / cols;
  float col = floor(slantPx / colW);
  float rn = hash(col + seed);
  if (hash(col * 1.37 + seed + 7.1) > uAmount) return 0.0;

  // 칸 가운데로부터의 거리(기기 픽셀). 난수가 칸 안 위치도 조금 흔든다.
  float center = (0.5 + (rn - 0.5) * 0.6) * colW;
  float d = abs(slantPx - col * colW - center) * uDpr;
  float w = width * (0.6 + 0.8 * rn) * uDpr;
  float edge = clamp(w + 0.5 - d, 0.0, 1.0);
  if (edge <= 0.0) return 0.0;

  float y = px.y / uSize.y * per;
  float v = fract(y + rn * 7.0 - uTime * speed * uSpeed * (0.75 + 0.5 * rn) * per);
  float l = len * uLength * per * (0.7 + 0.6 * rn);
  float streak = smoothstep(1.0 - l, 1.0, v) * step(v, 0.995);
  return streak * edge * alpha * (0.6 + 0.4 * rn);
}

// 바닥 칸마다 빗방울 하나가 제 주기로 떨어져 납작한 고리를 퍼뜨린다.
float splashes(vec2 px) {
  float top = uSize.y * uGround;
  if (px.y < top - 8.0) return 0.0;
  // 멀수록(위쪽) 칸이 작다 — 원근.
  float depth = clamp((px.y - top) / (uSize.y - top), 0.0, 1.0);
  float scale = mix(0.45, 1.0, depth);
  vec2 cell = vec2(64.0, 26.0) * scale;
  vec2 g = vec2(px.x, px.y - top) / cell;
  vec2 id0 = floor(g);
  float acc = 0.0;
  for (int j = -1; j <= 1; j++) {
    for (int i = -1; i <= 1; i++) {
      vec2 id = id0 + vec2(float(i), float(j));
      float h = hash2(id + 3.7);
      if (h > uAmount * 0.9 + 0.05) continue;
      float period = 0.7 + hash2(id + 11.1) * 0.9;
      float t = uTime / period + hash2(id + 23.9);
      float cycle = floor(t);
      float age = fract(t);
      vec2 jitter = vec2(hash2(id + cycle * 1.31), hash2(id + cycle * 2.17 + 5.0));
      vec2 c = (id + 0.15 + jitter * 0.7) * cell;
      vec2 dv = vec2(px.x, px.y - top) - c;
      dv.y /= 0.32;
      float r = age * cell.x * 0.55;
      float dist = length(dv);
      float ring = 1.0 - smoothstep(0.0, 1.4 * scale + 0.6, abs(dist - r));
      float fade = (1.0 - age) * (1.0 - age);
      // 두 번째 고리가 조금 늦게 따라온다.
      float r2 = max(age - 0.18, 0.0) * cell.x * 0.45;
      float ring2 = (1.0 - smoothstep(0.0, 1.2 * scale + 0.6, abs(dist - r2))) * step(0.18, age);
      float hit = (1.0 - smoothstep(0.0, 2.5 * scale, dist)) * (1.0 - smoothstep(0.0, 0.12, age));
      acc += (ring + ring2 * 0.5) * fade * 0.55 + hit * 0.8;
    }
  }
  return acc * smoothstep(top - 8.0, top + 24.0, px.y) * mix(0.55, 1.0, depth);
}

void main() {
  vec2 px = FlutterFragCoord().xy;
  float a = 0.0;
  a += layer(px, 150.0, 1.6, 1.1, 0.05, 0.28, 0.22, 11.0);
  a += layer(px, 80.0, 1.2, 1.6, 0.08, 0.45, 0.32, 29.0);
  a += layer(px, 36.0, 0.9, 2.4, 0.12, 0.7, 0.42, 53.0);
  a += splashes(px);
  a = clamp(a, 0.0, 1.0);
  fragColor = vec4(uColor * a, a);
}
