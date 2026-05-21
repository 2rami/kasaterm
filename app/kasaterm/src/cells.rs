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

/// macOS Terminal.app "GitHub Dark Dimmed" — the user's active
/// Default Window Settings. Decoded from
/// `~/Library/Preferences/com.apple.Terminal.plist` (the colors are
/// `NSKeyedArchiver` bytes there; the values below are the sRGB
/// 8-bit triples after unarchiving). The 6×6×6 cube + 24-step
/// grayscale ramp follow xterm's standard 256-color extension.
fn ansi_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    let base: [[u8; 3]; 16] = [
        [99, 110, 123],   // 0  black     (#636e7b)
        [244, 112, 103],  // 1  red       (#f47067)
        [87, 171, 90],    // 2  green     (#57ab5a)
        [198, 144, 38],   // 3  yellow    (#c69026)
        [83, 155, 245],   // 4  blue      (#539bf5)
        [176, 131, 240],  // 5  magenta   (#b083f0)
        [57, 197, 207],   // 6  cyan      (#39c5cf)
        [144, 157, 171],  // 7  white     (#909dab)
        [99, 110, 123],   // 8  br black  (#636e7b)
        [255, 147, 138],  // 9  br red    (#ff938a)
        [107, 196, 109],  // 10 br green  (#6bc46d)
        [218, 170, 63],   // 11 br yellow (#daaa3f)
        [108, 182, 255],  // 12 br blue   (#6cb6ff)
        [220, 189, 251],  // 13 br magenta(#dcbdfb)
        [86, 212, 221],   // 14 br cyan   (#56d4dd)
        [205, 217, 229],  // 15 br white  (#cdd9e5)
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
/// Decoded from the user's Terminal.app `GitHub Dark Dimmed` profile
/// (the active Default Window Settings): bg `#252c35`, fg `#bbc6d1`.
// Default foreground = pure white, matching Ghostty's default
// (#FFFFFF). The old [187,198,209] came from the user's Terminal.app
// "GitHub Dark Dimmed" profile and read noticeably greyer than
// Ghostty's body text.
pub const DEFAULT_FG: [u8; 4] = [255, 255, 255, 0xff];
pub const DEFAULT_BG: [u8; 4] = [37, 44, 53, 0xff];

/// Cursor + selection accents from the same profile. Cursor is
/// `CursorColor` (a bright "GitHub link blue") and selection is
/// `SelectionColor` (a muted blue). Alpha tuned so selected text
/// stays readable underneath.
pub const ITERM_CURSOR: [u8; 4] = [100, 173, 247, 0xff];
pub const ITERM_SELECTION: [u8; 4] = [49, 99, 139, 0x99];

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
        _ => return None,
    })
}

/// Try to draw a cell as one or more GPU rects covering the block
/// glyph's sub-regions. Returns true if the character was a known
/// block element and got painted via rect — caller skips the
/// font-glyph path. Returns false otherwise.
fn try_render_block(
    sugarloaf: &mut Sugarloaf<'_>,
    ch: char,
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    fg: [u8; 4],
) -> bool {
    let Some(rects) = block_rects(ch) else { return false };
    for (x0, y0, x1, y1, alpha) in rects {
        let color = [
            fg[0] as f32 / 255.0,
            fg[1] as f32 / 255.0,
            fg[2] as f32 / 255.0,
            (fg[3] as f32 / 255.0) * alpha,
        ];
        sugarloaf.rect(
            None,
            cell_x + x0 * cell_w,
            cell_y + y0 * cell_h,
            (x1 - x0) * cell_w,
            (y1 - y0) * cell_h,
            color,
            0.0,
            0,
        );
    }
    true
}

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
    // Background pass: precompute the row's bg colors once, then walk
    // runs over the cached slice. The previous version called cell_bg()
    // twice per cell (once for the entry test, once for the run-extend
    // test), which on a 164-col grid burned ~10k extra palette lookups
    // per frame; the rest of the math is cheap so the cache wins.
    let bg_row: Vec<[u8; 4]> = row.iter().map(cell_bg).collect();
    let mut col: usize = 0;
    while col < row.len() {
        let bg = bg_row[col];
        if bg == DEFAULT_BG {
            col += 1;
            continue;
        }
        let start = col;
        while col < row.len() && bg_row[col] == bg {
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
        // Block Element shortcut. Block glyphs from D2Coding (and
        // most monospace fonts) don't fill the cell advance fully,
        // so consecutive █ characters render with visible gaps. Drop
        // out to GPU-rect drawing for the U+2580..U+259F range; the
        // result is a contiguous bar regardless of font choice. This
        // also breaks the text-batching run so the previous cell's
        // run doesn't accidentally swallow the block as a glyph.
        let first = cell.ch.chars().next();
        if let Some(c) = first {
            if (0x2580..=0x259F).contains(&(c as u32)) && cell.ch.chars().count() == 1 {
                let fg = cell_fg(cell);
                let x = origin_x + col as f32 * cell_w;
                if try_render_block(sugarloaf, c, x, origin_y, cell_w, cell_h, fg) {
                    col += 1;
                    continue;
                }
            }
        }
        // Treat code points that are likely to be serviced by a
        // proportional fallback font (Symbols, dingbats, geometric
        // shapes, Miscellaneous Technical) as wide so they get their
        // own standalone draw call. That keeps the per-glyph advance
        // pinned to a single cell instead of leaking into the next
        // one — symptom that caused `⏵` from the bypass-permissions
        // row to overlap the leading `b`.
        let is_wide = cell.ch.chars().count() != 1
            || cell.ch.chars().next().is_some_and(|c| {
                let cp = c as u32;
                cp > 0xFFFF || (0x2300..=0x27BF).contains(&cp)
            });
        let fg = cell_fg(cell);
        let bold = cell.bold;
        let italic = cell.italic;
        let start = col;
        let mut text = String::new();
        text.push_str(&cell.ch);
        col += 1;
        if !is_wide {
            // Extend the run while neighbour cells share the same attrs,
            // fit on the monospace grid (single-code-unit chars), and
            // aren't block elements (those took the GPU-rect branch
            // above and need to stay out of the text-shape batch).
            while col < row.len() {
                let n = &row[col];
                if n.ch.is_empty() || n.ch == " " {
                    break;
                }
                let n_wide = n.ch.chars().count() != 1
                    || n.ch.chars().next().is_some_and(|c| {
                        let cp = c as u32;
                        cp > 0xFFFF || (0x2300..=0x27BF).contains(&cp)
                    });
                let n_block = n.ch.chars().count() == 1
                    && n.ch
                        .chars()
                        .next()
                        .is_some_and(|c| (0x2580..=0x259F).contains(&(c as u32)));
                if n_wide
                    || n_block
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

/// Paint a Hangul / kana preedit string at the cursor cell.
///
/// Two layered rects + the glyphs:
/// 1. Solid background fill in `DEFAULT_BG` so the in-progress jamo
///    isn't visually composited with whatever cell content was
///    underneath (committed text, cursor block, selection — any of
///    which would muddy the composition).
/// 2. Thin underline accent at the bottom of the rect so the user
///    can tell preedit is active even when the jamo glyph happens
///    to match the body color.
/// 3. Glyphs in the cursor accent color (`accent`) — a clearly
///    different hue from `DEFAULT_FG`, so an unfinished "ㅎ" doesn't
///    blend with a finished "한" on the same line.
///
/// CJK glyphs typically take 2 cells per character (wide), so the
/// rect width scales by the visual width estimate rather than the
/// raw code-point count.
pub fn render_preedit(
    sugarloaf: &mut Sugarloaf<'_>,
    text: &str,
    px: f32,
    py: f32,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
    accent: [u8; 4],
    baseline_offset: f32,
) {
    let chars = text.chars().count().max(1) as f32;
    // Korean / Japanese / Chinese cells generally render double-wide;
    // estimate that here so the background rect actually covers all
    // the glyphs sugarloaf will lay down.
    let w = chars * cell_w * 2.0;
    // (1) Opaque BG so underlying text doesn't bleed through.
    sugarloaf.rect(
        None,
        px,
        py,
        w,
        cell_h,
        [
            DEFAULT_BG[0] as f32 / 255.0,
            DEFAULT_BG[1] as f32 / 255.0,
            DEFAULT_BG[2] as f32 / 255.0,
            1.0,
        ],
        0.0,
        0,
    );
    // (2) Accent underline — 2px bar at bottom edge.
    sugarloaf.rect(
        None,
        px,
        py + cell_h - 2.0,
        w,
        2.0,
        [
            accent[0] as f32 / 255.0,
            accent[1] as f32 / 255.0,
            accent[2] as f32 / 255.0,
            1.0,
        ],
        0.0,
        0,
    );
    // (3) Glyphs go through the exact same render_row path the body
    // uses. Calling sugarloaf.text.draw directly here landed on a
    // slightly different shaping route (no per-cell wide handling,
    // primary-only font lookup), so the in-progress Hangul glyph
    // visually floated above the committed `한` next to it. Building a
    // throwaway Cell row and reusing render_row keeps the per-glyph
    // path identical to the rest of the grid — same fallback chain,
    // same vertical metric, same row top reference.
    let synthetic: Vec<Cell> = text
        .chars()
        .map(|c| Cell {
            ch: c.to_string(),
            fg: Color::Rgb(accent[0], accent[1], accent[2]),
            bg: Color::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            dim: false,
        })
        .collect();
    render_row(
        sugarloaf,
        &synthetic,
        px,
        py,
        cell_w,
        cell_h,
        font_size,
        baseline_offset,
    );
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
            dim: false,
        }
    }

    #[test]
    fn ansi_base_palette_matches_user_terminal_profile() {
        // Values come from the user's macOS Terminal.app profile
        // `GitHub Dark Dimmed` (active Default Window Settings).
        // Decoded via NSKeyedUnarchiver on the bytes in
        // ~/Library/Preferences/com.apple.Terminal.plist.
        let p = ansi_palette();
        assert_eq!(p[0], [99, 110, 123], "black");
        assert_eq!(p[1], [244, 112, 103], "red");
        assert_eq!(p[4], [83, 155, 245], "blue (GitHub link blue)");
        assert_eq!(p[7], [144, 157, 171], "white");
        assert_eq!(p[15], [205, 217, 229], "bright white");
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
        // ANSI 9 = bright red. Terminal.app GitHub Dark Dimmed
        // has this slot at #ff938a (warm salmon).
        assert_eq!(
            color_to_rgba(&Color::Idx(9), DEFAULT_FG),
            [255, 147, 138, 0xff],
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
