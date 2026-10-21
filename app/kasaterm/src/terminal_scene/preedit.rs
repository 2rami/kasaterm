use crate::{screenread, GridCell};
use kasa_bridge::screen::Color;
use unicode_width::UnicodeWidthChar;

pub(crate) struct Placement {
    pub(crate) cursor: (usize, usize),
    pub(crate) spans: Vec<(usize, usize, usize)>,
    pub(super) source_map: Vec<Vec<Option<(usize, usize)>>>,
}

// The PTY has not received these glyphs yet. Only the disposable display grid
// may make room for them; writing spaces to the PTY would change the draft.
pub(super) fn insert(
    rows: &mut Vec<Vec<GridCell>>,
    cursor: (usize, usize),
    text: &str,
    accent: [u8; 4],
) -> Option<Placement> {
    if text.is_empty() {
        return None;
    }
    let prompt = screenread::prompt_box(rows)?;
    let input = prompt.rows();
    let (r, c) = cursor;
    if !input.contains(&r) {
        return None;
    }
    let row = rows.get(r)?;
    let cols = row.len();
    let marker = input.clone().find_map(|r| {
        let col = rows[r]
            .iter()
            .position(|c| !screenread::is_blank_glyph(c.ch))?;
        matches!(rows[r][col].ch, '❯' | '›').then_some((r, col))
    })?;
    let left = marker.1 + 2;
    if c < left || c >= cols || cols < left + 2 {
        return None;
    }
    let top = match prompt {
        screenread::PromptBox::Bordered { top, .. } => top,
        screenread::PromptBox::Filled { .. } => input.start,
    };
    let fill = GridCell {
        bg: row[c].bg.clone(),
        ..GridCell::blank()
    };
    let mut line = row[..c].to_vec();
    let mut origins: Vec<_> = (0..c).map(|col| Some((r, col))).collect();
    let start = line.len();
    for ch in text.chars() {
        let width = ch.width()?.clamp(1, 2);
        let cell = GridCell {
            ch,
            fg: Color::Rgb(accent[0], accent[1], accent[2]),
            underline: true,
            ..fill.clone()
        };
        line.push(cell.clone());
        if width == 2 {
            line.push(GridCell { ch: '\0', ..cell });
        }
    }
    let inserted_end = line.len();
    origins.resize(inserted_end, None);
    let end = row
        .iter()
        .rposition(|cell| !screenread::is_blank_glyph(cell.ch))
        .map_or(c, |i| (i + row[i].ch.width().unwrap_or(1).max(1)).min(cols))
        .max(c);
    // A dim placeholder is a suggestion, not text waiting after the caret.
    let placeholder = c == left
        && r == marker.0
        && row[c..end]
            .iter()
            .filter(|cell| !screenread::is_blank_glyph(cell.ch))
            .all(|cell| cell.dim);
    if !placeholder {
        line.extend_from_slice(&row[c..end]);
        origins.extend((c..end).map(|col| Some((r, col))));
    }
    let mut wrapped = vec![Vec::new()];
    let mut wrapped_map = vec![Vec::new()];
    let mut at = 0;
    let mut anchor = None;
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    while at < line.len() {
        let width = line[at].ch.width().unwrap_or(1).clamp(1, 2);
        if wrapped.last()?.len() + width > cols {
            wrapped.last_mut()?.resize(cols, fill.clone());
            wrapped.push(vec![fill.clone(); left]);
            wrapped_map.last_mut()?.resize(cols, None);
            wrapped_map.push(vec![None; left]);
        }
        if at == start {
            anchor = Some((wrapped.len() - 1, wrapped.last()?.len()));
        }
        if (start..inserted_end).contains(&at) {
            let (r, c) = (wrapped.len() - 1, wrapped.last()?.len());
            match spans.last_mut() {
                Some((row, _, end)) if *row == r => *end = c + width,
                _ => spans.push((r, c, c + width)),
            }
        }
        wrapped.last_mut()?.push(line[at].clone());
        wrapped_map.last_mut()?.push(origins[at]);
        if width == 2 {
            wrapped.last_mut()?.push(GridCell {
                ch: '\0',
                ..line[at].clone()
            });
            wrapped_map
                .last_mut()?
                .push(origins.get(at + 1).copied().flatten());
        }
        at += width;
    }
    for row in &mut wrapped {
        row.resize(cols, fill.clone());
    }
    for row in &mut wrapped_map {
        row.resize(cols, None);
    }
    let extra = wrapped.len() - 1;
    // Keep the footer and lower border stationary. Codex's empty bottom pad
    // can absorb a wrap; otherwise borrow rows immediately above the prompt.
    let pads = (r + 1..input.end)
        .rev()
        .take_while(|&i| rows[i].iter().all(|c| screenread::is_blank_glyph(c.ch)))
        .count()
        .min(extra);
    let rise = extra - pads;
    if rise > top {
        return None;
    }
    let (ar, ac) = anchor?;
    let mut source_map: Vec<Vec<_>> = rows
        .iter()
        .enumerate()
        .map(|(r, row)| (0..row.len()).map(|c| Some((r, c))).collect())
        .collect();
    rows.drain(input.end - pads..input.end);
    source_map.drain(input.end - pads..input.end);
    rows.splice(r..=r, wrapped);
    source_map.splice(r..=r, wrapped_map);
    rows.drain(top - rise..top);
    source_map.drain(top - rise..top);
    Some(Placement {
        cursor: (r + ar - rise, ac),
        spans: spans
            .into_iter()
            .map(|(row, start, end)| (r + row - rise, start, end))
            .collect(),
        source_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, cols: usize) -> Vec<GridCell> {
        let mut out = Vec::new();
        for ch in text.chars() {
            out.push(GridCell {
                ch,
                ..GridCell::blank()
            });
            if ch.width() == Some(2) {
                out.push(GridCell {
                    ch: '\0',
                    ..GridCell::blank()
                });
            }
        }
        out.resize(cols, GridCell::blank());
        out
    }

    fn fixture(codex: bool, text: &str, cols: usize) -> Vec<Vec<GridCell>> {
        let mut rows = vec![
            row("history", cols),
            row("", cols),
            row(&"─".repeat(cols), cols),
            row(&format!("{} {text}", if codex { '›' } else { '❯' }), cols),
            row(&"─".repeat(cols), cols),
            row("STATUS", cols),
        ];
        if codex {
            rows[2] = row("", cols);
            rows[4] = row("", cols);
            for row in &mut rows[2..5] {
                for c in row {
                    c.bg = Color::Rgb(40, 45, 55);
                }
            }
        }
        rows
    }

    fn text(rows: &[Vec<GridCell>]) -> String {
        rows.iter()
            .map(|row| {
                row.iter()
                    .filter(|c| c.ch != '\0')
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn middle_composition_keeps_korean_and_ascii_suffixes() {
        for codex in [false, true] {
            for preedit in ["ㄷ", "도", "되", "돼", "되ㄱ"] {
                let mut rows = fixture(codex, "앞뒤문장 ABC", 32);
                let placed = insert(&mut rows, (3, 4), preedit, [1, 2, 3, 255]).unwrap();
                assert_eq!(placed.cursor, (3, 4));
                assert_eq!(
                    placed.spans,
                    vec![(
                        3,
                        4,
                        4 + preedit.chars().map(|c| c.width().unwrap()).sum::<usize>()
                    )]
                );
                assert!(text(&rows).contains(&format!("앞{preedit}뒤문장 ABC")));
                assert!(rows[3][4].underline);
            }
        }
    }

    #[test]
    fn wrapping_keeps_wide_glyphs_borders_and_footer() {
        for codex in [false, true] {
            let mut rows = fixture(codex, "가나다라마바사아자", 20);
            let footer = rows[5].clone();
            let top = rows[2].clone();
            assert!(insert(&mut rows, (3, 18), "되", [1, 2, 3, 255]).is_some());
            assert_eq!(rows.len(), 6);
            assert_eq!(rows[5], footer);
            assert!(text(&rows).contains("가나다라마바사아되\n  자"));
            if !codex {
                assert_eq!(rows[1], top);
                assert_eq!(rows[4], top);
            }
            for row in rows {
                for (c, cell) in row.iter().enumerate() {
                    if cell.ch.width() == Some(2) {
                        assert_eq!(row.get(c + 1).map(|c| c.ch), Some('\0'));
                    }
                }
            }
        }
    }

    #[test]
    fn continuation_row_insertion_preserves_other_input_lines() {
        let mut rows = fixture(false, "첫줄", 20);
        rows.insert(4, row("  abcDEF", 20));
        assert_eq!(
            insert(&mut rows, (4, 5), "되", [1, 2, 3, 255])
                .unwrap()
                .cursor,
            (4, 5)
        );
        assert!(text(&rows).contains("❯ 첫줄\n  abc되DEF"));
    }

    #[test]
    fn cancellation_is_a_fresh_projection_not_a_terminal_edit() {
        let source = fixture(false, "앞뒤", 20);
        let mut view = source.clone();
        insert(&mut view, (3, 4), "되", [1, 2, 3, 255]);
        assert_ne!(view, source);
        let mut next = source.clone();
        assert!(insert(&mut next, (3, 4), "", [1, 2, 3, 255]).is_none());
        assert_eq!(next, source);
        assert!(insert(&mut next, (0, 2), "되", [1, 2, 3, 255]).is_none());
        assert_eq!(next, source);
    }

    #[test]
    fn inserted_spans_wrap_and_source_cells_keep_their_mapping() {
        let mut rows = fixture(false, "abcdefghijklmnopqr", 20);
        let source = rows.clone();
        let placed = insert(&mut rows, (3, 18), "되ㄱ", [1, 2, 3, 255]).unwrap();
        assert_eq!(placed.spans, vec![(2, 18, 20), (3, 2, 4)]);
        assert!(text(&rows).contains("abcdefghijklmnop되\n  ㄱqr"));
        for (r, map) in placed.source_map.iter().enumerate() {
            for (c, at) in map.iter().enumerate() {
                if let Some((sr, sc)) = at {
                    assert_eq!(rows[r][c], source[*sr][*sc]);
                }
            }
        }
    }

    #[test]
    fn placeholder_is_not_inserted_into_the_draft_preview() {
        let mut rows = fixture(false, "Try this suggestion", 32);
        for c in &mut rows[3][2..] {
            c.dim = true;
        }
        insert(&mut rows, (3, 2), "되", [1, 2, 3, 255]).unwrap();
        assert_eq!(text(&rows[3..4]), "❯ 되");
    }

    #[test]
    fn foreground_protection_survives_prompt_growth() {
        let mut rows = fixture(false, "abcdefghijklmnopqr", 20);
        rows.splice(
            0..2,
            [
                row("╭────────╮", 20),
                row("│ notice │", 20),
                row("╰────────╯", 20),
            ],
        );
        let source = screenread::TuiForeground::capture(&rows);
        let placed = insert(&mut rows, (4, 18), "되", [1, 2, 3, 255]).unwrap();
        let foreground = source.project(&rows, &placed.source_map);
        assert_eq!(foreground.prefix_end(0, 20), 0);
        assert_eq!(foreground.prefix_end(1, 20), 0);
        assert_eq!(
            screenread::TuiForeground::capture(&rows).prefix_end(1, 20),
            20
        );
    }
}
