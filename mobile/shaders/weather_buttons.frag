#include <flutter/runtime_effect.glsl>

// 단추마다 올라앉은 물방울 — 데스크톱 weather/shaders/buttons.wgsl 과 같은 모양을 뒤를 비추지
// 않는 막으로 그린다(단추 글자를 굴절시키지 않는다 — 글 보호). 한 줄에 붙은 단추는 매끈한 최솟값
// 거리장(Inigo Quilez 의 다항 smooth-min)으로 이어지고, 누르면 찌그러지며 고리가 퍼지고,
// 꺼진 단추는 납작한 막이다.

uniform vec2 uSize;
uniform float uCount;
uniform vec3 uTint;
uniform float uDark;
// 가운데 x, y, 반폭, 반높이.
uniform vec4 uB[24];
// 눌림, 부풂(켜짐 1 · 꺼짐 0), 고리 나이(0~1, 없으면 음수), 줄 번호.
uniform vec4 uS[24];

out vec4 fragColor;

float smin(float a, float b, float k) {
  float h = max(k - abs(a - b), 0.0) / k;
  return min(a, b) - h * h * k * 0.25;
}

float sdRoundBox(vec2 p, vec2 b, float r) {
  vec2 q = abs(p) - b + r;
  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float sdBtn(vec2 p, int i) {
  vec4 b = uB[i];
  vec4 s = uS[i];
  float off = 1.0 - clamp(s.y, 0.0, 1.2);
  float sx = 1.0 + 0.16 * s.x + 0.1 * off;
  float sy = max(1.0 - 0.24 * s.x - 0.3 * off, 0.3);
  // 아래쪽에 앉아 있으니 찌그러질 때 바닥은 그대로다.
  vec2 c = vec2(b.x, b.y + b.w * (1.0 - sy));
  vec2 q = (p - c) / vec2(sx, sy);
  return sdRoundBox(q, b.zw, min(min(b.z, b.w), 12.0)) * min(sx, sy);
}

float field(vec2 p) {
  int n = int(uCount);
  float total = 1e9;
  float acc = 1e9;
  float row = -1.0;
  for (int i = 0; i < 24; i++) {
    if (i >= n) break;
    float d = sdBtn(p, i);
    float ri = uS[i].w;
    if (ri != row) {
      total = min(total, acc);
      acc = d;
      row = ri;
    } else {
      acc = smin(acc, d, 10.0);
    }
  }
  return min(total, acc);
}

void main() {
  vec2 px = FlutterFragCoord().xy;
  int n = int(uCount);
  vec4 outc = vec4(0.0);

  for (int i = 0; i < 24; i++) {
    if (i >= n) break;
    float age = uS[i].z;
    if (age < 0.0 || age > 1.0) continue;
    vec4 b = uB[i];
    float w = length(px - b.xy) - min(b.z, b.w) - sqrt(age) * 34.0;
    float env = exp(-w * w / 10.0) * (1.0 - age) * (1.0 - age);
    float a = max(0.0, sin(w * 1.1)) * env * 0.35;
    outc = outc * (1.0 - a) + vec4(uTint, 1.0) * a;
  }

  float f = field(px);
  if (f > 8.0) {
    fragColor = outc;
    return;
  }
  // 이어진 방울은 가까운 단추들의 상태를 섞는다 — 가장 가까운 것만 쓰면 목 한가운데서 이음매가 진다.
  float wsum = 0.0;
  vec4 sn = vec4(0.0);
  vec4 bn = vec4(0.0);
  for (int i = 0; i < 24; i++) {
    if (i >= n) break;
    float w = exp(-max(sdBtn(px, i) - f, 0.0) / 3.0);
    wsum += w;
    sn += uS[i] * w;
    bn += uB[i] * w;
  }
  sn /= wsum;
  bn /= wsum;
  float inflate = clamp(sn.y, 0.0, 1.2);

  // 방울 밑 그림자 — 떠 있는 물은 아래로 조금 그늘을 떨군다.
  float fs = field(px - vec2(0.0, 2.0));
  float shade = (1.0 - smoothstep(-1.0, 5.0, fs)) * step(0.0, f) * 0.16 * inflate;
  outc = outc * (1.0 - shade) + vec4(0.0, 0.0, 0.0, 1.0) * shade;

  if (f < 1.0) {
    float e = 1.0;
    vec2 grad = vec2(field(px + vec2(e, 0.0)) - field(px - vec2(e, 0.0)),
                     field(px + vec2(0.0, e)) - field(px - vec2(0.0, e)));
    vec2 n2 = grad / max(length(grad), 1e-4);
    float depth = max(-f, 0.0);
    float band = min(bn.z, bn.w) * 0.75;
    float m = pow(1.0 - clamp(depth / band, 0.0, 1.0), 2.2);
    vec3 nn = normalize(vec3(n2 * m * 1.3, 1.0));
    vec3 l = normalize(vec3(-0.35, -0.9, 0.75));
    vec3 h = normalize(l + vec3(0.0, 0.0, 1.0));
    float nh = max(dot(nn, h), 0.0);
    float spec = (pow(nh, 90.0) * 1.1 + pow(nh, 12.0) * 0.1) * (0.3 + 0.7 * inflate);
    float rim = (1.0 - smoothstep(0.0, 1.2, depth)) * mix(0.18, 0.4, inflate);
    float body = mix(0.05, 0.12, inflate) * (uDark > 0.5 ? 1.0 : 0.7);
    float edge = 1.0 - smoothstep(-0.75, 0.75, f);
    // 물빛 막 + 테두리 + 반사광. 밝은 테마에선 흰 반사광이 안 보여 테를 짙게 둔다.
    vec4 film = vec4(uTint * body, body);
    vec3 rimColor = uDark > 0.5 ? vec3(1.0) : uTint * 0.55;
    film = film * (1.0 - rim) + vec4(rimColor, 1.0) * rim;
    float sp = min(spec, 1.0);
    film = film * (1.0 - sp) + vec4(1.0) * sp;
    outc = mix(outc, film, edge);
  }
  fragColor = outc;
}
