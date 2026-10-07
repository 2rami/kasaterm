// Phase 1 cell pipeline. One quad per glyph instance, indexed via
// `vertex_index % 6` so we don't ship a vertex buffer at all. The
// instance carries the cell's pixel rect, the glyph's atlas UV rect,
// and the foreground colour. R8 atlas sampled as alpha; B&W path
// outputs (fg.rgb, fg.a * alpha).

struct Uniforms {
    // Screen size in physical pixels — used to project pixel-space
    // cell rects into clip space without a CPU-side matrix multiply.
    screen_px: vec2<f32>,
    // text_gamma: WezTerm-style alpha curve on the glyph coverage mask.
    //   >1.0 boosts mid-tones (crisper, "darker" antialiased edges)
    //   1.0  passthrough (legacy behavior)
    //   <1.0 lifts mids (softer, foggy)
    // 1.3 = WezTerm's default. We apply pow(alpha, 1/gamma).
    // text_contrast: extra multiplier on the post-gamma alpha. 1.0 = no
    // change; small bumps (1.05) sharpen further without crushing mids.
    text_gamma: f32,
    text_contrast: f32,
    // color_sat: HSL-style saturation multiplier applied to fg.rgb (and
    // the colored-glyph texel.rgb). 1.0 = passthrough. >1 punches up
    // colours toward their primaries (sRGB green that lands at the same
    // chromaticity as P3 green after the layer tag, etc). Cells whose
    // bg/fg are pure white / black stay neutral because they have zero
    // chroma to scale.
    color_sat: f32,
    // 0.0 = passthrough (sRGB stays sRGB).
    // 1.0 = sRGB→DisplayP3 Bradford matrix in linear light, re-encode.
    //   Only meaningful when the host's CAMetalLayer is actually tagged
    //   DisplayP3 (KASATERM_P3_ROOT path). Without the layer tag, this
    //   washes colours out — the bytes become P3-encoded but the layer
    //   is still treated as sRGB by macOS, so they display dim.
    p3_convert: f32,
    // Monotonic seconds for GPU-driven animation (the decoration bands). The
    // CPU rewrites only this each present, so a busy pane animates without
    // re-emitting any chrome instances — idle stays at 0 CPU rebuild work.
    time: f32,
    _pad: f32,
};

// sRGB ↔ linear-light conversions (the IEC 61966-2-1 piecewise curve).
// We split scalar / vector forms so the matrix dot products below stay
// readable.
fn srgb_to_linear_s(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_to_linear_s(c.r), srgb_to_linear_s(c.g), srgb_to_linear_s(c.b));
}
fn linear_to_srgb_s(c: f32) -> f32 {
    let cc = max(c, 0.0);
    if (cc <= 0.0031308) { return cc * 12.92; }
    return 1.055 * pow(cc, 1.0 / 2.4) - 0.055;
}
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_to_srgb_s(c.r), linear_to_srgb_s(c.g), linear_to_srgb_s(c.b));
}

// Bradford-adapted sRGB D65 primaries → DisplayP3 D65 primaries, in
// linear light. Lifted directly from sugarloaf's renderer.metal so the
// byte-level output matches what kasaterm's sugarloaf opt-in path
// produces — same numbers, same gamut.
fn srgb_to_p3(linear_srgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(linear_srgb, vec3<f32>(0.82246197, 0.17753803, 0.0)),
        dot(linear_srgb, vec3<f32>(0.03319420, 0.96680580, 0.0)),
        dot(linear_srgb, vec3<f32>(0.01708263, 0.07239744, 0.91051993))
    );
}

// One-shot wrapper. When `u.p3_convert > 0.5`, walk the colour through
// the matrix; otherwise pass through. Branching cost is negligible
// (uniform predicate, fully predicated by the driver).
fn prepare_output(srgb: vec3<f32>) -> vec3<f32> {
    if (u.p3_convert > 0.5) {
        let lin = srgb_to_linear(srgb);
        let p3_lin = srgb_to_p3(lin);
        return linear_to_srgb(p3_lin);
    }
    return srgb;
}

// Push fg toward its primary chromaticity by `sat`. 1.0 = identity. We
// move the chroma component (rgb - luma) outward and re-add luma so
// brightness is preserved. Numerically stable for sat in [0, ~3].
fn boost_saturation(rgb: vec3<f32>, sat: f32) -> vec3<f32> {
    // BT.709 luma weights — terminal cells are dominantly mono / pastel
    // so this is "good enough" perceptual luma.
    let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mono = vec3<f32>(luma);
    return clamp(mix(mono, rgb, sat), vec3<f32>(0.0), vec3<f32>(1.0));
}


@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) cell_px: vec4<f32>,     // x, y, w, h (physical pixels)
    @location(1) uv_min: vec2<f32>,
    @location(2) uv_max: vec2<f32>,
    @location(3) fg: vec4<f32>,
    @location(4) flags: u32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

@vertex
fn vs_main(in: VsIn, @builtin(vertex_index) vi: u32) -> VsOut {
    // Quad expansion via vertex index. CCW triangles, top-left origin
    // matches winit's surface convention so y grows downward in pixel
    // space and we just flip once at the end.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];
    let raw_px = vec2<f32>(in.cell_px.x + c.x * in.cell_px.z,
                           in.cell_px.y + c.y * in.cell_px.w);
    // pixel_perfect_quad — round quad corners to integer physical
    // pixels so glyph edges align with the pixel grid instead of
    // bleeding sub-pixel coverage into the neighbouring column. Box
    // drawings and ASCII rules suddenly read razor-sharp; colour
    // chips stop dithering against the body bg.
    let px = vec2<f32>(round(raw_px.x), round(raw_px.y));
    // px / screen → 0..1, then *2 - 1 → -1..1 clip space. Y flip
    // because clip space is bottom-up.
    let ndc = vec2<f32>(px.x / u.screen_px.x * 2.0 - 1.0,
                        1.0 - px.y / u.screen_px.y * 2.0);
    let uv = mix(in.uv_min, in.uv_max, c);

    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    out.fg = in.fg;
    out.flags = in.flags;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_tex, atlas_sampler, in.uv);
    // 미분은 균일 흐름에서만 부를 수 있어 갈래 밖에서 미리 잰다(테두리 갈래가 쓴다).
    let duv = fwidth(in.uv);
    // 테두리(flags & 4 한 바퀴 · flags & 8 점선): 쿼드 전체가 둥근 사각 윤곽 하나다. uv 가 -1..1 이라
    // 1/fwidth(uv) 가 쿼드의 장치 px 반폭·반높이이고, 거기서 둥근 사각까지의 거리 d 와 윤곽 위 자리 s(왼쪽 위
    // 직선 시작에서 시계 방향 거리)를 잰다. 안팎 경계는 1px 로 부드럽게 끊는다. 모양은 상위 비트에 실려 온다
    // (pipeline.rs `edge_shape_bits`).
    if ((in.flags & 12u) != 0u) {
        let f_a = f32((in.flags >> 6u) & 0x3fu);
        let f_b = f32((in.flags >> 12u) & 0x3fu);
        let half = 1.0 / max(duv, vec2<f32>(1e-6, 1e-6));
        let r = min(f32((in.flags >> 18u) & 0x3fu) * 0.5, min(half.x, half.y));
        let period = max(f32((in.flags >> 24u) & 0xfu) * 0.5, 0.5);
        let frac = f32((in.flags >> 28u) & 0xfu) / 15.0;
        let p = in.uv * half;
        let q = abs(p) - (half - vec2<f32>(r, r));
        let d = -(length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r);
        let a = max(half.x - r, 0.0);
        let b = max(half.y - r, 0.0);
        let qa = 1.5707963 * r;
        let perim = 4.0 * (a + b + qa);
        let dx = abs(p.x) - a;
        let dy = abs(p.y) - b;
        var s: f32;
        if (dx > 0.0 && dy > 0.0) {
            let v = p - vec2<f32>(sign(p.x) * a, sign(p.y) * b);
            if (p.x > 0.0 && p.y < 0.0) {
                s = 2.0 * a + r * atan2(v.x, -v.y);
            } else if (p.x > 0.0) {
                s = 2.0 * a + qa + 2.0 * b + r * atan2(v.y, v.x);
            } else if (p.y > 0.0) {
                s = 4.0 * a + 2.0 * qa + 2.0 * b + r * atan2(-v.x, v.y);
            } else {
                s = 4.0 * a + 3.0 * qa + 4.0 * b + r * atan2(-v.y, -v.x);
            }
        } else if (dy >= dx) {
            if (p.y < 0.0) {
                s = p.x + a;
            } else {
                s = 4.0 * a + 2.0 * qa + 2.0 * b - (p.x + a);
            }
        } else if (p.x > 0.0) {
            s = 2.0 * a + qa + p.y + b;
        } else {
            s = 4.0 * a + 3.0 * qa + 4.0 * b - (p.y + b);
        }
        var reach: f32;
        var shade: f32;
        if ((in.flags & 4u) != 0u) {
            // 한 바퀴: 머리가 지나간 뒤 얼마나 됐는지(behind)로 꼬리를 그린다. 머리에서 가장 굵고 진하며 꼬리
            // 끝으로 가며 가늘어지고 옅어진다. 머리 앞 1px 은 부드럽게 — 잘린 앞머리가 튀어 보이지 않게.
            let head = fract(u.time / period) * perim;
            let behind = head - s + select(0.0, perim, s > head);
            let k = max(1.0 - behind / max(frac * perim, 1.0), 0.0);
            let lead = clamp(1.0 - (perim - behind), 0.0, 1.0);
            shade = max(k, lead);
            if (shade <= 0.0) {
                discard;
            }
            reach = mix(f_b * 0.25, f_a * 0.25, max(k, lead));
        } else {
            // 점선: 둘레를 한 칸 길이에 가장 가까운 정수 칸으로 나눠 이음매(s = 0 = 둘레)에서 무늬가 안 끊긴다.
            // 무늬가 `period` 초에 한 칸씩 시계 방향으로 흐르고, 선의 양 끝은 1px 로 부드럽다.
            let n = max(round(perim / max(f_b, 1.0)), 1.0);
            let cyc = perim / n;
            let ph = fract(s / cyc - u.time / period) * cyc;
            let on = frac * cyc;
            shade = clamp(min(ph, on - ph) + 0.5, 0.0, 1.0);
            if (shade <= 0.0) {
                discard;
            }
            reach = f_a * 0.25;
        }
        let cover = clamp(reach - d + 0.5, 0.0, 1.0) * clamp(d + 0.5, 0.0, 1.0);
        if (cover <= 0.0) {
            discard;
        }
        let rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(rgb), in.fg.a * shade * cover);
    }
    // 채우기 띠(flags & 16, FLAG_BAND_FILL): 왼쪽부터 2.4초에 걸쳐 차고 다시 시작한다 — 끝이 있는 일을
    // 「칸이 차는」 모양으로 보인다. 시간으로 채우므로 찬 칸이 실제 퍼센트는 아니다.
    if ((in.flags & 16u) != 0u) {
        let fill = fract(u.time / 2.4);
        let infill = step(in.uv.x, fill);
        let a = in.fg.a * mix(0.18, 1.0, infill);
        let rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(rgb), a);
    }
    // Color glyphs (emoji) are baked as full RGBA — draw them verbatim,
    // letting fg.a act as a global opacity. Coverage masks are baked as
    // white×alpha, so fg.rgb × tex.a reproduces the monochrome path.
    if ((in.flags & 1u) != 0u) {
        let sat_rgb = boost_saturation(texel.rgb, u.color_sat);
        return vec4<f32>(prepare_output(sat_rgb), texel.a * in.fg.a);
    }
    // SVG icon mask: tint through coverage but keep the raster's own linear
    // anti-aliasing — the text gamma/contrast curve below jaggies thin strokes.
    if ((in.flags & 2u) != 0u) {
        let icon_rgb = boost_saturation(in.fg.rgb, u.color_sat);
        return vec4<f32>(prepare_output(icon_rgb), in.fg.a * clamp(texel.a, 0.0, 1.0));
    }
    let alpha_raw = clamp(texel.a, 0.0, 1.0);
    let alpha_gamma = pow(alpha_raw, 1.0 / max(u.text_gamma, 0.001));
    let alpha = clamp(alpha_gamma * u.text_contrast, 0.0, 1.0);
    // `prepare_output` is the sRGB→Display P3 hop when KASATERM_P3_ROOT
    // is on, identity otherwise. Same pattern sugarloaf uses — the
    // byte we write is gamma-encoded P3, the CAMetalLayer is tagged
    // DisplayP3, and macOS scans out the P3 chromaticity.
    let fg_sat = boost_saturation(in.fg.rgb, u.color_sat);
    return vec4<f32>(prepare_output(fg_sat), in.fg.a * alpha);
}
