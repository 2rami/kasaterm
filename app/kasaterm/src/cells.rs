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

use kasa_bridge::screen::{Cell, Color};

/// ANSI 256-color lookup. 0-15 come from the active theme (see
/// `theme::ansi16` — runtime-swappable so a theme switch recolors the
/// terminal body); 16-231 is the standard xterm 6×6×6 cube and 232-255
/// the 24-step grayscale ramp, both computed. O(1), no table build.
///
/// (Earlier we hardcoded a handful of 256-palette overrides to
///  reverse claude code's nearest-cube quantisation. Once we found
///  that the real cause was `TMUX` being set in the child env —
///  which made chalk fall back to ANSI-256 mode — and removed it,
///  claude code emits truecolor escapes directly and the standard
///  xterm 6×6×6 cube is correct again.)
fn ansi_color(i: u8) -> [u8; 3] {
    match i {
        0..=15 => crate::theme::ansi16(i as usize),
        16..=231 => {
            let steps = [0u8, 95, 135, 175, 215, 255];
            let n = i as usize - 16;
            [steps[n / 36], steps[(n / 6) % 6], steps[n % 6]]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

/// Default foreground / background when a cell has `Color::Default`.
/// Decoded from the user's Terminal.app `GitHub Dark Dimmed` profile
/// (the active Default Window Settings): bg `#252c35`, fg `#bbc6d1`.
// Default foreground = pure white, matching Ghostty's default
// (#FFFFFF). The old [187,198,209] came from the user's Terminal.app
// "GitHub Dark Dimmed" profile and read noticeably greyer than
// Ghostty's body text.
#[inline]
pub fn default_fg() -> [u8; 4] {
    crate::theme::fg()
}
/// Terminal body background — the single source is the theme token so chrome
/// and body share one palette.
#[inline]
pub fn default_bg() -> [u8; 4] {
    crate::theme::bg()
}

/// Cursor + selection accents. Cursor uses the shared accent so it matches the
/// focus ring / links across the whole UI; selection is a muted blue.
#[inline]
pub fn iterm_cursor() -> [u8; 4] {
    crate::theme::accent()
}
pub const ITERM_SELECTION: [u8; 4] = [49, 99, 139, 0x99];
/// Inline-autosuggestion ghost text. A dim, low-contrast grey-blue that
/// sits clearly behind committed foreground text — fish/zsh style.
pub const GHOST_FG: [u8; 4] = [120, 132, 148, 0xff];

fn color_to_rgba(c: &Color, default: [u8; 4]) -> [u8; 4] {
    match c {
        Color::Default => default,
        Color::Idx(i) => {
            let p = ansi_color(*i);
            [p[0], p[1], p[2], 0xff]
        }
        Color::Rgb(r, g, b) => [*r, *g, *b, 0xff],
    }
}


/// 셀 전경색 — tmux `window-style fg=<색>` 등가 pane 틴트 지원. `dfg` 가 이 pane 의
/// "기본 전경색": 테마 default fg 를 쓰는 셀만 이 색이 되고, 명시 색(ANSI 16/256/
/// truecolor) 셀은 그대로다(`color_to_rgba` 의 Default 분기만 타므로). inverse 셀의
/// dim 혼합에 쓰이는 fg 참조도 같은 규칙 — tmux 와 동일하게 reverse 도 틴트를 따른다.
/// 무틴트 pane 은 `default_fg()` 를 넘긴다.
pub fn cell_fg_with(cell: &Cell, dfg: [u8; 4]) -> [u8; 4] {
    let mut fg = color_to_rgba(&cell.fg, dfg);
    if cell.inverse {
        fg = color_to_rgba(&cell.bg, default_bg());
    }
    // SGR 2 (faint). Claude Code uses this for ghost-text autosuggestions;
    // without it the suggestion reads as committed input. Mix toward bg by
    // ~55% so the glyph stays legible but visibly secondary.
    if cell.dim {
        let bg = if cell.inverse {
            color_to_rgba(&cell.fg, dfg)
        } else {
            color_to_rgba(&cell.bg, default_bg())
        };
        let t = 0.55_f32;
        for i in 0..3 {
            fg[i] = (fg[i] as f32 * (1.0 - t) + bg[i] as f32 * t).round() as u8;
        }
        // Faint is a deliberate request to recede. Lifting it back to a
        // contrast floor would erase the distinction the app just drew.
        return fg;
    }
    if names_own_color(if cell.inverse { &cell.bg } else { &cell.fg }) {
        fg = crate::theme::enforce_min_contrast(fg, cell_bg_with(cell, dfg));
    }
    fg
}

/// Whether the cell picked this color itself rather than inheriting one the
/// theme controls. Only those need the contrast guard: `Default` is the theme's
/// own fg, and ANSI 0-15 are remapped per palette, so both are already legible
/// by construction — running the guard on them would second-guess the theme.
fn names_own_color(c: &Color) -> bool {
    match c {
        Color::Default => false,
        Color::Idx(i) => *i >= 16,
        Color::Rgb(..) => true,
    }
}

/// 셀 배경색 — bg 자체는 무틴트(스펙)지만, inverse 셀의 배경 채움은 정의상 fg 색이므로
/// default-fg 셀이면 pane 틴트 `dfg` 를 따른다(claude 블록커서 = inverse 공백).
pub fn cell_bg_with(cell: &Cell, dfg: [u8; 4]) -> [u8; 4] {
    let mut bg = color_to_rgba(&cell.bg, default_bg());
    if cell.inverse {
        bg = color_to_rgba(&cell.fg, dfg);
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

    // Junction/corner strokes. A half stroke stops at the **far** edge of the
    // crossing stroke (0.53 light / 0.56 heavy), not at the cell centre — ending
    // it at 0.5 leaves a notch where the two meet.
    const H_L: (f32, f32, f32, f32, f32) = (0.0, 0.47, 1.0, 0.53, 1.0);
    const V_L: (f32, f32, f32, f32, f32) = (0.47, 0.0, 0.53, 1.0, 1.0);
    const HR_L: (f32, f32, f32, f32, f32) = (0.47, 0.47, 1.0, 0.53, 1.0);
    const HL_L: (f32, f32, f32, f32, f32) = (0.0, 0.47, 0.53, 0.53, 1.0);
    const VD_L: (f32, f32, f32, f32, f32) = (0.47, 0.47, 0.53, 1.0, 1.0);
    const VU_L: (f32, f32, f32, f32, f32) = (0.47, 0.0, 0.53, 0.53, 1.0);
    const H_H: (f32, f32, f32, f32, f32) = (0.0, 0.44, 1.0, 0.56, 1.0);
    const V_H: (f32, f32, f32, f32, f32) = (0.44, 0.0, 0.56, 1.0, 1.0);
    const HR_H: (f32, f32, f32, f32, f32) = (0.44, 0.44, 1.0, 0.56, 1.0);
    const HL_H: (f32, f32, f32, f32, f32) = (0.0, 0.44, 0.56, 0.56, 1.0);
    const VD_H: (f32, f32, f32, f32, f32) = (0.44, 0.44, 0.56, 1.0, 1.0);
    const VU_H: (f32, f32, f32, f32, f32) = (0.44, 0.0, 0.56, 0.56, 1.0);
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
        // Corners and junctions. Without these the straight runs came from GPU
        // quads while every corner fell back to the font, and the two never
        // lined up — markdown tables read as loose horizontal rules with the
        // frame missing (2026-08-15 신고). Mixed light/heavy junctions are left
        // to the font: they need per-side widths, which a static rect table
        // can't express, and they don't appear in practice.
        '\u{250C}' => &[HR_L, VD_L],
        '\u{2510}' => &[HL_L, VD_L],
        '\u{2514}' => &[HR_L, VU_L],
        '\u{2518}' => &[HL_L, VU_L],
        '\u{251C}' => &[V_L, HR_L],
        '\u{2524}' => &[V_L, HL_L],
        '\u{252C}' => &[H_L, VD_L],
        '\u{2534}' => &[H_L, VU_L],
        '\u{253C}' => &[H_L, V_L],
        '\u{250F}' => &[HR_H, VD_H],
        '\u{2513}' => &[HL_H, VD_H],
        '\u{2517}' => &[HR_H, VU_H],
        '\u{251B}' => &[HL_H, VU_H],
        '\u{2523}' => &[V_H, HR_H],
        '\u{252B}' => &[V_H, HL_H],
        '\u{2533}' => &[H_H, VD_H],
        '\u{253B}' => &[H_H, VU_H],
        '\u{254B}' => &[H_H, V_H],
        _ => return None,
    })
}

