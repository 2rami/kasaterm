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

/// A known input border may contain a session name or teammate label. Resize
/// only its dash padding, never the label or arbitrary code/table punctuation.
/// Synthetic cells have no source position: clicking their new area must not
/// pretend that a corresponding source column exists.
fn input_border(row: &[GridCell], row_index: usize, cols: usize) -> Option<ProjectedLine> {
    let mut cells: Vec<_> = row.iter().cloned().enumerate()
        .map(|(col, cell)| (cell, Some((row_index, col)))).collect();
    let mut runs = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        if cells[start].0.ch != '─' { start += 1; continue; }
        let mut end = start + 1;
        while end < cells.len() && cells[end].0.ch == '─' { end += 1; }
        if end - start >= 3 { runs.push((start, end)); }
        start = end;
    }
    let &(start, _) = runs.iter().max_by_key(|(start, end)| end - start)?;
    if cols >= cells.len() {
        let mut fill = cells[start].0.clone();
        fill.wrapped = false;
        fill.leading_wide_spacer = false;
        cells.splice(start..start, std::iter::repeat_n((fill, None), cols - cells.len()));
    } else {
        let mut remove = cells.len() - cols;
        // Keep at least one dash per run, so labels remain delimited. If even
        // that cannot fit, preserve all text via ordinary wrapping instead.
        if runs.iter().map(|(s, e)| e - s - 1).sum::<usize>() < remove { return None; }
        for (start, end) in runs.into_iter().rev() {
            let count = remove.min(end - start - 1);
            cells.drain(start..start + count);
            remove -= count;
            if remove == 0 { break; }
        }
    }
    let (mut cells, source_map): (Vec<_>, Vec<_>) = cells.into_iter().unzip();
    for cell in &mut cells { cell.wrapped = false; }
    Some(ProjectedLine { cells, source_map })
}

fn code_or_table(row: &[GridCell]) -> bool {
    let text: String = row.iter().map(|cell| cell.ch).collect();
    let trimmed = text.trim_start();
    trimmed.starts_with(['|', '│', '┃', '║']) || trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Claude's tool-title widget indents wrapped arguments by six cells, unlike
/// ordinary answer paragraphs (two cells). Recognise the title, not arbitrary
/// indented shell/code output. The first row's short explicit newline is not a
/// wrap and stays untouched below.
fn claude_tool_title(row: &[GridCell]) -> bool {
    let text: String = row.iter().map(|cell| cell.ch).collect();
    let Some(body) = text.strip_prefix("⏺ ").or_else(|| text.strip_prefix("● ")) else { return false };
    let Some((name, _)) = body.split_once('(') else { return false };
    !name.is_empty() && name.len() <= 40 && name.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
}

fn tool_title_continuation(previous: &[GridCell], next: &[GridCell]) -> Option<(usize, bool)> {
    let lead = next.iter().take_while(|cell| matches!(cell.ch, ' ' | '\u{a0}')).count();
    (lead == 6 && previous.last().is_some_and(|cell| !matches!(cell.ch, ' ' | '\0'))
        && next.get(lead).is_some_and(|cell| !matches!(cell.ch, '⎿' | '─' | '│')))
        .then_some((lead, false))
}

fn codex_tool_title(row: &[GridCell]) -> bool {
    let text: String = row.iter().map(|cell| cell.ch).collect();
    text.starts_with("• Ran ") || text.starts_with("• Running ")
}

/// Codex wraps command titles two cells before the terminal edge and prefixes
/// each continuation with `  │ `. This is a command widget, not a table. Keep
/// short explicit command newlines and the collapsed-output marker separate.
fn codex_tool_continuation(previous: &[GridCell], next: &[GridCell]) -> Option<(usize, bool)> {
    if !next.iter().take(4).map(|cell| cell.ch).eq("  │ ".chars()) { return None; }
    let previous_text: String = previous.iter().filter(|cell| cell.ch != '\0').map(|cell| cell.ch).collect();
    let next_text: String = next.iter().skip(4).filter(|cell| cell.ch != '\0').map(|cell| cell.ch).collect();
    if next_text.trim_start().starts_with('…') { return None; }
    let first_word = next_text.split_whitespace().next()?;
    let usable = previous.len().saturating_sub(2);
    let occupied = previous.iter().rposition(|cell| !matches!(cell.ch, ' ' | '\0'))? + 1;
    let first_width: usize = first_word.chars().map(|ch| ch.width().unwrap_or(1)).sum();
    if occupied > usable || occupied + 12 < usable || occupied + 1 + first_width <= usable { return None; }
    let previous_word = previous_text.split_whitespace().next_back()?;
    let token_char = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | ':' | '-' | '@' | '~');
    let token_split = occupied == usable && previous_word.chars().all(token_char) && first_word.chars().all(token_char)
        && previous_word.chars().chain(first_word.chars()).any(|ch| matches!(ch, '_' | '.' | '/' | ':' | '-'));
    Some((4, !token_split))
}

fn project_region(source: &[Vec<GridCell>], start: usize, end: usize, cursor: SourcePos, cols: usize, input_borders: &[usize], prose: bool) -> Vec<ProjectedLine> {
    let mut output = Vec::new();
    let mut row_index = start;
    let mut fence: Option<char> = None;
    while row_index < end {
        let row_text: String = source[row_index].iter().map(|cell| cell.ch).collect();
        let fence_marker = if row_text.trim_start().starts_with("```") { Some('`') }
            else if row_text.trim_start().starts_with("~~~") { Some('~') } else { None };
        let fenced = fence.is_some() || fence_marker.is_some();
        if let Some(marker) = fence_marker {
            if fence == Some(marker) { fence = None; } else if fence.is_none() { fence = Some(marker); }
        }
        if input_borders.contains(&row_index) {
            if let Some(border) = input_border(&source[row_index], row_index, cols) {
                output.push(border);
                row_index += 1;
                continue;
            }
        }
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
        let tool_title = claude_tool_title(&source[first_row]);
        let codex_title = codex_tool_title(&source[first_row]);
        let indented_code = source[first_row].iter().take_while(|cell| cell.ch == ' ').count() >= 4;
        let mut logical: Vec<(GridCell, Option<SourcePos>)> = Vec::new();
        let mut omit_leading = 0;
        let fill;
        loop {
            let row = &source[row_index];
            let soft_wrap = row.last().is_some_and(|c| c.wrapped)
                && row_index + 1 < end && rule_cell(&source[row_index + 1]).is_none();
            // Claude/Codex draw word-wrapped paragraphs using hard cursor moves.
            // Reuse the mobile continuation rule only inside a recognised agent
            // body. Explicit input newlines, code fences/indentation and tables
            // retain their hard breaks; ordinary shell grids stay strict VT.
            let paragraph = (!soft_wrap && prose && !fenced && !indented_code && row.len() != cols
                && row_index + 1 < end
                && (codex_title || (!code_or_table(row) && !code_or_table(&source[row_index + 1]))))
                .then(|| {
                    let next = &source[row_index + 1];
                    if codex_title { return codex_tool_continuation(row, next); }
                    (tool_title.then(|| tool_title_continuation(row, next)).flatten())
                        .or_else(|| kasa_bridge::reflow::paragraph_continuation(row, next, row.len()))
                })
                .flatten();
            let joins_next = soft_wrap || paragraph.is_some();
            // Trailing terminal padding is not another paragraph. Keep interior
            // spaces, all soft-wrapped cells, and the actual cursor's blank cell.
            let tail = row.last();
            let significant = row.iter().rposition(|c| !matches!(c.ch, ' ' | '\0') || c.hidden
                || tail.is_some_and(|fill| c.bg != fill.bg || c.inverse != fill.inverse
                    || c.underline != fill.underline)).map_or(0, |i| i + 1);
            let cursor_end = if cursor.0 == row_index { cursor.1.saturating_add(1).min(row.len()) } else { 0 };
            let mut length = if soft_wrap { row.len() } else { significant.max(cursor_end) };
            // A wide glyph's spacer may itself be trailing whitespace.
            if length > 0 && row[length - 1].ch.width().unwrap_or(1) == 2 && length < row.len() { length += 1; }
            logical.extend(row[..length].iter().cloned().enumerate().skip(omit_leading).filter(|(_, cell)| !cell.leading_wide_spacer).map(|(col, mut cell)| {
                cell.wrapped = false;
                (cell, Some((row_index, col)))
            }));
            omit_leading = paragraph.map_or(0, |(skip, _)| skip);
            if paragraph.is_some_and(|(_, separator)| separator) {
                // The separator belongs to the same rendered paragraph. Losing
                // its fill breaks uniform user-prompt bands after a mirror join.
                let mut separator = logical.last().map(|(cell, _)| cell.clone()).unwrap_or_else(GridCell::blank);
                separator.ch = ' ';
                separator.wrapped = false;
                separator.leading_wide_spacer = false;
                logical.push((separator, None));
            }
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
            line.source_map.push(*pos);
            index += 1;
            if wide {
                // The wire grid uses either a space or NUL for the wide spacer.
                // Consume it only when adjacent in the original source row.
                let spacer = logical.get(index).filter(|(next, next_pos)|
                    pos.is_some_and(|pos| *next_pos == Some((pos.0, pos.1 + 1))) && matches!(next.ch, ' ' | '\0'));
                if width == 2 {
                    let (mut spacer_cell, spacer_pos) = spacer.map(|(c, p)| (c.clone(), *p))
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
    let recognised_prompt = crate::screenread::prompt_box(source);
    let agent_body = recognised_prompt.is_some() && input_top.is_some();
    let input_borders = match recognised_prompt {
        Some(crate::screenread::PromptBox::Bordered { top, bottom, .. }) if input_top == Some(top) => vec![top, bottom],
        _ => Vec::new(),
    };
    let body_end = input_top.unwrap_or(source_end);
    let body_lines = project_region(source, 0, body_end, source_cursor, cols, &input_borders, agent_body);
    let pinned = project_region(source, body_end, source_end, source_cursor, cols, &input_borders, false);
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

    fn agent_screen(body: &[&str], cols: usize, label: bool) -> Vec<Vec<GridCell>> {
        let mut source: Vec<_> = body.iter().map(|text| {
            let mut row = Vec::new();
            for ch in text.chars() {
                row.push(GridCell { ch, ..GridCell::blank() });
                if ch.width() == Some(2) { row.push(GridCell { ch: '\0', ..GridCell::blank() }); }
            }
            row.resize(cols, GridCell::blank()); row
        }).collect();
        source.push(line(&if label { format!("{} Session {}", "─".repeat(12), "─".repeat(cols - 21)) } else { "─".repeat(cols) }));
        let mut prompt = line("❯ ready"); prompt.resize(cols, GridCell::blank()); source.push(prompt);
        source.push(line(&"─".repeat(cols)));
        source
    }

    fn codex_screen(body: &[&str], cols: usize) -> Vec<Vec<GridCell>> {
        let mut source = agent_screen(body, cols, false);
        source.truncate(body.len());
        for content in ["", "› ready", ""] {
            let mut row = line(content); row.resize(cols, GridCell::blank());
            for cell in &mut row { cell.bg = kasa_bridge::screen::Color::Rgb(63, 69, 77); }
            source.push(row);
        }
        source
    }

    #[test]
    fn observed_codex_command_wrappers_reflow_without_joining_collapsed_output() {
        let source = codex_screen(&[
            "• Ran curl --max-time 3 -fsS http://127.0.0.1:18986/version;",
            "  │ curl -fsS http://127.0.0.1:8765/machines | jq '.machines",
            "  │ | map({label,base,online,ssh})'; git status --short;",
            "  │ … +1 lines",
        ], 62);
        let original = source.clone();
        let view = project(&source, (5, 3), 200, 10, Some(4), None);
        assert_eq!(view.body_lines.len(), 2);
        let output = text(&view.rows).join("\n");
        assert!(output.contains("/version; curl -fsS"));
        assert!(output.contains("'.machines | map("));
        assert!(output.contains("  │ … +1 lines"));
        assert_eq!(view.body_lines[0].source_map[61], Some((1, 4)));
        assert_eq!(source, original);

        let source = codex_screen(&["• Ran cat <<'EOF'", "  │ explicit command newline", "  │ EOF"], 62);
        let view = project(&source, (4, 3), 200, 10, Some(3), None);
        assert_eq!(view.body_lines.len(), 3, "short explicit command lines stay separate");
    }

    #[test]
    fn tall_wide_mirror_uses_history_before_padding_and_scrolls_continuously() {
        let body: Vec<String> = (0..36).map(|i| format!("live event {i:02}")).collect();
        let live = codex_screen(&body.iter().map(String::as_str).collect::<Vec<_>>(), 62);
        let mut history: Vec<_> = (0..120).map(|i| {
            let mut row = line(&format!("earlier event {i:03}")); row.resize(62, GridCell::blank()); row
        }).collect();
        history.extend_from_slice(&live);
        let original = history.clone();
        let view = project_history(&history, &live, (37, 3), 120, 200, 90, None);
        assert!(view.rows[..87].iter().all(|row| row.iter().any(|cell| cell.ch != ' ')), "available history must fill the tall viewer");
        assert!(text(&view.rows)[0].starts_with("earlier event"));
        assert_eq!(view.cursor, Some((88, 3)));
        assert!(view.source_map[0].iter().all(Option::is_none), "history is not a source mouse target");
        assert_eq!(view.source_map[86][0], Some((35, 0)));
        let older = project_history(&history, &live, (37, 3), 120, 200, 90, Some(1));
        assert_eq!(text(&older.rows)[1], text(&view.rows)[0]);
        assert_eq!(older.cursor, view.cursor);
        assert_eq!(text(&older.rows)[87..], text(&view.rows)[87..]);
        assert_eq!(history, original, "history projection never changes the source");
    }

    #[test]
    fn wrapped_input_preserves_explicit_newlines_and_source_cursor_mapping() {
        let mut source = codex_screen(&[], 20);
        source[1] = line("› a wrapped input te");
        source[1].resize(20, GridCell::blank());
        source[1][19].wrapped = true;
        let fill = source[0][0].bg.clone();
        let mut continuation = line("xt"); continuation.resize(20, GridCell::blank());
        let mut explicit = line("next explicit line"); explicit.resize(20, GridCell::blank());
        source.insert(2, continuation);
        source.insert(3, explicit);
        for row in &mut source { for cell in row { cell.bg = fill.clone(); } }
        let view = project(&source, (3, 7), 60, 10, Some(0), None);
        let output = text(&view.rows);
        assert!(output.iter().any(|row| row == "› a wrapped input text"));
        assert!(output.iter().any(|row| row == "next explicit line"));
        let (row, col) = view.cursor.unwrap();
        assert_eq!(view.source_map[row][col], Some((3, 7)));
        assert_eq!(view.rows[row][col].ch, source[3][7].ch);
    }

    #[test]
    fn codex_branch_paragraph_uses_mobile_hanging_indent() {
        let source = codex_screen(&["  └ word word word word word word word word word word", "    continued end"], 60);
        let view = project(&source, (3, 3), 100, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 1);
        assert!(text(&view.rows)[4].ends_with("continued end"));
        for (col, pos) in view.body_lines[0].source_map.iter().enumerate() {
            if let Some((row, source_col)) = pos { assert_eq!(view.body_lines[0].cells[col].ch, source[*row][*source_col].ch); }
        }
    }

    #[test]
    fn short_old_rows_need_canonical_history_resync_not_guessed_joins() {
        // These may be stale 26-column wraps after the source widens to 93,
        // or deliberate newlines. Projection cannot distinguish them: the
        // transport must replace history with the source's canonical rows.
        let source = codex_screen(&["  └ /tmp/mirror-fixture-", "  source/example/", "  native.png"], 93);
        let view = project(&source, (4, 3), 200, 9, Some(3), None);
        assert_eq!(view.body_lines.len(), 3);

        let mut records = codex_screen(&["  └ Search some text", "    Read another file"], 93);
        for cell in &mut records[0][4..10] { cell.fg = kasa_bridge::screen::Color::Idx(6); }
        for cell in &mut records[1][4..8] { cell.fg = kasa_bridge::screen::Color::Idx(6); }
        let view = project(&records, (3, 3), 200, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 2, "the next coloured action keyword starts its own record");

        let code = codex_screen(&["  └ let value = 1;", "  return value;"], 93);
        let view = project(&code, (3, 3), 200, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 2, "explicit code rows must retain newlines");
    }

    #[test]
    fn source_width_changes_keep_agent_paragraph_at_viewer_width() {
        let narrow_body = ["  - word word word word word word word word word word", "    continued end"];
        let wide_body = ["  - word word word word word word word word word word continued end"];
        let narrow = agent_screen(&narrow_body, 60, true);
        let wide = agent_screen(&wide_body, 100, true);
        let before = project(&wide, (2, 3), 80, 8, Some(1), None);
        let after = project(&narrow, (3, 3), 80, 8, Some(2), None);
        assert_eq!(text(&before.rows), text(&after.rows), "source resize changed the viewer's paragraph or input width");
        assert_eq!(after.cursor, before.cursor);
        assert_eq!(after.source_map[4][54], Some((1, 4)), "continued text must still map to the source row");
        assert!(after.rows.iter().all(|row| row.len() == 80));
    }

    #[test]
    fn labelled_input_border_resizes_without_wrapping_or_losing_label() {
        let source = agent_screen(&["body"], 60, true);
        for cols in [30, 90] {
            let view = project(&source, (2, 3), cols, 8, Some(1), None);
            assert_eq!(view.rows.len(), 8);
            assert!(text(&view.rows)[5].contains(" Session "));
            assert!(view.rows[5].iter().all(|cell| !cell.wrapped));
            assert_eq!(view.rows[5].len(), cols);
            assert_eq!(view.cursor, Some((6, 3)));
            for (col, pos) in view.source_map[5].iter().enumerate() {
                if let Some((row, source_col)) = pos { assert_eq!(view.rows[5][col].ch, source[*row][*source_col].ch); }
            }
        }
    }

    #[test]
    fn mobile_paragraph_rule_preserves_code_tables_and_separate_bullets() {
        for body in [
            vec!["    word word word word word word word word word word", "    continued end"],
            vec!["```", "word word word word word word word word word word word", "continued end", "```"],
            vec!["| word word word word word word word word word word |", "| continued end |"],
            vec!["  - word word word word word word word word word word", "  - continued end"],
        ] {
            let source = agent_screen(&body, 60, false);
            let view = project(&source, (body.len() + 1, 3), 100, 20, Some(body.len()), None);
            assert_eq!(view.body_lines.len(), body.len(), "code/table/list hard break was merged: {body:?}");
        }
    }

    #[test]
    fn arbitrary_shell_and_explicit_multiline_input_keep_hard_breaks() {
        let body = ["  - word word word word word word word word word word", "    continued end"];
        let source = agent_screen(&body, 60, false);
        let strict = project(&source[..2], (0, 0), 100, 6, None, None);
        assert_eq!(strict.body_lines.len(), 2);
        let mut input = agent_screen(&[], 60, false);
        input.insert(2, line("    an explicit next input line"));
        let view = project(&input, (1, 3), 100, 8, Some(0), None);
        assert!(text(&view.rows).iter().any(|row| row == "    an explicit next input line"));
    }

    #[test]
    fn mobile_join_keeps_cjk_cells_and_split_paths_mapped_to_the_source() {
        let source = agent_screen(&[
            "- 경로는 /Users/kasa/Desktop/momewomo/kasaterm/crates/kasa-b",
            "ridge/src/reflow.rs 이다.",
        ], 60, false);
        let view = project(&source, (3, 3), 100, 8, Some(2), None);
        let body = &view.body_lines[0];
        assert_eq!(view.body_lines.len(), 1);
        let output: String = body.cells.iter().filter(|cell| cell.ch != '\0').map(|cell| cell.ch).collect();
        assert!(output.contains("경로는 /Users/kasa/Desktop/momewomo/kasaterm/crates/kasa-bridge/src/reflow.rs 이다."));
        for (col, pos) in body.source_map.iter().enumerate() {
            if let Some((row, source_col)) = pos { assert_eq!(body.cells[col].ch, source[*row][*source_col].ch); }
        }
    }

    #[test]
    fn codex_body_uses_mobile_reflow_and_filled_input_uses_viewer_width() {
        let mut source = agent_screen(&[
            "  - word word word word word word word word word word",
            "    continued end",
        ], 60, false);
        source.truncate(2);
        let fill = kasa_bridge::screen::Color::Rgb(63, 69, 77);
        for content in ["", "› ready", ""] {
            let mut row = line(content); row.resize(60, GridCell::blank());
            for cell in &mut row { cell.bg = fill.clone(); }
            source.push(row);
        }
        let view = project(&source, (3, 3), 100, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 1);
        assert!(text(&view.rows)[4].ends_with("word continued end"));
        assert!(view.rows[5..].iter().all(|row| row.len() == 100 && row.iter().all(|cell| cell.bg == fill)));
        assert_eq!(view.cursor, Some((6, 3)));
    }

    #[test]
    fn observed_claude_full_width_tool_title_joins_its_six_cell_continuation() {
        // Anonymised shape of the read-only 96-column production capture:
        // a Bash title reaches the last source column in the middle of CSS;
        // Claude paints the remainder six columns in on a new hard row.
        let prefix = "⏺ Bash(sed -i 's|";
        let suffix = "position:relative";
        let first = format!("{prefix}{}{suffix}", "x".repeat(96 - prefix.chars().count() - suffix.len()));
        let source = agent_screen(&[&first, "      ;height:480px;|' page.html)"], 96, false);
        let view = project(&source, (3, 3), 180, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 1);
        assert!(text(&view.rows)[4].contains("position:relative;height:480px;|' page.html)"));
        assert_eq!(view.body_lines[0].source_map[96], Some((1, 6)));
    }

    #[test]
    fn short_tool_title_newline_is_not_guessed_to_be_a_wrap() {
        let source = agent_screen(&["⏺ Bash(python3 - <<'EOF'", "      print('explicit command newline'))"], 96, false);
        let view = project(&source, (3, 3), 180, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 2);
    }

    #[test]
    fn observed_result_bullet_nbsp_padding_uses_mobile_paragraph_reflow() {
        let first = format!("  ⎿ \u{a0}{}", ["word"; 10].join(" "));
        let source = agent_screen(&[&first, "     continued result"], 60, false);
        let view = project(&source, (3, 3), 100, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 1);
        assert!(text(&view.rows)[4].ends_with("word continued result"));
        assert_eq!(view.body_lines[0].source_map[55], Some((1, 5)));
    }

    #[test]
    fn joined_past_user_prompt_keeps_a_uniform_coloured_band() {
        let first = format!("❯ {}", ["word"; 10].join(" "));
        let mut source = agent_screen(&[&first, "  continued prompt"], 60, false);
        let fill = kasa_bridge::screen::Color::Rgb(240, 225, 235);
        for row in &mut source[..2] {
            for cell in row { cell.bg = fill.clone(); cell.fg = kasa_bridge::screen::Color::Rgb(50, 40, 45); }
        }
        let view = project(&source, (3, 3), 100, 8, Some(2), None);
        assert_eq!(view.body_lines.len(), 1);
        let body = &view.body_lines[0];
        assert!(body.cells.iter().all(|cell| cell.bg == fill), "a default-colour separator split the prompt band");
        let separator = first.chars().count();
        assert_eq!(body.source_map[separator], None);
        assert_eq!(body.cells[separator].ch, ' ');
        assert_eq!(body.cells[separator].fg, source[0][0].fg);
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
