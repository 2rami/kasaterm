struct Camera {
    clip_from_world: mat4x4<f32>,
    eye_direction: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct Vertex {
    @location(0) point: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) material: vec4<f32>,
    @location(3) position: vec3<f32>,
    @location(4) focus: f32,
};

struct Surface {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) local: vec3<f32>,
    @location(3) @interpolate(flat) material: vec4<f32>,
    @location(4) @interpolate(flat) focus: f32,
};

@vertex
fn project(v: Vertex) -> Surface {
    let world = v.position + v.point;
    return Surface(camera.clip_from_world * vec4<f32>(world, 1.0), world, v.normal, v.point, v.material, v.focus);
}

fn seam(coord: f32, width: f32) -> f32 {
    let d = abs(fract(coord + 0.5) - 0.5);
    return 1.0 - smoothstep(0.008, max(0.018, width * 1.2), d);
}

@fragment
fn shade(s: Surface) -> @location(0) vec4<f32> {
    let mode = s.material.w;
    let base = s.material.rgb;
    let n = normalize(s.normal);
    let key = normalize(vec3<f32>(-0.52, 0.83, 0.46));
    let pixel = fwidth(s.world.xz);
    if mode == 1.0 {
        return vec4<f32>(mix(base * 0.48, base, s.focus), 1.0);
    }
    if mode == 2.0 {
        let pool = exp(-dot(s.world.xz * vec2<f32>(0.11, 0.14), s.world.xz * vec2<f32>(0.11, 0.14)));
        let joints = max(seam(s.world.x / 2.25, pixel.x / 2.25), seam(s.world.z / 2.25, pixel.y / 2.25));
        return vec4<f32>(base * (0.48 + 0.63 * pool) - joints * vec3<f32>(0.015, 0.018, 0.023), 1.0);
    }
    if mode == 3.0 {
        let wash = exp(-s.world.x * s.world.x * 0.012 - (s.local.y - 1.75) * (s.local.y - 1.75) * 0.17);
        let panels = seam(s.world.x / 2.6, pixel.x / 2.6);
        let kick = exp(-abs(s.local.y - 0.16) * 10.0);
        let strip = 1.0 - smoothstep(0.013, 0.028, abs(s.local.y - 0.16));
        return vec4<f32>(base * (0.72 + wash * 0.72) - panels * 0.012
            + vec3<f32>(0.06, 0.18, 0.21) * (0.4 * kick + 0.75 * strip), 1.0);
    }
    if mode == 4.0 {
        let light = 0.25 + 0.58 * smoothstep(-0.5, 1.7, 0.74 * s.local.y - abs(s.local.x) * 0.4);
        let reflection = 0.05 * smoothstep(0.2, 0.7, s.local.y + s.local.x * 0.46);
        return vec4<f32>(base * light + vec3<f32>(0.02, 0.025, 0.045) + reflection, 1.0);
    }
    if mode == 5.0 {
        let r = length((s.local.xz - vec2<f32>(0.0, 0.10)) / vec2<f32>(1.78, 1.43));
        return vec4<f32>(base, 0.67 * (1.0 - smoothstep(0.43, 1.0, r)));
    }
    if mode == 6.0 {
        return vec4<f32>(base, s.focus * 0.9);
    }
    let diffuse = max(dot(n, key), 0.0);
    let facing = max(dot(n, camera.eye_direction.xyz), 0.0);
    let rim = pow(1.0 - facing, 3.0) * 0.11;
    var lit = base * (0.53 + 0.46 * diffuse) + vec3<f32>(0.13, 0.31, 0.45) * rim;
    if mode == 7.0 {
        let contact = length(s.local.xz * vec2<f32>(0.84, 1.23));
        let shadow = (1.0 - smoothstep(0.28, 1.03, contact)) * (0.42 - 0.22 * s.focus);
        lit = lit * (1.0 - shadow) + vec3<f32>(0.018, 0.090, 0.11) * s.focus;
    } else {
        lit = lit + vec3<f32>(0.028, 0.12, 0.17) * (rim * 4.0) * s.focus;
    }
    return vec4<f32>(min(lit, vec3<f32>(1.0)), 1.0);
}
