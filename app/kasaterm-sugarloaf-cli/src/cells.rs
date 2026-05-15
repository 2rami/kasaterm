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

/// iTerm2 Default Dark + xterm 256-color extension. RGB triples in
/// 0..255, alpha implicit 255. Index 0..15 are the 16 base ANSI
/// colors; 16..231 are the 6×6×6 cube; 232..255 are the grayscale ramp.
fn ansi_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    // 16 base ANSI — iTerm2 Default Dark style
    let base: [[u8; 3]; 16] = [
        [0x1c, 0x1c, 0x1c], // 0 black
        [0xc9, 0x1b, 0x00], // 1 red
        [0x00, 0xc2, 0x00], // 2 green
        [0xc7, 0xc4, 0x00], // 3 yellow
        [0x00, 0x37, 0xda], // 4 blue
        [0xc9, 0x30, 0xc7], // 5 magenta
        [0x00, 0xc5, 0xc7], // 6 cyan
        [0xc7, 0xc7, 0xc7], // 7 white
        [0x67, 0x67, 0x67], // 8 bright black
        [0xff, 0x6d, 0x67], // 9 bright red
        [0x5f, 0xf9, 0x67], // 10 bright green
        [0xfe, 0xfb, 0x67], // 11 bright yellow
        [0x68, 0x71, 0xff], // 12 bright blue
        [0xff, 0x76, 0xff], // 13 bright magenta
        [0x5f, 0xfd, 0xff], // 14 bright cyan
        [0xfe, 0xfe, 0xfe], // 15 bright white
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
/// Matches kasaterm-cli's `TERM_BG` / `TERM_FG` so the comparison stays
/// honest.
pub const DEFAULT_FG: [u8; 4] = [0xea, 0xee, 0xf4, 0xff];
pub const DEFAULT_BG: [u8; 4] = [0x1c, 0x20, 0x26, 0xff];

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
    // Foreground pass: one text.draw per cell. Wasteful (re-shapes
    // per-cell) but functionally correct for the MVP. Optimisation to
    // grouped text runs or Grid::write_row comes after the conversion
    // logic is validated end-to-end.
    let y = origin_y + text_baseline_offset;
    for (i, cell) in row.iter().enumerate() {
        if cell.ch == " " {
            continue;
        }
        let opts = DrawOpts {
            font_size,
            color: cell_fg(cell),
            bold: cell.bold,
            italic: cell.italic,
            font_id: None,
        };
        let x = origin_x + i as f32 * cell_w;
        sugarloaf.text_mut().draw(x, y, &cell.ch, &opts);
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
    let color = [0.20, 0.40, 0.85, 0.35];
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
pub fn render_screen(
    sugarloaf: &mut Sugarloaf<'_>,
    rows: &[Vec<Cell>],
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
) {
    // Empirical: text baseline sits ~0.78 * cell_h below row top with
    // Cascadia at 14pt. Tune later from real font metrics; for the MVP
    // this gets the glyphs inside the right grid cell.
    let baseline = cell_h * 0.78;
    for (r, row) in rows.iter().enumerate() {
        render_row(
            sugarloaf,
            row,
            origin_x,
            origin_y + r as f32 * cell_h,
            cell_w,
            cell_h,
            font_size,
            baseline,
        );
    }
}
