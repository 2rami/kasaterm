//! Viewer-only cell projection. Never resize or mutate the canonical terminal.
use crate::GridCell;
use unicode_width::UnicodeWidthChar;

pub(crate) type SourcePos = (usize, usize); // (row, column), never (x, y)

#[derive(Clone, Debug)]
pub(crate) struct ProjectedLine {
    pub cells: Vec<GridCell>,
    pub source_map: Vec<Option<SourcePos>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Projection {
    pub rows: Vec<Vec<GridCell>>,
    pub source_map: Vec<Vec<Option<SourcePos>>>,
    pub cursor: Option<SourcePos>,
    /// All body lines, including those above/below the current viewer viewport.
    pub body_lines: Vec<ProjectedLine>,
    pub max_scroll: usize,
    pub scroll_from_bottom: usize,
}

fn blank_line(cols: usize) -> ProjectedLine {
    ProjectedLine { cells: vec![GridCell::blank(); cols], source_map: vec![None; cols] }
}

fn finish_line(line: &mut ProjectedLine, cols: usize, fill: &GridCell, soft_wrap: bool) -> ProjectedLine {
    let mut padding = fill.clone();
    padding.ch = ' ';
    padding.wrapped = false;
    padding.hidden = false;
    padding.leading_wide_spacer = soft_wrap;
    line.cells.resize(cols, padding);
    line.source_map.resize(cols, None);
    if let Some(last) = line.cells.last_mut() { last.wrapped = soft_wrap; }
    std::mem::replace(line, ProjectedLine { cells: Vec::new(), source_map: Vec::new() })
}

/// Only unlabelled horizontal rules are elastic. Tables, code, labelled borders
/// and ordinary hard line breaks are not inferred to be paragraphs.
fn rule_cell(row: &[GridCell]) -> Option<&GridCell> {
    let mut glyphs = row.iter().filter(|c| !matches!(c.ch, ' ' | '\0'));
    let first = glyphs.next()?;
    if !matches!(first.ch, '─' | '━' | '═' | '╌' | '╍' | '┄' | '┅' | '┈' | '┉' | '-' | '=') { return None; }
    let mut count = 1;
    for cell in glyphs {
        if cell.ch != first.ch { return None; }
        count += 1;
    }
    (count >= 3).then_some(first)
}

fn project_region(source: &[Vec<GridCell>], start: usize, end: usize, cursor: SourcePos, cols: usize) -> Vec<ProjectedLine> {
    let mut output = Vec::new();
    let mut row_index = start;
    while row_index < end {
        if let Some(rule) = rule_cell(&source[row_index]) {
            let mut cells = vec![rule.clone(); cols];
            for cell in &mut cells { cell.wrapped = false; }
            output.push(ProjectedLine {
                cells,
                source_map: (0..cols).map(|c| (c < source[row_index].len()).then_some((row_index, c))).collect(),
            });
            row_index += 1;
            continue;
        }

        let first_row = row_index;
        let mut logical: Vec<(GridCell, SourcePos)> = Vec::new();
        let fill;
        loop {
            let row = &source[row_index];
            let joins_next = row.last().is_some_and(|c| c.wrapped)
                && row_index + 1 < end && rule_cell(&source[row_index + 1]).is_none();
            // Trailing terminal padding is not another paragraph. Keep interior
            // spaces, all soft-wrapped cells, and the actual cursor's blank cell.
            let tail = row.last();
            let significant = row.iter().rposition(|c| !matches!(c.ch, ' ' | '\0') || c.hidden
                || tail.is_some_and(|fill| c.bg != fill.bg || c.inverse != fill.inverse
                    || c.underline != fill.underline)).map_or(0, |i| i + 1);
            let cursor_end = if cursor.0 == row_index { cursor.1.saturating_add(1).min(row.len()) } else { 0 };
            let mut length = if joins_next { row.len() } else { significant.max(cursor_end) };
            // A wide glyph's spacer may itself be trailing whitespace.
            if length > 0 && row[length - 1].ch.width().unwrap_or(1) == 2 && length < row.len() { length += 1; }
            logical.extend(row[..length].iter().cloned().enumerate().filter(|(_, cell)| !cell.leading_wide_spacer).map(|(col, mut cell)| {
                cell.wrapped = false;
                (cell, (row_index, col))
            }));
            row_index += 1;
            if !joins_next {
                fill = row.last().cloned().unwrap_or_else(GridCell::blank);
                break;
            }
        }

        let mut line = ProjectedLine { cells: Vec::new(), source_map: Vec::new() };
        let mut index = 0;
        while index < logical.len() {
            let (cell, pos) = &logical[index];
            let wide = cell.ch.width().unwrap_or(1) == 2;
            let width = if wide { 2.min(cols) } else { 1 };
            if line.cells.len() + width > cols {
                output.push(finish_line(&mut line, cols, &fill, true));
            }
            line.cells.push(cell.clone());
            line.source_map.push(Some(*pos));
            index += 1;
            if wide {
                // The wire grid uses either a space or NUL for the wide spacer.
                // Consume it only when adjacent in the original source row.
                let spacer = logical.get(index).filter(|(next, next_pos)|
                    *next_pos == (pos.0, pos.1 + 1) && matches!(next.ch, ' ' | '\0'));
                if width == 2 {
                    let (mut spacer_cell, spacer_pos) = spacer.map(|(c, p)| (c.clone(), Some(*p)))
                        .unwrap_or_else(|| (GridCell::blank(), None));
                    spacer_cell.ch = '\0';
                    line.cells.push(spacer_cell);
                    line.source_map.push(spacer_pos);
                }
                if spacer.is_some() { index += 1; }
            }
        }
        if logical.is_empty() {
            // Empty hard rows stay real rows; preserve their colour and provenance.
            for (col, cell) in source[first_row].iter().take(cols).enumerate() {
                let mut cell = cell.clone();
                cell.wrapped = false;
                line.cells.push(cell);
                line.source_map.push(Some((first_row, col)));
            }
        }
        output.push(finish_line(&mut line, cols, &fill, false));
    }
    output
}

/// Render at independent viewer dimensions. `None` follows the live cursor;
/// `Some(0)` explicitly selects the body tail. Input and its footer remain pinned; input taller than the viewport
/// is clipped around its cursor. Callers should use at least two columns for CJK.
pub(crate) fn project(
    source: &[Vec<GridCell>], source_cursor: SourcePos,
    cols: usize, rows: usize, input_top: Option<usize>, scroll_from_bottom: Option<usize>,
) -> Projection {
    if cols == 0 || rows == 0 {
        return Projection { rows: vec![vec![]; rows], source_map: vec![vec![]; rows], cursor: None,
            body_lines: Vec::new(), max_scroll: 0, scroll_from_bottom: 0 };
    }
    // Canonical terminal height is not content height. Keeping dozens of empty
    // source rows below a shell prompt would put only blank padding in a short
    // viewer. Preserve visible background/conceal metadata and the cursor row;
    // only discard the unused tail, then pad to the viewer's own height below.
    let content_end = source.iter().rposition(|row| row.iter().any(|cell|
        !matches!(cell.ch, ' ' | '\0') || cell.hidden || cell.inverse || cell.underline
            || cell.bg != kasa_bridge::screen::Color::Default)).map_or(0, |row| row + 1);
    let source_end = content_end.max(source_cursor.0.saturating_add(1)).min(source.len());
    let input_top = input_top.filter(|&top| top < source_end);
    let body_end = input_top.unwrap_or(source_end);
    let body_lines = project_region(source, 0, body_end, source_cursor, cols);
    let pinned = project_region(source, body_end, source_end, source_cursor, cols);
    let pinned_height = pinned.len().min(rows);
    let body_height = rows - pinned_height;
    let max_scroll = body_lines.len().saturating_sub(body_height);
    let follow_cursor = scroll_from_bottom.is_none();
    let scroll_from_bottom = scroll_from_bottom.unwrap_or(0).min(max_scroll);
    let mut body_stop = body_lines.len().saturating_sub(scroll_from_bottom);
    let mut body_start = body_stop.saturating_sub(body_height);
    // A full-screen TUI may keep substantial content below its cursor. At the
    // live position follow the cursor instead of blindly showing the body tail.
    // Explicit viewer scrolling must never snap back to that cursor.
    if input_top.is_none() && follow_cursor && body_height > 0 {
        if let Some(cursor_line) = body_lines.iter().position(|line| line.source_map.contains(&Some(source_cursor))) {
            if cursor_line < body_start {
                body_start = cursor_line.saturating_sub(body_height - 1);
                body_stop = (body_start + body_height).min(body_lines.len());
            }
        }
    }
    // Cursor following may have moved away from the tail. Report the actual
    // viewport offset so the next wheel step starts here, not at the old tail.
    let scroll_from_bottom = body_lines.len().saturating_sub(body_stop);
    let visible_body = &body_lines[body_start..body_stop];
    let mut visible = Vec::with_capacity(rows);
    if input_top.is_some() {
        visible.extend((visible_body.len()..body_height).map(|_| blank_line(cols)));
    }
    visible.extend_from_slice(visible_body);
    while visible.len() < body_height { visible.push(blank_line(cols)); }
    let mut pinned_start = pinned.len().saturating_sub(pinned_height);
    if let Some(cursor_line) = pinned.iter().position(|line| line.source_map.contains(&Some(source_cursor))) {
        if cursor_line < pinned_start { pinned_start = cursor_line; }
    }
    visible.extend_from_slice(&pinned[pinned_start..pinned_start + pinned_height]);
    let cursor = visible.iter().enumerate().find_map(|(row, line)|
        line.source_map.iter().position(|pos| *pos == Some(source_cursor)).map(|col| (row, col)));
    Projection {
        rows: visible.iter().map(|line| line.cells.clone()).collect(),
        source_map: visible.into_iter().map(|line| line.source_map).collect(),
        cursor, body_lines, max_scroll, scroll_from_bottom,
    }
}

/// History is viewer-local too. Keep the real live input below it, and never
/// translate a click on historical text into a click on the source's live grid.
pub(crate) fn project_history(
    history: &[Vec<GridCell>], live: &[Vec<GridCell>], cursor: SourcePos,
    history_offset: usize, cols: usize, rows: usize, scroll: Option<usize>,
) -> Projection {
    let top = crate::screenread::pinned_input_top(live).filter(|top| *top <= cursor.0);
    let body_len = top.map_or(history.len(), |top| top.saturating_add(history_offset).min(history.len()));
    let mut combined = history[..body_len].to_vec();
    if let Some(top) = top { combined.extend_from_slice(&live[top..]); }
    let projected_cursor = top.map_or((usize::MAX, usize::MAX), |top| (body_len + cursor.0 - top, cursor.1));
    let mut view = project(&combined, projected_cursor, cols, rows, top.map(|_| body_len), Some(scroll.unwrap_or(0)));
    let remap = |pos: &mut Option<SourcePos>| {
        *pos = pos.and_then(|(row, col)| {
            if row < body_len { row.checked_sub(history_offset).map(|row| (row, col)) }
            else { top.map(|top| (top + row - body_len, col)) }
        });
    };
    for row in &mut view.source_map { for pos in row { remap(pos); } }
    for line in &mut view.body_lines { for pos in &mut line.source_map { remap(pos); } }
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(text: &str) -> Vec<GridCell> {
        text.chars().map(|ch| GridCell { ch, ..GridCell::blank() }).collect()
    }
    fn text(rows: &[Vec<GridCell>]) -> Vec<String> {
        rows.iter().map(|r| r.iter().map(|c| if c.ch == '\0' { ' ' } else { c.ch }).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn long_hard_rows_wrap_without_merging_code_or_table_rows() {
        let source = vec![line("abcdefgh"), line("  code"), line("|a|b|")];
        let view = project(&source, (0, 7), 4, 6, None, None);
        assert_eq!(text(&view.rows), ["abcd", "efgh", "  co", "de", "|a|b", "|"]);
        assert_eq!(view.cursor, Some((1, 3)));
        assert_eq!(view.source_map[3][1], Some((1, 5)));
    }

    #[test]
    fn soft_wrapped_rows_join_and_recompute_wrap_flags() {
        let mut source = vec![line("abcd"), line("ef  "), line("NEW")];
        source[0][3].wrapped = true;
        let original = source.clone();
        let view = project(&source, (1, 1), 6, 3, None, None);
        assert_eq!(text(&view.rows), ["abcdef", "NEW", ""]);
        assert_eq!(view.cursor, Some((0, 5)));
        assert!(!view.rows[0][5].wrapped);
        assert_eq!(source, original, "projection must not mutate source cells");
    }

    #[test]
    fn wide_glyph_never_straddles_a_viewer_row_and_spacer_maps_back() {
        let mut source = vec![line("abc한 글 ")];
        source[0][3].bold = true;
        source[0][3].fg = kasa_bridge::screen::Color::Rgb(1, 2, 3);
        let view = project(&source, (0, 6), 4, 3, None, None);
        assert_eq!(text(&view.rows), ["abc", "한 글", ""]);
        assert_eq!(view.source_map[1][1], Some((0, 4)));
        assert_eq!(view.source_map[1][3], Some((0, 6)));
        assert_eq!(view.cursor, Some((1, 3)));
        assert!(view.rows[1][0].bold);
        assert_eq!(view.rows[1][0].fg, source[0][3].fg);
        assert_eq!(view.rows[1][1].ch, '\0');
    }

    #[test]
    fn input_is_pinned_and_earlier_body_is_scrollable_without_source_changes() {
        let source = vec![line("1111222233334444"), line("────────"), line("❯ hi"), line("────────")];
        let view = project(&source, (2, 3), 4, 5, Some(1), None);
        assert_eq!(text(&view.rows), ["3333", "4444", "────", "❯ hi", "────"]);
        assert_eq!(view.body_lines.len(), 4);
        assert_eq!(view.max_scroll, 2);
        assert_eq!(view.cursor, Some((3, 3)));
        let older = project(&source, (2, 3), 4, 5, Some(1), Some(99));
        assert_eq!(text(&older.rows), ["1111", "2222", "────", "❯ hi", "────"]);
        assert_eq!(older.scroll_from_bottom, 2);
        assert_eq!(older.cursor, view.cursor);
    }

    #[test]
    fn filled_prompt_colour_expands_without_wrapping_padding() {
        let mut input = line("› hi            ");
        for cell in &mut input { cell.bg = kasa_bridge::screen::Color::Rgb(8, 9, 10); }
        let source = vec![line("body"), input];
        let view = project(&source, (1, 4), 6, 4, Some(1), None);
        assert_eq!(text(&view.rows), ["", "", "body", "› hi"]);
        assert!(view.rows[3].iter().all(|c| c.bg == source[1][0].bg));
        assert_eq!(view.cursor, Some((3, 4)));
    }

    #[test]
    fn coloured_blank_segments_survive_reflow_and_explicit_tail_does_not_follow_cursor() {
        let mut source = vec![line("x       ")];
        source[0][5].bg = kasa_bridge::screen::Color::Rgb(10, 20, 30);
        let view = project(&source, (0, 0), 4, 3, None, None);
        assert_eq!(view.rows[1][1].bg, source[0][5].bg);
        assert_eq!(view.source_map[1][1], Some((0, 5)));
        let source: Vec<_> = (0..20).map(|_| line("abc")).collect();
        let live = project(&source, (0, 0), 4, 3, None, None);
        assert_eq!(live.source_map[0][0], Some((0, 0)));
        let tail = project(&source, (0, 0), 4, 3, None, Some(0));
        assert_eq!(tail.source_map[0][0], Some((17, 0)));
        assert_eq!(tail.cursor, None);
        assert_eq!(tail.scroll_from_bottom, 0);
    }

    #[test]
    fn wide_margin_spacers_are_not_real_spaces_and_history_keeps_live_input() {
        let mut source = vec![line("abcd "), line("한   ")];
        source[0][4].wrapped = true;
        source[0][4].leading_wide_spacer = true;
        let view = project(&source, (1, 0), 8, 2, None, None);
        assert_eq!(view.rows[0][4].ch, '한');
        assert_eq!(view.source_map[0][4], Some((1, 0)));
        let view = project(&[line("abc한 ")], (0, 3), 4, 2, None, None);
        assert!(view.rows[0][3].leading_wide_spacer);
        let live = vec![line("body"), line("────────────"), line("❯ live input"), line("────────────")];
        let history = vec![line("old text"); 4];
        let view = project_history(&history, &live, (2, 3), 8, 8, 6, None);
        assert!(text(&view.rows).iter().any(|row| row == "❯ live i"));
        assert!(view.source_map[0].iter().all(Option::is_none));
        assert_eq!(view.cursor, Some((3, 3)));
        assert_eq!(view.source_map[3][3], Some((2, 3)));
    }

    #[test]
    fn cursor_on_blank_input_and_overheight_input_remains_visible() {
        let source = vec![line("body"), line("❯ abcdefghijklmnop"), line("footer")];
        let view = project(&source, (1, 2), 4, 2, Some(1), None);
        assert_eq!(view.cursor, Some((0, 2)));
        assert_eq!(view.rows.len(), 2);
        let blank = project(&[line("        ")], (0, 6), 4, 3, Some(0), None);
        assert_eq!(blank.cursor, Some((2, 2)));
    }

    #[test]
    fn blank_rows_and_nul_cells_survive_and_padding_has_no_fake_source() {
        let source = vec![line("x\0y"), line("    "), line("z")];
        let view = project(&source, (2, 0), 6, 3, None, None);
        assert_eq!(view.rows[0][1].ch, '\0');
        assert_eq!(view.source_map[0][1], Some((0, 1)));
        assert_eq!(view.source_map[0][5], None);
        assert_eq!(view.source_map[1][3], Some((1, 3)));
        assert_eq!(text(&view.rows), ["x y", "", "z"]);
    }

    #[test]
    fn all_viewport_shapes_have_bounded_cells_and_valid_source_coordinates() {
        let source = vec![line("abc한 xyz"), line("long body line"), line("────"), line("❯ input"), line("────")];
        for cols in 1..10 {
            for rows in 1..10 {
                let view = project(&source, (3, 5), cols, rows, Some(2), Some(5));
                assert_eq!(view.rows.len(), rows);
                for (cells, map) in view.rows.iter().zip(&view.source_map) {
                    assert_eq!(cells.len(), cols);
                    assert_eq!(map.len(), cols);
                    assert!(map.iter().flatten().all(|&(r, c)| r < source.len() && c < source[r].len()));
                }
            }
        }
        assert!(project(&source, (0, 0), 4, 0, None, None).rows.is_empty());
    }

    #[test]
    fn tall_source_shell_discards_only_unused_tail_and_repads_each_viewer() {
        let mut source = vec![line("                    "); 80];
        source[0] = line("first output        ");
        source[3] = line("$                   ");
        let original = source.clone();
        for height in [10, 100] {
            let view = project(&source, (3, 2), 20, height, None, None);
            assert_eq!(view.cursor, Some((3, 2)));
            assert_eq!(view.body_lines.len(), 4);
            assert_eq!(view.rows.len(), height);
            assert_eq!(text(&view.rows)[3], "$");
            assert_eq!(view.max_scroll, 0);
            assert!(view.source_map[4..].iter().flatten().all(Option::is_none));
        }
        assert_eq!(source, original);
    }

    #[test]
    fn cursor_above_a_full_tui_tail_is_followed_only_at_live_position() {
        let source: Vec<_> = (0..80).map(|row| line(&format!("line {row:02}"))).collect();
        let live = project(&source, (3, 2), 20, 10, None, None);
        assert_eq!(live.cursor, Some((3, 2)));
        assert_eq!(live.source_map[0][0], Some((0, 0)));
        assert_eq!(live.source_map[9][0], Some((9, 0)));
        assert_eq!(live.body_lines.len(), 80);
        assert_eq!(live.max_scroll, 70);
        let scrolled = project(&source, (3, 2), 20, 10, None, Some(1));
        assert_eq!(scrolled.cursor, None);
        assert_eq!(scrolled.source_map[0][0], Some((69, 0)));
        assert_eq!(scrolled.scroll_from_bottom, 1);
    }

    #[test]
    fn trailing_background_hidden_and_interior_blank_rows_are_content() {
        let mut source = vec![line("          "); 80];
        source[3] = line("$         ");
        source[12][0].bg = kasa_bridge::screen::Color::Rgb(1, 2, 3);
        source[18][0].hidden = true;
        let view = project(&source, (3, 2), 10, 30, None, None);
        assert_eq!(view.body_lines.len(), 19);
        assert_eq!(view.rows[12][0].bg, source[12][0].bg);
        assert!(view.rows[18][0].hidden);
        assert_eq!(view.source_map[17][0], Some((17, 0)));
        assert!(view.source_map[19..].iter().flatten().all(Option::is_none));
    }

    #[test]
    fn wheel_steps_are_continuous_after_automatic_cursor_following() {
        let source: Vec<_> = (0..80).map(|row| line(&format!("line {row:02}"))).collect();
        let live = project(&source, (40, 2), 20, 10, None, None);
        assert_eq!(live.source_map[0][0], Some((31, 0)));
        assert_eq!(live.scroll_from_bottom, 39);
        let up = project(&source, (40, 2), 20, 10, None, Some(live.scroll_from_bottom + 1));
        assert_eq!(up.source_map[0][0], Some((30, 0)));
        assert_eq!(up.scroll_from_bottom, 40);
        let down = project(&source, (40, 2), 20, 10, None, Some(live.scroll_from_bottom - 1));
        assert_eq!(down.source_map[0][0], Some((32, 0)));
        assert_eq!(down.scroll_from_bottom, 38);

        let top = project(&source, (3, 2), 20, 10, None, None);
        assert_eq!(top.scroll_from_bottom, 70);
        let up = project(&source, (3, 2), 20, 10, None, Some(top.scroll_from_bottom + 1));
        assert_eq!(up.source_map[0][0], Some((0, 0)), "top-boundary wheel must not jump down");
        let down = project(&source, (3, 2), 20, 10, None, Some(top.scroll_from_bottom - 1));
        assert_eq!(down.source_map[0][0], Some((1, 0)));
    }
}
