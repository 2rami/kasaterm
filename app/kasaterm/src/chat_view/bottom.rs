//! 진짜 화면 바닥(스피너·입력칸·상태줄·선택지 창)을 대화 칸 폭으로 다시 짓는다. 거울은 원본 칸 폭이 보는
//! 칸과 달라, 원본 격자째 옮기면 원본이 좁을 때 입력 줄·선택지가 그 폭에서 접힌 채로, 넓을 때는 잘려 보인다.
//!
//! 접어 둔 문단은 폰·터미널 거울과 같은 규칙(`kasa_bridge::reflow::paragraph_continuation`)으로 되이어 이 폭에서
//! 다시 접고 이어지는 조각은 글머리 뒤에 맞춘다. 가로줄은 칸 끝까지 다시 긋고, 오른쪽에 붙은 글(`◉ xhigh ·
//! /effort`)은 오른쪽에 붙이며, 바탕이 칸 끝까지 칠해진 줄(코덱스 입력칸)은 이 폭 끝까지 칠한다. 키는 그대로
//! 원본 TUI 로 가니 바뀌는 것은 보이는 자리뿐이고, 원본 커서는 옮겨 간 글자를 따라간다.

use super::*;
use kasa_bridge::screen::Color;
use unicode_width::UnicodeWidthChar;

/// 낱말을 지키려고 되돌아가는 최대 칸 수 — 폰 거울(`reflow.rs WORD_BACKOFF`)과 같다.
const WORD_BACKOFF: usize = 12;

pub(super) struct Fitted {
    pub(super) rows: Vec<Vec<GridCell>>,
    /// 원본 커서가 옮겨 간 (행, 열).
    pub(super) cursor: Option<(usize, usize)>,
}

#[derive(Clone)]
struct Glyph {
    cell: GridCell,
    width: usize,
    /// 원본 바닥의 (행, 열). 새로 끼운 칸(들여쓰기·낱말 사이)은 None.
    at: Option<(usize, usize)>,
}

impl Glyph {
    fn space(&self) -> bool {
        self.cell.ch == ' '
    }
}

/// `rows` 는 바닥 행들(원본 폭 `src_cols`), `cursor` 는 그 안의 (행, 열).
pub(super) fn fit(rows: &[Vec<GridCell>], src_cols: usize, cursor: Option<(usize, usize)>, cols: usize) -> Fitted {
    if src_cols == cols {
        let rows = rows.iter().map(|r| sized(r.clone(), cols, &GridCell::blank())).collect();
        return Fitted { rows, cursor: cursor.map(|(r, c)| (r, c.min(cols.saturating_sub(1)))) };
    }
    let mut out: Vec<Vec<Glyph>> = Vec::new();
    let mut fills: Vec<GridCell> = Vec::new();
    let mut r = 0;
    while r < rows.len() {
        let row = &rows[r];
        if let Some(line) = rule(row, src_cols, cols) {
            out.push(line);
            fills.push(GridCell::blank());
            r += 1;
            continue;
        }
        if cursor.is_none_or(|c| c.0 != r) {
            if let Some((left, right)) = aligned(row, r, src_cols) {
                let margin = src_cols - right_edge(row);
                let (lw, rw) = (width(&left), width(&right));
                if !left.is_empty() && lw + 2 + rw + margin > cols {
                    let indent = kasa_bridge::reflow::hanging_indent_of(&plain(row));
                    out.extend(wrap(&left, cols, indent));
                    fills.extend(std::iter::repeat_n(GridCell::blank(), out.len() - fills.len()));
                }
                let mut line = if lw + 2 + rw + margin <= cols { left } else { Vec::new() };
                let at = cols.saturating_sub(margin + rw).max(width(&line) + usize::from(!line.is_empty()) * 2);
                line.extend(std::iter::repeat_n(blank_glyph(), at - width(&line)));
                line.extend(right);
                out.push(line);
                fills.push(GridCell::blank());
                r += 1;
                continue;
            }
        }
        let fill = crate::screenread::band_bg(row).map_or_else(GridCell::blank, |bg| {
            let mut c = GridCell::blank();
            c.bg = bg;
            c
        });
        let mut glyphs = glyphs_of(row, r, cursor, fill.bg != Color::Default);
        let indent = kasa_bridge::reflow::hanging_indent_of(&plain(row));
        let mut last = r;
        while last + 1 < rows.len() && rule(&rows[last + 1], src_cols, cols).is_none() {
            let next = &rows[last + 1];
            if cursor.is_none_or(|c| c.0 != last + 1) && aligned(next, last + 1, src_cols).is_some() {
                break;
            }
            // 입력칸은 본문보다 한 칸 앞에서 접는다(칸 폭 − 2, 64칸 실측) — 그 가장자리로 잰다.
            let Some((skip, separator)) = kasa_bridge::reflow::paragraph_continuation(&plain(&rows[last]), &plain(next), src_cols - 1) else {
                break;
            };
            if separator {
                glyphs.push(Glyph { cell: fill.clone(), width: 1, at: None });
            }
            let mut more = glyphs_of(next, last + 1, cursor, fill.bg != Color::Default);
            more.retain(|g| g.at.is_none_or(|(_, col)| col >= skip));
            glyphs.extend(more);
            last += 1;
        }
        let lines = wrap(&glyphs, cols, indent);
        fills.extend(std::iter::repeat_n(fill, lines.len()));
        out.extend(lines);
        r = last + 1;
    }
    let found = cursor.and_then(|c| locate(&out, c));
    let rows = out
        .iter()
        .zip(&fills)
        .map(|(line, fill)| {
            let mut cells = Vec::with_capacity(cols);
            for g in line {
                if cells.len() + g.width > cols {
                    break;
                }
                cells.push(g.cell.clone());
                if g.width == 2 {
                    let mut spacer = g.cell.clone();
                    spacer.ch = '\0';
                    cells.push(spacer);
                }
            }
            sized(cells, cols, fill)
        })
        .collect();
    Fitted { rows, cursor: found.map(|(r, c)| (r, c.min(cols.saturating_sub(1)))) }
}

fn sized(mut row: Vec<GridCell>, cols: usize, fill: &GridCell) -> Vec<GridCell> {
    row.truncate(cols);
    row.resize(cols, fill.clone());
    row
}

fn blank_glyph() -> Glyph {
    Glyph { cell: GridCell::blank(), width: 1, at: None }
}

fn width(g: &[Glyph]) -> usize {
    g.iter().map(|g| g.width).sum()
}

fn right_edge(row: &[GridCell]) -> usize {
    row.iter().rposition(|c| !matches!(c.ch, ' ' | '\0')).map_or(0, |i| i + 1)
}

/// 문단 판정용 사본 — 칸 끝까지 칠한 바탕은 글로 세지 않고, 코덱스 입력 글머리 `›` 는 claude 의 `❯` 처럼
/// 글머리로 친다(공용 규칙 `kasa_bridge::reflow` 는 폰과 짝이라 거기 글머리를 늘리지 않는다).
fn plain(row: &[GridCell]) -> Vec<GridCell> {
    let mut row = row.to_vec();
    if crate::screenread::band_bg(&row).is_some() {
        for c in row.iter_mut().rev().take_while(|c| matches!(c.ch, ' ' | '\0')) {
            c.bg = Color::Default;
        }
    }
    if let Some(mark) = row.iter_mut().find(|c| !matches!(c.ch, ' ' | '\0')).filter(|c| crate::screenread::is_codex_composer_marker(c.ch)) {
        mark.ch = '❯';
    }
    row
}

/// 한 행의 글자 — 넓은 글자 뒤 자리지기는 그 글자에 묻고, 뒤의 빈칸은 버린다(커서가 앉은 칸까지는 남긴다).
/// `band` 면 칸 끝까지 칠한 바탕은 글로 세지 않는다 — 다시 칠한다.
fn glyphs_of(row: &[GridCell], r: usize, cursor: Option<(usize, usize)>, band: bool) -> Vec<Glyph> {
    let shown = |c: &GridCell| !matches!(c.ch, ' ' | '\0') || c.inverse || c.underline || (!band && c.bg != Color::Default);
    let mut end = row.iter().rposition(shown).map_or(0, |i| i + 1);
    if let Some((_, cc)) = cursor.filter(|c| c.0 == r) {
        end = end.max((cc + 1).min(row.len()));
    }
    let mut out = Vec::with_capacity(end);
    let mut col = 0;
    while col < end {
        let cell = &row[col];
        if cell.ch == '\0' {
            col += 1;
            continue;
        }
        let w = cell.ch.width().unwrap_or(1).clamp(1, 2);
        out.push(Glyph { cell: cell.clone(), width: w, at: Some((r, col)) });
        col += if w == 2 && row.get(col + 1).is_some_and(|c| matches!(c.ch, ' ' | '\0')) { 2 } else { 1 };
    }
    out
}

/// 가로줄 — 입력칸 머리의 세션 이름표는 오른쪽에 그대로 두고 칸 끝까지 다시 긋는다. 한 가지 선 글자로만 된
/// 줄(선택지 창의 `╌` 칸막이)도 같다.
fn rule(row: &[GridCell], src_cols: usize, cols: usize) -> Option<Vec<Glyph>> {
    let marks: Vec<usize> = (0..row.len()).filter(|&i| !matches!(row[i].ch, ' ' | '\0')).collect();
    let first = *marks.first()?;
    let dash = row[first].clone();
    if !matches!(dash.ch, '─' | '━' | '═' | '╌' | '╍' | '┄' | '┅' | '┈' | '┉') {
        return None;
    }
    let glyph = |cell: &GridCell| Glyph { cell: cell.clone(), width: 1, at: None };
    let lead = marks.iter().take_while(|&&i| row[i].ch == dash.ch).count();
    if lead == marks.len() {
        if lead < 8 {
            return None;
        }
        let margin = src_cols.saturating_sub(marks[lead - 1] + 1);
        let mut line: Vec<Glyph> = std::iter::repeat_n(blank_glyph(), first).collect();
        line.extend(std::iter::repeat_n(glyph(&dash), cols.saturating_sub(first + margin)));
        return Some(line);
    }
    // 이름표 줄 — 앞 가로줄 다음부터 마지막 선 아닌 글자까지.
    if first != 0 || dash.ch != '─' || marks.iter().filter(|&&i| row[i].ch == '─').count() < 10 {
        return None;
    }
    let last = *marks.iter().rev().find(|&&i| row[i].ch != '─')?;
    let label: Vec<Glyph> = glyphs_of(&row[marks[lead]..=last], 0, None, false).into_iter().map(|g| Glyph { at: None, ..g }).collect();
    let mut line: Vec<Glyph> = std::iter::repeat_n(glyph(&dash), cols.saturating_sub(width(&label) + 3)).collect();
    line.push(blank_glyph());
    line.extend(label);
    line.push(blank_glyph());
    line.push(glyph(&dash));
    Some(line)
}

/// 오른쪽에 붙은 글 — 원본 오른쪽 끝(두 칸 안)에 닿고, 앞 글과 넉 칸 넘게 떨어졌거나(`  ? for shortcuts …
/// ◉ xhigh`) 홀로 칸 오른쪽 절반에 선 줄. (왼쪽 글, 오른쪽 글).
fn aligned(row: &[GridCell], r: usize, src_cols: usize) -> Option<(Vec<Glyph>, Vec<Glyph>)> {
    if crate::screenread::band_bg(row).is_some() {
        return None;
    }
    let end = right_edge(row);
    if end == 0 || src_cols.saturating_sub(end) > 2 {
        return None;
    }
    let lead = row.iter().take_while(|c| matches!(c.ch, ' ' | '\0')).count();
    let mut gap_end = end;
    let mut gap = 0;
    let mut split = None;
    for i in (lead..end).rev() {
        if matches!(row[i].ch, ' ' | '\0') && row[i].bg == Color::Default {
            gap += 1;
            if gap >= 4 {
                split = Some((i, gap_end));
            }
        } else {
            if split.is_some() {
                break;
            }
            gap = 0;
            gap_end = i;
        }
    }
    let glyphs = glyphs_of(row, r, None, false);
    match split {
        Some((_, right)) => {
            let (left, right): (Vec<Glyph>, Vec<Glyph>) = glyphs.into_iter().partition(|g| g.at.is_some_and(|(_, c)| c < right));
            let left: Vec<Glyph> = {
                let keep = left.iter().rposition(|g| !g.space()).map_or(0, |i| i + 1);
                left[..keep].to_vec()
            };
            Some((left, right))
        }
        None if lead * 2 > src_cols => Some((Vec::new(), glyphs.into_iter().skip_while(Glyph::space).collect())),
        None => None,
    }
}

/// 이 폭으로 접는다 — 가까운 빈칸에서 끊고(그 빈칸은 앞 줄 끝에 남긴다), 둘째 조각부터 `indent` 만큼 들인다.
fn wrap(g: &[Glyph], cols: usize, indent: usize) -> Vec<Vec<Glyph>> {
    if g.is_empty() {
        return vec![Vec::new()];
    }
    let indent = indent.min(cols / 2);
    let mut lines = Vec::new();
    let mut i = 0;
    while i < g.len() {
        let pad = if lines.is_empty() { 0 } else { indent };
        let budget = cols - pad;
        let mut j = i;
        let mut w = 0;
        while j < g.len() && w + g[j].width <= budget {
            w += g[j].width;
            j += 1;
        }
        if j == i {
            j = i + 1;
        }
        if j < g.len() && !g[j].space() {
            let mut back = 0;
            if let Some(k) = (i + 1..j).rev().find(|&k| {
                back += g[k].width;
                g[k].space() || back > WORD_BACKOFF
            }) {
                if g[k].space() {
                    j = k + 1;
                }
            }
        }
        let mut line: Vec<Glyph> = std::iter::repeat_n(blank_glyph(), pad).collect();
        line.extend_from_slice(&g[i..j]);
        // 끊은 자리의 빈칸 하나는 다음 줄 머리에 두지 않는다 — 그 칸에 앉은 커서는 앞 줄 끝에 선다.
        if j < g.len() && g[j].space() {
            if width(&line) < cols {
                line.push(g[j].clone());
            }
            j += 1;
        }
        lines.push(line);
        i = j;
    }
    lines
}

fn locate(lines: &[Vec<Glyph>], cursor: (usize, usize)) -> Option<(usize, usize)> {
    let mut before: Option<(usize, usize)> = None;
    for (r, line) in lines.iter().enumerate() {
        let mut col = 0;
        for g in line {
            match g.at {
                Some(at) if at == cursor => return Some((r, col)),
                Some((sr, sc)) if sr == cursor.0 && sc < cursor.1 => before = Some((r, col + g.width)),
                _ => {}
            }
            col += g.width;
        }
    }
    before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, width: usize) -> Vec<GridCell> {
        let mut out = Vec::new();
        for ch in text.chars() {
            let mut c = GridCell::blank();
            c.ch = ch;
            out.push(c);
            if ch.width() == Some(2) {
                let mut sp = GridCell::blank();
                sp.ch = '\0';
                out.push(sp);
            }
        }
        out.resize(width, GridCell::blank());
        out
    }

    fn text(r: &[GridCell]) -> String {
        r.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    /// 2026-10-15 62열 칸의 진짜 claude 화면 바닥(풀스크린, 입력 줄 셋으로 접힘).
    fn narrow_bottom() -> Vec<Vec<GridCell>> {
        let rule = "─".repeat(62);
        [
            format!("{}◉ xhigh · /effort", " ".repeat(43)),
            rule.clone(),
            "❯ 거울 칸의 바닥 입력 줄이 원본 폭에서 접힌 채로 보이는지".into(),
            "  확인하려고 아주 긴 문장을 넣어 본다 the quick brown fox".into(),
            "  jumps over the lazy dog again and again".into(),
            rule,
            String::new(),
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)".into(),
        ]
        .iter()
        .map(|l| row(l, 62))
        .collect()
    }

    #[test]
    fn a_narrow_source_input_is_rejoined_at_the_wide_pane() {
        let src = narrow_bottom();
        let fitted = fit(&src, 62, Some((4, 41)), 120);
        let got: Vec<String> = fitted.rows.iter().map(|r| text(r)).collect();
        assert!(fitted.rows.iter().all(|r| r.len() == 120));
        assert_eq!(got[0], format!("{}◉ xhigh · /effort", " ".repeat(101)));
        assert_eq!(got[1], "─".repeat(120));
        assert_eq!(
            got[2],
            "❯ 거울 칸의 바닥 입력 줄이 원본 폭에서 접힌 채로 보이는지 확인하려고 아주 긴 문장을 넣어 본다 the quick brown fox jumps"
        );
        assert_eq!(got[3], "  over the lazy dog again and again");
        assert_eq!(got[4], "─".repeat(120));
        assert_eq!(got.len(), 7);
        // 커서는 「again」 뒤 — 이어 붙은 줄의 끝을 따라간다.
        assert_eq!(fitted.cursor, Some((3, 2 + "over the lazy dog again and again".len())));
    }

    /// 2026-10-15 64열 리그 칸 — 둘째 줄은 61칸에서 끝나고 다음 낱말(5칸)이 들어갈 자리가 한 칸 모자랐다.
    #[test]
    fn the_input_box_wraps_one_column_before_the_prose_edge() {
        let src: Vec<Vec<GridCell>> = [
            "❯ 거울 칸의 바닥 입력 줄이 원본 폭에서 접힌 채로 보이는지",
            "  확인하려고 아주 긴 문장을 넣어 본다 the quick brown fox",
            "  jumps over the lazy dog again and again",
        ]
        .iter()
        .map(|l| row(l, 64))
        .collect();
        let fitted = fit(&src, 64, None, 200);
        let got: Vec<String> = fitted.rows.iter().map(|r| text(r)).collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].ends_with("brown fox jumps over the lazy dog again and again"), "{got:?}");
    }

    #[test]
    fn a_wide_source_input_is_folded_under_its_mark_at_the_narrow_pane() {
        let long = "❯ the quick brown fox jumps over the lazy dog and keeps running across the whole wide field";
        let src = vec![row(&"─".repeat(120), 120), row(long, 120), row(&"─".repeat(120), 120)];
        let fitted = fit(&src, 120, Some((1, long.chars().count())), 40);
        let got: Vec<String> = fitted.rows.iter().map(|r| text(r)).collect();
        assert_eq!(got[0], "─".repeat(40));
        assert!(got[1..got.len() - 1].iter().all(|l| l.chars().count() <= 40), "{got:?}");
        assert!(got[2].starts_with("  ") && !got[2].starts_with("   "), "{got:?}");
        let joined: String = got[1..got.len() - 1].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
        assert_eq!(joined, long);
        let (cr, cc) = fitted.cursor.unwrap();
        assert_eq!(cr, got.len() - 2);
        assert_eq!(cc, got[cr].chars().count());
    }

    #[test]
    fn the_session_label_and_right_items_stay_on_the_right_when_narrower() {
        let mut label = "─".repeat(50);
        label.push_str(" 작업 이름 ─");
        let status = format!("  ? for shortcuts{}◉ xhigh · /effort", " ".repeat(70 - 17 - 17));
        let src = vec![row(&label, 70), row(&status, 70)];
        let fitted = fit(&src, 70, None, 30);
        let got: Vec<String> = fitted.rows.iter().map(|r| text(r)).collect();
        assert!(got[0].ends_with(" 작업 이름 ─") && got[0].starts_with("──"), "{got:?}");
        assert_eq!(got[1], "  ? for shortcuts");
        assert_eq!(got[2], format!("{}◉ xhigh · /effort", " ".repeat(13)));
    }

    #[test]
    fn a_filled_codex_input_is_painted_to_the_new_edge_and_keeps_explicit_newlines() {
        let bg = Color::Rgb(72, 77, 84);
        let band = |t: &str| {
            let mut r = row(t, 50);
            r.iter_mut().for_each(|c| c.bg = bg.clone());
            r
        };
        let src = vec![band(""), band("› 첫 줄"), band("  둘째 줄"), band(""), row("  gpt-5.5 high · 83% left", 50)];
        let fitted = fit(&src, 50, Some((2, 10)), 80);
        let got: Vec<String> = fitted.rows.iter().map(|r| text(r)).collect();
        assert_eq!(got, ["", "› 첫 줄", "  둘째 줄", "", "  gpt-5.5 high · 83% left"]);
        assert!(fitted.rows[..4].iter().all(|r| r.len() == 80 && r.iter().all(|c| c.bg == bg)));
        assert_eq!(fitted.cursor, Some((2, 10)));

        // 넓은 원본에서 접힌 코덱스 입력은 되이어 좁은 칸에서 글머리 뒤로 다시 접힌다.
        let wide: Vec<Vec<GridCell>> = [
            "› 코덱스 입력칸도 거울 폭에 맞춰 바탕까지 칠해지는지 보려는 긴 입력 줄 the quick brown fox jumps over the",
            "  lazy dog and keeps on running far",
        ]
        .iter()
        .map(|t| {
            let mut r = row(t, 109);
            r.iter_mut().for_each(|c| c.bg = bg.clone());
            r
        })
        .collect();
        let got: Vec<String> = fit(&wide, 109, None, 69).rows.iter().map(|r| text(r)).collect();
        assert!(got[1..].iter().all(|l| l.starts_with("  ") && !l.starts_with("   ")), "{got:?}");
        let joined: Vec<&str> = got.iter().map(|l| l.trim()).collect();
        assert!(joined.join(" ").ends_with("over the lazy dog and keeps on running far"), "{got:?}");
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn a_wrapped_menu_option_is_rejoined_and_the_divider_redrawn() {
        let src: Vec<Vec<GridCell>> = [
            "╌".repeat(62),
            " Do you want to proceed?".into(),
            " ❯ 1. Yes".into(),
            "   2. Yes, allow reading from /private/tmp/kt-mc from this".into(),
            "      project".into(),
            "   3. No".into(),
        ]
        .iter()
        .map(|l| row(l, 62))
        .collect();
        let fitted = fit(&src, 62, None, 100);
        let got: Vec<String> = fitted.rows.iter().map(|r| text(r)).collect();
        assert_eq!(got[0], "╌".repeat(100));
        assert_eq!(got[3], "   2. Yes, allow reading from /private/tmp/kt-mc from this project");
        assert_eq!(got[4], "   3. No");
    }

    #[test]
    fn the_same_width_is_left_as_it_is() {
        let src = narrow_bottom();
        let fitted = fit(&src, 62, Some((4, 41)), 62);
        assert_eq!(fitted.rows, src);
        assert_eq!(fitted.cursor, Some((4, 41)));
    }
}
