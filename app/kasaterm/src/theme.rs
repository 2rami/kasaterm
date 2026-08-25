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

/// 「내 손을 기다린다」 색. 상태 언어의 네 번째 자리 — accent=도는 중,
/// success=끝남, danger=잘못됨, 그리고 이것은 **막혀서 나를 부르는 중**이다.
/// 예전엔 danger 를 빌려 썼는데 빨강은 "고장났다"로 읽혀, 승인 한 번이면 풀릴
/// 일이 사고처럼 보였다(거노: "내가 엔터해야되는 건 핑크색으로").
///
/// 팔레트 슬롯이 아니라 고정값인 이유: 이건 테마 취향이 아니라 신호다. 테마마다
/// 달라지면 같은 뜻이 창마다 다른 색으로 읽힌다. 밝기를 중간에 둬 밝은 테마의
/// 흰 바탕과 어두운 테마의 검정 바탕 양쪽에서 다 떠오른다.
///
/// 핑크였다가 주황으로 옮겼다(2026-08-11 지시: "선택하는거 뜨면 주황색으로
/// 깜빡이게"). 한 군데만 바꾸지 않고 토큰째 옮기는 건 위 문단 그대로의 이유다 —
/// 같은 신호가 사이드바에선 주황, pane 헤더에선 핑크면 두 색을 각각 외워야 한다.
pub fn attention() -> [u8; 4] {
    [250, 140, 42, 255]
}

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

/// 호박색 인광 모니터. `shape: "pixel"` 과 짝이 되라고 만든 팔레트다 — 각진
/// 실루엣과 픽셀 글꼴만으로는 "모서리 없는 다크 테마" 에서 멈추고, 색까지
/// 그 시절을 가리켜야 레트로로 읽힌다.
///
/// 단색으로는 안 갔다. 인광 하나로 화면을 칠하면 그럴싸하지만 ANSI 16색이
/// 전부 같은 호박색이 되어 ls 도 diff 도 못 읽는다. 크롬만 호박으로 묶고
/// 셀 색은 그 시절 하드웨어(CGA/EGA)의 채도로 가져와 구분을 남겼다.
const AMBER_CRT: Palette = Palette {
    // 인광 화면의 검정은 순수 검정이 아니라 미열이 도는 갈색이다.
    bg: [0x1A, 0x13, 0x0C, 255],
    fg: [0xFF, 0xB0, 0x00, 255],
    surface: [0x25, 0x1B, 0x11, 255],
    surface_hover: [0x36, 0x27, 0x17, 255],
    surface_active: [0x48, 0x34, 0x1D, 255],
    border: [0x5E, 0x45, 0x22, 150],
    text: [0xFF, 0xB0, 0x00, 255],
    // 흐린 층을 다른 다크 테마보다 높게 잡았다. 픽셀 글꼴은 획이 1px 이라
    // 같은 명도라도 회색 글꼴보다 훨씬 묽게 보인다 — 첫 값으로는 부제와
    // "listen 중인 포트 없음" 이 배경에 잠겼다.
    text_dim: [0xDB, 0xA2, 0x4A, 255],
    text_mute: [0xA0, 0x76, 0x36, 255],
    success: [0x8F, 0xCF, 0x45, 255],
    danger: [0xFF, 0x5A, 0x33, 255],
    syn_keyword: [0xFF, 0x8C, 0x42, 255],
    syn_string: [0xC2, 0xDC, 0x50, 255],
    syn_number: [0xFF, 0xD9, 0x66, 255],
    syn_comment: [0xA0, 0x76, 0x36, 255],
    syn_function: [0xFF, 0xB0, 0x00, 255],
    syn_type: [0xFF, 0x7A, 0xC8, 255],
    ansi: [
        [0x25, 0x1B, 0x11], [0xE8, 0x50, 0x3F], [0x8F, 0xCF, 0x45], [0xFF, 0xB0, 0x00],
        [0x5B, 0x9B, 0xE8], [0xE8, 0x5F, 0xBF], [0x46, 0xC8, 0xD8], [0xE8, 0xD0, 0xA8],
        [0xA0, 0x76, 0x36], [0xFF, 0x74, 0x5C], [0xAD, 0xE0, 0x60], [0xFF, 0xD9, 0x66],
        [0x7C, 0xB5, 0xF5], [0xFF, 0x84, 0xD4], [0x6E, 0xE0, 0xEE], [0xFF, 0xF0, 0xD8],
    ],
};

/// 나쵸네코. 캐릭터 테마와 짝이 되라고 만든 팔레트다 — 나쵸 pane 은 헤더 밴드가
/// `#55D6C2` 이고(`theme-src-nacho/roster.json`, panebridge 의 탭 색과 같은 값),
/// 창 전체를 나쵸로 두고 싶을 때 그 민트가 겉돌지 않는 바닥이 필요하다.
///
/// 바닥을 중성 회색으로 두지 않았다. 민트는 차가운 색이라 미열이 도는 회색 위에
/// 얹으면 서로를 밀어내 액센트가 화면에서 뜬다 — 배경에도 같은 청록기를 옅게
/// 섞어야 한 벌로 읽힌다. 대신 셀 색까지 민트로 묶지는 않았다(AMBER_CRT 가 단색을
/// 피한 것과 같은 이유). 캐릭터가 은발에 청록 눈이라, 함수·링크 자리에는 원화에서
/// 그대로 뽑은 눈 색(`#59AEC8`)을 두어 같은 계열 안에서 단차를 준다.
const NACHO: Palette = Palette {
    bg: [0x15, 0x1B, 0x1D, 255],
    fg: [0xE4, 0xEC, 0xEC, 255],
    surface: [0x1C, 0x24, 0x26, 255],
    surface_hover: [0x25, 0x2F, 0x31, 255],
    surface_active: [0x30, 0x3C, 0x3E, 255],
    border: [0x3C, 0x4B, 0x4D, 130],
    text: [0xE4, 0xEC, 0xEC, 255],
    text_dim: [0xA4, 0xB4, 0xB4, 255],
    text_mute: [0x6E, 0x7E, 0x7F, 255],
    success: [0x7F, 0xD9, 0xA0, 255],
    danger: [0xFF, 0x7B, 0x72, 255],
    syn_keyword: [0x55, 0xD6, 0xC2, 255],   // 나쵸 민트 — 로스터 header_color 와 같은 값
    syn_string: [0xA8, 0xE0, 0xB4, 255],
    syn_number: [0xF2, 0xD6, 0x8A, 255],
    syn_comment: [0x6E, 0x7E, 0x7F, 255],
    syn_function: [0x59, 0xAE, 0xC8, 255],  // 원화에서 뽑은 눈 색
    syn_type: [0xC9, 0xD8, 0xE4, 255],      // 은발
    ansi: [
        [0x1C, 0x24, 0x26], [0xFF, 0x7B, 0x72], [0x7F, 0xD9, 0xA0], [0xF2, 0xD6, 0x8A],
        [0x59, 0xAE, 0xC8], [0xC2, 0x9B, 0xE0], [0x55, 0xD6, 0xC2], [0xE4, 0xEC, 0xEC],
        [0x6E, 0x7E, 0x7F], [0xFF, 0x9B, 0x94], [0xA8, 0xE0, 0xB4], [0xFF, 0xE7, 0xAC],
        [0x8A, 0xCB, 0xDD], [0xD8, 0xBA, 0xEE], [0x86, 0xE6, 0xD8], [0xFF, 0xFF, 0xFF],
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
    ("amber-crt", "Amber CRT", &AMBER_CRT),
    ("nacho", "Nachoneko", &NACHO),
];

/// 지금 store_palette 가 **미리보기** 픽으로 불렸나. OS 색 패널의 휠을 돌리는
/// 동안 초당 수십 번 오는 경로라, 화면 밖으로 나가는 쓰기(claude 설정 파일)는
/// 이때 건너뛴다 — 손을 뗄 때 진짜 커밋이 같은 값으로 한 번 더 지나간다.
static PREVIEWING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
    // pane 안에서 도는 TUI 도 같은 색을 봐야 한다. 이 셋(fg/bg/cursor)이 OSC
    // 10/11/12 질의의 답이 되고, Claude Code 의 `theme: auto` 는 그 배경색으로
    // 밝은 테마와 어두운 테마를 가른다 — 안 넘기면 라이트로 바꿔도 안쪽만 어둡다.
    kasa_pty::set_host_colors(
        (p.bg[0], p.bg[1], p.bg[2]),
        (p.fg[0], p.fg[1], p.fg[2]),
        // 커서는 팔레트에 없다 — 밝은 배경에선 fg 쪽이, 어두운 배경에선 ANSI 7
        // (off-white)이 실제 커서에 가깝다.
        if is_light(p.bg) {
            (p.fg[0], p.fg[1], p.fg[2])
        } else {
            (p.ansi[7][0], p.ansi[7][1], p.ansi[7][2])
        },
    );
    // Accent intentionally not touched here — see set_accent.
    //
    // pane 안에서 **이미 도는** Claude Code 까지 따라오게 하는 건 호스트 색
    // 응답이 아니라 이쪽이다 — 설정 파일을 고쳐 쓰면 Claude 가 감시하다 즉시
    // 리로드한다. 팔레트가 실제로 갈리는 지점은 여기뿐이라(프리셋·custom·
    // system 폴링 전부 통과) 훅도 여기 하나면 된다.
    // 미리보기(색 패널 드래그) 중에는 건너뛴다 — 이 한 줄이 claude 설정 파일과
    // 커스텀 테마 파일을 굽는다.
    if !PREVIEWING.load(Ordering::Relaxed) {
        crate::socket::sync_claude_theme(is_light(p.bg));
    }
}

/// 배경이 밝은 쪽인가. ITU-R BT.601 휘도 — 사람이 느끼는 밝기에 맞춰 녹색에
/// 가중치를 준다. 단순 평균은 순수 파랑(#0000FF)을 밝다고 판정한다.
fn is_light(bg: [u8; 4]) -> bool {
    let l = 0.299 * f32::from(bg[0]) + 0.587 * f32::from(bg[1]) + 0.114 * f32::from(bg[2]);
    l > 128.0
}

/// 지금 팔레트가 라이트인가 — 테마 플립 감지(실행 중 claude 재테마)가 틱마다
/// 비교하는 값. `is_light` 판정과 같은 축을 쓰므로 `sync_claude_theme` 이 쓰는
/// 명암과 절대 어긋나지 않는다.
pub(crate) fn current_is_light() -> bool {
    is_light(bg())
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

/// Active theme key ("dark", "catppuccin-mocha", "custom:<slug>"…) for the
/// settings screen's selected state. Colors alone can't tell presets apart, so
/// the key is tracked at set time.
///
/// 문자열을 소유하는 이유: 커스텀 팔레트 키는 설정 파일에서 읽은 slug 를 달고
/// 태어나(`custom:midnight`) 컴파일 시점에 존재하지 않는다.
static CURRENT_THEME: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn theme_name() -> String {
    CURRENT_THEME
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "dark".to_string())
}

fn set_current(key: &str) {
    if let Ok(mut g) = CURRENT_THEME.lock() {
        *g = Some(key.to_string());
    }
}

/// OS 가 지금 밝은 모드인가. 판단이 안 서면 `None` — 그때는 다크로 둔다(이 앱의
/// 원래 모습이고, 모르는 상태에서 화면을 하얗게 뒤집는 것보다 낫다).
///
/// macOS 는 창이 아니라 **시스템 설정 자체**를 읽는다. `window.theme()` 을 쓰면
/// 우리가 창마다 `with_theme(Theme::Dark)` 로 장식을 고정해 둔 값이 되돌아와,
/// 시스템이 무엇이든 항상 다크라고 답한다.
#[cfg(target_os = "macos")]
fn system_is_light() -> Option<bool> {
    use objc2_foundation::{NSString, NSUserDefaults};
    // AppleInterfaceStyle 은 다크일 때만 "Dark" 로 있고, 라이트면 키가 아예 없다.
    let key = NSString::from_str("AppleInterfaceStyle");
    let style = NSUserDefaults::standardUserDefaults().stringForKey(&key);
    Some(!style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("Dark")))
}

#[cfg(not(target_os = "macos"))]
fn system_is_light() -> Option<bool> {
    // 다른 플랫폼은 창에서 받아 온 값을 쓴다 — handler 가 winit 의 theme 을
    // 여기 넣어 준다(Windows 는 그 경로가 레지스트리를 본다).
    let v = SYSTEM_IS_LIGHT.load(Ordering::Relaxed);
    (v != 0).then(|| v == 2)
}

/// `system_is_light` 의 비-macOS 백업 저장소. 0=모름, 1=다크, 2=라이트.
static SYSTEM_IS_LIGHT: AtomicU32 = AtomicU32::new(0);

/// `system` 모드일 때 **마지막으로 적용한** 해석 결과(0=dark, 1=light). 폴링이
/// 이것과 지금 OS 값을 비교해 바뀐 순간에만 다시 칠한다.
static SYSTEM_RESOLVED: AtomicU32 = AtomicU32::new(0);

/// OS 다크/라이트가 방금 바뀌었으면 새 팔레트를 적용하고 `true`.
///
/// 알림을 구독하지 않고 폴링하는 건, 알림 옵저버가 플랫폼마다 다른 배선을
/// 요구하는 데 비해 이 조회가 캐시된 값 하나를 읽는 정도로 싸기 때문이다.
/// 사람이 시스템 테마를 바꾸는 빈도를 생각하면 한 틱 늦게 따라가도 무해하다.
pub fn poll_system_theme() -> bool {
    if theme_name() != "system" {
        return false;
    }
    // 호출부는 매 프레임 도는 틱이라 게이트를 여기 둔다 — 부르는 쪽마다 타이머를
    // 챙기게 하면 새 호출부가 생길 때 조용히 빠진다.
    {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static LAST: Mutex<Option<Instant>> = Mutex::new(None);
        let Ok(mut g) = LAST.lock() else { return false };
        if g.is_some_and(|t| t.elapsed() < Duration::from_millis(700)) {
            return false;
        }
        *g = Some(Instant::now());
    }
    let now = u32::from(system_is_light() == Some(true));
    if SYSTEM_RESOLVED.swap(now, Ordering::Relaxed) == now {
        return false;
    }
    // 슬롯 배정을 다시 읽어 그 밝기용 테마로 갈아입는다 — 내장 light/dark 로
    // 못 박으면 슬롯에 다른 팔레트를 배정한 뜻이 OS 플립 순간 사라진다.
    apply_system_palette();
    true
}

/// winit 이 창 테마를 알려줄 때 handler 가 부른다. macOS 에서는 창 장식을 고정해
/// 두어 이 값이 시스템을 안 나타내므로 무시한다.
pub fn note_window_theme(is_light: bool) {
    if cfg!(not(target_os = "macos")) {
        SYSTEM_IS_LIGHT.store(if is_light { 2 } else { 1 }, Ordering::Relaxed);
    }
}

/// `theme: "system"` 이 지금 어느 프리셋을 가리키는가.
pub fn system_theme_key() -> &'static str {
    if system_is_light() == Some(true) {
        "light"
    } else {
        "dark"
    }
}

/// system 모드에서 이 밝기 슬롯이 입을 테마 키 — 프리셋 키 또는 "custom".
/// 기본은 내장 light/dark 라 설정이 없으면 종전과 똑같이 동작한다.
/// (2026-08-15 지시 「시스템설정으로하면 라이트랑 다크밖에 못 쓰는데 그걸
/// 따로 설정할수있게」— OS 는 밝기만 알려 주고, 그 밝기에 무슨 팔레트를
/// 입을지는 사용자가 정한다.)
pub fn system_slot_theme(light: bool) -> String {
    system_slot_theme_in(&crate::socket::read_settings(), light)
}

fn system_slot_theme_in(s: &serde_json::Value, light: bool) -> String {
    let fallback = if light { "light" } else { "dark" };
    let raw = s
        .get(if light { "theme_system_light" } else { "theme_system_dark" })
        .and_then(|x| x.as_str())
        .unwrap_or(fallback);
    // 커스텀 배정은 **지금 실재하는 카드 키**로 굳혀 돌려준다. 설정 화면의 슬롯
    // 배지는 카드 키와 문자열로 견주므로, 옛 `"custom"` 이나 지워진 slug 를 그대로
    // 내보내면 어느 카드도 안 눌린 것처럼 보인다 — 배정해 둔 사실이 화면에서
    // 사라진다. 가리키던 팔레트가 없어졌으면 내장으로 떨어지고, 이는
    // `apply_system_palette` 의 폴백과 같은 판정이다.
    if let Some(slug) = custom_key(raw) {
        let list = custom_themes(s);
        return match find_custom(&list, slug) {
            Some(e) => format!("custom:{}", custom_slug(e)),
            None => fallback.to_string(),
        };
    }
    raw.to_string()
}

/// system 모드가 지금 이 순간 실제로 입힐 팔레트를 굳힌다 — OS 밝기가 가리키는
/// 슬롯의 배정 테마(프리셋 or custom)를 적용하고, CURRENT_THEME 은 "system"
/// 으로 유지한다(슬롯이 custom 이어도 — apply_custom_theme 이 "custom" 을 적는
/// 것을 되돌린다. 저장/표시 정본은 여전히 "시스템 따라가기"다).
fn apply_system_palette() {
    let s = crate::socket::read_settings();
    let light = system_theme_key() == "light";
    let slot = system_slot_theme_in(&s, light);
    // 배정이 커스텀인데 그 팔레트가 지워졌으면 아래 프리셋 분기로 떨어진다 —
    // 지운 팔레트를 기다리느라 화면이 안 바뀌면 「시스템 따라가기가 고장났다」로
    // 읽힌다.
    let applied = custom_key(&slot).is_some_and(|slug| {
        let list = custom_themes(&s);
        find_custom(&list, slug).map(|e| store_palette(&custom_palette(e))).is_some()
    });
    if !applied {
        if let Some((_, _, p)) = THEME_PRESETS
            .iter()
            .find(|(k, _, _)| *k == slot.as_str())
            // 배정된 테마가 사라졌으면(설정 파일 수기 수정 등) 내장 기본으로.
            .or_else(|| THEME_PRESETS.iter().find(|(k, _, _)| *k == system_theme_key()))
        {
            store_palette(p);
        }
    }
    set_current("system");
}

/// Switch to a preset theme by key; unknown keys fall back to dark.
/// `custom:<slug>`(및 옛 `custom`)은 설정 파일의 팔레트를 다시 읽는다.
pub fn set_theme(mode: &str) {
    if let Some(slug) = custom_key(mode) {
        apply_custom_theme(&crate::socket::read_settings(), slug);
        return;
    }
    // 저장되는 값은 "system" 그대로다 — 지금 해석한 결과(dark/light)를 적어 버리면
    // 다음에 켤 때 그 순간의 OS 설정이 고정값으로 굳어 따라다니길 그만둔다.
    if mode == "system" {
        SYSTEM_RESOLVED.store(u32::from(system_theme_key() == "light"), Ordering::Relaxed);
        apply_system_palette();
        return;
    }
    let (key, _, p) = THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == mode)
        .unwrap_or(&THEME_PRESETS[0]);
    store_palette(p);
    set_current(key);
}

// ── 커스텀 팔레트 ────────────────────────────────────────────────────────
// 프리셋은 불변이고 편집은 복제본에 쌓인다. 그 복제본이 **여럿**일 수 있다는 것이
// 이 절의 전부다(2026-08-15 지시 「커스텀 테마 하나밖에 추가못해?」).
//
// 저장 정본은 `custom_themes` 배열이고, 하나뿐이던 시절의 `custom_theme` 오브젝트는
// 읽기에서만 살아 첫 항목으로 들어온다 — 쓰기는 언제나 배열로 나가므로 아무 편집이나
// 한 번 하면 자연히 새 형식으로 넘어간다. 옛 키를 지우지는 않는다: 지워 봐야 얻는
// 것이 없고, 구버전으로 되돌아간 사람의 팔레트가 통째로 사라진다.

/// 옛 단일 커스텀이 배열로 들어올 때 갖는 slug.
pub const LEGACY_CUSTOM_SLUG: &str = "custom";

/// 테마 키가 커스텀을 가리키면 그 slug — 옛 값 `"custom"` 은 빈 문자열(=첫 항목).
pub fn custom_key(key: &str) -> Option<&str> {
    if key == LEGACY_CUSTOM_SLUG {
        Some("")
    } else {
        key.strip_prefix("custom:")
    }
}

/// settings.json 의 커스텀 팔레트들. 새 형식이 있으면 **그것만** 읽는다 — 둘 다
/// 있을 때 옛 오브젝트를 덧붙이면 마이그레이션 뒤 첫 항목이 두 벌로 보인다.
pub fn custom_themes(s: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(arr) = s.get("custom_themes").and_then(|x| x.as_array()) {
        return arr.iter().filter(|e| e.is_object()).cloned().collect();
    }
    match s.get("custom_theme") {
        Some(v) if v.is_object() => vec![v.clone()],
        _ => Vec::new(),
    }
}

/// 항목의 slug — 옛 오브젝트에는 없으므로 그때는 `"custom"`.
pub fn custom_slug(e: &serde_json::Value) -> String {
    e.get("slug")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(LEGACY_CUSTOM_SLUG)
        .to_string()
}

/// 화면에 뜨는 이름. 사용자가 고칠 수 있고, 없으면 카드가 비어 보이므로 기본값을 준다.
pub fn custom_label(e: &serde_json::Value) -> String {
    e.get("label")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Custom")
        .to_string()
}

/// 목록에서 slug 하나를 고른다. 빈 slug(옛 `"custom"`)거나 못 찾으면 첫 항목 —
/// 설정 파일을 손으로 고쳐 사라진 slug 를 가리키게 됐을 때 아무것도 안 입는 것보다
/// 낫다.
pub fn find_custom<'a>(
    list: &'a [serde_json::Value],
    slug: &str,
) -> Option<&'a serde_json::Value> {
    if !slug.is_empty() {
        if let Some(e) = list.iter().find(|e| custom_slug(e) == slug) {
            return Some(e);
        }
    }
    list.first()
}

/// 새 팔레트의 (slug, 라벨) — 기존 것과 안 겹치는 `palette-N` / `Custom N`.
pub fn next_custom_name(list: &[serde_json::Value]) -> (String, String) {
    (1u32..)
        .map(|n| (format!("palette-{n}"), format!("Custom {n}")))
        .find(|(slug, label)| {
            !list.iter().any(|e| &custom_slug(e) == slug || &custom_label(e) == label)
        })
        .unwrap_or_else(|| (LEGACY_CUSTOM_SLUG.to_string(), "Custom".to_string()))
}

/// 커스텀 항목 → 실제 팔레트. base 프리셋 위에 적힌 키만 덮는다. 모양:
/// `{ "slug": "palette-1", "label": "Custom 1", "base": "dark",
///    "bg": "#252c35", …, "ansi": ["#1d1f21", …×16] }`
/// Unknown/missing keys keep the base value, so a partial entry is fine.
pub fn custom_palette(e: &serde_json::Value) -> Palette {
    let base_key = e.get("base").and_then(|x| x.as_str()).unwrap_or("dark");
    let (_, _, base) = THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == base_key)
        .unwrap_or(&THEME_PRESETS[0]);
    let mut p: Palette = **base;
    let Some(o) = e.as_object() else { return p };
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
    p
}

/// 커스텀 하나를 입는다. CURRENT_THEME 에는 언제나 **찾아낸 항목의** slug 를
/// 적는다(옛 `"custom"` 으로 들어와도) — 화면의 선택 표시가 목록의 어느 카드인지
/// 정확히 가리켜야 한다.
fn apply_custom_theme(s: &serde_json::Value, slug: &str) {
    let list = custom_themes(s);
    let Some(e) = find_custom(&list, slug) else {
        // 목록이 비었는데 custom 이 걸려 있다 — 마지막 팔레트를 지웠거나 설정
        // 파일을 손으로 비운 경우다. 색이 그대로면 「지워지지 않았다」로 읽히므로
        // 내장 기본으로 되돌린다.
        store_palette(&DARK);
        set_current("dark");
        return;
    };
    store_palette(&custom_palette(e));
    set_current(&format!("custom:{}", custom_slug(e)));
}

/// 설정 파일에 없는 팔레트를 **화면에만** 입힌다. `set_theme` 과 달리 저장된
/// 설정을 다시 읽지 않고 넘겨받은 값을 그대로 쓰며, pane 안 claude 의 테마
/// 파일도 건드리지 않는다.
///
/// 색을 고르는 동안(OS 색 패널의 휠 · 네이티브 피커 드래그) 매 이벤트가 이리로
/// 오기 때문이다. 그때마다 settings.json 을 쓰면 파일 쓰기가 폭주하고, 정작
/// 화면은 원자 슬롯만 갈면 되므로 파일을 거칠 이유가 없다. 굳히는 것은 손을
/// 뗄 때 오는 진짜 커밋(`set_theme`)의 몫이다.
pub fn preview_custom_theme(s: &serde_json::Value, slug: &str) {
    PREVIEWING.store(true, Ordering::Relaxed);
    apply_custom_theme(s, slug);
    PREVIEWING.store(false, Ordering::Relaxed);
}

/// 지금 편집 대상이 되는 커스텀의 slug — 커스텀을 입고 있지 않으면 `None`.
pub fn active_custom_slug() -> Option<String> {
    let name = theme_name();
    custom_key(&name).map(|s| s.to_string())
}

/// 지금 입고 있는 색을 **새** 커스텀 항목으로 복제한다(이름은 목록과 안 겹치게).
///
/// 커스텀을 입고 있으면 그 항목을 통째로 베낀다 — 「지금 팔레트를 복제」가 base 만
/// 물려받으면 눈앞의 색이 안 따라와, 복제했는데 다른 색이 나온다.
pub fn clone_current_custom(s: &serde_json::Value) -> serde_json::Value {
    let list = custom_themes(s);
    let (slug, label) = next_custom_name(&list);
    let name = theme_name();
    let mut e = match custom_key(&name).and_then(|want| find_custom(&list, want)) {
        Some(src) => src.clone(),
        None => {
            // "system" 은 팔레트가 아니다 — 그 순간 가리키는 프리셋이 시작점.
            let base = if name == "system" { system_theme_key().to_string() } else { name };
            custom_theme_seed(&base, &slug, &label)
        }
    };
    if let Some(o) = e.as_object_mut() {
        o.insert("slug".to_string(), serde_json::Value::String(slug));
        o.insert("label".to_string(), serde_json::Value::String(label));
    }
    e
}

/// "#rrggbb" / "rrggbb" → RGB. Anything else → None (key is skipped).
pub(crate) fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

pub(crate) fn hex_str(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `custom_theme` 이 다루는 UI 색 — (settings.json 키, Palette 필드 접근자).
/// `apply_custom_theme` 의 `hex(...)` 호출 목록과 짝이다: 여기 늘리면 저쪽도
/// 늘려야 화면과 파일이 같은 것을 말한다. 라벨을 따로 안 두는 건 설정 화면이
/// 키를 그대로 보여 주기 때문 — 파일을 손으로 고칠 때 같은 이름을 찾게 된다.
/// syn_* 는 뺐다: apply_custom_theme 이 아직 안 읽는 키를 UI 에 먼저 내면
/// 고쳐도 안 먹는 칸이 생긴다.
pub const PALETTE_KEYS: &[(&str, fn(&Palette) -> [u8; 4])] = &[
    ("bg", |p| p.bg),
    ("fg", |p| p.fg),
    ("surface", |p| p.surface),
    ("surface_hover", |p| p.surface_hover),
    ("surface_active", |p| p.surface_active),
    ("border", |p| p.border),
    ("text", |p| p.text),
    ("text_dim", |p| p.text_dim),
    ("text_mute", |p| p.text_mute),
    ("success", |p| p.success),
    ("danger", |p| p.danger),
];

/// 프리셋 하나를 커스텀 항목 JSON 으로 복제한다 — 팔레트 편집의 시작점.
/// 부분 파일도 동작은 하지만(빠진 키는 base 값) 모든 키를 명시해 쓴다:
/// 파일을 열었을 때 고칠 수 있는 키가 다 보여야 발견이 된다.
pub fn custom_theme_seed(base_key: &str, slug: &str, label: &str) -> serde_json::Value {
    let (key, _, p) = THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == base_key)
        .unwrap_or(&THEME_PRESETS[0]);
    let mut o = serde_json::Map::new();
    o.insert("slug".to_string(), serde_json::Value::String(slug.to_string()));
    o.insert("label".to_string(), serde_json::Value::String(label.to_string()));
    o.insert("base".to_string(), serde_json::Value::String((*key).to_string()));
    for (k, get) in PALETTE_KEYS {
        let c = get(p);
        o.insert((*k).to_string(), serde_json::Value::String(hex_str([c[0], c[1], c[2]])));
    }
    o.insert(
        "ansi".to_string(),
        serde_json::Value::Array(
            p.ansi.iter().map(|c| serde_json::Value::String(hex_str(*c))).collect(),
        ),
    );
    serde_json::Value::Object(o)
}

pub fn set_accent(name: &str) {
    S_ACCENT.store(pack(accent_color(name)), Ordering::Relaxed);
    // 강조색은 팔레트 적용 경로(`apply_palette`)를 안 지난다 — 그래서 claude 쪽
    // 커스텀 테마도 여기서 따로 다시 구워야 한다. 안 그러면 강조색만 바꿨을 때
    // 터미널은 새 색인데 claude 는 옛 색으로 남는다.
    crate::socket::write_claude_custom_theme(is_light(bg()));
}

/// Apply persisted theme + accent from settings.json at launch.
pub fn apply_from_settings() {
    let s = crate::socket::read_settings();
    let mode = s.get("theme").and_then(|x| x.as_str()).unwrap_or("dark");
    set_theme(mode);
    let accent = s.get("accent").and_then(|x| x.as_str()).unwrap_or("blue");
    set_accent(accent);
    // KASATERM_SHAPE overrides the stored key so a silhouette can be previewed
    // (or screenshot-verified) without editing the live settings file — the
    // settings file is shared with the running app, so a test that rewrote it
    // would change the user's own window mid-session.
    let shape = std::env::var("KASATERM_SHAPE").ok().unwrap_or_else(|| {
        s.get("shape")
            .and_then(|x| x.as_str())
            .unwrap_or("rounded")
            .to_string()
    });
    set_shape(&shape);
    if let Some(v) = s.get("min_contrast").and_then(|x| x.as_f64()) {
        set_min_contrast(v as f32);
    }
}

// ── Shape axis ───────────────────────────────────────────────────────────
// Form lives on its own axis, independent of the color Palette: a palette says
// what something is colored, a shape says what silhouette it's cut to. Keeping
// them apart means any palette can be worn with any silhouette (Schale's colors
// with pixel corners), which is the same reason accent isn't baked into Palette
// — see `set_accent`. Baking radii into Palette would multiply presets instead.

/// Define an atomic f32 slot + its reader fn. f32 has no atomic type, so the
/// bit pattern rides in an AtomicU32 (`to_bits` is const since 1.83).
macro_rules! f32_slot {
    ($slot:ident, $get:ident, $default:expr) => {
        static $slot: AtomicU32 = AtomicU32::new(($default as f32).to_bits());
        #[inline]
        pub fn $get() -> f32 {
            f32::from_bits($slot.load(Ordering::Relaxed))
        }
    };
}

f32_slot!(S_RADIUS_SM, radius_sm, 6.0);
f32_slot!(S_RADIUS_MD, radius_md, 9.0);
f32_slot!(S_BORDER_W, border_w, 1.0);
f32_slot!(S_SHADOW_OFFSET, shadow_offset, 0.0);
f32_slot!(S_ROUNDNESS, roundness, 1.0);

/// One silhouette. Radii feed the native `round_rect` helper; the rest are the
/// knobs a pixel/sharp look needs beyond corners.
#[derive(Clone, Copy)]
pub struct Shape {
    pub radius_sm: f32,
    pub radius_md: f32,
    /// Structural hairline thickness (logical px).
    pub border_w: f32,
    /// Hard drop-shadow offset; 0 = none. Pixel UIs use a blur-less offset that
    /// shrinks under a press — that displacement is what reads as physical.
    pub shadow_offset: f32,
    /// How far shapes that mean *circle* or *capsule* — status dots, avatar
    /// chips, pill toggles, scrollbar thumbs — bend toward square. 1 = true
    /// circle, 0 = square. Separate from the radii because a circle isn't a
    /// rounded corner: squaring it is a deliberate silhouette choice (a pixel
    /// UI wants square dots, not slightly-rounded ones), and scaling it off
    /// `radius_sm` would make a 6px dot and a 200px panel bend together.
    /// Consumed through the `circle_rect` / `pill_rect` helpers.
    pub roundness: f32,
    /// Draw chrome labels and icons from the bundled dot-matrix assets instead
    /// of the terminal font and the Lucide set. It rides on Shape rather than a
    /// separate axis because a dot-matrix typeface next to 6px-rounded corners
    /// reads as a mistake, not a choice — and unlike the radii, there is no
    /// in-between value to mix.
    pub pixel_chrome: bool,
}

/// Whether chrome text and icons come from the pixel assets (see `Shape::pixel_chrome`).
pub fn pixel_chrome() -> bool {
    S_PIXEL_CHROME.load(Ordering::Relaxed) != 0
}
static S_PIXEL_CHROME: AtomicU32 = AtomicU32::new(0);

/// Today's look — the values that shipped as `RADIUS_SM` / `RADIUS_MD`, so a
/// fresh config renders byte-identical to before the axis existed.
const SHAPE_ROUNDED: Shape = Shape {
    radius_sm: 6.0,
    radius_md: 9.0,
    border_w: 1.0,
    shadow_offset: 0.0,
    roundness: 1.0,
    pixel_chrome: false,
};

/// Softened corners kept only where they aid hit-reading; no shadow. Dots and
/// toggles stay round — this sharpens the *chrome*, not every mark on it.
const SHAPE_SHARP: Shape = Shape {
    radius_sm: 2.0,
    radius_md: 3.0,
    border_w: 1.0,
    shadow_offset: 0.0,
    roundness: 1.0,
    pixel_chrome: false,
};

/// Square corners, doubled rules, hard offset shadow — the munder pixel-kit
/// language (PixelButton/PixelBadge), which is form rather than color.
const SHAPE_PIXEL: Shape = Shape {
    radius_sm: 0.0,
    radius_md: 0.0,
    border_w: 2.0,
    shadow_offset: 3.0,
    roundness: 0.0,
    pixel_chrome: true,
};

/// Selectable silhouettes: (settings.json key, display label, shape).
pub const SHAPE_PRESETS: &[(&str, &str, &Shape)] = &[
    ("rounded", "Rounded", &SHAPE_ROUNDED),
    ("sharp", "Sharp", &SHAPE_SHARP),
    ("pixel", "Pixel", &SHAPE_PIXEL),
];

/// Active shape key for the settings screen's selected state — the numbers
/// alone can't tell presets apart, so the key is tracked at set time (mirrors
/// CURRENT_THEME).
static CURRENT_SHAPE: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

pub fn shape_name() -> &'static str {
    CURRENT_SHAPE.lock().ok().and_then(|g| *g).unwrap_or("rounded")
}

/// Switch silhouette by key; unknown keys fall back to rounded.
pub fn set_shape(key: &str) {
    let (k, _, s) = SHAPE_PRESETS
        .iter()
        .find(|(k, _, _)| *k == key)
        .unwrap_or(&SHAPE_PRESETS[0]);
    S_RADIUS_SM.store(s.radius_sm.to_bits(), Ordering::Relaxed);
    S_RADIUS_MD.store(s.radius_md.to_bits(), Ordering::Relaxed);
    S_BORDER_W.store(s.border_w.to_bits(), Ordering::Relaxed);
    S_SHADOW_OFFSET.store(s.shadow_offset.to_bits(), Ordering::Relaxed);
    S_ROUNDNESS.store(s.roundness.to_bits(), Ordering::Relaxed);
    S_PIXEL_CHROME.store(s.pixel_chrome as u32, Ordering::Relaxed);
    if let Ok(mut g) = CURRENT_SHAPE.lock() {
        *g = Some(k);
    }
}

// ── Minimum contrast ─────────────────────────────────────────────────────
// A palette can only rescue the colors it owns. ANSI 0-15 get remapped per
// theme (that's why the light sets ship a grey "white"), but an app that names
// a color outright — truecolor, or the 256-cube — bypasses the palette
// entirely, and Claude Code names near-white for its completion text. On a
// light background that text is simply gone. This is the terminal's job to
// catch, the same guard Ghostty and iTerm2 expose.

f32_slot!(S_MIN_CONTRAST, min_contrast, 2.5);

/// Contrast floor for app-named cell colors, as a WCAG ratio (1 = off). The
/// default only rescues text that is genuinely unreadable: at 2.5, grey-on-white
/// down to about #949494 is left untouched, so deliberately quiet UI keeps
/// reading as quiet.
pub fn set_min_contrast(v: f32) {
    S_MIN_CONTRAST.store(v.clamp(1.0, 21.0).to_bits(), Ordering::Relaxed);
}

/// sRGB byte → linear, cached: the guard runs per cell, and `powf` on six
/// channels of every colored glyph is real work for a table of 256 answers.
fn srgb_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        }
        t
    })
}

/// WCAG relative luminance.
fn luminance(c: [u8; 4]) -> f32 {
    let l = srgb_lut();
    0.2126 * l[c[0] as usize] + 0.7152 * l[c[1] as usize] + 0.0722 * l[c[2] as usize]
}

fn contrast_of(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Push `fg` toward black or white — whichever the background isn't — until it
/// clears the contrast floor. Hue rides along the lerp rather than being
/// recomputed, so a washed-out orange darkens into orange instead of turning
/// into grey text.
pub fn enforce_min_contrast(fg: [u8; 4], bg: [u8; 4]) -> [u8; 4] {
    enforce_contrast_at(fg, bg, min_contrast())
}

/// Selectable floors. Stored as a number so a hand-edited settings.json can name
/// any value; the screen just offers the four worth clicking.
pub const CONTRAST_PRESETS: &[(&str, f32)] =
    &[("Off", 1.0), ("Low", 2.0), ("Default", 2.5), ("High", 3.5)];

/// `enforce_min_contrast` against an explicit floor — lets the settings screen
/// preview each preset without disturbing the live one.
pub fn enforce_contrast_at(fg: [u8; 4], bg: [u8; 4], min: f32) -> [u8; 4] {
    if min <= 1.0 {
        return fg;
    }
    let l_bg = luminance(bg);
    if contrast_of(luminance(fg), l_bg) >= min {
        return fg;
    }
    let target = if l_bg > 0.18 { 0.0f32 } else { 255.0 };
    let mix = |t: f32| {
        let mut c = fg;
        for i in 0..3 {
            c[i] = (fg[i] as f32 + (target - fg[i] as f32) * t).round() as u8;
        }
        c
    };
    // The floor may be unreachable (a mid-grey background caps how far either
    // direction gets), so settle on the closest the search reached rather than
    // looping forever chasing it.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..8 {
        let mid = (lo + hi) * 0.5;
        if contrast_of(luminance(mix(mid)), l_bg) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(hi)
}

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

/// 크롬 판(사이드바 · 타이틀 스트립)의 바닥색. 본문(`bg`)과 한 톤 갈라 사이드바가
/// "터미널의 왼쪽 여백"이 아니라 별도의 판으로 읽히게 한다.
///
/// 고정 색이 아니라 `bg`↔`surface_hover` 중간인 건 방향 때문이다 — 대비로 가는
/// 방향이 테마마다 반대다(어두운 테마는 밝게, 밝은 테마는 어둡게). 팔레트가 이미
/// 그 방향을 알고 있으니 절반만 따라가면 여덟 테마에서 전부 맞는다.
pub fn panel_bg() -> [u8; 4] {
    lerp(bg(), surface_hover(), 0.5)
}

/// 판 위에 **올라온 부액션 버튼**의 채움 — `base` 는 그 버튼이 얹힌 배경색.
///
/// 고정 토큰을 못 쓰는 건 배경이 자리마다 달라서다. 예전엔 여기에 `surface` 를
/// 깔았는데, 그건 팔레트에서 패널 바닥보다도 **어두운** 색이라 버튼이 올라온
/// 판이 아니라 파인 구멍으로 보였다 — 호버해야 비로소 버튼처럼 밝아지는 게 그
/// 증거였다. `bg→surface_active` 가 이 테마에서 "들리는 방향"이니, 그 델타만큼
/// 배경에서 밀어 올리면 어느 배경 위에서도, 여덟 테마 전부에서 한 단계 뜬다.
pub fn raised_on(base: [u8; 4], hover: bool) -> [u8; 4] {
    let (a, b) = (bg(), surface_active());
    let t = if hover { 1.0 } else { 0.5 };
    let step = |i: usize| {
        (base[i] as f32 + (b[i] as f32 - a[i] as f32) * t).clamp(0.0, 255.0).round() as u8
    };
    [step(0), step(1), step(2), 255]
}

/// 캐릭터명 → 고정 accent (pane 번호와 무관하게 학생=색 고정). 정본은 로스터
/// (`characters.json` 의 `header_color`, build.rs 가 굽는다) — 수동 12명 목록으로
/// 남아 있던 동안 신규 67명이 전부 무색(테두리·글로우·sm테마 폴백)이었다
/// (2026-08-12 지시: "애들 색도 다입혀야돼"). 수동 값은 로스터와 동일했다.
/// 미배정(순수 셸)은 None → 호출부가 테두리를 안 그린다.
pub fn character_accent(name: &str) -> Option<[u8; 4]> {
    // 학생 아님 — claude agents(에이전트 목록 뷰)의 SCHALE 조직 정체성 색.
    // render 가 argv(is_claude_agents)+프사 슬롯 부재로 목록 뷰를 판정해
    // 타이틀바 이름·테두리에만 쓴다(pane_character 엔 저장 안 함, 세션 진입
    // 시 배정 학생을 가리지 않게). 로스터엔 없어 랜덤 배정 후보 아님.
    if name == "샬레" {
        return Some(unpack(0x3a6eb4_ff));
    }
    roster().accents.iter().find(|(n, _)| *n == name).map(|(_, rgb)| unpack((rgb << 8) | 0xff))
}

/// 활성·번들·설치 테마를 통틀어 이름을 찾는다 — 색은 **활성 명부 우선**.
///
/// tell 마커(`⟦이름⟧`)는 **발신 pane 의 테마**로 찍히는데 받는 쪽은 자기 명부를
/// 본다. 둘이 다르면 이름이 안 걸려 마커가 걷히지 않고 날것으로 화면에 남았다
/// (2026-08-24 실측: 치이카와 활성 + 블루아카 이름 pane 이 공존하는 화면).
///
/// **아무 이름이나 받아 주면 안 되는 이유**: 이 조회는 가짜 마커 문지기를 겸한다 —
/// 사람이 본문에 친 `⟦메모⟧` 까지 걷어내면 화면 내용을 조용히 바꾸는 셈이다.
/// 마커는 kasaterm 의 tell 경로만 찍고 그 이름은 반드시 어느 명부엔가 있으므로,
/// **아는 명부의 합집합**이 오탐 억제력을 유지하는 경계다.
pub fn character_accent_any(name: &str) -> Option<[u8; 4]> {
    character_accent(name).or_else(|| accent_beyond_active(name, other_rosters()))
}

/// 활성 밖 갈래 — 번들 정적, 그다음 주어진 명부들. **명부 주입 버전인 이유**는
/// 이 갈래가 활성 테마와 설치 상태에 따라서만 갈리기 때문이다. 실제 파일에
/// 기대면 테마를 고른 컴퓨터에서만 도는 검증이 되고, 그건 이 파일이 방금 한 번
/// 겪은 병이다(`build_roster` 주석).
fn accent_beyond_active(name: &str, extra: &[Roster]) -> Option<[u8; 4]> {
    // 번들은 컴파일 내장이라 IO 가 0 이고, 지금 화면의 흔한 방향(테마 활성 +
    // 옛 이름)이 대개 여기서 닫힌다 — 파일에서 온 명부를 훑기 전에 본다.
    CHARACTER_ACCENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .or_else(|| {
            extra.iter().find_map(|r| r.accents.iter().find(|(n, _)| *n == name).map(|(_, c)| *c))
        })
        .map(|rgb| unpack((rgb << 8) | 0xff))
}

/// 합집합 슬러그 — `character_accent_any` 와 같은 범위를 슬러그로. 활성 우선.
///
/// ⚠️ **슬러그가 나왔다고 그림이 있는 건 아니다.** 비활성 테마의 그림은 활성 자산
/// 경로(`students_dir()`)에 없다 — 프사 자리를 세우기 전에 그림 실재를 따로
/// 확인해야 한다(`screenread::tell_face_slug`).
pub fn character_slug_any(name: &str) -> Option<&'static str> {
    character_slug(name).or_else(|| slug_beyond_active(name, other_rosters()))
}

/// `accent_beyond_active` 의 슬러그 짝 — 같은 이유로 명부를 주입받는다.
fn slug_beyond_active(name: &str, extra: &[Roster]) -> Option<&'static str> {
    CHARACTER_SLUGS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).or_else(|| {
        extra.iter().find_map(|r| r.slugs.iter().find(|(n, _)| *n == name).map(|(_, s)| *s))
    })
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
    // **활성 밖까지 본다**(`_any`). pane 을 다른 테마 학생으로 바꾸는 건 지원되는
    // 조작인데(2026-08-24 지시), 활성 로스터만 보면 그렇게 바뀐 pane 은 색을 못 찾아
    // 폴백으로 떨어진다 — 이름·얼굴은 새 학생인데 테두리·프사·스피너만 앞 학생 색으로
    // 남는다(2026-08-25: star-rail 로 바꾼 %12 가 그랬다. 「미니맵 인포는 바뀌었는데
    // 스피너나 pane 테마가 안 바뀐다」). 활성 안 학생은 `_any` 가 활성을 먼저 보므로
    // 그대로다.
    accent_n_with(name, ordinal, other_rosters())
}

/// 위의 순수부 — 명부를 주입받는 이유는 `accent_beyond_active` 와 같다(설치 테마에
/// 기대면 테마를 고른 컴퓨터에서만 도는 검증이 된다).
fn accent_n_with(name: &str, ordinal: usize, extra: &[Roster]) -> Option<[u8; 4]> {
    character_accent(name)
        .or_else(|| accent_beyond_active(name, extra))
        .map(|c| accent_variant(c, ordinal))
}

// 캐릭터명 ↔ 에셋 슬러그 대응표(`assets/students/profile/<slug>.png`, arona-ui 디렉토리명·shim
// 팀원 로마자 이름과 동일). 정/역방향이 같은 표를 읽는다.
//
// 표 자체는 `collab-hooks/characters.json` 에서 build.rs 가 생성한다 — 정본이 둘이면
// 어긋나고, 어긋나도 오류가 안 난다(슬러그가 inbox 파일명이라 브리프가 아무도 안 읽는
// 우편함에 들어간다). 새 테마는 그 JSON 하나만 갈아 끼우면 된다. 슬러그 중복·형식
// 위반은 build.rs 가 컴파일 에러로 막는다.
include!(concat!(env!("OUT_DIR"), "/character_slugs.rs"));

/// 지금 화면이 쓰는 이름↔슬러그·이름→색 표.
#[derive(Clone, Copy)]
struct Roster {
    slugs: &'static [(&'static str, &'static str)],
    accents: &'static [(&'static str, u32)],
}

static ROSTER: std::sync::RwLock<Option<Roster>> = std::sync::RwLock::new(None);

/// 활성 로스터. 테마(또는 사용자 override)의 `theme.json`/`characters.json` 이 앞서고,
/// 읽을 게 없으면 위 코드젠 상수(번들 기본값)로 떨어진다 — 스프라이트가 이미 쓰는
/// 순서(override 먼저, 번들 폴백)와 같다.
///
/// 코드젠을 남겨 둔 이유: 번들 로스터의 정본이자 슬러그 중복·형식 위반을 컴파일 때
/// 막는 관문이다. 런타임 테마엔 그 관문이 없으니, 깨진 테마는 여기서 조용히 번들로
/// 되돌아가는 것이 로스터가 텅 빈 채 도는 것보다 낫다.
fn roster() -> Roster {
    if let Some(r) = *ROSTER.read().unwrap() {
        return r;
    }
    let mut w = ROSTER.write().unwrap();
    // 잠금을 바꿔 잡는 사이 다른 스레드가 이미 구웠을 수 있다.
    if let Some(r) = *w {
        return r;
    }
    let r = build_roster();
    *w = Some(r);
    r
}

/// 테마를 갈아 끼운 뒤 부른다 — 다음 조회가 새 로스터를 굽는다.
pub fn invalidate_roster() {
    *ROSTER.write().unwrap() = None;
    // 합집합 캐시도 **함께** 비운다. 한쪽만 비우면 새 테마의 이름이 「다른 테마」
    // 목록에 옛 채로 남아, 활성과 합집합이 같은 이름을 두 색으로 답한다.
    *OTHER_ROSTERS.write().unwrap() = None;
    // 그림 쪽 합집합도 같은 이유로 짝이다. 이 목록은 **활성을 뺀 나머지**라, 테마를
    // 갈아 끼우면 빠지는 폴더와 들어오는 폴더가 동시에 바뀐다 — 안 비우면 새 활성
    // 폴더가 목록에 남아 같은 그림을 두 번 뒤지고, 옛 활성은 영영 안 들어온다.
    crate::sprites::invalidate_theme_sprite_dirs();
}

/// 설치된 나머지 테마들의 명부 — 활성에도 번들에도 없는 이름의 마지막 보루.
///
/// 파일 IO 라 캐시가 필수다(조회가 렌더 경로에서 온다). 위 `invalidate_roster` 가
/// 활성 로스터와 짝으로 비운다.
static OTHER_ROSTERS: std::sync::RwLock<Option<&'static [Roster]>> =
    std::sync::RwLock::new(None);

fn other_rosters() -> &'static [Roster] {
    if let Some(r) = *OTHER_ROSTERS.read().unwrap() {
        return r;
    }
    let mut w = OTHER_ROSTERS.write().unwrap();
    // 잠금을 바꿔 잡는 사이 다른 스레드가 이미 구웠을 수 있다.
    if let Some(r) = *w {
        return r;
    }
    let r = build_other_rosters();
    *w = Some(r);
    r
}

/// 테마 폴더를 훑어 **파싱되는 것만** 모은다 — best-effort. 폴더가 없거나 깨진
/// `theme.json` 하나 때문에 나머지 조회를 포기할 이유가 없다.
fn build_other_rosters() -> &'static [Roster] {
    // 테스트는 이 컴퓨터의 테마 폴더와 무관해야 한다 — `build_roster` 와 같은 이유.
    if cfg!(test) {
        return &[];
    }
    let Some(root) = kasa_mcp::character::themes_root() else { return &[] };
    let Ok(rd) = std::fs::read_dir(root) else { return &[] };
    let mut out: Vec<Roster> = Vec::new();
    for e in rd.flatten() {
        let Ok(raw) = std::fs::read_to_string(e.path().join("theme.json")) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { continue };
        // 파싱은 활성 로스터와 **같은 갈래**를 탄다 — 파서가 둘이 되면 같은
        // theme.json 을 두 벌로 해석하고, 어긋나도 오류가 안 난다.
        if let Some(r) = roster_from(&v) {
            out.push(r);
        }
    }
    Box::leak(out.into_boxed_slice())
}

/// 활성 로스터의 이름↔슬러그 전부 — 전 캐릭터를 훑어야 하는 곳(테마 복제)용.
pub fn character_slugs() -> &'static [(&'static str, &'static str)] {
    roster().slugs
}

fn build_roster() -> Roster {
    // 테스트에서는 사용자 파일을 아예 안 읽는다. 활성 로스터는 이 컴퓨터의
    // `~/.config/kasaterm`(고른 테마 → characters.json override)에서 오는데, 테스트는
    // 번들 이름("아로나"·"히마리")으로 조회한다 — 사용자가 테마를 고르는 순간 그 조회가
    // 전부 None 이 되어 **그 컴퓨터에서만** 테스트가 깨진다(2026-08-24 실측: 치이카와
    // 테마를 켜자 theme·screenread·sprites 12개 실패). 코드 회귀가 아닌 것이 회귀처럼
    // 보이는 쪽이 훨씬 비싸다.
    let chars = (!cfg!(test)).then(kasa_mcp::character::characters_json).flatten();
    roster_of(chars.as_ref())
}

/// 소스 주입 버전 — 「테마가 있으면 그것, 없거나 깨졌으면 번들」 배선만 떼어 놓은 것.
/// `build_roster` 가 파일을 읽는 한 이 갈래는 사용자 환경 없이 검증할 수 없었다.
fn roster_of(chars: Option<&serde_json::Value>) -> Roster {
    let bundled = Roster { slugs: CHARACTER_SLUGS, accents: CHARACTER_ACCENTS };
    chars.and_then(roster_from).unwrap_or(bundled)
}

/// JSON 주입 버전(테스트용) — 활성 로스터 해석과 분리해 파일 없이 검증한다.
/// 쓸 만한 항목이 하나도 없으면 None → 호출부가 번들로 떨어진다.
///
/// **`&'static` 으로 새는 것은 의도다.** 조회 함수들이 `Option<&'static str>` 을 주고
/// 호출부가 그걸 그대로 들고 다닌다 — String 으로 바꾸면 렌더 경로가 매 프레임
/// 할당하게 된다. 로스터는 80명 남짓이고 테마 전환은 사용자가 손으로 하는 일이라,
/// 전환 한 번에 수 KB 가 남는 쪽을 택했다.
fn roster_from(chars: &serde_json::Value) -> Option<Roster> {
    let mut slugs: Vec<(&'static str, &'static str)> = Vec::new();
    let mut accents: Vec<(&'static str, u32)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in roster_entries(chars) {
        let (Some(name), Some(slug)) = (
            m.get("name").and_then(|v| v.as_str()),
            m.get("slug").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        // `leader` 는 `leaders[0]` 을 한 번 더 적어 둔 하위호환 필드다(build.rs 와
        // 같은 규칙으로 접는다). 슬러그가 빈 캐릭터는 그림도 인박스도 없어 건너뛴다.
        if name.is_empty() || slug.is_empty() || !seen.insert(name.to_string()) {
            continue;
        }
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        slugs.push((name, Box::leak(slug.to_string().into_boxed_str())));
        if let Some(c) = m.get("header_color").and_then(|v| v.as_str()).and_then(parse_hex_rgb) {
            accents.push((name, c));
        }
    }
    if slugs.is_empty() {
        return None;
    }
    Some(Roster {
        slugs: Box::leak(slugs.into_boxed_slice()),
        accents: Box::leak(accents.into_boxed_slice()),
    })
}

/// leader/leaders/members 를 한 줄로 편다.
fn roster_entries(chars: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut v = Vec::new();
    if let Some(l) = chars.get("leader") {
        v.push(l);
    }
    for k in ["leaders", "members"] {
        if let Some(a) = chars.get(k).and_then(|x| x.as_array()) {
            v.extend(a.iter());
        }
    }
    v
}

/// `"#6BCF7F"` → `0x6bcf7f`. 어긋나면 None — 그 캐릭터만 무색으로 두고 넘어간다.
/// 로스터 전체를 번들로 되돌릴 일은 아니다(색 하나 빠진 것과 테마가 통째로 안 뜨는
/// 것은 사용자에게 전혀 다른 사고다).
fn parse_hex_rgb(s: &str) -> Option<u32> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

/// 캐릭터명 → 에셋 슬러그.
pub fn character_slug(name: &str) -> Option<&'static str> {
    roster().slugs.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// 캐릭터명 → **teammate agent 이름에 쓰는** 슬러그. 로스터에 없는 커스텀 캐릭터는
/// 해시 축약으로 떨어진다(inbox 파일명이 이 슬러그라 한글은 "---" 로 붕괴한다).
///
/// 셰임이 굽는 case 분기(`teammate_case_arms`)와 split 이 미리 알려 주는 이름
/// (`PtyBackend::pane_agent`)이 **이 하나를** 쓴다. 두 벌이 되면 부른 쪽이 닿지 않는
/// 인박스에 브리프를 넣고도 성공으로 읽는다 — 어긋나도 오류가 안 나는 종류의 버그다.
pub fn agent_slug(name: &str) -> String {
    character_slug(name)
        .map(String::from)
        .unwrap_or_else(|| kasa_mcp::team::ascii_ident(name))
}

/// 슬러그 → 캐릭터명 — 팀원 agent 이름("aru-9c88")의 로마자 앞부분에서 보낸
/// 학생을 역추적할 때 쓴다(접힌 팀메시지 줄 학생색).
pub fn slug_character(slug: &str) -> Option<&'static str> {
    roster().slugs.iter().find(|(_, s)| *s == slug).map(|(n, _)| *n)
}

/// `slug_character` 의 합집합 판 — agent 이름(`midori-p4-v32`)의 로마자 머리로
/// 학생을 되짚는 자리들이 쓴다.
///
/// 이 역매핑이 활성 명부만 보면, 테마를 바꾼 순간 옛 이름 pane 이 보낸 메시지의
/// 발신자가 통째로 「모르는 사람」이 된다 — 학생색·프사·이름 표시가 한꺼번에 빠지는
/// 실사고가 이미 한 번 있었다(2026-08-20, 발신 pane dismiss 로 명부가 사라진 경우).
pub fn slug_character_any(slug: &str) -> Option<&'static str> {
    slug_character(slug).or_else(|| name_beyond_active(slug, other_rosters()))
}

/// `slug_beyond_active` 의 역방향 — 같은 이유로 명부를 주입받는다.
fn name_beyond_active(slug: &str, extra: &[Roster]) -> Option<&'static str> {
    CHARACTER_SLUGS.iter().find(|(_, s)| *s == slug).map(|(n, _)| *n).or_else(|| {
        extra.iter().find_map(|r| r.slugs.iter().find(|(_, s)| *s == slug).map(|(n, _)| *n))
    })
}

/// claude 시작 배너의 "Welcome back <user>!" 를 대체할 배정 학생 인사말 —
/// 각 캐릭터 페르소나 말투로. `user` 는 원 배너에서 추출한 사용자 이름(하드코딩
/// 금지, characters.json 의 user_title="선생님" 을 뒤에 붙인다).
///
/// ⚠️ 개별 페르소나가 없는 학생도 **범용 존대 한 줄로 반드시 커버한다** —
/// 12명만 알고 나머지를 None 으로 돌려보내던 동안, 그 학생 pane 은 인사말이
/// 안 바뀌는 것에서 끝나지 않고 **배너 테두리 학생색까지 통째로 빠졌다**
/// (호출부가 None 에서 조기 반환, 2026-08-20 거노 스샷·히나 pane 실측).
/// 아바타 12명 목록과 같은 병이다: 로스터 부분집합 하드코딩은 조용히 샌다.
pub fn character_welcome(name: &str, user: &str) -> Option<String> {
    // 인사말은 원 배너("Welcome back <user>!") 폭에 맞춘 한 문장 — 2컬럼 배너의
    // 왼쪽 컬럼을 넘기면 호출부가 "…"로 자른다(거노 실사고: 긴 인사말 잘림).
    let g = match name {
        "아로나" => format!("어서 오세요 {user} 선생님!"),
        "프라나" => format!("{user} 선생님, 오셨군요."),
        "미도리" => format!("{user} 선생님, 오셨어요."),
        "모모이" => format!("{user} 선생님, 어서 오세요!"),
        "유즈" => format!("{user} 선생님… 오셨네요."),
        "아리스" => format!("{user} 선생님, 돌아왔구나!"),
        "유우카" => format!("{user} 선생님, 오셨네요."),
        "시로코" => format!("{user} 선생님, 오셨어요."),
        "호시노" => format!("{user} 선생님~ 왔구나~"),
        "코하루" => format!("어, 어서오세요 {user} 선생님…!"),
        "히마리" => format!("{user} 선생님, 어서 오세요."),
        "아루" => format!("훗, 왔군 {user} 선생님!"),
        _ => format!("{user} 선생님, 어서 오세요."),
    };
    Some(g)
}

/// `[r,g,b,a]` → CSS hex. 알파가 불투명하면 6자리로 — 대부분이 그렇고, 8자리를
/// 강요하면 사람이 읽는 값이 전부 `ff` 꼬리를 달게 된다. CSS 는 둘 다 받는다.
fn css_hex(c: [u8; 4]) -> String {
    if c[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
    }
}

/// 지금 화면에 쓰이는 디자인 토큰 전부 — 웹뷰 UI 가 `--kt-*` CSS 변수로 심어
/// 네이티브와 같은 색·같은 실루엣으로 그린다(`GET /design-tokens`).
///
/// **`Palette`/`Shape` 구조체를 직렬화하지 않는다.** 그 둘은 *프리셋*이고, 살아
/// 있는 값은 `color_slot!`·`f32_slot!` 의 atomic 슬롯 안에 있다. 사용자가 팔레트에서
/// 색을 하나 고치면(custom 테마) 슬롯만 바뀌고 프리셋 상수는 그대로다 — 구조체를
/// 직렬화하면 그 편집이 통째로 무시된 값이 나가는데, **타입이 맞아 오류가 안 난다.**
/// 그래서 여기서는 reader 함수만 부른다. 이 규칙이 깨지면 웹 화면의 색이 네이티브와
/// 조용히 갈린다.
/// 캐릭터별 고정 accent 를 `{이름: hex}` 로. 색 정본은 활성 로스터 하나이고,
/// 이 함수는 그것을 웹이 읽을 모양으로만 옮긴다.
fn character_accents_json() -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (name, _) in character_slugs() {
        if let Some(c) = character_accent(name) {
            m.insert((*name).to_string(), serde_json::Value::String(css_hex(c)));
        }
    }
    // 샬레는 학생이 아니라 조직 색이라 로스터에 이름이 없다 — 에이전트 목록
    // 뷰가 이 색을 쓰므로 따로 넣는다.
    if let Some(c) = character_accent("샬레") {
        m.insert("샬레".to_string(), serde_json::Value::String(css_hex(c)));
    }
    serde_json::Value::Object(m)
}

pub fn tokens_json() -> serde_json::Value {
    serde_json::json!({
        // 프리셋 키·accent 이름은 색만으로 되짚을 수 없어 따로 추적된다
        // (`CURRENT_THEME`) — 웹 UI 의 선택 표시가 이걸 쓴다.
        "theme": theme_name(),
        "accent_name": accent_name(),
        "palette": {
            "bg": css_hex(bg()),
            "fg": css_hex(fg()),
            "surface": css_hex(surface()),
            "surface_hover": css_hex(surface_hover()),
            "surface_active": css_hex(surface_active()),
            "border": css_hex(border()),
            "accent": css_hex(accent()),
            "text": css_hex(text()),
            "text_dim": css_hex(text_dim()),
            "text_mute": css_hex(text_mute()),
            "success": css_hex(success()),
            "danger": css_hex(danger()),
            // 테마 슬롯이 아니라 고정값이다 — 「내 손을 기다린다」는 취향이 아니라
            // 신호이고, 테마마다 달라지면 같은 뜻이 창마다 다른 색으로 읽힌다.
            "attention": css_hex(attention()),
            "syn_keyword": css_hex(syn_keyword()),
            "syn_string": css_hex(syn_string()),
            "syn_number": css_hex(syn_number()),
            "syn_comment": css_hex(syn_comment()),
            "syn_function": css_hex(syn_function()),
            "syn_type": css_hex(syn_type()),
        },
        "ansi": (0..16)
            .map(|i| {
                let c = ansi16(i);
                css_hex([c[0], c[1], c[2], 255])
            })
            .collect::<Vec<_>>(),
        // 캐릭터별 고정 accent(이름 → hex). 팔레트와 달리 **테마가 정하는 값**이라
        // 여기 실어야 한다 — 전에는 웹이 이 표를 `lib/mcp.ts` 에 손으로 베껴 뒀고,
        // 그게 번들 로스터에 얼어붙어 있어서 사용자가 만든 테마의 색이 아로나
        // 창에 영영 닿지 않았다(커스텀 캐릭터는 아예 표에 없어 색이 pane 번호
        // 순환색으로 떨어졌다). 정본은 활성 로스터 하나다.
        "character_accents": character_accents_json(),
        // 형태는 색과 독립된 축이다(같은 팔레트를 어떤 실루엣으로도 입을 수 있게).
        // px 숫자로 내보내 웹이 `calc()` 없이 그대로 쓴다.
        "shape": {
            "radius_sm": radius_sm(),
            "radius_md": radius_md(),
            "border_w": border_w(),
            "shadow_offset": shadow_offset(),
            "roundness": roundness(),
            "pixel_chrome": pixel_chrome(),
        },
    })
}

#[cfg(test)]
mod roster_tests {
    use super::*;

    /// 런타임 로스터는 같은 JSON 에서 **코드젠과 똑같은 표**를 구워야 한다.
    ///
    /// 두 벌이 어긋나도 컴파일도 실행도 통과한다 — 슬러그는 teammate inbox 파일명이라,
    /// 한쪽에만 있는 학생에게 보낸 브리프는 아무도 안 읽는 우편함에 들어가고 보낸 쪽은
    /// 성공으로 읽는다. 코드젠을 폴백으로 남긴 이상 이 대조가 그 위험을 대신 막는다.
    #[test]
    fn runtime_roster_matches_codegen() {
        let raw = include_str!("../collab-hooks/characters.json");
        let chars: serde_json::Value = serde_json::from_str(raw).expect("번들 로스터 파싱");
        let r = roster_from(&chars).expect("번들 로스터를 굽지 못했다");
        assert_eq!(r.slugs, CHARACTER_SLUGS, "이름↔슬러그 표가 코드젠과 어긋난다");
        assert_eq!(r.accents, CHARACTER_ACCENTS, "이름→색 표가 코드젠과 어긋난다");
    }

    /// 쓸 항목이 없는 JSON 은 번들로 떨어진다 — 깨진 테마가 로스터를 비우면 캐릭터
    /// 배정이 통째로 멈추는데, 그건 오류가 아니라 「학생이 안 뜬다」로만 보인다.
    #[test]
    fn empty_or_broken_roster_falls_back() {
        assert!(roster_from(&serde_json::json!({})).is_none());
        assert!(roster_from(&serde_json::json!({ "members": [] })).is_none());
        // 이름만 있고 슬러그가 없는 항목은 그림도 인박스도 없어 쓸 수 없다.
        assert!(roster_from(&serde_json::json!({ "members": [{ "name": "이름만" }] })).is_none());
    }

    /// 쓸 게 없으면 **실제로 번들이 서는지**. 위 테스트는 `roster_from` 이 None 을
    /// 준다는 데까지고, 그 None 이 번들로 이어지는 배선은 파일을 읽는 `build_roster`
    /// 안에 묻혀 있어 사용자 환경 없이는 검증할 수 없었다.
    #[test]
    fn roster_of_falls_back_to_bundled() {
        assert_eq!(roster_of(None).slugs, CHARACTER_SLUGS);
        assert_eq!(roster_of(Some(&serde_json::json!({ "members": [] }))).slugs, CHARACTER_SLUGS);
        let themed = serde_json::json!({ "members": [{ "name": "가", "slug": "ga" }] });
        assert_eq!(roster_of(Some(&themed)).slugs, &[("가", "ga")]);
    }

    /// **테스트는 이 컴퓨터의 `~/.config/kasaterm` 과 무관해야 한다.** 이게 깨졌다면
    /// 활성 로스터가 다시 사용자 설정을 타는 것이고, 그러면 테마를 고른 컴퓨터에서만
    /// 번들 이름으로 조회하는 테스트 십수 개가 한꺼번에 무너진다 — 코드 회귀가 아닌데
    /// 회귀로 보이므로 원인을 찾는 데 그만큼이 든다.
    #[test]
    fn active_roster_is_bundled_under_test() {
        assert_eq!(roster().slugs, CHARACTER_SLUGS, "테스트가 사용자 로스터를 읽고 있다");
        assert_eq!(roster().accents, CHARACTER_ACCENTS);
    }

    /// 색이 어긋난 캐릭터 하나가 로스터 전체를 번들로 되돌리면 안 된다 — 색 하나가
    /// 빠지는 것과 테마가 통째로 안 뜨는 것은 사용자에게 전혀 다른 사고다.
    #[test]
    fn bad_color_drops_only_that_accent() {
        let v = serde_json::json!({
            "members": [
                { "name": "가", "slug": "ga", "header_color": "#FF0000" },
                { "name": "나", "slug": "na", "header_color": "빨강" },
                { "name": "다", "slug": "da" },
            ]
        });
        let r = roster_from(&v).expect("색이 어긋나도 로스터는 서야 한다");
        assert_eq!(r.slugs.len(), 3);
        assert_eq!(r.accents, &[("가", 0xff0000)]);
    }

    /// `leader` 는 `leaders[0]` 을 한 번 더 적어 둔 하위호환 필드라 접혀야 한다.
    #[test]
    fn duplicate_leader_entry_folds() {
        let one = serde_json::json!({ "name": "아로나", "slug": "arona" });
        let v = serde_json::json!({ "leader": one, "leaders": [one], "members": [] });
        let r = roster_from(&v).unwrap();
        assert_eq!(r.slugs, &[("아로나", "arona")]);
    }

    /// 합집합 조회 — 활성 밖 갈래가 **양방향**을 다 닫는가.
    ///
    /// tell 마커는 발신 pane 의 테마로 찍히므로 두 방향이 다 생긴다: 테마를 켠
    /// 화면의 옛 이름(번들), 번들 화면의 새 테마 이름(설치 테마). 한쪽만 닫으면
    /// 반대 방향에서 마커가 그대로 노출된다.
    #[test]
    fn accent_beyond_active_covers_both_directions() {
        // 활성 밖 + 번들 안. 색까지 번들 표와 같아야 한다 — 되는대로 아무 색이나
        // 칠하면 「색이 곧 학생」이 깨진다.
        let midori = CHARACTER_ACCENTS.iter().find(|(n, _)| *n == "미도리").map(|(_, c)| *c);
        assert_eq!(
            accent_beyond_active("미도리", &[]),
            midori.map(|rgb| unpack((rgb << 8) | 0xff)),
            "테마를 켠 화면의 옛 이름이 안 닫힌다"
        );

        // 활성 밖 + 설치 테마 안.
        let themed = serde_json::json!({
            "members": [{ "name": "하치와레", "slug": "hachiware", "header_color": "#4A90D9" }]
        });
        let extra = [roster_from(&themed).expect("테마 명부")];
        assert_eq!(accent_beyond_active("하치와레", &extra), Some([0x4a, 0x90, 0xd9, 255]));
        assert_eq!(slug_beyond_active("하치와레", &extra), Some("hachiware"));
        // 역방향도 같은 범위여야 한다 — agent 이름(`hachiware-p2-1uc`)의 로마자
        // 머리로 학생을 되짚는 자리가 이걸 쓴다.
        assert_eq!(name_beyond_active("hachiware", &extra), Some("하치와레"));
        assert_eq!(name_beyond_active("midori", &[]), Some("미도리"));

        // 어느 명부에도 없으면 None — **이 None 이 가짜 마커 문지기다.** 사람이
        // 본문에 친 `⟦메모⟧` 까지 걷어내면 화면 내용을 조용히 바꾸는 셈이 된다.
        assert!(accent_beyond_active("없는캐릭", &extra).is_none());
        assert!(character_accent_any("없는캐릭").is_none());
        assert!(character_slug_any("없는캐릭").is_none());
        assert!(name_beyond_active("없는슬러그", &extra).is_none());
        assert!(slug_character_any("없는슬러그").is_none());
    }

    /// pane 색(테두리·프사·스피너)이 **합집합을 보는가**.
    ///
    /// 2026-08-25: star-rail 로 바꾼 `%12` 가 이름·얼굴만 새 학생이고 색은 앞
    /// 학생 것으로 남았다. 원인은 이 자리가 활성 로스터만 보는 조회를 쓰고 있던
    /// 것 — 다른 테마 학생으로 바꾸는 건 지원되는 조작이라(2026-08-24 지시)
    /// 활성 밖 이름이 정상적으로 들어온다.
    #[test]
    fn pane_accent_covers_installed_themes() {
        let themed = serde_json::json!({
            "members": [{ "name": "하치와레", "slug": "hachiware", "header_color": "#4A90D9" }]
        });
        let extra = [roster_from(&themed).expect("테마 명부")];
        // 활성 밖이지만 설치 테마 안 — 색이 나와야 한다. 되돌리면 여기서 None 이다.
        assert_eq!(
            accent_n_with("하치와레", 0, &extra),
            Some([0x4a, 0x90, 0xd9, 255]),
            "다른 테마로 바꾼 pane 이 색을 못 찾는다"
        );
        // 같은 학생이 둘 이상 떠 있을 때의 변주도 활성 안과 같은 사다리를 탄다.
        assert_eq!(
            accent_n_with("하치와레", 1, &extra),
            Some(accent_variant([0x4a, 0x90, 0xd9, 255], 1))
        );
        // 활성 안 이름은 그대로 — 넓힌 조회가 기존 색을 바꾸면 안 된다.
        assert_eq!(accent_n_with("미도리", 0, &extra), character_accent("미도리"));
        // 어느 명부에도 없으면 None. 이 None 이 「색이 곧 학생」의 문지기다.
        assert!(accent_n_with("없는캐릭", 0, &extra).is_none());
    }

    /// 합집합이어도 **활성이 먼저다** — 같은 이름을 두 명부가 다른 색으로 가지면
    /// 화면은 지금 고른 테마의 색을 보여야 한다. 그리고 로스터 밖 특례(샬레)가
    /// 합집합 경로에서도 살아 있어야 한다(에이전트 목록 뷰의 조직색).
    #[test]
    fn accent_any_prefers_active_and_keeps_schale() {
        let active = character_accent("미도리").expect("번들 활성");
        assert_eq!(character_accent_any("미도리"), Some(active));
        assert_eq!(character_accent_any("샬레"), character_accent("샬레"));
        assert!(character_accent_any("샬레").is_some());
    }

    /// 캐시를 비우고 다시 부르면 새로 굽는다.
    ///
    /// 데드락 감시도 겸한다 — `build_roster` 안에서 누가 `roster()` 를 부르면 쓰기
    /// 잠금을 잡은 채 읽기를 기다려 그대로 멈춘다. 그 회귀는 이 테스트가 매달린다.
    #[test]
    fn cache_invalidates_without_deadlock() {
        let before = roster().slugs.len();
        invalidate_roster();
        assert_eq!(roster().slugs.len(), before);
    }
}

#[cfg(test)]
mod custom_theme_tests {
    use super::*;
    use serde_json::json;

    /// 하나뿐이던 시절의 `custom_theme` 오브젝트는 첫 항목으로 읽혀야 한다 —
    /// 안 그러면 업데이트한 사람의 팔레트가 목록에서 통째로 사라진다.
    #[test]
    fn legacy_object_reads_as_first_entry() {
        let s = json!({ "custom_theme": { "base": "light", "bg": "#112233" } });
        let list = custom_themes(&s);
        assert_eq!(list.len(), 1);
        assert_eq!(custom_slug(&list[0]), LEGACY_CUSTOM_SLUG);
        assert_eq!(custom_palette(&list[0]).bg, [0x11, 0x22, 0x33, 255]);
        // 옛 `theme: "custom"` 값도 그 항목을 가리킨다.
        assert_eq!(custom_key("custom"), Some(""));
        assert!(find_custom(&list, "").is_some());
    }

    /// 새 형식이 있으면 옛 오브젝트는 안 읽는다 — 둘 다 읽으면 마이그레이션 뒤
    /// 첫 항목이 두 벌로 보인다.
    #[test]
    fn array_wins_over_legacy_object() {
        let s = json!({
            "custom_theme": { "base": "dark", "bg": "#000000" },
            "custom_themes": [{ "slug": "a", "label": "A", "base": "dark" }],
        });
        let list = custom_themes(&s);
        assert_eq!(list.len(), 1);
        assert_eq!(custom_slug(&list[0]), "a");
    }

    /// `custom:<slug>` 는 그 항목을, 없는 slug 는 첫 항목으로 떨어진다(파일을
    /// 손으로 고쳐 사라진 slug 를 가리키게 됐을 때 아무것도 안 입는 것보다 낫다).
    #[test]
    fn slug_selects_entry_and_falls_back() {
        let s = json!({ "custom_themes": [
            { "slug": "one", "base": "dark", "bg": "#010101" },
            { "slug": "two", "base": "dark", "bg": "#020202" },
        ]});
        let list = custom_themes(&s);
        assert_eq!(custom_key("custom:two"), Some("two"));
        assert_eq!(custom_palette(find_custom(&list, "two").unwrap()).bg, [2, 2, 2, 255]);
        assert_eq!(custom_palette(find_custom(&list, "없다").unwrap()).bg, [1, 1, 1, 255]);
        assert_eq!(custom_key("dark"), None);
    }

    /// 새 이름은 기존 slug·라벨 어느 쪽과도 안 겹친다 — 겹치면 두 카드가 같은
    /// 이름으로 뜨고, slug 가 겹치면 편집이 남의 팔레트로 들어간다.
    #[test]
    fn new_names_avoid_collisions() {
        let list = vec![
            json!({ "slug": "palette-1", "label": "Custom 1" }),
            json!({ "slug": "palette-3", "label": "Custom 2" }),
        ];
        // 3번째 후보는 slug 만 겹치는데도 통째로 건너뛴다 — 한쪽만 피하면
        // `palette-3` 두 벌이 생겨 편집이 남의 팔레트로 들어간다.
        let (slug, label) = next_custom_name(&list);
        assert_eq!((slug.as_str(), label.as_str()), ("palette-4", "Custom 4"));
        assert_eq!(next_custom_name(&[]).0, "palette-1");
    }

    /// system 밝기 슬롯은 실재하는 카드 키로 굳혀 나가야 한다 — 설정 화면이 그
    /// 문자열을 카드 키와 견주므로, 옛 `"custom"` 이나 지워진 slug 를 그대로
    /// 내보내면 배정해 둔 사실이 화면에서 사라진다.
    #[test]
    fn system_slot_normalizes_custom_keys() {
        // 옛 값 "custom" → 첫 항목의 실제 키.
        let s = json!({
            "theme_system_dark": "custom",
            "custom_themes": [{ "slug": "one", "base": "dark" }],
        });
        assert_eq!(system_slot_theme_in(&s, false), "custom:one");
        // 가리키던 팔레트가 사라졌으면 내장으로 — apply_system_palette 의 폴백과
        // 같은 판정이라 화면과 실제 색이 어긋나지 않는다.
        let gone = json!({ "theme_system_dark": "custom:없다", "custom_themes": [] });
        assert_eq!(system_slot_theme_in(&gone, false), "dark");
        // 프리셋과 미설정은 그대로.
        let preset = json!({ "theme_system_light": "schale-light" });
        assert_eq!(system_slot_theme_in(&preset, true), "schale-light");
        assert_eq!(system_slot_theme_in(&json!({}), true), "light");
    }

    /// 시드는 base 를 그대로 베끼고, 부분 항목은 빠진 키만 base 에서 채운다.
    #[test]
    fn seed_copies_base_and_partial_entry_inherits() {
        let seed = custom_theme_seed("catppuccin-mocha", "s", "L");
        assert_eq!(custom_slug(&seed), "s");
        assert_eq!(custom_label(&seed), "L");
        assert_eq!(custom_palette(&seed).bg, CATPPUCCIN_MOCHA.bg);
        assert_eq!(custom_palette(&seed).ansi, CATPPUCCIN_MOCHA.ansi);
        // bg 만 적힌 항목: 나머지는 base 값.
        let partial = json!({ "base": "gruvbox-dark", "bg": "#ff0000" });
        let p = custom_palette(&partial);
        assert_eq!(p.bg, [0xff, 0, 0, 255]);
        assert_eq!(p.text, GRUVBOX_DARK.text);
    }
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

    /// 로스터 전원이 accent 를 가져야 한다 — 12명 수동 목록 시절 신규 67명이
    /// 무색(테두리·글로우·sm테마 폴백)이었다(2026-08-12 지시: "애들 색도 다입혀야돼").
    /// build.rs 가 header_color 누락을 빌드에서 거부하지만, 표와 이 함수의 연결이
    /// 끊기는 회귀는 여기서 잡는다.
    #[test]
    fn every_roster_member_has_an_accent() {
        for (name, slug) in CHARACTER_SLUGS {
            let a = character_accent(name)
                .unwrap_or_else(|| panic!("{name}({slug}) 의 accent 가 없다"));
            assert_eq!(a[3], 255, "{name}: 알파는 불투명");
        }
        // 수동 시절 교정값이 로스터와 같았음을 대표 표본으로 재확인 — 코드젠 이관이
        // 색을 바꾸지 않았다는 보증.
        assert_eq!(character_accent("미도리"), Some([0x6b, 0xcf, 0x7f, 255]));
        assert_eq!(character_accent("아리스"), Some([0x4c, 0x6e, 0xf5, 255]));
        // 로스터 밖 특례.
        assert!(character_accent("샬레").is_some());
        assert!(character_accent("없는학생").is_none());
    }
}
