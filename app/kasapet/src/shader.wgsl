struct VOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) mask_uv: vec4<f32>,
};
struct Xf {
  mvp: mat4x4<f32>,
  mask_mtx: mat4x4<f32>,
  channel: vec4<f32>,
  opacity: f32,
  use_mask: f32,
  inverted: f32,
  _pad: f32,
};
@group(0) @binding(0) var<uniform> xf: Xf;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var mask_tex: texture_2d<f32>;

@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
  var o: VOut;
  let v = vec4<f32>(p, 0.0, 1.0);
  o.pos = xf.mvp * v;
  o.uv = uv;
  o.mask_uv = xf.mask_mtx * v;
  return o;
}

@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
  var c = textureSample(tex, samp, i.uv);
  c = vec4<f32>(c.rgb * c.a, c.a) * xf.opacity;
  if (xf.use_mask > 0.5) {
    // Live2D 마스크 좌표는 w 나눗셈 없이 곧바로 0..1 로 온다.
    let m = textureSample(mask_tex, samp, i.mask_uv.xy / i.mask_uv.w);
    var a = dot(m, xf.channel);
    if (xf.inverted > 0.5) { a = 1.0 - a; }
    c = c * a;
  }
  return c;
}

// 마스크 텍스처를 채우는 패스 — 알파만 채널에 쓴다.
@fragment fn fs_mask(i: VOut) -> @location(0) vec4<f32> {
  let c = textureSample(tex, samp, i.uv);
  return xf.channel * c.a;
}

// 말풍선 — 미리 그려 둔 그림을 그대로. 앱에 옮기면 앱 글꼴로 그린다.
@fragment fn fs_plain(i: VOut) -> @location(0) vec4<f32> {
  let c = textureSample(tex, samp, i.uv);
  return vec4<f32>(c.rgb * c.a, c.a) * xf.opacity;
}
