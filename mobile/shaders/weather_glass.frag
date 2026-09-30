#include <flutter/runtime_effect.glsl>

// 카드 한 장의 유리 — 맺힌 물방울, 아래 테두리에 고인 물, 김, 유리 표면의 파문 고리.
// 물방울 그림(uDrops)은 raindrop-fx(SardineFish, MIT)의 법선 스프라이트를 찍은 것이고(RG = 법선,
// A = 덮임), 합성 방식도 그 compose 패스를 따랐다. 물방울은 카드 모습(uBg)을 렌즈처럼 뒤집어
// 비추고, 고인 물은 수면 높이(uPool)로 그린다. 카드 밖에는 아무것도 그리지 않는다.

uniform vec2 uSize;
uniform float uFog;
uniform float uLevel;
uniform float uPool[32];
uniform vec3 uFogColor;
// x, y, 나이(0~1), 세기. 파문은 빛 고리만 — 글자를 밀지 않는다(글 보호).
uniform vec4 uRipple[8];
uniform sampler2D uBg;
uniform sampler2D uDrops;

out vec4 fragColor;

float poolAt(float x) {
  float f = clamp(x / uSize.x, 0.0, 1.0) * 31.0;
  int i = int(floor(f));
  int j = int(min(float(i + 1), 31.0));
  return mix(uPool[i], uPool[j], fract(f));
}

void main() {
  vec2 px = FlutterFragCoord().xy;
  vec2 uv = px / uSize;
  vec4 outc = vec4(0.0);

  // 김 — 닦은 지 오래일수록 조금 뿌옇다.
  outc = vec4(uFogColor * uFog, uFog);

  for (int i = 0; i < 8; i++) {
    vec4 w = uRipple[i];
    if (w.w <= 0.0) continue;
    float k = w.z;
    float d = length(px - w.xy) - sqrt(k) * 56.0 * w.w;
    float env = exp(-d * d / 12.0) * (1.0 - k) * (1.0 - k) * w.w;
    float crest = max(0.0, sin(d * 0.9)) * env * 0.22;
    float trough = max(0.0, -sin(d * 0.9)) * env * 0.12;
    outc = outc * (1.0 - crest) + vec4(uFogColor, 1.0) * crest;
    outc = outc * (1.0 - trough) + vec4(0.0, 0.0, 0.0, 1.0) * trough;
  }

  vec4 d = texture(uDrops, uv);
  d.rgb /= max(d.a, 0.001);
  float mask = smoothstep(0.62, 0.8, d.a);
  if (mask > 0.0) {
    vec2 off = d.xy - vec2(0.5);
    vec3 n = normalize(vec3(off.x * 2.4, -off.y * 2.4, 1.0));
    vec2 ruv = clamp((px - off * 22.0) / uSize, 0.0, 1.0);
    vec3 bg = texture(uBg, ruv).rgb;
    vec3 l = normalize(vec3(-0.6, 0.8, 1.2));
    float lambert = clamp(dot(l, n), 0.0, 1.0);
    float spec = pow(clamp(dot(reflect(-l, n), vec3(0.0, 0.0, 1.0)), 0.0, 1.0), 24.0);
    // 가장자리는 어둡게 — 어두운 카드 위에서 방울 윤곽이 서려면 테가 있어야 한다.
    float rim = 1.0 - smoothstep(0.62, 0.9, d.a);
    vec3 drop = bg * (0.82 + 0.22 * lambert) + vec3(0.04, 0.06, 0.08) * lambert + vec3(spec * 0.5);
    drop *= 1.0 - rim * 0.5;
    outc = mix(outc, vec4(drop, 1.0), mask);
  }

  float surface = uSize.y - (uLevel + poolAt(px.x));
  if (uLevel > 0.05 && px.y > surface - 1.5) {
    float depth = px.y - surface;
    float slope = poolAt(px.x + 2.0) - poolAt(px.x - 2.0);
    // 물 밑 카드는 수면 기울기만큼 밀리고 푸르게 가라앉는다.
    // 고인 물이 글을 미는 폭은 7pt 까지(글 보호 — 데스크톱 연속 굴절 상한과 같다).
    vec2 wuv = clamp(vec2(px.x + clamp(slope * 6.0, -7.0, 7.0), px.y - 1.5) / uSize, 0.0, 1.0);
    vec3 under = texture(uBg, wuv).rgb * 0.55 + vec3(0.1, 0.2, 0.32);
    // 수면 바로 아래가 가장 밝고 바닥으로 갈수록 짙다 — 물의 두께가 보이게.
    under *= 1.0 - 0.35 * clamp(depth / max(uLevel, 1.0), 0.0, 1.0);
    float line = 1.0 - smoothstep(0.0, 1.4, abs(depth));
    float glow = exp(-max(depth, 0.0) / 2.5) * 0.25;
    vec3 water = under + vec3(0.6, 0.78, 0.95) * (line * 0.8 + glow);
    float a = smoothstep(-1.5, 0.5, depth);
    outc = mix(outc, vec4(water, 1.0), a);
  }
  fragColor = outc;
}
