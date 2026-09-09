//! Viewer-local Codex diff presentation. Never rewrite the source terminal.
use crate::GridCell;
use kasa_bridge::screen::Color;

/// Numbered Codex patch gutter; ordinary code, shell output and tables do not
/// have this shape. Returns the first content column after the +/- marker.
pub(crate) fn gutter(row: &[GridCell]) -> Option<usize> {
    let mut i = row.iter().take_while(|c| c.ch == ' ').count();
    let start = i;
    while row.get(i).is_some_and(|c| c.ch.is_ascii_digit()) { i += 1; }
    if i == start || i - start > 8 { return None; }
    while row.get(i).is_some_and(|c| c.ch == ' ') { i += 1; }
    if !row.get(i).is_some_and(|c| matches!(c.ch, '+' | '-')) { return None; }
    i += 1;
    // Spaces after the marker belong to the code, not the numbered gutter.
    // Codex continues at this column even when the code itself is indented.
    Some(i)
}

fn kind(color: &Color) -> Option<bool> {
    let Color::Rgb(r, g, b) = *color else { return None };
    if g > r.saturating_add(8) && g > b.saturating_add(5) { Some(true) }
    else if r > g.saturating_add(8) && r > b.saturating_add(5) { Some(false) }
    else { None }
}

/// Source-painted line breaks inside one numbered diff line are continuations,
/// unlike the next numbered code line. Keep the real code indentation/newlines.
pub(crate) fn continuation(previous: &[GridCell], next: &[GridCell], indent: usize) -> Option<(usize, bool)> {
    if gutter(next).is_some() || previous.len() != next.len() { return None; }
    let fill = previous.iter().find(|c| kind(&c.bg).is_some())?.bg.clone();
    if !next.iter().any(|c| c.bg == fill) { return None; }
    let lead = next.iter().take_while(|c| matches!(c.ch, ' ' | '\0')).count();
    if lead < indent || lead == next.len() { return None; }
    // A hard-drawn patch continuation has no line number and retains the
    // patch's fill. After a source resize the old short rows are padded to the
    // NEW terminal width, so proximity to that right edge proves nothing.
    // Remove only the gutter; extra indentation is real code, not padding.
    Some((indent, false))
}

/// Only colors actually used by a numbered patch are candidates. Recolor their
/// wrapped rows too, retaining additions/deletions and stronger word highlights.
pub(crate) fn localize(rows: &mut [Vec<GridCell>], background: [u8; 4], added: [u8; 4], removed: [u8; 4]) {
    let mut fills = Vec::new();
    for row in rows.iter().filter(|row| gutter(row).is_some()) {
        for cell in row {
            if let Some(addition) = kind(&cell.bg) {
                if !fills.iter().any(|(color, _)| color == &cell.bg) {
                    fills.push((cell.bg.clone(), addition));
                }
            }
        }
    }
    for row in rows {
        for cell in row {
            if let Some((Color::Rgb(r, g, b), addition)) = fills.iter().find(|(fill, _)| fill == &cell.bg) {
                let chroma = r.max(g).max(b) - r.min(g).min(b);
                let strength = if chroma > 65 { 0.28 } else { 0.15 };
                let fill = crate::theme::lerp(background, if *addition { added } else { removed }, strength);
                cell.bg = Color::Rgb(fill[0], fill[1], fill[2]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(text: &str, fill: Color) -> Vec<GridCell> {
        let mut row: Vec<_> = text.chars().map(|ch| GridCell { ch, bg: fill.clone(), ..GridCell::blank() }).collect();
        row.resize(40, GridCell { bg: fill, ..GridCell::blank() }); row
    }
    #[test]
    fn light_diff_is_dark_on_dark_viewer_without_changing_text_or_source() {
        let green = Color::Rgb(221, 250, 224);
        let red = Color::Rgb(255, 224, 225);
        let source = vec![row(" 13 + added text", green.clone()), row("      continuation", green),
            row(" 14 - removed text", red), row("ordinary panel", Color::Rgb(240, 240, 240))];
        for bg in [[30, 33, 40, 255], [246, 246, 248, 255]] {
            let mut view = source.clone();
            localize(&mut view, bg, [80, 160, 100, 255], [205, 80, 80, 255]);
            assert_ne!(view[0][0].bg, source[0][0].bg);
            assert_eq!(view[0][0].bg, view[1][0].bg);
            assert_ne!(view[0][0].bg, view[2][0].bg);
            assert_eq!(view[3], source[3]);
            for (a, b) in view.iter().flatten().zip(source.iter().flatten()) {
                assert_eq!((a.ch, &a.fg, a.dim, a.inverse), (b.ch, &b.fg, b.dim, b.inverse));
            }
        }
    }
    #[test]
    fn ordinary_colored_output_is_not_a_patch() {
        let mut rows = vec![row("green output", Color::Rgb(220, 250, 224))];
        let original = rows.clone();
        localize(&mut rows, [30, 30, 30, 255], [0, 150, 0, 255], [200, 0, 0, 255]);
        assert_eq!(rows, original);
    }
}
