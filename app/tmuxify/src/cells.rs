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

use sugarloaf::text::DrawOpts;
use sugarloaf::Sugarloaf;
use tmux_bridge::screen::{Cell, Color};

/// User's iTerm2 Default profile (decoded from
/// `~/Library/Preferences/com.googlecode.iterm2.plist` →
/// `New Bookmarks[0]`). The 6×6×6 cube + 24-step grayscale ramp
/// below follow xterm's standard 256-color extension.
fn ansi_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    let base: [[u8; 3]; 16] = [
        [19, 24, 29],     // 0  black     (#13181d)
        [180, 60, 41],    // 1  red       (#b43c29)
        [0, 193, 0],      // 2  green     (#00c100)
        [199, 196, 0],    // 3  yellow    (#c7c400)
        [39, 67, 199],    // 4  blue      (#2743c7)
        [191, 63, 189],   // 5  magenta   (#bf3fbd)
        [0, 197, 199],    // 6  cyan      (#00c5c7)
        [199, 199, 199],  // 7  white     (#c7c7c7)
        [103, 103, 103],  // 8  br black  (#676767)
        [220, 121, 116],  // 9  br red    (#dc7974)
        [87, 230, 144],   // 10 br green  (#57e690)
        [236, 225, 0],    // 11 br yellow (#ece100)
        [166, 170, 241],  // 12 br blue   (#a6aaf1)
        [224, 125, 224],  // 13 br magenta(#e07de0)
        [95, 253, 255],   // 14 br cyan   (#5ffdff)
        [254, 255, 255],  // 15 br white  (#feffff)
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
    // 24-step grayscale ramp: 232..255
    for i in 0..24 {
        let v = 8 + (i as u8) * 10;
        p[232 + i] = [v, v, v];
    }
    p
}

/// Default foreground / background when a cell has `Color::Default`.
/// Decoded from the user's iTerm2 Default profile —
/// `Foreground Color` = #0f0f0f, `Background Color` = #f9f9f9. A
/// light-on-dark profile would flip these; we follow the user's
/// actual config so the comparison stays honest.
pub const DEFAULT_FG: [u8; 4] = [15, 15, 15, 0xff];
pub const DEFAULT_BG: [u8; 4] = [249, 249, 249, 0xff];

/// Cursor + selection accents from the same profile so the on-screen
/// chrome matches iTerm2's behavior — cursor is `Cursor Color`,
/// selection is `Selection Color`.
pub const ITERM_CURSOR: [u8; 4] = [0, 0, 0, 0xff];
pub const ITERM_SELECTION: [u8; 4] = [179, 214, 255, 0x66];

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

fn rgba_to_float(rgba: [u8; 4]) -> [f32; 4] {
    [
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    ]
}

pub fn cell_fg(cell: &Cell) -> [u8; 4] {
    let mut fg = color_to_rgba(&cell.fg, DEFAULT_FG);
    if cell.inverse {
        fg = color_to_rgba(&cell.bg, DEFAULT_BG);
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
pub fn render_row(
    sugarloaf: &mut Sugarloaf<'_>,
    row: &[Cell],
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
    text_baseline_offset: f32,
) {
    // Background pass: emit a rect for every non-default bg run. We
    // collapse adjacent same-color cells into one rect so a long
    // selection highlight is one quad, not 80.
    let mut col: usize = 0;
    while col < row.len() {
        let bg = cell_bg(&row[col]);
        if bg == DEFAULT_BG {
            col += 1;
            continue;
        }
        let start = col;
        while col < row.len() && cell_bg(&row[col]) == bg {
            col += 1;
        }
        let width = (col - start) as f32 * cell_w;
        let x = origin_x + start as f32 * cell_w;
        sugarloaf.rect(None, x, origin_y, width, cell_h, rgba_to_float(bg), 0.0, 0);
    }
    // Foreground pass: batch consecutive cells with identical attrs into
    // a single text.draw call. A 100-col line of git output spent 100 calls
    // per row × 33 rows × 60fps = ~200K calls/sec under the per-cell path;
    // most rows have long runs of identical (fg, bold, italic), so the
    // batched walk collapses that to a handful of calls per row. Sugarloaf
    // shapes each draw() argument independently, so combining same-attr
    // strings means swash sees one shaping job instead of N.
    //
    // We can't blindly merge across cells whose width != cell_w (CJK /
    // emoji) — those would land at the wrong x if a single run laid out
    // proportionally. Treat any cell whose `ch` is more than one code unit
    // wide as a run-breaker and emit it standalone.
    let y = origin_y + text_baseline_offset;
    let mut col: usize = 0;
    while col < row.len() {
        let cell = &row[col];
        if cell.ch.is_empty() || cell.ch == " " {
            col += 1;
            continue;
        }
        let is_wide = cell.ch.chars().count() != 1
            || cell.ch.chars().next().is_some_and(|c| (c as u32) > 0xFFFF);
        let fg = cell_fg(cell);
        let bold = cell.bold;
        let italic = cell.italic;
        let start = col;
        let mut text = String::new();
        text.push_str(&cell.ch);
        col += 1;
        if !is_wide {
            // Extend the run while neighbour cells share the same attrs and
            // also fit on the monospace grid (single-code-unit chars).
            while col < row.len() {
                let n = &row[col];
                if n.ch.is_empty() || n.ch == " " {
                    break;
                }
                let n_wide = n.ch.chars().count() != 1
                    || n.ch.chars().next().is_some_and(|c| (c as u32) > 0xFFFF);
                if n_wide
                    || cell_fg(n) != fg
                    || n.bold != bold
                    || n.italic != italic
                {
                    break;
                }
                text.push_str(&n.ch);
                col += 1;
            }
        }
        let opts = DrawOpts {
            font_size,
            color: fg,
            bold,
            italic,
            font_id: None,
        };
        let x = origin_x + start as f32 * cell_w;
        sugarloaf.text_mut().draw(x, y, &text, &opts);
    }
}

/// Translucent selection overlay covering cells from `anchor` to `end`
/// in reading order. Drawn after the per-cell glyphs so the text shows
/// through. One quad per row (or one for single-row selections) — cheap.
pub fn render_selection_overlay(
    sugarloaf: &mut Sugarloaf<'_>,
    anchor: (u16, u16),
    end: (u16, u16),
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
) {
    let (start, stop) = if (anchor.1, anchor.0) <= (end.1, end.0) {
        (anchor, end)
    } else {
        (end, anchor)
    };
    // iTerm2-ish selection color: muted blue at 35% alpha.
    // Selection color = user's iTerm2 `Selection Color`
    // (#b3d6ff at ~40% alpha so text underneath stays readable).
    let color = [
        ITERM_SELECTION[0] as f32 / 255.0,
        ITERM_SELECTION[1] as f32 / 255.0,
        ITERM_SELECTION[2] as f32 / 255.0,
        ITERM_SELECTION[3] as f32 / 255.0,
    ];
    if start.1 == stop.1 {
        let x = origin_x + start.0 as f32 * cell_w;
        let y = origin_y + start.1 as f32 * cell_h;
        let w = (stop.0 - start.0 + 1) as f32 * cell_w;
        sugarloaf.rect(None, x, y, w, cell_h, color, 0.0, 0);
        return;
    }
    // Multi-row: head fragment, full middle rows, tail fragment. We don't
    // know col count from the cell grid here, so caller passes anchor/end
    // already clamped. Middle rows extend to a generous width — set large
    // enough that any realistic terminal column fits.
    let max_w = 4000.0;
    // First row: from start.col to end-of-line.
    {
        let x = origin_x + start.0 as f32 * cell_w;
        let y = origin_y + start.1 as f32 * cell_h;
        sugarloaf.rect(None, x, y, max_w - x, cell_h, color, 0.0, 0);
    }
    // Middle rows: full width.
    for r in (start.1 + 1)..stop.1 {
        let y = origin_y + r as f32 * cell_h;
        sugarloaf.rect(None, origin_x, y, max_w, cell_h, color, 0.0, 0);
    }
    // Last row: from col 0 to stop.col inclusive.
    {
        let y = origin_y + stop.1 as f32 * cell_h;
        let w = (stop.0 + 1) as f32 * cell_w;
        sugarloaf.rect(None, origin_x, y, w, cell_h, color, 0.0, 0);
    }
}

/// Paint a Hangul/kana preedit string at the cursor cell. Background is
/// a lightly tinted rect so the in-progress jamo is distinguishable from
/// committed text; glyphs use the default foreground at the same font
/// size as the body.
pub fn render_preedit(
    sugarloaf: &mut Sugarloaf<'_>,
    text: &str,
    px: f32,
    py: f32,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
) {
    let w = (text.chars().count().max(1) as f32) * cell_w * 1.2;
    sugarloaf.rect(
        None,
        px,
        py,
        w,
        cell_h,
        [
            DEFAULT_FG[0] as f32 / 255.0,
            DEFAULT_FG[1] as f32 / 255.0,
            DEFAULT_FG[2] as f32 / 255.0,
            0.18,
        ],
        0.0,
        0,
    );
    let opts = DrawOpts {
        font_size,
        color: DEFAULT_FG,
        bold: false,
        italic: false,
        font_id: None,
    };
    sugarloaf.text_mut().draw(px, py + cell_h * 0.78, text, &opts);
}

/// Draw the full screen. Rows are addressed top-down starting from
/// `origin_y`. Caller is responsible for emitting the background fill
/// before calling this — cells with default bg do not paint a rect.
/// `baseline_offset` is logical pixels from the row top to the glyph
/// baseline — caller should pass the sugarloaf-measured value rather
/// than a guess so descenders don't clip and adjacent rows don't kiss.
pub fn render_screen(
    sugarloaf: &mut Sugarloaf<'_>,
    rows: &[Vec<Cell>],
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
    baseline_offset: f32,
) {
    for (r, row) in rows.iter().enumerate() {
        render_row(
            sugarloaf,
            row,
            origin_x,
            origin_y + r as f32 * cell_h,
            cell_w,
            cell_h,
            font_size,
            baseline_offset,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_with(fg: Color, bg: Color, inverse: bool) -> Cell {
        Cell {
            ch: "a".into(),
            fg,
            bg,
            bold: false,
            italic: false,
            underline: false,
            inverse,
        }
    }

    #[test]
    fn ansi_base_palette_matches_user_iterm2_profile() {
        // Values come from the user's iTerm2 plist
        // (`New Bookmarks[0]`). Re-decode and update here if the
        // profile changes — palette drift produces silent visual
        // regressions otherwise.
        let p = ansi_palette();
        assert_eq!(p[0], [19, 24, 29], "black");
        assert_eq!(p[1], [180, 60, 41], "red (rust orange in user profile)");
        assert_eq!(p[4], [39, 67, 199], "blue");
        assert_eq!(p[7], [199, 199, 199], "white");
        assert_eq!(p[15], [254, 255, 255], "bright white");
    }

    #[test]
    fn ansi_color_cube_indices_are_well_formed() {
        let p = ansi_palette();
        // 6×6×6 cube starts at 16. Index 16 = darkest (all 0), 231 = brightest.
        assert_eq!(p[16], [0, 0, 0]);
        assert_eq!(p[231], [255, 255, 255]);
        // A mid-cube color: r=2, g=3, b=4 → 16 + 2*36 + 3*6 + 4 = 110
        // step values: [0, 95, 135, 175, 215, 255]
        assert_eq!(p[110], [135, 175, 215]);
    }

    #[test]
    fn ansi_grayscale_ramp_is_monotonic() {
        let p = ansi_palette();
        // 232..255 = 24-step ramp, value = 8 + i*10
        assert_eq!(p[232], [8, 8, 8]);
        assert_eq!(p[233], [18, 18, 18]);
        for i in 232..255 {
            assert!(p[i][0] < p[i + 1][0], "ramp should increase at i={i}");
        }
    }

    #[test]
    fn color_default_resolves_to_caller_default() {
        assert_eq!(color_to_rgba(&Color::Default, DEFAULT_FG), DEFAULT_FG);
        assert_eq!(color_to_rgba(&Color::Default, DEFAULT_BG), DEFAULT_BG);
    }

    #[test]
    fn color_rgb_passes_through_unchanged() {
        assert_eq!(
            color_to_rgba(&Color::Rgb(0xff, 0x5c, 0x57), DEFAULT_FG),
            [0xff, 0x5c, 0x57, 0xff],
        );
    }

    #[test]
    fn color_idx_picks_palette_entry() {
        // ANSI 9 = bright red. User's profile has this slot at
        // #dc7974 (a softer salmon than xterm's pure bright red).
        assert_eq!(
            color_to_rgba(&Color::Idx(9), DEFAULT_FG),
            [220, 121, 116, 0xff],
        );
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let c = cell_with(Color::Rgb(0xff, 0, 0), Color::Rgb(0, 0xff, 0), true);
        assert_eq!(cell_fg(&c), [0, 0xff, 0, 0xff], "fg = original bg under inverse");
        assert_eq!(cell_bg(&c), [0xff, 0, 0, 0xff], "bg = original fg under inverse");
    }

    #[test]
    fn non_inverse_keeps_fg_and_bg() {
        let c = cell_with(Color::Rgb(0xff, 0, 0), Color::Rgb(0, 0xff, 0), false);
        assert_eq!(cell_fg(&c), [0xff, 0, 0, 0xff]);
        assert_eq!(cell_bg(&c), [0, 0xff, 0, 0xff]);
    }

    #[test]
    fn inverse_default_pair_swaps_to_terminal_defaults() {
        // Both fg and bg are Default — under inverse, fg should pull
        // the cell's bg lookup (which resolves to terminal default BG),
        // and vice versa. That's the standard SGR 7 behavior.
        let c = cell_with(Color::Default, Color::Default, true);
        assert_eq!(cell_fg(&c), DEFAULT_BG);
        assert_eq!(cell_bg(&c), DEFAULT_FG);
    }
}
