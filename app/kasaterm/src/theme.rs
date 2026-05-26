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

/// Terminal body / window background. The single base color — the title
/// strip, sidebar, and pane-header band all share it so chrome and the
/// terminal read as one continuous surface (no depth seam).
pub const BG: [u8; 4] = [37, 44, 53, 255];
/// Sunken content surface (markdown code blocks) — darker than BG so a code
/// block reads as recessed into the page. Not used for chrome backgrounds.
pub const SURFACE: [u8; 4] = [26, 29, 35, 255];
/// Interactive states layered on the unified BG: each step *lighter* than BG
/// so hover/selected reads as raised, not as a darker hole.
pub const SURFACE_HOVER: [u8; 4] = [48, 56, 67, 255];
pub const SURFACE_ACTIVE: [u8; 4] = [60, 70, 84, 255];
/// Hairlines, dividers, inactive borders. A muted gray a clear step above
/// BG so seams read as soft lines, not black gaps.
pub const BORDER: [u8; 4] = [80, 92, 110, 255];
/// Single accent — selection ring, cursor, links, active markers.
pub const ACCENT: [u8; 4] = [90, 140, 230, 255];
pub const TEXT: [u8; 4] = [236, 238, 243, 255];
pub const TEXT_DIM: [u8; 4] = [160, 166, 176, 255];
pub const TEXT_MUTE: [u8; 4] = [120, 126, 138, 255];
/// Status colors (git panel, etc.).
pub const SUCCESS: [u8; 4] = [63, 185, 80, 255];
pub const WARN: [u8; 4] = [210, 153, 34, 255];
pub const DANGER: [u8; 4] = [248, 81, 73, 255];

/// Syntax-highlight palette for markdown code blocks (One Dark-ish, tuned
/// to read on the SURFACE code-block background).
pub const SYN_KEYWORD: [u8; 4] = [198, 120, 221, 255]; // purple — fn/let/if/…
pub const SYN_STRING: [u8; 4] = [152, 195, 121, 255]; // green — "…" '…'
pub const SYN_NUMBER: [u8; 4] = [209, 154, 102, 255]; // orange — 42, 0.1
pub const SYN_COMMENT: [u8; 4] = [106, 115, 130, 255]; // muted gray — // #
pub const SYN_FUNCTION: [u8; 4] = [97, 175, 239, 255]; // blue — foo(
pub const SYN_TYPE: [u8; 4] = [229, 192, 123, 255]; // yellow — Capitalized

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
