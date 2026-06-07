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
pub const BORDER: [u8; 4] = [80, 92, 110, 110];
/// Single accent — selection ring, cursor, links, active markers.
pub const ACCENT: [u8; 4] = [90, 140, 230, 255];
pub const TEXT: [u8; 4] = [236, 238, 243, 255];
pub const TEXT_DIM: [u8; 4] = [160, 166, 176, 255];
pub const TEXT_MUTE: [u8; 4] = [120, 126, 138, 255];
/// Status colors (git panel, etc.).
pub const SUCCESS: [u8; 4] = [63, 185, 80, 255];
/// Destructive-action accent — the confirm-close modal's 닫기 button.
pub const DANGER: [u8; 4] = [224, 88, 78, 255];

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

// ─── Icons ─────────────────────────────────────────────────────
// Single source of truth for chrome icon sizing. Previously every call
// site picked its own glyph size (14/15/18/19) and chip size (25/26/30),
// which read as inconsistent. All icon buttons now derive from these.
//
// Sizes are logical px (draw_text multiplies by self.scale internally).

/// Base glyph size for chrome icons ("medium" — unifies the old 14..19 spread).
pub const ICON_SIZE: f32 = 16.0;

/// u8 sRGB RGBA → f32 [0,1] RGBA, for the sugarloaf path which takes floats.

/// Same color with an explicit alpha override (overlays / drop-zones).
pub const fn with_alpha(c: [u8; 4], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}

/// Linear blend `t` of the way from `a` to `b` (`t` clamped to 0..1), opaque.
/// Used for transient tints like the completion-flash header pulse.
pub fn lerp(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]), 255]
}
