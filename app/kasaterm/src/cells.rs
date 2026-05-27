//! Convert tmux-bridge cell grids into sugarloaf draw calls.
//!
//! MVP path uses `Sugarloaf::rect` (per-cell bg) plus `Text::draw`
//! (per-cell glyph). That's one draw call per cell which is wasteful
//! compared with Rio's `Grid::write_row` batched pipeline — but it
//! proves the conversion logic before we invest in the bigger Grid
//! integration.
//!
//! Color conversion lives here too: tmux-bridge `Color::Idx(u8)`
//! indexes into a 256-entry ANSI palette derived from the iTerm2
//! Default Dark theme (matches what kasaterm has been using, so the
//! A/B comparison stays apples-to-apples).

use tmux_bridge::screen::{Cell, Color};

/// macOS Terminal.app "GitHub Dark Dimmed" — the user's active
/// Default Window Settings. Decoded from
/// `~/Library/Preferences/com.apple.Terminal.plist` (the colors are
/// `NSKeyedArchiver` bytes there; the values below are the sRGB
/// 8-bit triples after unarchiving). The 6×6×6 cube + 24-step
/// grayscale ramp follow xterm's standard 256-color extension.
fn ansi_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    // Ghostty default palette (Tomorrow Night Bright tone) — lifted
    // verbatim from ghostty-org/ghostty src/terminal/color.zig Name.default.
    // Combined with the cell-renderer's sRGB→DisplayP3 shader matrix,
    // this is what produces the same colour byte values the user sees
    // in ghostty itself.
    let base: [[u8; 3]; 16] = [
        [0x1D, 0x1F, 0x21], // 0  black
        [0xCC, 0x66, 0x66], // 1  red
        [0xB5, 0xBD, 0x68], // 2  green
        [0xF0, 0xC6, 0x74], // 3  yellow
        [0x81, 0xA2, 0xBE], // 4  blue
        [0xB2, 0x94, 0xBB], // 5  magenta
        [0x8A, 0xBE, 0xB7], // 6  cyan
        [0xC5, 0xC8, 0xC6], // 7  white
        [0x66, 0x66, 0x66], // 8  br black
        [0xD5, 0x4E, 0x53], // 9  br red
        [0xB9, 0xCA, 0x4A], // 10 br green
        [0xE7, 0xC5, 0x47], // 11 br yellow
        [0x7A, 0xA6, 0xDA], // 12 br blue
        [0xC3, 0x97, 0xD8], // 13 br magenta
        [0x70, 0xC0, 0xB1], // 14 br cyan
        [0xEA, 0xEA, 0xEA], // 15 br white
    ];
    p[..16].copy_from_slice(&base);
    // 216-color cube: 16..231
    let steps = [0u8, 95, 135, 175, 215, 255];
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                p[16 + r * 36 + g * 6 + b] = [steps[r], steps[g], steps[b]];
            }
        }
    }
    // (Earlier we hardcoded a handful of 256-palette overrides to
    //  reverse claude code's nearest-cube quantisation. Once we found
    //  that the real cause was `TMUX` being set in the child env —
    //  which made chalk fall back to ANSI-256 mode — and removed it,
    //  claude code emits truecolor escapes directly and the standard
    //  xterm 6×6×6 cube is correct again.)
    // 24-step grayscale ramp: 232..255
    for i in 0..24 {
        let v = 8 + (i as u8) * 10;
        p[232 + i] = [v, v, v];
    }
    p
}

/// Default foreground / background when a cell has `Color::Default`.
/// Decoded from the user's Terminal.app `GitHub Dark Dimmed` profile
/// (the active Default Window Settings): bg `#252c35`, fg `#bbc6d1`.
// Default foreground = pure white, matching Ghostty's default
// (#FFFFFF). The old [187,198,209] came from the user's Terminal.app
// "GitHub Dark Dimmed" profile and read noticeably greyer than
// Ghostty's body text.
pub const DEFAULT_FG: [u8; 4] = [255, 255, 255, 0xff];
/// Terminal body background — the single source is the theme token so chrome
/// and body share one palette.
pub const DEFAULT_BG: [u8; 4] = crate::theme::BG;

/// Cursor + selection accents. Cursor uses the shared accent so it matches the
/// focus ring / links across the whole UI; selection is a muted blue.
pub const ITERM_CURSOR: [u8; 4] = crate::theme::ACCENT;
pub const ITERM_SELECTION: [u8; 4] = [49, 99, 139, 0x99];
/// Inline-autosuggestion ghost text. A dim, low-contrast grey-blue that
/// sits clearly behind committed foreground text — fish/zsh style.
pub const GHOST_FG: [u8; 4] = [120, 132, 148, 0xff];

fn color_to_rgba(c: &Color, default: [u8; 4]) -> [u8; 4] {
    match c {
        Color::Default => default,
        Color::Idx(i) => {
            let p = ansi_palette()[*i as usize];
            [p[0], p[1], p[2], 0xff]
        }
        Color::Rgb(r, g, b) => [*r, *g, *b, 0xff],
    }
}


pub fn cell_fg(cell: &Cell) -> [u8; 4] {
    let mut fg = color_to_rgba(&cell.fg, DEFAULT_FG);
    if cell.inverse {
        fg = color_to_rgba(&cell.bg, DEFAULT_BG);
    }
    // SGR 2 (faint). Claude Code uses this for ghost-text autosuggestions;
    // without it the suggestion reads as committed input. Mix toward bg by
    // ~55% so the glyph stays legible but visibly secondary.
    if cell.dim {
        let bg = if cell.inverse {
            color_to_rgba(&cell.fg, DEFAULT_FG)
        } else {
            color_to_rgba(&cell.bg, DEFAULT_BG)
        };
        let t = 0.55_f32;
        for i in 0..3 {
            fg[i] = (fg[i] as f32 * (1.0 - t) + bg[i] as f32 * t).round() as u8;
        }
    }
    fg
}

pub fn cell_bg(cell: &Cell) -> [u8; 4] {
    let mut bg = color_to_rgba(&cell.bg, DEFAULT_BG);
    if cell.inverse {
        bg = color_to_rgba(&cell.fg, DEFAULT_FG);
    }
    bg
}

/// Draw one row of cells. `origin_y` is the row's top in logical pixels.
/// `text_baseline_offset` shifts where the glyph baseline lands relative
/// to the row top — sugarloaf's `text.draw` uses the baseline as the
/// reference y, so we add roughly the cell ascent.
/// Sub-rect fills (in cell-fraction coords 0..1) for the unicode
/// Block Elements range (U+2580..U+259F) plus shade variants. Each
/// entry is `(x0, y0, x1, y1, alpha_multiplier)` so a single block
/// glyph can decompose into 1–2 rects without any font involvement.
///
/// Why this is here instead of in the font: D2Coding's block glyphs
/// (and most monospace fonts) don't extend to the full cell advance
/// width — they leave a 1–2px gap on the right side of each glyph.
/// A run like `██████` becomes a striped bar instead of a solid
/// rectangle. Drawing the cell as a GPU quad sized to the actual
/// `cell_w` × `cell_h` fixes this without depending on font choice.
pub fn block_rects(ch: char) -> Option<&'static [(f32, f32, f32, f32, f32)]> {
    // Macro-ish: each constant is a slice of (x0, y0, x1, y1, alpha)
    // rectangles. Multi-quadrant blocks use two entries.
    const HALF_TOP: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 0.5, 1.0)];
    const HALF_BOTTOM: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.5, 1.0, 1.0, 1.0)];
    const HALF_LEFT: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 0.5, 1.0, 1.0)];
    const HALF_RIGHT: &[(f32, f32, f32, f32, f32)] = &[(0.5, 0.0, 1.0, 1.0, 1.0)];
    const FULL: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 1.0)];
    const SHADE_25: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 0.25)];
    const SHADE_50: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 0.5)];
    const SHADE_75: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 0.75)];
    Some(match ch {
        // Lower N/8 blocks (U+2581..U+2587) — bottom anchored.
        '\u{2581}' => &[(0.0, 7.0 / 8.0, 1.0, 1.0, 1.0)],
        '\u{2582}' => &[(0.0, 3.0 / 4.0, 1.0, 1.0, 1.0)],
        '\u{2583}' => &[(0.0, 5.0 / 8.0, 1.0, 1.0, 1.0)],
        '\u{2584}' => HALF_BOTTOM,
        '\u{2585}' => &[(0.0, 3.0 / 8.0, 1.0, 1.0, 1.0)],
        '\u{2586}' => &[(0.0, 1.0 / 4.0, 1.0, 1.0, 1.0)],
        '\u{2587}' => &[(0.0, 1.0 / 8.0, 1.0, 1.0, 1.0)],
        // Full / left N/8 / right blocks.
        '\u{2588}' => FULL,
        '\u{2589}' => &[(0.0, 0.0, 7.0 / 8.0, 1.0, 1.0)],
        '\u{258A}' => &[(0.0, 0.0, 3.0 / 4.0, 1.0, 1.0)],
        '\u{258B}' => &[(0.0, 0.0, 5.0 / 8.0, 1.0, 1.0)],
        '\u{258C}' => HALF_LEFT,
        '\u{258D}' => &[(0.0, 0.0, 3.0 / 8.0, 1.0, 1.0)],
        '\u{258E}' => &[(0.0, 0.0, 1.0 / 4.0, 1.0, 1.0)],
        '\u{258F}' => &[(0.0, 0.0, 1.0 / 8.0, 1.0, 1.0)],
        '\u{2590}' => HALF_RIGHT,
        // Shades — full block at reduced alpha.
        '\u{2591}' => SHADE_25,
        '\u{2592}' => SHADE_50,
        '\u{2593}' => SHADE_75,
        // Upper 1/8, right 1/8.
        '\u{2580}' => HALF_TOP,
        '\u{2594}' => &[(0.0, 0.0, 1.0, 1.0 / 8.0, 1.0)],
        '\u{2595}' => &[(7.0 / 8.0, 0.0, 1.0, 1.0, 1.0)],
        // Quadrants — corners + multi-corner combinations.
        '\u{2596}' => &[(0.0, 0.5, 0.5, 1.0, 1.0)],
        '\u{2597}' => &[(0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{2598}' => &[(0.0, 0.0, 0.5, 0.5, 1.0)],
        '\u{2599}' => &[(0.0, 0.0, 0.5, 1.0, 1.0), (0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{259A}' => &[(0.0, 0.0, 0.5, 0.5, 1.0), (0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{259B}' => &[(0.0, 0.0, 1.0, 0.5, 1.0), (0.0, 0.5, 0.5, 1.0, 1.0)],
        '\u{259C}' => &[(0.0, 0.0, 1.0, 0.5, 1.0), (0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{259D}' => &[(0.5, 0.0, 1.0, 0.5, 1.0)],
        '\u{259E}' => &[(0.5, 0.0, 1.0, 0.5, 1.0), (0.0, 0.5, 0.5, 1.0, 1.0)],
        '\u{259F}' => &[(0.5, 0.0, 1.0, 0.5, 1.0), (0.0, 0.5, 1.0, 1.0, 1.0)],
        // Box-drawing single-line characters. Glyph variants across fonts
        // leave gaps between adjacent cells (the prompt input box on
        // claude code reads as `---` instead of a continuous line under
        // CascadiaCodeNF). Render them as cell-wide GPU quads so the
        // line touches both edges and joins seamlessly with its neighbours.
        // Stroke width approximates ghostty's: ~1.5px at our cell height
        // = 6% of the cell.
        '\u{2500}' => &[(0.0, 0.47, 1.0, 0.53, 1.0)],   // ─ light horizontal
        '\u{2501}' => &[(0.0, 0.44, 1.0, 0.56, 1.0)],   // ━ heavy horizontal
        '\u{2502}' => &[(0.47, 0.0, 0.53, 1.0, 1.0)],   // │ light vertical
        '\u{2503}' => &[(0.44, 0.0, 0.56, 1.0, 1.0)],   // ┃ heavy vertical
        // Light / heavy dashed variants — draw as continuous lines.
        // The "dashed" visual is preserved by the eye when the line is
        // thin; trying to draw discrete dashes here just reintroduces
        // the gap we're fixing.
        '\u{2504}' | '\u{2508}' | '\u{254C}' => &[(0.0, 0.47, 1.0, 0.53, 1.0)],
        '\u{2505}' | '\u{2509}' | '\u{254D}' => &[(0.0, 0.44, 1.0, 0.56, 1.0)],
        '\u{2506}' | '\u{250A}' | '\u{254E}' => &[(0.47, 0.0, 0.53, 1.0, 1.0)],
        '\u{2507}' | '\u{250B}' | '\u{254F}' => &[(0.44, 0.0, 0.56, 1.0, 1.0)],
        // Double-line horizontal / vertical (two parallel strokes).
        '\u{2550}' => &[
            (0.0, 0.40, 1.0, 0.46, 1.0),
            (0.0, 0.54, 1.0, 0.60, 1.0),
        ],
        '\u{2551}' => &[
            (0.40, 0.0, 0.46, 1.0, 1.0),
            (0.54, 0.0, 0.60, 1.0, 1.0),
        ],
        _ => return None,
    })
}

