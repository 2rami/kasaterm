//! Central design tokens — colors + corner radii shared by every UI surface.
//!
//! One source of truth so the native render paths (gpu + sugarloaf), the
//! sidebar, pane headers, focus ring, and the wry webview panels all stay
//! visually consistent. Native code references these `[u8; 4]` / `f32`
//! consts directly; the wry panels use the matching hex (see `THEME_CSS`)
//! injected as CSS variables.
//!
//! Values are raw sRGB bytes — the gpu path sRGB-decodes on upload and the
//! sugarloaf path divides by 255 (see `f32_rgba`).

/// Terminal body / window background. Kept at the familiar value.
pub const BG: [u8; 4] = [37, 44, 53, 255];
/// Chrome surfaces (sidebar, title strip, panels) — a touch darker than the
/// body so chrome reads as a distinct depth layer.
pub const SURFACE: [u8; 4] = [26, 29, 35, 255];
pub const SURFACE_HOVER: [u8; 4] = [34, 38, 46, 255];
pub const SURFACE_ACTIVE: [u8; 4] = [46, 50, 59, 255];
/// Hairlines, dividers, inactive borders.
pub const BORDER: [u8; 4] = [16, 18, 23, 255];
/// Single accent — selection ring, cursor, links, active markers.
pub const ACCENT: [u8; 4] = [90, 140, 230, 255];
pub const TEXT: [u8; 4] = [236, 238, 243, 255];
pub const TEXT_DIM: [u8; 4] = [160, 166, 176, 255];
pub const TEXT_MUTE: [u8; 4] = [120, 126, 138, 255];
/// Status colors (git panel, etc.).
pub const SUCCESS: [u8; 4] = [63, 185, 80, 255];
pub const WARN: [u8; 4] = [210, 153, 34, 255];
pub const DANGER: [u8; 4] = [248, 81, 73, 255];

/// Corner radii (logical px) for the native `round_rect` helper.
pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 9.0;

/// u8 sRGB RGBA → f32 [0,1] RGBA, for the sugarloaf path which takes floats.
pub fn f32_rgba(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

/// Same color with an explicit alpha override (overlays / drop-zones).
pub const fn with_alpha(c: [u8; 4], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}
