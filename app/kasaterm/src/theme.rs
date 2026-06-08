//! Central design tokens — colors + corner radii shared by every UI surface.
//!
//! Colors are runtime-swappable: each is an atomic slot read through a `pub fn`
//! (a Relaxed load — effectively free) and written by `set_theme` / `set_accent`
//! so the settings screen can switch theme/accent live without threading a
//! palette through every call site. Defaults are the original dark palette, so
//! before any settings load the UI looks exactly as it always has.
//!
//! Values are raw sRGB bytes — the gpu path sRGB-decodes on upload and the
//! sugarloaf path divides by 255 (see `f32_rgba`).

use std::sync::atomic::{AtomicU32, Ordering};

const fn pack(c: [u8; 4]) -> u32 {
    ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | (c[3] as u32)
}
fn unpack(v: u32) -> [u8; 4] {
    [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// Define an atomic color slot + its reader fn, seeded with the dark default.
macro_rules! color_slot {
    ($slot:ident, $get:ident, $default:expr) => {
        static $slot: AtomicU32 = AtomicU32::new(pack($default));
        #[inline]
        pub fn $get() -> [u8; 4] {
            unpack($slot.load(Ordering::Relaxed))
        }
    };
}

// Terminal body / window background — the single base color shared by the title
// strip, sidebar, and pane-header band so chrome and terminal read as one
// surface. Interactive states (hover/active) layer *lighter* than bg so they
// read as raised. Border = muted hairline. Accent = selection ring / cursor /
// links. SYN_* = markdown code-block syntax (One Dark-ish).
color_slot!(S_BG, bg, [37, 44, 53, 255]);
color_slot!(S_FG, fg, [255, 255, 255, 255]);
color_slot!(S_SURFACE, surface, [26, 29, 35, 255]);
color_slot!(S_SURFACE_HOVER, surface_hover, [48, 56, 67, 255]);
color_slot!(S_SURFACE_ACTIVE, surface_active, [60, 70, 84, 255]);
color_slot!(S_BORDER, border, [80, 92, 110, 110]);
color_slot!(S_ACCENT, accent, [90, 140, 230, 255]);
color_slot!(S_TEXT, text, [236, 238, 243, 255]);
color_slot!(S_TEXT_DIM, text_dim, [160, 166, 176, 255]);
color_slot!(S_TEXT_MUTE, text_mute, [120, 126, 138, 255]);
color_slot!(S_SUCCESS, success, [63, 185, 80, 255]);
color_slot!(S_DANGER, danger, [224, 88, 78, 255]);
color_slot!(S_SYN_KEYWORD, syn_keyword, [198, 120, 221, 255]);
color_slot!(S_SYN_STRING, syn_string, [152, 195, 121, 255]);
color_slot!(S_SYN_NUMBER, syn_number, [209, 154, 102, 255]);
color_slot!(S_SYN_COMMENT, syn_comment, [106, 115, 130, 255]);
color_slot!(S_SYN_FUNCTION, syn_function, [97, 175, 239, 255]);
color_slot!(S_SYN_TYPE, syn_type, [229, 192, 123, 255]);

/// One base theme. Accent is stored separately (it layers on any base) so a
/// theme swap never clobbers the user's accent choice.
struct Palette {
    bg: [u8; 4],
    fg: [u8; 4],
    surface: [u8; 4],
    surface_hover: [u8; 4],
    surface_active: [u8; 4],
    border: [u8; 4],
    text: [u8; 4],
    text_dim: [u8; 4],
    text_mute: [u8; 4],
    success: [u8; 4],
    danger: [u8; 4],
    syn_keyword: [u8; 4],
    syn_string: [u8; 4],
    syn_number: [u8; 4],
    syn_comment: [u8; 4],
    syn_function: [u8; 4],
    syn_type: [u8; 4],
}

const DARK: Palette = Palette {
    bg: [37, 44, 53, 255],
    fg: [255, 255, 255, 255],
    surface: [26, 29, 35, 255],
    surface_hover: [48, 56, 67, 255],
    surface_active: [60, 70, 84, 255],
    border: [80, 92, 110, 110],
    text: [236, 238, 243, 255],
    text_dim: [160, 166, 176, 255],
    text_mute: [120, 126, 138, 255],
    success: [63, 185, 80, 255],
    danger: [224, 88, 78, 255],
    syn_keyword: [198, 120, 221, 255],
    syn_string: [152, 195, 121, 255],
    syn_number: [209, 154, 102, 255],
    syn_comment: [106, 115, 130, 255],
    syn_function: [97, 175, 239, 255],
    syn_type: [229, 192, 123, 255],
};

const LIGHT: Palette = Palette {
    bg: [247, 248, 250, 255],
    fg: [38, 42, 50, 255],
    surface: [236, 238, 241, 255],
    surface_hover: [228, 231, 236, 255],
    surface_active: [214, 219, 227, 255],
    border: [196, 202, 211, 180],
    text: [28, 32, 38, 255],
    text_dim: [92, 98, 108, 255],
    text_mute: [140, 146, 156, 255],
    success: [38, 148, 64, 255],
    danger: [205, 66, 56, 255],
    syn_keyword: [166, 38, 164, 255],
    syn_string: [72, 150, 70, 255],
    syn_number: [152, 104, 1, 255],
    syn_comment: [160, 166, 176, 255],
    syn_function: [56, 110, 230, 255],
    syn_type: [152, 104, 1, 255],
};

fn store_palette(p: &Palette) {
    S_BG.store(pack(p.bg), Ordering::Relaxed);
    S_FG.store(pack(p.fg), Ordering::Relaxed);
    S_SURFACE.store(pack(p.surface), Ordering::Relaxed);
    S_SURFACE_HOVER.store(pack(p.surface_hover), Ordering::Relaxed);
    S_SURFACE_ACTIVE.store(pack(p.surface_active), Ordering::Relaxed);
    S_BORDER.store(pack(p.border), Ordering::Relaxed);
    S_TEXT.store(pack(p.text), Ordering::Relaxed);
    S_TEXT_DIM.store(pack(p.text_dim), Ordering::Relaxed);
    S_TEXT_MUTE.store(pack(p.text_mute), Ordering::Relaxed);
    S_SUCCESS.store(pack(p.success), Ordering::Relaxed);
    S_DANGER.store(pack(p.danger), Ordering::Relaxed);
    S_SYN_KEYWORD.store(pack(p.syn_keyword), Ordering::Relaxed);
    S_SYN_STRING.store(pack(p.syn_string), Ordering::Relaxed);
    S_SYN_NUMBER.store(pack(p.syn_number), Ordering::Relaxed);
    S_SYN_COMMENT.store(pack(p.syn_comment), Ordering::Relaxed);
    S_SYN_FUNCTION.store(pack(p.syn_function), Ordering::Relaxed);
    S_SYN_TYPE.store(pack(p.syn_type), Ordering::Relaxed);
    // Accent intentionally not touched here — see set_accent.
}

/// Accent presets offered in the settings screen; first is the default blue.
pub const ACCENT_PRESETS: &[(&str, [u8; 4])] = &[
    ("blue", [90, 140, 230, 255]),
    ("green", [63, 170, 90, 255]),
    ("orange", [228, 140, 60, 255]),
    ("purple", [168, 118, 228, 255]),
    ("pink", [228, 100, 160, 255]),
];

pub fn accent_color(name: &str) -> [u8; 4] {
    ACCENT_PRESETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or(ACCENT_PRESETS[0].1)
}

/// Current accent's preset name (for the settings screen's selected state).
pub fn accent_name() -> &'static str {
    let cur = accent();
    ACCENT_PRESETS
        .iter()
        .find(|(_, c)| *c == cur)
        .map(|(n, _)| *n)
        .unwrap_or("blue")
}

/// Whether the light base theme is active (bg luminance test — robust to which
/// palette set it).
pub fn is_light() -> bool {
    bg()[0] as u16 + bg()[1] as u16 + bg()[2] as u16 > 384
}

pub fn set_theme(mode: &str) {
    store_palette(if mode == "light" { &LIGHT } else { &DARK });
}

pub fn set_accent(name: &str) {
    S_ACCENT.store(pack(accent_color(name)), Ordering::Relaxed);
}

/// Apply persisted theme + accent from settings.json at launch.
pub fn apply_from_settings() {
    let s = crate::socket::read_settings();
    let mode = s.get("theme").and_then(|x| x.as_str()).unwrap_or("dark");
    set_theme(mode);
    let accent = s.get("accent").and_then(|x| x.as_str()).unwrap_or("blue");
    set_accent(accent);
}

/// Corner radii (logical px) for the native `round_rect` helper.
pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 9.0;

/// Base glyph size for chrome icons (logical px; draw_text multiplies by scale).
pub const ICON_SIZE: f32 = 16.0;

/// Same color with an explicit alpha override (overlays / drop-zones).
pub const fn with_alpha(c: [u8; 4], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}

/// Linear blend `t` of the way from `a` to `b` (`t` clamped to 0..1), opaque.
pub fn lerp(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]), 255]
}
