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

/// Terminal ANSI 0-15, runtime-swappable alongside the UI tokens so a theme
/// switch recolors the terminal body too (256-cube / grayscale stay computed).
/// Seeded with the dark default (Ghostty Tomorrow Night Bright) so pre-settings
/// frames look unchanged.
static S_ANSI: [AtomicU32; 16] = {
    const fn seed(c: [u8; 3]) -> AtomicU32 {
        AtomicU32::new(pack([c[0], c[1], c[2], 255]))
    }
    [
        seed(ANSI_TOMORROW[0]), seed(ANSI_TOMORROW[1]), seed(ANSI_TOMORROW[2]),
        seed(ANSI_TOMORROW[3]), seed(ANSI_TOMORROW[4]), seed(ANSI_TOMORROW[5]),
        seed(ANSI_TOMORROW[6]), seed(ANSI_TOMORROW[7]), seed(ANSI_TOMORROW[8]),
        seed(ANSI_TOMORROW[9]), seed(ANSI_TOMORROW[10]), seed(ANSI_TOMORROW[11]),
        seed(ANSI_TOMORROW[12]), seed(ANSI_TOMORROW[13]), seed(ANSI_TOMORROW[14]),
        seed(ANSI_TOMORROW[15]),
    ]
};

/// Current ANSI color `i` (0..16) as RGB. Cells resolve `Color::Idx(<16)`
/// through this so the terminal body follows the active theme.
#[inline]
pub fn ansi16(i: usize) -> [u8; 3] {
    let c = unpack(S_ANSI[i & 15].load(Ordering::Relaxed));
    [c[0], c[1], c[2]]
}

/// One base theme. Accent is stored separately (it layers on any base) so a
/// theme swap never clobbers the user's accent choice.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: [u8; 4],
    pub fg: [u8; 4],
    pub surface: [u8; 4],
    pub surface_hover: [u8; 4],
    pub surface_active: [u8; 4],
    pub border: [u8; 4],
    pub text: [u8; 4],
    pub text_dim: [u8; 4],
    pub text_mute: [u8; 4],
    pub success: [u8; 4],
    pub danger: [u8; 4],
    pub syn_keyword: [u8; 4],
    pub syn_string: [u8; 4],
    pub syn_number: [u8; 4],
    pub syn_comment: [u8; 4],
    pub syn_function: [u8; 4],
    pub syn_type: [u8; 4],
    pub ansi: [[u8; 3]; 16],
}

/// Ghostty default 16 (Tomorrow Night Bright) — the palette kasaterm has always
/// shipped; kept as the dark themes' terminal set so existing screens don't
/// shift. (Moved here from cells.rs when ANSI joined the theme system.)
const ANSI_TOMORROW: [[u8; 3]; 16] = [
    [0x1D, 0x1F, 0x21], [0xCC, 0x66, 0x66], [0xB5, 0xBD, 0x68], [0xF0, 0xC6, 0x74],
    [0x81, 0xA2, 0xBE], [0xB2, 0x94, 0xBB], [0x8A, 0xBE, 0xB7], [0xC5, 0xC8, 0xC6],
    [0x66, 0x66, 0x66], [0xD5, 0x4E, 0x53], [0xB9, 0xCA, 0x4A], [0xE7, 0xC5, 0x47],
    [0x7A, 0xA6, 0xDA], [0xC3, 0x97, 0xD8], [0x70, 0xC0, 0xB1], [0xEA, 0xEA, 0xEA],
];

/// GitHub Light (Primer) — white/bright-white are grey so every slot stays
/// legible on a light background (stock "Tomorrow day" keeps white=#fff which
/// disappears).
const ANSI_GITHUB_LIGHT: [[u8; 3]; 16] = [
    [0x24, 0x29, 0x2E], [0xD7, 0x3A, 0x49], [0x22, 0x86, 0x3A], [0xB0, 0x88, 0x00],
    [0x03, 0x66, 0xD6], [0x6F, 0x42, 0xC1], [0x1B, 0x7C, 0x83], [0x6A, 0x73, 0x7D],
    [0x95, 0x9D, 0xA5], [0xCB, 0x24, 0x31], [0x28, 0xA7, 0x45], [0xDB, 0xAB, 0x09],
    [0x21, 0x88, 0xFF], [0x8A, 0x63, 0xD2], [0x31, 0x92, 0xAA], [0xD1, 0xD5, 0xDA],
];

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
    ansi: ANSI_TOMORROW,
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
    ansi: ANSI_GITHUB_LIGHT,
};

// ── Preset themes (official palettes) ────────────────────────────────────
// UI tokens are mapped from each theme's canonical role names; ANSI sets are
// the schemes' published terminal palettes.

const CATPPUCCIN_MOCHA: Palette = Palette {
    bg: [0x1E, 0x1E, 0x2E, 255],            // base
    fg: [0xCD, 0xD6, 0xF4, 255],            // text
    surface: [0x18, 0x18, 0x25, 255],       // mantle
    surface_hover: [0x31, 0x32, 0x44, 255], // surface0
    surface_active: [0x45, 0x47, 0x5A, 255], // surface1
    border: [0x6C, 0x70, 0x86, 110],        // overlay0
    text: [0xCD, 0xD6, 0xF4, 255],
    text_dim: [0xA6, 0xAD, 0xC8, 255],      // subtext0
    text_mute: [0x7F, 0x84, 0x9C, 255],     // overlay1
    success: [0xA6, 0xE3, 0xA1, 255],       // green
    danger: [0xF3, 0x8B, 0xA8, 255],        // red
    syn_keyword: [0xCB, 0xA6, 0xF7, 255],   // mauve
    syn_string: [0xA6, 0xE3, 0xA1, 255],    // green
    syn_number: [0xFA, 0xB3, 0x87, 255],    // peach
    syn_comment: [0x7F, 0x84, 0x9C, 255],   // overlay1
    syn_function: [0x89, 0xB4, 0xFA, 255],  // blue
    syn_type: [0xF9, 0xE2, 0xAF, 255],      // yellow
    ansi: [
        [0x45, 0x47, 0x5A], [0xF3, 0x8B, 0xA8], [0xA6, 0xE3, 0xA1], [0xF9, 0xE2, 0xAF],
        [0x89, 0xB4, 0xFA], [0xF5, 0xC2, 0xE7], [0x94, 0xE2, 0xD5], [0xBA, 0xC2, 0xDE],
        [0x58, 0x5B, 0x70], [0xF3, 0x8B, 0xA8], [0xA6, 0xE3, 0xA1], [0xF9, 0xE2, 0xAF],
        [0x89, 0xB4, 0xFA], [0xF5, 0xC2, 0xE7], [0x94, 0xE2, 0xD5], [0xA6, 0xAD, 0xC8],
    ],
};

const CATPPUCCIN_LATTE: Palette = Palette {
    bg: [0xEF, 0xF1, 0xF5, 255],            // base
    fg: [0x4C, 0x4F, 0x69, 255],            // text
    surface: [0xE6, 0xE9, 0xEF, 255],       // mantle
    surface_hover: [0xDC, 0xE0, 0xE8, 255], // crust
    surface_active: [0xCC, 0xD0, 0xDA, 255], // surface0
    border: [0xBC, 0xC0, 0xCC, 180],        // surface1
    text: [0x4C, 0x4F, 0x69, 255],
    text_dim: [0x6C, 0x6F, 0x85, 255],      // subtext0
    text_mute: [0x8C, 0x8F, 0xA1, 255],     // overlay1
    success: [0x40, 0xA0, 0x2B, 255],       // green
    danger: [0xD2, 0x0F, 0x39, 255],        // red
    syn_keyword: [0x88, 0x39, 0xEF, 255],   // mauve
    syn_string: [0x40, 0xA0, 0x2B, 255],    // green
    syn_number: [0xFE, 0x64, 0x0B, 255],    // peach
    syn_comment: [0x9C, 0xA0, 0xB0, 255],   // overlay0
    syn_function: [0x1E, 0x66, 0xF5, 255],  // blue
    syn_type: [0xDF, 0x8E, 0x1D, 255],      // yellow
    ansi: [
        [0x5C, 0x5F, 0x77], [0xD2, 0x0F, 0x39], [0x40, 0xA0, 0x2B], [0xDF, 0x8E, 0x1D],
        [0x1E, 0x66, 0xF5], [0xEA, 0x76, 0xCB], [0x17, 0x92, 0x99], [0xAC, 0xB0, 0xBE],
        [0x6C, 0x6F, 0x85], [0xD2, 0x0F, 0x39], [0x40, 0xA0, 0x2B], [0xDF, 0x8E, 0x1D],
        [0x1E, 0x66, 0xF5], [0xEA, 0x76, 0xCB], [0x17, 0x92, 0x99], [0xBC, 0xC0, 0xCC],
    ],
};

const GRUVBOX_DARK: Palette = Palette {
    bg: [0x28, 0x28, 0x28, 255],
    fg: [0xEB, 0xDB, 0xB2, 255],
    surface: [0x1D, 0x20, 0x21, 255],       // bg0_h
    surface_hover: [0x3C, 0x38, 0x36, 255], // bg1
    surface_active: [0x50, 0x49, 0x45, 255], // bg2
    border: [0x66, 0x5C, 0x54, 110],        // bg3
    text: [0xEB, 0xDB, 0xB2, 255],          // fg1
    text_dim: [0xBD, 0xAE, 0x93, 255],      // fg3
    text_mute: [0x92, 0x83, 0x74, 255],     // gray
    success: [0xB8, 0xBB, 0x26, 255],       // bright green
    danger: [0xFB, 0x49, 0x34, 255],        // bright red
    syn_keyword: [0xD3, 0x86, 0x9B, 255],   // purple
    syn_string: [0xB8, 0xBB, 0x26, 255],    // green
    syn_number: [0xD7, 0x99, 0x21, 255],    // yellow
    syn_comment: [0x92, 0x83, 0x74, 255],   // gray
    syn_function: [0x83, 0xA5, 0x98, 255],  // blue
    syn_type: [0xFA, 0xBD, 0x2F, 255],      // bright yellow
    ansi: [
        [0x28, 0x28, 0x28], [0xCC, 0x24, 0x1D], [0x98, 0x97, 0x1A], [0xD7, 0x99, 0x21],
        [0x45, 0x85, 0x88], [0xB1, 0x62, 0x86], [0x68, 0x9D, 0x6A], [0xA8, 0x99, 0x84],
        [0x92, 0x83, 0x74], [0xFB, 0x49, 0x34], [0xB8, 0xBB, 0x26], [0xFA, 0xBD, 0x2F],
        [0x83, 0xA5, 0x98], [0xD3, 0x86, 0x9B], [0x8E, 0xC0, 0x7C], [0xEB, 0xDB, 0xB2],
    ],
};

const TOKYO_NIGHT: Palette = Palette {
    bg: [0x1A, 0x1B, 0x26, 255],
    fg: [0xC0, 0xCA, 0xF5, 255],
    surface: [0x16, 0x16, 0x1E, 255],
    surface_hover: [0x29, 0x2E, 0x42, 255],
    surface_active: [0x3B, 0x42, 0x61, 255],
    border: [0x3B, 0x42, 0x61, 130],
    text: [0xC0, 0xCA, 0xF5, 255],
    text_dim: [0xA9, 0xB1, 0xD6, 255],
    text_mute: [0x56, 0x5F, 0x89, 255],
    success: [0x9E, 0xCE, 0x6A, 255],
    danger: [0xF7, 0x76, 0x8E, 255],
    syn_keyword: [0xBB, 0x9A, 0xF7, 255],   // magenta
    syn_string: [0x9E, 0xCE, 0x6A, 255],    // green
    syn_number: [0xFF, 0x9E, 0x64, 255],    // orange
    syn_comment: [0x56, 0x5F, 0x89, 255],
    syn_function: [0x7A, 0xA2, 0xF7, 255],  // blue
    syn_type: [0xE0, 0xAF, 0x68, 255],      // yellow
    ansi: [
        [0x15, 0x16, 0x1E], [0xF7, 0x76, 0x8E], [0x9E, 0xCE, 0x6A], [0xE0, 0xAF, 0x68],
        [0x7A, 0xA2, 0xF7], [0xBB, 0x9A, 0xF7], [0x7D, 0xCF, 0xFF], [0xA9, 0xB1, 0xD6],
        [0x41, 0x48, 0x68], [0xF7, 0x76, 0x8E], [0x9E, 0xCE, 0x6A], [0xE0, 0xAF, 0x68],
        [0x7A, 0xA2, 0xF7], [0xBB, 0x9A, 0xF7], [0x7D, 0xCF, 0xFF], [0xC0, 0xCA, 0xF5],
    ],
};

// Blue Archive "Schale" pair — hexes extracted from fan-repo canon (vitepress
// vars.less / momotalk mixin.scss / BA-style-homepage index.css; 거노 수집분,
// [[reference_ba_styling_repos]]). green/red have no canon source and are
// derived to fit — tune by eye. Light follows the game's white/sky UI; dark is
// the same family re-rooted on the BA navy (#003153/#2a323e) since the game
// itself has no dark mode.
const SCHALE_LIGHT: Palette = Palette {
    bg: [0xEA, 0xEF, 0xF5, 255],            // general background
    fg: [0x2A, 0x32, 0x3E, 255],            // momotalk font-black
    surface: [0xFF, 0xFF, 0xFF, 255],       // white cards
    surface_hover: [0xE1, 0xE7, 0xEC, 255], // list-active
    surface_active: [0xD3, 0xDD, 0xE8, 255],
    border: [0xCD, 0xD3, 0xDC, 180],        // chatborder
    text: [0x2A, 0x32, 0x3E, 255],
    text_dim: [0x4C, 0x58, 0x66, 255],      // font-color-grey
    text_mute: [0x87, 0x92, 0x9E, 255],     // momotalk font-grey
    success: [0x2A, 0xA8, 0x76, 255],
    danger: [0xE8, 0x50, 0x4F, 255],
    syn_keyword: [0x12, 0x8A, 0xFA, 255],   // the BA blue
    syn_string: [0x2A, 0xA8, 0x76, 255],
    syn_number: [0xD2, 0x64, 0x9A, 255],    // momotalk pink, darkened for white bg
    syn_comment: [0x87, 0x92, 0x9E, 255],
    syn_function: [0x0E, 0x9C, 0xBE, 255],  // sky cyan family, darkened
    syn_type: [0xB5, 0x8A, 0x00, 255],      // gold #ffe401, darkened for white bg
    ansi: [
        [0x2A, 0x32, 0x3E], [0xE8, 0x50, 0x4F], [0x2A, 0xA8, 0x76], [0xC9, 0x8A, 0x00],
        [0x12, 0x8A, 0xFA], [0xEF, 0x62, 0x92], [0x0E, 0x9C, 0xBE], [0x68, 0x78, 0x8F],
        [0x87, 0x92, 0x9E], [0xF0, 0x68, 0x60], [0x35, 0xC0, 0x8B], [0xE0, 0xA8, 0x00],
        [0x34, 0x93, 0xF9], [0xF2, 0x72, 0xA0], [0x10, 0xB3, 0xD7], [0xCD, 0xD3, 0xDC],
    ],
};

const SCHALE_DARK: Palette = Palette {
    bg: [0x10, 0x17, 0x20, 255],
    fg: [0xDC, 0xE6, 0xF2, 255],
    surface: [0x18, 0x21, 0x2C, 255],
    surface_hover: [0x1F, 0x2B, 0x39, 255],
    surface_active: [0x26, 0x35, 0x47, 255],
    border: [0x2E, 0x3D, 0x4F, 130],
    text: [0xDC, 0xE6, 0xF2, 255],
    text_dim: [0x9F, 0xB0, 0xC3, 255],
    text_mute: [0x68, 0x78, 0x8F, 255],     // momotalk grey-active
    success: [0x4C, 0xD3, 0x9A, 255],
    danger: [0xFF, 0x6B, 0x6B, 255],
    syn_keyword: [0xFC, 0x96, 0xAB, 255],   // momotalk pink
    syn_string: [0x7C, 0xE8, 0xB8, 255],
    syn_number: [0xFF, 0xB3, 0x42, 255],    // focus orange
    syn_comment: [0x68, 0x78, 0x8F, 255],
    syn_function: [0x4A, 0xA8, 0xFF, 255],  // BA blue, lifted for navy bg
    syn_type: [0xFF, 0xE4, 0x01, 255],      // canon gold
    ansi: [
        [0x18, 0x21, 0x2C], [0xFF, 0x6B, 0x6B], [0x4C, 0xD3, 0x9A], [0xFF, 0xE4, 0x01],
        [0x4A, 0xA8, 0xFF], [0xFC, 0x96, 0xAB], [0x6C, 0xE6, 0xFF], [0xDC, 0xE6, 0xF2],
        [0x68, 0x78, 0x8F], [0xFF, 0x94, 0x94], [0x7C, 0xE8, 0xB8], [0xFF, 0xF0, 0x66],
        [0x77, 0xDE, 0xFF], [0xFF, 0xB7, 0xC9], [0xA5, 0xF0, 0xFF], [0xFF, 0xFF, 0xFF],
    ],
};

/// Selectable themes: (settings.json key, display label, palette). "dark" /
/// "light" keep their historical keys so existing settings files keep working.
pub const THEME_PRESETS: &[(&str, &str, &Palette)] = &[
    ("dark", "Dark", &DARK),
    ("light", "Light", &LIGHT),
    ("catppuccin-mocha", "Catppuccin Mocha", &CATPPUCCIN_MOCHA),
    ("catppuccin-latte", "Catppuccin Latte", &CATPPUCCIN_LATTE),
    ("gruvbox-dark", "Gruvbox Dark", &GRUVBOX_DARK),
    ("tokyo-night", "Tokyo Night", &TOKYO_NIGHT),
    ("schale-light", "Schale Light", &SCHALE_LIGHT),
    ("schale-dark", "Schale Dark", &SCHALE_DARK),
];

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
    for (i, c) in p.ansi.iter().enumerate() {
        S_ANSI[i].store(pack([c[0], c[1], c[2], 255]), Ordering::Relaxed);
    }
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

/// Active theme key ("dark", "catppuccin-mocha", "custom"…) for the settings
/// screen's selected state. Colors alone can't tell presets apart, so the key
/// is tracked at set time.
static CURRENT_THEME: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

pub fn theme_name() -> &'static str {
    CURRENT_THEME.lock().ok().and_then(|g| *g).unwrap_or("dark")
}

/// Switch to a preset theme by key; unknown keys fall back to dark. "custom"
/// re-reads the settings file's palette overrides.
pub fn set_theme(mode: &str) {
    if mode == "custom" {
        apply_custom_theme(&crate::socket::read_settings());
        return;
    }
    let (key, _, p) = THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == mode)
        .unwrap_or(&THEME_PRESETS[0]);
    store_palette(p);
    if let Ok(mut g) = CURRENT_THEME.lock() {
        *g = Some(key);
    }
}

/// Custom theme: `custom_theme` object in settings.json overrides individual
/// keys on top of a base preset. Shape:
/// `{ "base": "dark", "bg": "#252c35", …, "ansi": ["#1d1f21", …×16] }`
/// Unknown/missing keys keep the base value, so a partial file is fine.
fn apply_custom_theme(s: &serde_json::Value) {
    let obj = s.get("custom_theme");
    let base_key = obj
        .and_then(|o| o.get("base"))
        .and_then(|x| x.as_str())
        .unwrap_or("dark");
    let (_, _, base) = THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == base_key)
        .unwrap_or(&THEME_PRESETS[0]);
    let mut p: Palette = **base;
    if let Some(o) = obj.and_then(|o| o.as_object()) {
        let hex = |key: &str, dst: &mut [u8; 4]| {
            if let Some(c) = o.get(key).and_then(|x| x.as_str()).and_then(parse_hex) {
                *dst = [c[0], c[1], c[2], dst[3]];
            }
        };
        hex("bg", &mut p.bg);
        hex("fg", &mut p.fg);
        hex("surface", &mut p.surface);
        hex("surface_hover", &mut p.surface_hover);
        hex("surface_active", &mut p.surface_active);
        hex("border", &mut p.border);
        hex("text", &mut p.text);
        hex("text_dim", &mut p.text_dim);
        hex("text_mute", &mut p.text_mute);
        hex("success", &mut p.success);
        hex("danger", &mut p.danger);
        if let Some(arr) = o.get("ansi").and_then(|x| x.as_array()) {
            for (i, v) in arr.iter().take(16).enumerate() {
                if let Some(c) = v.as_str().and_then(parse_hex) {
                    p.ansi[i] = c;
                }
            }
        }
    }
    store_palette(&p);
    if let Ok(mut g) = CURRENT_THEME.lock() {
        *g = Some("custom");
    }
}

/// "#rrggbb" / "rrggbb" → RGB. Anything else → None (key is skipped).
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
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

/// 캐릭터명 → 고정 accent (pane 번호와 무관하게 학생=색 고정). 전원 원작색
/// 기준으로 교정(거노): 아로나=하늘, 프라나=은백(흰 계열), 유즈=핑크레드(분홍
/// 머리·빨간 리본), 아리스=남색. 미도리 민트·모모이 코랄은 원작과 이미 일치.
/// 미배정(순수 셸)은 None → 호출부가 테두리를 안 그린다.
pub fn character_accent(name: &str) -> Option<[u8; 4]> {
    Some(unpack(match name {
        "아로나" => 0x4a90e2_ff, // sky
        "프라나" => 0xe6e9f0_ff, // silver-white
        "미도리" => 0x6bcf7f_ff, // mint
        "모모이" => 0xff6b6b_ff, // coral
        "유즈" => 0xe64980_ff,   // pink-red
        "아리스" => 0x4c6ef5_ff, // navy-indigo
        // 확장 로스터(거노 2026-07-13 확정 6명) — 원작 외형 기준.
        "유우카" => 0x7a5fd4_ff, // violet (청보라 머리)
        "시로코" => 0x8fb8d8_ff, // gray-blue (회색머리·하늘 스카프)
        "호시노" => 0xf2a0c0_ff, // soft pink (분홍 머리)
        "코하루" => 0xf27b9b_ff, // rose (핑크 트윈테일·핑크 헤일로)
        "히마리" => 0xa88be0_ff, // lavender (은보라 머리)
        "아루" => 0xe85d4a_ff,   // red-orange (붉은 머리·주황 헤일로)
        // 학생 아님 — claude agents(에이전트 목록 뷰)의 SCHALE 조직 정체성 색.
        // render 가 argv(is_claude_agents)+프사 슬롯 부재로 목록 뷰를 판정해
        // 타이틀바 이름·테두리에만 쓴다(pane_character 엔 저장 안 함, 세션 진입
        // 시 배정 학생을 가리지 않게). 로스터엔 없어 랜덤 배정 후보 아님.
        "샬레" => 0x3a6eb4_ff,
        _ => return None,
    }))
}

/// 같은 학생이 여러 pane 에 떠 있을 때 n번째(0-기준) 인스턴스의 accent 변주 —
/// 학생 지정 스폰이 중복을 허용하므로 색으로 인스턴스를 구분한다(거노).
/// 0=원색, 이후 파스텔↔딥톤 교대 사다리. hue 는 유지해 학생 정체성과 타 학생
/// 색 충돌을 피하고, 단차는 한눈에 갈리게 크게(거노: 첫 판 20%는 미묘했음).
/// 프라나처럼 원색이 흰 계열이면 밝히는 쪽이 안 보여 딥톤 사다리만 탄다.
pub fn accent_variant(base: [u8; 4], ordinal: usize) -> [u8; 4] {
    if ordinal == 0 {
        return base;
    }
    let lum = 0.299 * base[0] as f32 + 0.587 * base[1] as f32 + 0.114 * base[2] as f32;
    let dark = [24, 26, 34, 255];
    let light = [255, 255, 255, 255];
    if lum > 170.0 {
        // 밝은 원색: 딥톤 단독 사다리 — 1번째 35%, 이후 25%p 씩 진하게.
        lerp(base, dark, (0.35 + 0.25 * (ordinal - 1) as f32).min(0.75))
    } else if ordinal % 2 == 1 {
        // 홀수 순번: 파스텔 — 45% 부터 시작해 회차마다 20%p 연하게.
        lerp(base, light, (0.45 + 0.20 * ((ordinal - 1) / 2) as f32).min(0.8))
    } else {
        // 짝수 순번: 딥톤 — 40% 부터 시작해 회차마다 20%p 진하게.
        lerp(base, dark, (0.40 + 0.20 * (ordinal / 2 - 1) as f32).min(0.8))
    }
}

/// pane 의 같은-학생 인스턴스 순번(0-기준) — pane_character 맵에서 같은 이름인
/// pane 들을 id 숫자순으로 세워 이 pane 의 위치를 돌려준다(accent_variant 의
/// ordinal). pane 이 닫히면 뒷 순번이 앞으로 당겨져 색도 원색 쪽으로 이동한다.
pub fn character_ordinal(
    chars: &std::collections::HashMap<String, String>,
    pane: &str,
) -> usize {
    let Some(name) = chars.get(pane) else { return 0 };
    let mut ids: Vec<&str> = chars
        .iter()
        .filter(|(_, n)| *n == name)
        .map(|(p, _)| p.as_str())
        .collect();
    ids.sort_by_key(|p| p.trim_start_matches('%').parse::<u64>().unwrap_or(u64::MAX));
    ids.iter().position(|p| *p == pane).unwrap_or(0)
}

/// character_accent 에 같은-학생 순번 변주를 얹은 판 — pane 테두리·입력박스
/// 보더(@배지)·배너가 공통으로 쓴다. (본문 틴트도 썼었으나 폐기 — 출력 글자는
/// 테마 기본 fg, 거노 2026-07-18.)
pub fn character_accent_n(name: &str, ordinal: usize) -> Option<[u8; 4]> {
    character_accent(name).map(|c| accent_variant(c, ordinal))
}

/// 캐릭터명 → 에셋 슬러그 (assets/students/<slug>.png, arona-ui 디렉토리명과 동일).
pub fn character_slug(name: &str) -> Option<&'static str> {
    Some(match name {
        "아로나" => "arona",
        "프라나" => "prana",
        "미도리" => "midori",
        "모모이" => "momoi",
        "유즈" => "yuzu",
        "아리스" => "arisu",
        "유우카" => "yuuka",
        "시로코" => "shiroko",
        "호시노" => "hoshino",
        "코하루" => "koharu",
        "히마리" => "himari",
        "아루" => "aru",
        _ => return None,
    })
}

#[cfg(test)]
mod accent_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ordinal_orders_same_character_panes_by_pane_id() {
        let mut m = HashMap::new();
        m.insert("%7".to_string(), "시로코".to_string());
        m.insert("%2".to_string(), "시로코".to_string());
        m.insert("%3".to_string(), "유즈".to_string());
        assert_eq!(character_ordinal(&m, "%2"), 0);
        assert_eq!(character_ordinal(&m, "%7"), 1);
        assert_eq!(character_ordinal(&m, "%3"), 0);
        assert_eq!(character_ordinal(&m, "%99"), 0); // 미배정 pane
    }

    #[test]
    fn variant_distinguishes_duplicates_including_light_bases() {
        // 어떤 로스터 색이든 0·1·2번째가 서로 달라야 중복 pane 이 구분된다 —
        // 특히 프라나(흰 계열)는 밝히는 변주가 안 보이므로 사다리 방향 검증.
        for name in [
            "아로나", "프라나", "미도리", "모모이", "유즈", "아리스", "유우카", "시로코",
            "호시노", "코하루", "히마리", "아루",
        ] {
            let base = character_accent(name).unwrap();
            let v1 = accent_variant(base, 1);
            let v2 = accent_variant(base, 2);
            assert_eq!(accent_variant(base, 0), base, "{name}: 0번째는 원색");
            assert_ne!(v1, base, "{name}: 1번째 변주가 원색과 같음");
            assert_ne!(v2, base, "{name}: 2번째 변주가 원색과 같음");
            assert_ne!(v1, v2, "{name}: 1·2번째 변주가 서로 같음");
        }
    }
}
