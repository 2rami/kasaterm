// Phase 1 cell pipeline. One quad per glyph instance, indexed via
// `vertex_index % 6` so we don't ship a vertex buffer at all. The
// instance carries the cell's pixel rect, the glyph's atlas UV rect,
// and the foreground colour. R8 atlas sampled as alpha; B&W path
// outputs (fg.rgb, fg.a * alpha).

struct Uniforms {
    // Screen size in physical pixels — used to project pixel-space
    // cell rects into clip space without a CPU-side matrix multiply.
    screen_px: vec2<f32>,
    // Padding so the struct is std140-aligned; wgpu's WGSL backend
    // needs uniforms to be 16-byte multiples and this keeps the
    // future-self update (cell metrics, time, etc) cheap.
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) cell_px: vec4<f32>,     // x, y, w, h (physical pixels)
    @location(1) uv_min: vec2<f32>,
    @location(2) uv_max: vec2<f32>,
    @location(3) fg: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg: vec4<f32>,
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
    let px = vec2<f32>(in.cell_px.x + c.x * in.cell_px.z,
                       in.cell_px.y + c.y * in.cell_px.w);
    // px / screen → 0..1, then *2 - 1 → -1..1 clip space. Y flip
    // because clip space is bottom-up.
    let ndc = vec2<f32>(px.x / u.screen_px.x * 2.0 - 1.0,
                        1.0 - px.y / u.screen_px.y * 2.0);
    let uv = mix(in.uv_min, in.uv_max, c);

    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    out.fg = in.fg;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(atlas_tex, atlas_sampler, in.uv).r;
    return vec4<f32>(in.fg.rgb, in.fg.a * a);
}
