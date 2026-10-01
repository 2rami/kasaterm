// Prepended to shaders that need the window layout. Places are listed panes first, then
// panels, then bars; the first one holding a point owns it.

struct R {
    n: vec4f,               // count, streak density and alpha outside every place
    rect: array<vec4f, 24>, // x, y, w, h (px)
    a: array<vec4f, 24>,    // kind (0 pane, 1 panel, 2 bar), mist target, pool height px, unused
    b: array<vec4f, 24>,    // pool wave px, ripple damping, streak density, streak alpha
    c: array<vec4f, 24>,    // guard rect px: the focused input row, never wet (w = 0: none)
};

@group(0) @binding(9) var<uniform> rg: R;

fn inside(p: vec2f, r: vec4f) -> bool {
    return p.x >= r.x && p.y >= r.y && p.x < r.x + r.z && p.y < r.y + r.w;
}

fn guarded(p: vec2f, i: i32) -> bool {
    return rg.c[i].z > 0.0 && inside(p, rg.c[i]);
}

fn region_of(p: vec2f, kind: f32) -> i32 {
    let n = i32(rg.n.x);
    for (var i = 0; i < n; i++) {
        if (rg.a[i].x == kind && inside(p, rg.rect[i])) {
            return i;
        }
    }
    return -1;
}

fn region_any(p: vec2f) -> i32 {
    let n = i32(rg.n.x);
    for (var i = 0; i < n; i++) {
        if (inside(p, rg.rect[i])) {
            return i;
        }
    }
    return -1;
}
