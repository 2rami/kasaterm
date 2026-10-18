//! 그리기. 색은 ANSI 16색만 쓴다 — 카사텀 테마 팔레트가 그대로 입혀진다.

use crate::preview::{size_label, Preview};
use crate::App;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 이 폭부터 오른쪽에 미리보기를 붙인다.
const PREVIEW_MIN_COLS: u16 = 100;

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn mark_style(mark: char) -> Style {
    match mark {
        'A' => Style::new().fg(Color::Green),
        'D' | 'U' => Style::new().fg(Color::Red),
        'R' => Style::new().fg(Color::Cyan),
        '?' => Style::new().fg(Color::Magenta),
        _ => Style::new().fg(Color::Yellow),
    }
}

/// 폭에 맞게 자르고 끝에 … 을 단다(한글은 두 칸).
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// 뿌리 줄 끝에 붙는 저장소 요약 — 브랜치·앞뒤 커밋·더한 줄과 지운 줄.
fn repo_summary(app: &App) -> Vec<Span<'static>> {
    let Some(git) = &app.git else { return Vec::new() };
    let mut spans = vec![Span::styled(format!("  {}", git.branch.as_deref().unwrap_or("(분리됨)")), Style::new().fg(Color::Cyan))];
    if git.ahead > 0 {
        spans.push(Span::styled(format!(" ↑{}", git.ahead), dim()));
    }
    if git.behind > 0 {
        spans.push(Span::styled(format!(" ↓{}", git.behind), dim()));
    }
    if git.added > 0 {
        spans.push(Span::styled(format!(" +{}", git.added), Style::new().fg(Color::Green)));
    }
    if git.deleted > 0 {
        spans.push(Span::styled(format!(" -{}", git.deleted), Style::new().fg(Color::Red)));
    }
    spans
}

fn tree_lines(app: &mut App, area: Rect) -> Vec<Line<'static>> {
    let height = area.height as usize;
    if app.tree.selected < app.offset {
        app.offset = app.tree.selected;
    } else if height > 0 && app.tree.selected >= app.offset + height {
        app.offset = app.tree.selected + 1 - height;
    }
    app.offset = app.offset.min(app.tree.rows.len().saturating_sub(height.max(1)));
    app.tree_area = (area.y, area.height);
    let width = area.width as usize;
    let shell = app.shell_cwd();
    app.tree
        .rows
        .iter()
        .enumerate()
        .skip(app.offset)
        .take(height)
        .map(|(i, row)| {
            let lead = format!("{}{}", "  ".repeat(row.depth), if row.dir { if row.open { "▾ " } else { "▸ " } } else { "· " });
            let mark = app.git.as_ref().filter(|_| row.depth > 0).and_then(|git| git.mark(&row.path));
            let tail = if row.depth == 0 { repo_summary(app) } else { Vec::new() };
            let tail_w: usize = tail.iter().map(|span| span.content.width()).sum::<usize>() + if mark.is_some() { 2 } else { 0 };
            let name = fit(&row.name, width.saturating_sub(lead.width() + tail_w));
            let pad = width.saturating_sub(lead.width() + name.width() + tail_w);
            let mut name_style = match (row.depth, row.dir) {
                (0, _) => Style::new().add_modifier(Modifier::BOLD),
                (_, true) => Style::new().fg(Color::Blue),
                _ => Style::new(),
            };
            if shell == Some(row.path.as_path()) {
                name_style = name_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            let mut spans = vec![Span::styled(lead, dim()), Span::styled(name, name_style)];
            spans.extend(tail);
            spans.push(Span::raw(" ".repeat(pad)));
            if let Some(mark) = mark {
                spans.push(Span::styled(format!(" {mark}"), mark_style(mark)));
            }
            let line = Line::from(spans);
            if i == app.tree.selected {
                line.patch_style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect()
}

fn preview_lines(app: &App, area: Rect) -> Vec<Line<'static>> {
    let Some((path, preview)) = &app.preview else { return Vec::new() };
    let name = path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let mut lines = vec![Line::styled(fit(&name, area.width as usize), dim()), Line::raw("")];
    match preview {
        Preview::Text(text) => {
            let gutter = text.len().to_string().len();
            lines.extend(text.iter().enumerate().map(|(i, line)| {
                Line::from(vec![Span::styled(format!("{:>gutter$} ", i + 1), dim()), Span::raw(line.clone())])
            }));
        }
        Preview::Dir(names) if names.is_empty() => lines.push(Line::styled("빈 폴더", dim())),
        Preview::Dir(names) => lines.extend(names.iter().map(|name| {
            if name.ends_with('/') {
                Line::styled(name.clone(), Style::new().fg(Color::Blue))
            } else {
                Line::raw(name.clone())
            }
        })),
        Preview::Opaque { kind, size } => {
            lines.push(Line::raw(format!("{kind} · {}", size_label(*size))));
            lines.push(Line::styled("Enter 로 카사텀에서 열어요", dim()));
        }
        Preview::Error(error) => lines.push(Line::styled(error.clone(), Style::new().fg(Color::Red))),
    }
    lines
}

fn footer(app: &App) -> Line<'static> {
    if app.filtering || !app.tree.filter.is_empty() {
        let hint = if app.filtering { "▏  Enter 확정 · Esc 지우기" } else { "  Esc 지우기" };
        return Line::from(vec![
            Span::styled("/ ", dim()),
            Span::raw(app.tree.filter.clone()),
            Span::styled(hint, dim()),
        ]);
    }
    if let Some(note) = app.live_note() {
        return Line::styled(note.to_string(), Style::new().fg(Color::Yellow));
    }
    let lead = if app.following { "Enter 셸 이동·열기" } else { "Enter 펴기·열기" };
    Line::styled(format!("{lead}  ←→ 접기·펴기  / 거르기  . 숨김  p 미리보기  q 끝"), dim())
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let split = app.show_preview && body.width >= PREVIEW_MIN_COLS;
    let (list, side) = if split {
        let [list, side] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body);
        (list, Some(side))
    } else {
        (body, None)
    };
    let lines = tree_lines(app, list);
    frame.render_widget(Paragraph::new(lines), list);
    if let Some(side) = side {
        let block = Block::new().borders(Borders::LEFT).border_style(dim());
        let inner = block.inner(side);
        frame.render_widget(block, side);
        let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(1), ..inner };
        frame.render_widget(Paragraph::new(preview_lines(app, inner)), inner);
    }
    app.note_drawn = app.live_note().is_some();
    frame.render_widget(Paragraph::new(footer(app)), foot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_counts_wide_letters_and_marks_the_cut() {
        assert_eq!(fit("main.rs", 10), "main.rs");
        assert_eq!(fit("handler.rs", 6), "handl…");
        assert_eq!(fit("한글파일.md", 7), "한글파…");
        assert_eq!(fit("한글파일.md", 6), "한글…");
    }
}
