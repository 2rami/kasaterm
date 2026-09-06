//! 거울(view) pane 의 격자를 로컬 pane 폭에 맞춰 다시 접는다 — 폰 앱의
//! `mobile/lib/reflow.dart` 와 같은 규칙. 원본 격자는 저쪽 기계 pane 의 폭이고(거울은
//! resize 를 안 보낸다), 이쪽 pane 은 그 폭과 다르다. 폭이 좁으면 낱말 끝에서 접어
//! 글머리 아래로 들여쓰고, 넓으면 저쪽이 접어 둔 문단을 되이어 다시 펴며, 테두리·상자
//! 줄은 어느 쪽이든 폭에 맞춘다(2026-09-07 지시 「맥미니·맥북 둘 다 폭 따라가지 않게」).
//!
//! 순수 함수만 있다 — 렌더·PTY 를 모르고 `Row` 만 받아 `Row` 를 돌려준다.

use crate::screen::{Cell, Row};
use unicode_width::UnicodeWidthChar;

/// 한 글자와 그 모양. 넓은 글자는 `width == 2` 이고 원본 격자에선 뒤에 스페이서 칸이
/// 따른다 — PTY 쪽 `convert_cell` 이 `'\0'` 을 `' '` 로 바꿔 보내므로 문자는 빈칸이고
/// 「앞 글자가 넓다」로만 안다(안 걸러내면 한글마다 한 칸씩 벌어진다). 여기서는 하나로
/// 다루고 내보낼 때 다시 스페이서를 붙인다.
#[derive(Clone, Debug)]
struct Glyph {
    cell: Cell,
    width: usize,
}

impl Glyph {
    fn blank() -> Self {
        Glyph {
            cell: Cell::blank(),
            width: 1,
        }
    }
    fn ch(&self) -> char {
        self.cell.ch
    }
    /// 아무것도 안 보이는 칸 — 뒤에 달린 이런 칸은 잘라도 화면이 같다.
    fn is_blank(&self) -> bool {
        self.ch() == ' '
            && self.cell.bg == crate::screen::Color::Default
            && !self.cell.inverse
            && !self.cell.underline
    }
    /// 선 그리기 채움(─ ━ ═ …)이나 빈칸 — 늘이고 줄여도 뜻이 안 변하는 글자.
    fn is_filler(&self) -> bool {
        let c = self.ch();
        c == ' ' || ('\u{2500}'..='\u{259f}').contains(&c)
    }
    fn same_fill(&self, o: &Glyph) -> bool {
        self.is_filler()
            && self.ch() == o.ch()
            && self.width == 1
            && o.width == 1
            && self.cell.bg == o.cell.bg
            && self.cell.fg == o.cell.fg
    }
}

fn glyphs_of(row: &[Cell]) -> Vec<Glyph> {
    let mut out = Vec::with_capacity(row.len());
    let mut i = 0;
    while i < row.len() {
        let c = &row[i];
        if c.ch == '\0' {
            i += 1;
            continue;
        }
        let w = UnicodeWidthChar::width(c.ch).unwrap_or(1).clamp(1, 2);
        out.push(Glyph {
            cell: c.clone(),
            width: w,
        });
        i += 1;
        // 넓은 글자 뒤의 스페이서(빈칸)는 그 글자의 둘째 칸이다.
        if w == 2 && i < row.len() && matches!(row[i].ch, ' ' | '\0') {
            i += 1;
        }
    }
    out
}

fn cells_of(glyphs: &[Glyph], cols: usize) -> Row {
    let mut out: Row = Vec::with_capacity(cols);
    for g in glyphs {
        if out.len() + g.width > cols {
            break;
        }
        out.push(g.cell.clone());
        if g.width == 2 {
            let mut sp = Cell::blank();
            sp.bg = g.cell.bg.clone();
            out.push(sp);
        }
    }
    while out.len() < cols {
        out.push(Cell::blank());
    }
    out
}

fn width_of(g: &[Glyph]) -> usize {
    g.iter().map(|x| x.width).sum()
}

fn trimmed(row: &[Cell]) -> Vec<Glyph> {
    let mut g = glyphs_of(row);
    while g.last().is_some_and(|x| x.is_blank()) {
        g.pop();
    }
    g
}

/// 접힌 한 행: 조각들과 각 조각이 원본의 몇 번째 칸에서 시작하는지, 둘째 조각부터
/// 앞에 붙인 들여쓰기.
struct RowReflow {
    chunks: Vec<Vec<Glyph>>,
    starts: Vec<usize>,
    indent: usize,
}

const MARKS: &[char] = &['-', '*', '>', '•', '●', '⏺', '⎿', '❯', '▸', '▪'];

/// 글머리(- * > • ⎿ 1. 2)) 뒤 글이 시작하는 칸 — 이어지는 조각을 여기 맞춘다. 글머리가
/// 없으면 앞 빈칸 수.
fn hanging_indent(g: &[Glyph]) -> usize {
    let mut i = 0;
    while i < g.len() && g[i].ch() == ' ' {
        i += 1;
    }
    if i >= g.len() {
        return i;
    }
    let mut j = i;
    let first = g[i].ch();
    if MARKS.contains(&first) {
        j = i + 1;
    } else if first.is_ascii_digit() {
        let mut k = i;
        while k < g.len() && g[k].ch().is_ascii_digit() {
            k += 1;
        }
        if k < g.len() && (g[k].ch() == '.' || g[k].ch() == ')') && k - i <= 2 {
            j = k + 1;
        }
    }
    if j == i {
        return i;
    }
    if j < g.len() && g[j].ch() == ' ' {
        let mut k = j;
        while k < g.len() && g[k].ch() == ' ' {
            k += 1;
        }
        return width_of(&g[..k]);
    }
    i
}

/// 낱말을 지키려고 되돌아가는 최대 칸 수 — 이보다 긴 낱말은 그냥 자른다.
const WORD_BACKOFF: usize = 12;

/// 선 채움이 두 칸 이상 이어진 자리를 줄여 `excess` 칸을 덜어 낸다. 자리가 모자라면
/// None — 글줄이라는 뜻이니 접는 쪽으로 간다.
fn shrink_lines(g: &[Glyph], excess: usize) -> Option<Vec<Glyph>> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < g.len() {
        let mut j = i + 1;
        if g[i].ch() != ' ' && g[i].is_filler() {
            while j < g.len() && g[j].same_fill(&g[i]) {
                j += 1;
            }
        }
        if j - i >= 2 {
            runs.push((i, j - i));
        }
        i = j;
    }
    let room: usize = runs.iter().map(|r| r.1 - 1).sum();
    if runs.is_empty() || room < excess {
        return None;
    }
    let mut left = excess;
    while left > 0 {
        runs.sort_by(|a, b| b.1.cmp(&a.1));
        runs[0].1 -= 1;
        left -= 1;
    }
    let keep: std::collections::HashMap<usize, usize> = runs.into_iter().collect();
    let mut out = Vec::with_capacity(g.len());
    let mut i = 0;
    while i < g.len() {
        match keep.get(&i) {
            None => {
                out.push(g[i].clone());
                i += 1;
            }
            Some(&n) => {
                let mut j = i + 1;
                while j < g.len() && g[j].same_fill(&g[i]) {
                    j += 1;
                }
                out.extend(g[i..i + n].iter().cloned());
                i = j;
            }
        }
    }
    Some(out)
}

fn is_vertical(c: char) -> bool {
    matches!(c, '│' | '┃' | '║')
}

/// `shrink_lines` 의 반대 — 선 채움이 있으면 가장 긴 채움을 `deficit` 만큼 늘이고, 양끝이
/// 세로선인 상자 줄이면 오른쪽 세로선 앞 빈칸을 늘인다. 글줄이면 None.
fn stretch(g: &[Glyph], deficit: usize) -> Option<Vec<Glyph>> {
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < g.len() {
        let mut j = i + 1;
        if g[i].ch() != ' ' && g[i].is_filler() {
            while j < g.len() && g[j].same_fill(&g[i]) {
                j += 1;
            }
        }
        if j - i >= 2 && best.is_none_or(|b| j - i > b.1) {
            best = Some((i, j - i));
        }
        i = j;
    }
    if let Some((at, _)) = best {
        let mut out = g[..at].to_vec();
        out.extend(std::iter::repeat_n(g[at].clone(), deficit));
        out.extend_from_slice(&g[at..]);
        return Some(out);
    }
    if g.len() >= 2 && is_vertical(g[0].ch()) && is_vertical(g[g.len() - 1].ch()) {
        let inner = &g[g.len() - 2];
        let blank = if inner.ch() == ' ' {
            inner.clone()
        } else {
            Glyph::blank()
        };
        let mut out = g[..g.len() - 1].to_vec();
        out.extend(std::iter::repeat_n(blank, deficit));
        out.push(g[g.len() - 1].clone());
        return Some(out);
    }
    None
}

/// 한 행을 `cols` 폭으로. `src_cols` 는 원본 폭 — 폰보다 좁은 원본을 꽉 채운 테두리·상자
/// 줄을 폭까지 늘이는 데 쓴다.
fn reflow_glyphs(t: &[Glyph], cols: usize, src_cols: usize) -> RowReflow {
    if t.is_empty() {
        return RowReflow {
            chunks: vec![Vec::new()],
            starts: vec![0],
            indent: 0,
        };
    }
    let width = width_of(t);
    if width <= cols {
        let wide = if src_cols > 0 && src_cols < cols && width == src_cols {
            stretch(t, cols - src_cols)
        } else {
            None
        };
        return RowReflow {
            chunks: vec![wide.unwrap_or_else(|| t.to_vec())],
            starts: vec![0],
            indent: 0,
        };
    }
    // 테두리·상자 줄(╭────╮, │ 글 …빈칸… │): 앞 cols-1 칸 뒤가 같은 채움으로 이어지다
    // 마지막 한 칸으로 끝나면 가운데를 빼고 폭에 맞춘다.
    let mut head = Vec::new();
    let mut hw = 0;
    for g in t {
        if hw + g.width > cols - 1 {
            break;
        }
        head.push(g.clone());
        hw += g.width;
    }
    if head.len() < t.len() - 1 {
        let filler = &t[head.len()..t.len() - 1];
        let last = &t[t.len() - 1];
        if last.width == 1 && filler.iter().all(|c| c.same_fill(&filler[0])) {
            head.push(last.clone());
            return RowReflow {
                chunks: vec![head],
                starts: vec![0],
                indent: 0,
            };
        }
    }
    if let Some(s) = shrink_lines(t, width - cols) {
        return RowReflow {
            chunks: vec![s],
            starts: vec![0],
            indent: 0,
        };
    }
    let indent = hanging_indent(t).min(cols / 2);
    let pad: Vec<Glyph> = std::iter::repeat_n(Glyph::blank(), indent).collect();
    let mut chunks: Vec<Vec<Glyph>> = Vec::new();
    let mut starts = Vec::new();
    let mut i = 0;
    let mut col = 0;
    while i < t.len() {
        let start = col;
        let budget = if chunks.is_empty() {
            cols
        } else {
            cols - indent
        };
        let mut line: Vec<Glyph> = Vec::new();
        let mut lw = 0;
        let mut j = i;
        while j < t.len() && lw + t[j].width <= budget {
            line.push(t[j].clone());
            lw += t[j].width;
            j += 1;
        }
        if line.is_empty() {
            line.push(t[j].clone());
            lw += t[j].width;
            j += 1;
        }
        let emit = |chunks: &mut Vec<Vec<Glyph>>, line: Vec<Glyph>| {
            if chunks.is_empty() {
                chunks.push(line);
            } else {
                let mut l = pad.clone();
                l.extend(line);
                chunks.push(l);
            }
        };
        if j < t.len() {
            // 넘쳤다 — 낱말 가운데가 갈리지 않게 가까운 빈칸에서 끊고 그 빈칸은 버린다.
            if t[j].ch() == ' ' {
                emit(&mut chunks, line);
                starts.push(start);
                i = j + 1;
                col = start + lw + 1;
                continue;
            }
            let mut k = line.len() - 1;
            let mut back = 0;
            while k > 0 && back < WORD_BACKOFF && line[k].ch() != ' ' {
                back += line[k].width;
                k -= 1;
            }
            if k > 0 && line[k].ch() == ' ' {
                let kept: Vec<Glyph> = line[..k].to_vec();
                let kw = width_of(&kept);
                emit(&mut chunks, kept);
                starts.push(start);
                i += k + 1;
                col = start + kw + 1;
                continue;
            }
        }
        emit(&mut chunks, line);
        starts.push(start);
        i = j;
        col = start + lw;
    }
    RowReflow {
        chunks,
        starts,
        indent,
    }
}

/// 행이 문단의 한 줄인지 판정하는 재료.
struct Info {
    width: usize,
    lead: usize,
    indent: usize,
    prose: bool,
    words: usize,
    first_word: usize,
}

impl Info {
    fn marked(&self) -> bool {
        self.indent > self.lead
    }
}

fn info_of(t: &[Glyph]) -> Info {
    let width = width_of(t);
    let mut i = 0;
    while i < t.len() && t[i].ch() == ' ' {
        i += 1;
    }
    let prose = i < t.len() && !('\u{2500}'..='\u{259f}').contains(&t[i].ch());
    let mut words = 0;
    let mut in_word = false;
    let mut first_word = 0;
    for g in &t[i..] {
        let space = g.ch() == ' ';
        if !space && !in_word {
            words += 1;
        }
        if !space && words == 1 {
            first_word += g.width;
        }
        in_word = !space;
    }
    Info {
        width,
        lead: i,
        indent: hanging_indent(t),
        prose,
        words,
        first_word,
    }
}

/// 저쪽이 제 폭에서 낱말 끝으로 접어 둔 줄은 이만큼까지 짧을 수 있다.
const JOIN_SLACK: usize = 12;

/// `b` 가 `a` 의 이어지는 줄인가 — `a` 가 원본 폭을 거의 채우고, `b` 가 `a` 의 들여쓰기
/// 자리에서 글머리 없이 시작하면 저쪽(Ink)이 한 문단을 접어 둔 것이다. `a` 가 마지막
/// 칸까지 찼는데 `b` 가 0열에서 시작하면 터미널이 글자 단위로 자른 것(셸 출력) —
/// 그것도 이어진 줄이다.
fn continues(a: &Info, b: &Info, src_cols: usize) -> bool {
    let hard = a.width == src_cols && b.lead == 0;
    a.prose
        && b.prose
        && a.words > 1
        && a.width + JOIN_SLACK >= src_cols
        && a.width <= src_cols
        // 뒷줄 첫 낱말이 앞줄 남은 자리에 들어갔다면 거기서 접혔을 리가 없다.
        && a.width + 1 + b.first_word > src_cols
        && (b.lead == a.indent || hard)
        && !b.marked()
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '-' | '@' | '~')
}
fn joiner(c: char) -> bool {
    matches!(c, '_' | '.' | '/' | ':' | '-')
}

/// 앞줄 꼬리와 뒷줄 머리가 한 토큰(경로·식별자·주소)의 두 동강인가 — 양쪽 다 ASCII 토큰
/// 글자뿐이고 붙인 것에 `_ . / : -` 가 든다.
fn token_break(prev: &[Glyph], next: &[Glyph]) -> bool {
    let mut i = prev.len();
    while i > 0 && prev[i - 1].ch() != ' ' {
        if !token_char(prev[i - 1].ch()) {
            return false;
        }
        i -= 1;
    }
    let mut j = 0;
    while j < next.len() && next[j].ch() != ' ' {
        if !token_char(next[j].ch()) {
            return false;
        }
        j += 1;
    }
    if i == prev.len() || j == 0 {
        return false;
    }
    prev[i..].iter().any(|g| joiner(g.ch())) || next[..j].iter().any(|g| joiner(g.ch()))
}

/// 접어 둔 줄들을 한 줄로 되잇는다. `offsets[i]` 는 i 번째 줄의 글이 되이은 줄의 몇 번째
/// 칸에서 시작하는지.
fn join_rows(rows: &[Vec<Glyph>], infos: &[Info], src_cols: usize) -> (Vec<Glyph>, Vec<usize>) {
    let mut out: Vec<Glyph> = Vec::new();
    let mut offsets = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut from = 0;
        if i > 0 {
            from = infos[i].lead.min(row.len());
            // 앞줄이 마지막 칸까지 찼고 그 자리가 경로 한가운데면 글자 단위로 잘린 것
            // (`recall.p` + `y`) — 빈칸을 끼우면 없던 낱말이 생긴다.
            let glued = infos[i - 1].width == src_cols && token_break(&out, &row[from..]);
            if !glued {
                out.push(Glyph::blank());
            }
        }
        offsets.push(width_of(&out));
        out.extend_from_slice(&row[from..]);
    }
    (out, offsets)
}

/// 다시 접은 격자.
pub struct Reflowed {
    pub rows: Vec<Row>,
    pub cursor_row: u16,
    pub cursor_col: u16,
}

/// 창에 담기 전의 줄 전부 — 세로로 스크롤되는 곳(웹 거울)은 이걸 그대로 쓴다.
/// 뒤의 빈 줄은 커서가 앉은 줄까지만 남기고 잘라 낸다.
pub struct ReflowedLines {
    pub rows: Vec<Row>,
    pub cursor_row: Option<u16>,
    pub cursor_col: u16,
}

/// 행 하나만 `cols` 폭으로 — 지난 줄(스크롤백)처럼 이웃 행과 되이을 수 없는 자리.
pub fn reflow_row(row: &Row, src_cols: u16, cols: u16) -> Vec<Row> {
    let cols = cols.max(4) as usize;
    let rr = reflow_glyphs(&trimmed(row), cols, src_cols as usize);
    rr.chunks.iter().map(|c| cells_of(c, cols)).collect()
}

/// `src`(원본 폭 `src_cols`)를 `cols`×`rows` 로. 줄이 남으면 아래를 비우고, 넘치면 커서가
/// 든 줄이 보이게 아래쪽을 담는다(입력상자가 바닥에 있는 화면이 흔하다).
pub fn reflow_grid(
    src: &[Row],
    src_cols: u16,
    cursor: (u16, u16),
    cols: u16,
    rows: u16,
) -> Reflowed {
    let all = reflow_lines(src, src_cols, cursor, cols);
    let cols = cols.max(4) as usize;
    let rows_n = rows.max(1) as usize;
    let lines = all.rows;
    let cursor_line = all.cursor_row.map(|r| r as usize);
    // 창에 담기 — 넘치면 커서 줄이 보이는 아래쪽.
    let total = lines.len();
    let start = if total <= rows_n {
        0
    } else {
        let mut s = total - rows_n;
        if let Some(cl) = cursor_line {
            if cl < s {
                s = cl;
            }
        }
        s
    };
    let mut out_rows: Vec<Row> = lines.into_iter().skip(start).take(rows_n).collect();
    while out_rows.len() < rows_n {
        out_rows.push(vec![Cell::blank(); cols]);
    }
    let cursor_row = cursor_line
        .map(|cl| cl.saturating_sub(start).min(rows_n - 1))
        .unwrap_or(0);
    Reflowed {
        rows: out_rows,
        cursor_row: cursor_row as u16,
        cursor_col: all.cursor_col,
    }
}

/// `src` 의 모든 행을 `cols` 폭으로 다시 접은 줄 전부.
pub fn reflow_lines(src: &[Row], src_cols: u16, cursor: (u16, u16), cols: u16) -> ReflowedLines {
    let cols = cols.max(4) as usize;
    let sc = src_cols as usize;
    let trimmed: Vec<Vec<Glyph>> = src.iter().map(|r| trimmed(r)).collect();
    let infos: Vec<Info> = trimmed.iter().map(|t| info_of(t)).collect();
    let rewrap = sc != cols;

    let mut lines: Vec<Vec<Glyph>> = Vec::new();
    let mut cursor_line: Option<usize> = None;
    let mut cursor_col = 0usize;
    let (cr, cc) = (cursor.0 as usize, cursor.1 as usize);
    let mut r = 0;
    while r < src.len() {
        let mut n = 1;
        if rewrap {
            let mut last = &infos[r];
            while r + n < src.len() {
                let next = &infos[r + n];
                if !continues(last, next, sc) {
                    break;
                }
                last = next;
                n += 1;
            }
        }
        let (rr, offsets): (RowReflow, Vec<usize>) = if n == 1 {
            (reflow_glyphs(&trimmed[r], cols, sc), vec![0])
        } else {
            let (g, offsets) = join_rows(&trimmed[r..r + n], &infos[r..r + n], sc);
            (reflow_glyphs(&g, cols, sc), offsets)
        };
        for i in 0..n {
            if r + i == cr {
                let shift = if i == 0 {
                    0isize
                } else {
                    offsets[i] as isize - infos[r + i].lead as isize
                };
                let col = (cc as isize + shift).max(0) as usize;
                let mut k = 0;
                for (q, s) in rr.starts.iter().enumerate() {
                    if *s <= col {
                        k = q;
                    }
                }
                cursor_line = Some(lines.len() + k);
                let extra = if k > 0 { rr.indent } else { 0 };
                cursor_col = (col - rr.starts[k] + extra).min(cols - 1);
            }
        }
        lines.extend(rr.chunks);
        r += n;
    }
    // 뒤의 빈 줄은 커서 줄까지만.
    let keep = cursor_line.map(|c| c + 1).unwrap_or(0);
    while lines.len() > keep && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    ReflowedLines {
        rows: lines.iter().map(|l| cells_of(l, cols)).collect(),
        cursor_row: cursor_line.map(|c| c as u16),
        cursor_col: cursor_col as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str, cols: usize) -> Row {
        let mut out: Row = Vec::new();
        for ch in s.chars() {
            let mut c = Cell::blank();
            c.ch = ch;
            out.push(c);
            if UnicodeWidthChar::width(ch).unwrap_or(1) == 2 {
                let mut sp = Cell::blank();
                sp.ch = '\0';
                out.push(sp);
            }
        }
        while out.len() < cols {
            out.push(Cell::blank());
        }
        out
    }

    fn text(r: &Row) -> String {
        let g = glyphs_of(r);
        let s: String = g.iter().map(|x| x.ch()).collect();
        s.trim_end().to_string()
    }

    #[test]
    fn wide_char_spacer_is_a_space_in_the_grid() {
        // PTY 가 보내는 격자: 한글 뒤 칸은 `' '`. 그걸 글자로 세면 한 칸씩 벌어진다.
        let mut r: Row = Vec::new();
        for ch in "가나 다".chars() {
            let mut c = Cell::blank();
            c.ch = ch;
            r.push(c);
            if ch != ' ' {
                r.push(Cell::blank());
            }
        }
        while r.len() < 20 {
            r.push(Cell::blank());
        }
        let out = reflow_grid(&[r], 20, (0, 0), 40, 1);
        let g = glyphs_of(&out.rows[0]);
        let s: String = g.iter().map(|x| x.ch()).collect();
        assert_eq!(s.trim_end(), "가나 다");
        assert_eq!(out.rows[0].len(), 40);
    }

    fn lines(src: &[&str], src_cols: usize, cols: u16, rows: u16) -> Vec<String> {
        let g: Vec<Row> = src.iter().map(|s| row(s, src_cols)).collect();
        let out = reflow_grid(&g, src_cols as u16, (0, 0), cols, rows);
        let mut v: Vec<String> = out.rows.iter().map(text).collect();
        while v.last().is_some_and(|s| s.is_empty()) {
            v.pop();
        }
        v
    }

    #[test]
    fn narrow_target_wraps_at_words_under_bullet() {
        let src = ["  - one two three four five six seven eight nine ten"];
        let out = lines(&src, 60, 30, 10);
        assert_eq!(
            out,
            vec![
                "  - one two three four five".to_string(),
                "    six seven eight nine ten".to_string()
            ]
        );
    }

    #[test]
    fn border_rows_shrink_to_width() {
        let src = [&format!("╭{}╮", "─".repeat(58))[..]];
        assert_eq!(lines(&src, 60, 30, 5), vec![format!("╭{}╮", "─".repeat(28))]);
    }

    #[test]
    fn wide_target_rejoins_folded_paragraph() {
        let src = [
            "  - word word word word word word word word word word",
            "    continued end",
        ];
        // 60열 원본이 접어 둔 두 줄 → 80열이면 한 줄.
        let out = lines(&src, 60, 80, 5);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("word continued end"));
    }

    #[test]
    fn same_width_keeps_desktop_line_breaks() {
        let src = [
            "  - word word word word word word word word word word",
            "    continued end",
        ];
        assert_eq!(lines(&src, 60, 60, 5).len(), 2);
    }

    #[test]
    fn narrow_source_border_and_box_stretch() {
        let src = [
            &format!("╭{}╮", "─".repeat(26))[..],
            &format!("│ > hi{}│", " ".repeat(21))[..],
            &format!("╰{}╯", "─".repeat(26))[..],
        ];
        let out = lines(&src, 28, 44, 5);
        assert_eq!(out[0], format!("╭{}╮", "─".repeat(42)));
        assert_eq!(out[1], format!("│ > hi{}│", " ".repeat(37)));
        assert_eq!(out[2], format!("╰{}╯", "─".repeat(42)));
    }

    #[test]
    fn char_split_path_glues_without_space() {
        let src = ["- test_channel_recall_long.p", "  y 31/31 (x)"];
        assert_eq!(
            lines(&src, 28, 44, 5),
            vec!["- test_channel_recall_long.py 31/31 (x)".to_string()]
        );
    }

    #[test]
    fn terminal_hard_wrap_under_bullet_rejoins_at_col_zero() {
        // 셸이 60열에서 글자 단위로 자른 글머리 줄: 뒷줄이 0열에서 시작한다.
        let a = format!("- 경로는 {}", "/Users/kasa/Desktop/momewomo/kasaterm/crates/kasa-b");
        assert_eq!(a.chars().map(|c| if ('\u{ac00}'..='\u{d7a3}').contains(&c) { 2 } else { 1 }).sum::<usize>(), 60);
        let src = [&a[..], "ridge/src/reflow.rs 이다."];
        let out = lines(&src, 60, 100, 5);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("kasa-bridge/src/reflow.rs 이다."), "{}", out[0]);
    }

    #[test]
    fn two_words_at_edge_keep_a_space() {
        let src = ["- abc abc abc abc abc abc dd", "  next word"];
        assert_eq!(
            lines(&src, 28, 44, 5),
            vec!["- abc abc abc abc abc abc dd next word".to_string()]
        );
    }

    #[test]
    fn hangul_wide_chars_wrap_by_cell_width() {
        let src = ["가나다라마바사아자차카타파하 가나다라"];
        let out = lines(&src, 40, 20, 5);
        assert_eq!(out[0].chars().count(), 10); // 넓은 글자 10 = 20칸
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn cursor_follows_into_wrapped_chunk_and_bottom_aligns() {
        let g: Vec<Row> = vec![
            row("  - one two three four five six seven eight nine ten", 60),
            row("", 60),
            row("> typing", 60),
        ];
        // 커서는 「typing」의 t 위(행 2, 열 2).
        let out = reflow_grid(&g, 60, (2, 2), 30, 2);
        // 30열이면 첫 행이 둘로 갈려 총 4줄, 창은 2줄 — 커서 줄이 보이게 아래 둘.
        assert_eq!(out.rows.len(), 2);
        assert_eq!(text(&out.rows[1]), "> typing");
        assert_eq!((out.cursor_row, out.cursor_col), (1, 2));
    }

    #[test]
    fn lines_keep_everything_and_trim_trailing_blanks() {
        let g: Vec<Row> = vec![
            row("  - one two three four five six seven eight nine ten", 60),
            row("> typing", 60),
            row("", 60),
            row("", 60),
        ];
        let out = reflow_lines(&g, 60, (1, 2), 30);
        assert_eq!(out.rows.len(), 3);
        assert_eq!((out.cursor_row, out.cursor_col), (Some(2), 2));
        assert_eq!(reflow_row(&row(&format!("╭{}╮", "─".repeat(26)), 28), 28, 44).len(), 1);
        assert_eq!(reflow_row(&row(&"ab ".repeat(20), 60), 60, 20).len(), 3);
    }

    #[test]
    fn cursor_inside_box_stays_on_its_char_when_stretched() {
        let g: Vec<Row> = vec![
            row(&format!("╭{}╮", "─".repeat(26)), 28),
            row(&format!("│ > hi{}│", " ".repeat(21)), 28),
            row(&format!("╰{}╯", "─".repeat(26)), 28),
        ];
        let out = reflow_grid(&g, 28, (1, 6), 44, 3);
        assert_eq!((out.cursor_row, out.cursor_col), (1, 6));
    }
}
