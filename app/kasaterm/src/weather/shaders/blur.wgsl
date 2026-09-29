// Downsample + separable Gaussian for the mist backdrop.

@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;

@fragment
fn fs_down(in: VOut) -> @location(0) vec4f {
    let t = 1.0 / vec2f(textureDimensions(src));
    var c = textureSampleLevel(src, samp, in.uv + vec2f(-t.x, -t.y), 0.0);
    c += textureSampleLevel(src, samp, in.uv + vec2f(t.x, -t.y), 0.0);
    c += textureSampleLevel(src, samp, in.uv + vec2f(-t.x, t.y), 0.0);
    c += textureSampleLevel(src, samp, in.uv + vec2f(t.x, t.y), 0.0);
    return c * 0.25;
}

fn gauss(uv: vec2f, dir: vec2f) -> vec4f {
    let t = dir / vec2f(textureDimensions(src));
    var c = textureSampleLevel(src, samp, uv, 0.0) * 0.2270270270;
    c += textureSampleLevel(src, samp, uv + t * 1.3846153846, 0.0) * 0.3162162162;
    c += textureSampleLevel(src, samp, uv - t * 1.3846153846, 0.0) * 0.3162162162;
    c += textureSampleLevel(src, samp, uv + t * 3.2307692308, 0.0) * 0.0702702703;
    c += textureSampleLevel(src, samp, uv - t * 3.2307692308, 0.0) * 0.0702702703;
    return c;
}

@fragment
fn fs_blur_h(in: VOut) -> @location(0) vec4f {
    return gauss(in.uv, vec2f(1.0, 0.0));
}

@fragment
fn fs_blur_v(in: VOut) -> @location(0) vec4f {
    return gauss(in.uv, vec2f(0.0, 1.0));
}
