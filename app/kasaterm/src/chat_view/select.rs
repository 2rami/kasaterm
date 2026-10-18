//! 대화 보기의 글 고르기 — 카사뷰어 렌더 뷰(`MdRenderSel`)처럼 문서 좌표로 잡아 굴려도 남고, 본문 밖으로
//! 끌면 대화가 따라 굴러 화면 밖까지 고른다. 바닥(진짜 화면에서 옮긴 입력칸·상태줄)은 그 줄 번호로 따로 잡는다 —
//! 대화 줄이 늘어도 바닥 줄 번호는 안 밀린다.

use super::*;

/// 고른 자리가 선 줄. 대화 줄은 쌓인 줄(`classic::Cache::lines`)의 절대 번호, 바닥은 바닥 첫 줄부터의 번호다.
/// 대화가 바닥보다 앞이라 이 순서가 곧 읽는 순서다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Zone {
    Conv(usize),
    Bottom(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub(crate) zone: Zone,
    pub(crate) col: usize,
}

pub(crate) struct ChatSel {
    pub(crate) anchor: Pos,
    pub(crate) end: Pos,
    /// 손이 아직 눌려 있다. 떼도 선택은 남아 복사할 수 있다.
    pub(crate) dragging: bool,
    /// 누른 자리(px) — 거의 안 움직이고 뗐으면 선택이 아니라 클릭이다.
    pub(crate) press: (f32, f32),
    pub(crate) moved: bool,
    /// 연타 수(1 낱자·2 단어·3 줄). 단어·줄로 잡은 선택은 떼도 클릭으로 안 지운다.
    pub(crate) clicks: u8,
    /// 고를 때의 칸 폭 — 폭이 바뀌면 줄을 다시 펴 줄 번호가 달라지니 버린다.
    pub(crate) cols: usize,
}

impl ChatSel {
    pub(crate) fn at(pos: Pos, press: (f32, f32), cols: usize) -> Self {
        Self { anchor: pos, end: pos, dragging: true, press, moved: false, clicks: 1, cols }
    }

    pub(crate) fn range(&self) -> (Pos, Pos) {
        if self.anchor <= self.end {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }
}

/// 한 프레임에 보인 자리 — 대화 줄 `first..` 이 위 `hist` 줄에(쌓인 줄 `total` 까지), 그 아래 바닥 `bottom` 줄.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct View {
    pub(crate) first: usize,
    pub(crate) hist: usize,
    pub(crate) total: usize,
    pub(crate) bottom: usize,
    pub(crate) cols: usize,
}

impl View {
    /// 대화가 보이는 줄 수 — 짧은 대화면 `hist` 보다 적다.
    fn conv_rows(&self) -> usize {
        self.total.saturating_sub(self.first).min(self.hist)
    }

    /// 화면 칸 → 문서 자리. 대화 아래 빈 줄은 마지막 대화 줄 끝이다.
    pub(crate) fn pos_at(&self, row: usize, col: usize) -> Pos {
        if row < self.hist {
            if row < self.conv_rows() {
                return Pos { zone: Zone::Conv(self.first + row), col };
            }
            return Pos { zone: Zone::Conv(self.total.saturating_sub(1)), col: self.cols.saturating_sub(1) };
        }
        Pos { zone: Zone::Bottom((row - self.hist).min(self.bottom.saturating_sub(1))), col }
    }

    /// 문서 선택 → 이 화면에 칠할 칸. 화면 위·아래로 넘친 쪽은 보이는 끝에서 자른다. 안 보이면 None.
    pub(crate) fn screen(&self, sel: &ChatSel) -> Option<Selection> {
        let (a, b) = sel.range();
        let shown = self.conv_rows();
        let last_col = self.cols.saturating_sub(1);
        let start = match a.zone {
            Zone::Conv(l) if l < self.first => (0, 0),
            Zone::Conv(l) if l - self.first < shown => (a.col, l - self.first),
            // 위로 굴려 둔 사이 화면 아래에 가린 대화 줄 — 바닥 첫 칸부터 칠한다.
            Zone::Conv(_) => (0, self.hist),
            Zone::Bottom(r) => (a.col, self.hist + r),
        };
        let end = match b.zone {
            Zone::Conv(l) if l < self.first => return None,
            Zone::Conv(l) if l - self.first < shown => (b.col, l - self.first),
            Zone::Conv(_) => (last_col, shown.checked_sub(1)?),
            Zone::Bottom(r) => (b.col, self.hist + r),
        };
        if (start.1, start.0) > (end.1, end.0) {
            return None;
        }
        Some(Selection { anchor: (start.0 as u16, start.1 as u16), end: (end.0 as u16, end.1 as u16) })
    }
}

/// 고른 글을 읽는 재료 — 쌓인 대화 줄과 이번 프레임 바닥 줄.
pub(crate) struct Doc<'a> {
    pub(crate) lines: &'a [classic::Line],
    pub(crate) bottom: &'a [Vec<GridCell>],
}

impl Doc<'_> {
    fn row(&self, zone: Zone) -> Option<&[GridCell]> {
        match zone {
            Zone::Conv(l) => self.lines.get(l).map(|l| l.cells.as_slice()),
            Zone::Bottom(r) => self.bottom.get(r).map(Vec::as_slice),
        }
    }

    fn next(&self, zone: Zone) -> Option<Zone> {
        match zone {
            Zone::Conv(l) if l + 1 < self.lines.len() => Some(Zone::Conv(l + 1)),
            Zone::Conv(_) => (!self.bottom.is_empty()).then_some(Zone::Bottom(0)),
            Zone::Bottom(r) => (r + 1 < self.bottom.len()).then_some(Zone::Bottom(r + 1)),
        }
    }

    fn prev(&self, zone: Zone) -> Option<Zone> {
        match zone {
            Zone::Conv(l) => l.checked_sub(1).map(Zone::Conv),
            Zone::Bottom(0) => self.lines.len().checked_sub(1).map(Zone::Conv),
            Zone::Bottom(r) => Some(Zone::Bottom(r - 1)),
        }
    }

    /// 고른 범위의 글 — 칸 폭에서 꺾인 줄은 잇고 들여쓰기·얼굴 자리는 뺀다(`classic::Pen::finish` 의 표식).
    pub(crate) fn text(&self, sel: &ChatSel) -> String {
        let (a, b) = sel.range();
        let mut rows: Vec<&[GridCell]> = Vec::new();
        let mut zone = Some(a.zone);
        while let Some(z) = zone.filter(|z| *z <= b.zone) {
            if let Some(row) = self.row(z) {
                rows.push(row);
            }
            zone = self.next(z);
        }
        let Some(last) = rows.len().checked_sub(1) else { return String::new() };
        crate::extract_rows(&rows, (a.col, 0), (b.col, last))
    }

    /// 두 번 누르면 그 단어, 세 번이면 그 줄 — 터미널 칸과 같은 경계(`App::word_span`).
    pub(crate) fn span(&self, pos: Pos, clicks: u8) -> (Pos, Pos) {
        let Some(row) = self.row(pos.zone) else { return (pos, pos) };
        let (s, e) = if clicks == 2 {
            let (s, e) = App::word_span(row, pos.col);
            (s as usize, e as usize)
        } else {
            (0, row.len().saturating_sub(1))
        };
        (Pos { zone: pos.zone, col: s }, Pos { zone: pos.zone, col: e })
    }

    /// 선택 끝을 한 칸·한 단어·한 줄 옮긴다(⇧←→ · ⇧⌥←→ · ⇧↑↓). 끝은 그 칸을 품으므로 글자 사이 커서로 바꿔
    /// 옮긴 뒤 되돌린다 — 앵커 뒤면 커서 앞 글자, 앞이면 커서 뒤 글자가 끝이다. 줄 끝을 넘으면 앞뒤 줄로 간다.
    pub(crate) fn step(&self, anchor: Pos, end: Pos, step: Step) -> Pos {
        let (right, word) = match step {
            Step::Line(down) => {
                let zone = if down { self.next(end.zone) } else { self.prev(end.zone) };
                return zone.map_or(end, |zone| Pos { zone, col: end.col });
            }
            Step::Char(right) => (right, false),
            Step::Word(right) => (right, true),
        };
        let chars_of = |zone: Zone| self.row(zone).map(row_chars).unwrap_or_default();
        let forward = end >= anchor;
        let mut zone = end.zone;
        let mut chars = chars_of(zone);
        let mut caret = match chars.iter().position(|(c, _)| *c >= end.col) {
            None => chars.len(),
            Some(i) if forward && chars[i].0 == end.col => i + 1,
            Some(i) => i,
        };
        if right {
            if caret >= chars.len() {
                let Some(next) = self.next(zone) else { return end };
                zone = next;
                chars = chars_of(zone);
                caret = 0;
            }
            caret = if word { next_word(&chars, caret) } else { caret + 1 }.min(chars.len());
        } else {
            if caret == 0 {
                let Some(prev) = self.prev(zone) else { return end };
                zone = prev;
                chars = chars_of(zone);
                caret = chars.len();
            }
            caret = if word { prev_word(&chars, caret) } else { caret.saturating_sub(1) };
        }
        let at = |i: usize| chars.get(i).map_or(0, |(c, _)| *c);
        let after = Pos { zone, col: chars.get(caret).map_or(usize::MAX, |(c, _)| *c) };
        if after > anchor {
            match caret.checked_sub(1) {
                Some(i) => Pos { zone, col: at(i) },
                // 앵커 뒤인데 줄 맨 앞 — 앞 줄 끝에서 끝난다.
                None => match self.prev(zone) {
                    Some(prev) => Pos { zone: prev, col: chars_of(prev).last().map_or(0, |(c, _)| *c) },
                    None => Pos { zone, col: 0 },
                },
            }
        } else {
            Pos { zone, col: at(caret.min(chars.len().saturating_sub(1))) }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Step {
    Char(bool),
    Word(bool),
    Line(bool),
}

/// 줄의 글자와 그 칸 — 넓은 글자 뒤 칸과 글 아닌 자리(들여쓰기·꺾인 줄 채움)는 뺀다. 줄 끝 빈칸도 뺀다.
fn row_chars(row: &[GridCell]) -> Vec<(usize, char)> {
    let mut out: Vec<(usize, char)> =
        row.iter().enumerate().filter(|(_, c)| c.ch != '\0' && !c.leading_wide_spacer).map(|(i, c)| (i, c.ch)).collect();
    while out.last().is_some_and(|(_, c)| c.is_whitespace()) {
        out.pop();
    }
    out
}

/// 카사뷰어 편집기(`markdown.rs prev_word_col`)와 같은 낱말 — 빈칸을 건너 같은 갈래 글자 한 덩이.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_word(chars: &[(usize, char)], i: usize) -> usize {
    let mut i = i.min(chars.len());
    while i > 0 && chars[i - 1].1.is_whitespace() {
        i -= 1;
    }
    let Some(w) = i.checked_sub(1).map(|at| is_word(chars[at].1)) else { return 0 };
    while i > 0 && !chars[i - 1].1.is_whitespace() && is_word(chars[i - 1].1) == w {
        i -= 1;
    }
    i
}

fn next_word(chars: &[(usize, char)], i: usize) -> usize {
    let n = chars.len();
    let mut i = i.min(n);
    while i < n && chars[i].1.is_whitespace() {
        i += 1;
    }
    let Some(w) = chars.get(i).map(|(_, c)| is_word(*c)) else { return n };
    while i < n && !chars[i].1.is_whitespace() && is_word(chars[i].1) == w {
        i += 1;
    }
    i
}

/// 입력칸 누름 → claude 커서를 옮길 화살표 수. 같은 줄의 커서 칸 `from` 에서 누른 칸 `to` 까지의 글자 수
/// (넓은 글자 한 자 = 1), 오른쪽이 +. 입력 글 끝을 넘은 자리는 글 끝, 글머리(`❯ `) 위는 글 처음으로 붙인다.
pub(crate) fn caret_steps(row: &[GridCell], from: usize, to: usize) -> isize {
    let start = text_start(row);
    let end = row
        .iter()
        .rposition(|c| !matches!(c.ch, ' ' | '\0' | '\u{a0}') && !c.dim)
        .map_or(start, |i| i + 1)
        .max(start);
    // 줄 끝 빈칸을 친 뒤면 커서가 마지막 글자보다 뒤에 있다 — 그 빈칸도 입력 글이다.
    let end = end.max(from.min(row.len()));
    let to = to.clamp(start, end);
    let from = from.clamp(start, end);
    let glyphs = |a: usize, b: usize| row[a..b].iter().filter(|c| c.ch != '\0').count() as isize;
    if to >= from {
        glyphs(from, to)
    } else {
        -glyphs(to, from)
    }
}

/// 입력 줄의 글이 시작하는 칸 — 글머리 `❯`·`›` 뒤 한 칸. 이어지는 줄은 그 들여쓰기 뒤다.
fn text_start(row: &[GridCell]) -> usize {
    match row.iter().position(|c| matches!(c.ch, '❯' | '›' | '>')) {
        Some(at) => at + 2,
        None => row.iter().position(|c| !matches!(c.ch, ' ' | '\0' | '│')).unwrap_or(0),
    }
    .min(row.len())
}

impl App {
    /// 대화 칸의 문서 선택을 지금 화면 칸으로 — 그리기가 쓴다. 대화 선택이 없으면 None.
    pub(crate) fn chat_view_selection(&self, id: &str) -> Option<Selection> {
        let pane = self.chat_view.panes.get(id)?;
        let sel = pane.sel.as_ref()?;
        let view = self.chat_view.drawn.borrow().get(id)?.view;
        (view.cols == sel.cols).then(|| view.screen(sel)).flatten()
    }

    /// 대화 칸에 고른 글이 있나 — ⌘C 가 이 보기로 갈지의 판정.
    pub(crate) fn chat_view_has_selection(&self) -> bool {
        self.target_pane()
            .and_then(|id| self.chat_view.panes.get(&id))
            .and_then(|p| p.sel.as_ref())
            .is_some_and(|s| s.anchor != s.end || s.clicks > 1)
    }

    /// 활성 대화 칸에서 고른 글. 화면 밖으로 굴러간 줄까지 쌓인 대화에서 읽는다.
    pub(crate) fn chat_view_selection_text(&self) -> Option<String> {
        let id = self.target_pane()?;
        let pane = self.chat_view.panes.get(&id)?;
        let sel = pane.sel.as_ref()?;
        let drawn = self.chat_view.drawn.borrow();
        let d = drawn.get(&id)?;
        if d.view.cols != sel.cols {
            return None;
        }
        let cache = pane.cache.borrow();
        let doc = Doc { lines: &cache.lines, bottom: d.rows.get(d.view.hist..).unwrap_or_default() };
        Some(doc.text(sel)).filter(|t| !t.is_empty())
    }

    /// ⌘A — 대화 첫 줄부터 끝 줄까지(바닥 입력칸은 빼고). 대화 칸이 아니면 false.
    pub(crate) fn chat_view_select_all(&mut self) -> bool {
        let Some(id) = self.target_pane().filter(|id| self.chat_view_showing(id)) else { return false };
        let Some(view) = self.chat_view.drawn.borrow().get(&id).map(|d| d.view) else { return false };
        let Some(pane) = self.chat_view.panes.get_mut(&id) else { return false };
        let Some(last) = view.total.checked_sub(1) else { return true };
        let mut sel = ChatSel::at(Pos { zone: Zone::Conv(0), col: 0 }, (0.0, 0.0), view.cols);
        sel.end = Pos { zone: Zone::Conv(last), col: view.cols.saturating_sub(1) };
        sel.dragging = false;
        sel.moved = true;
        pane.sel = Some(sel);
        self.selection = None;
        self.chrome_dirty = true;
        true
    }

    /// 대화 칸 본문 누름 — 접힌 줄이면 펼치고, 아니면 선택을 잡는다(⇧ 면 있던 선택의 끝만 옮긴다).
    /// 그 칸 밖이면 false.
    pub(crate) fn chat_view_press(&mut self, x: f32, y: f32) -> bool {
        let Some((id, row, col)) = self.chat_view_hit(x, y) else { return false };
        // ⌘클릭은 링크 열기 몫이다(`links::click_modifier`).
        if crate::links::click_modifier(self.modifiers) && self.link_hit(x, y).is_some() {
            return false;
        }
        self.focus_pane(&id);
        self.selection = None;
        self.drag_anchor = None;
        let (view, toggle) = {
            let drawn = self.chat_view.drawn.borrow();
            let Some(d) = drawn.get(&id) else { return false };
            (d.view, d.toggles.get(row).copied().flatten())
        };
        let shift = self.modifiers.shift_key();
        let now = Instant::now();
        let clicks = match self.cell_click {
            Some((t, cell, n)) if cell == (col as u16, row as u16) && now.duration_since(t).as_millis() < 400 => n % 3 + 1,
            _ => 1,
        };
        self.cell_click = Some((now, (col as u16, row as u16), clicks));
        let Some(pane) = self.chat_view.panes.get_mut(&id) else { return false };
        if let (Some(key), false, 1) = (toggle, shift, clicks) {
            if !pane.open.remove(&key) {
                pane.open.insert(key);
            }
            // 펼치면 아래 줄 번호가 밀린다.
            pane.sel = None;
            self.chrome_dirty = true;
            return true;
        }
        let pos = view.pos_at(row, col);
        match pane.sel.as_mut().filter(|s| shift && s.cols == view.cols) {
            Some(sel) => {
                sel.end = pos;
                sel.dragging = true;
                sel.moved = true;
            }
            None => {
                let mut sel = ChatSel::at(pos, (x, y), view.cols);
                if clicks > 1 {
                    let cache = pane.cache.borrow();
                    let drawn = self.chat_view.drawn.borrow();
                    let bottom = drawn.get(&id).and_then(|d| d.rows.get(view.hist..)).unwrap_or_default();
                    let (s, e) = Doc { lines: &cache.lines, bottom }.span(pos, clicks);
                    sel.anchor = s;
                    sel.end = e;
                    sel.clicks = clicks;
                }
                pane.sel = Some(sel);
            }
        }
        self.chrome_dirty = true;
        true
    }

    /// 누른 채 끌기 — 끝을 커서 칸으로. 대화 줄에서 잡았으면 대화 안에, 바닥에서 잡았으면 바닥 안에 머문다.
    /// 끌고 있지 않으면 false.
    pub(crate) fn chat_view_drag(&mut self) -> bool {
        let Some(id) = self.chat_view.panes.iter().find(|(_, p)| p.sel.as_ref().is_some_and(|s| s.dragging)).map(|(id, _)| id.clone())
        else {
            return false;
        };
        let (x, y) = self.cursor_px;
        let Some((rect, cell, view)) = self.chat_view.drawn.borrow().get(&id).map(|d| (d.rect, d.cell, d.view)) else { return true };
        let Some(sel) = self.chat_view.panes.get_mut(&id).and_then(|p| p.sel.as_mut()) else { return true };
        if (x - sel.press.0).abs() > 3.0 || (y - sel.press.1).abs() > 3.0 {
            sel.moved = true;
        }
        if sel.clicks > 1 && !sel.moved {
            return true;
        }
        let col = (((x - rect.0) / cell.0).max(0.0) as usize).min(view.cols.saturating_sub(1));
        let raw = ((y - rect.1) / cell.1).floor();
        let row = match sel.anchor.zone {
            Zone::Conv(_) => raw.clamp(0.0, view.hist.saturating_sub(1) as f32) as usize,
            Zone::Bottom(_) => raw.clamp(view.hist as f32, (view.hist + view.bottom).saturating_sub(1) as f32) as usize,
        };
        sel.end = view.pos_at(row, col);
        self.chrome_dirty = true;
        true
    }

    /// 손을 뗐다. 안 움직인 한 번 누름은 선택이 아니라 클릭 — 지우고, 입력칸 위였으면 커서를 그리로 옮긴다.
    /// 대화 칸 끌기가 아니었으면 false.
    pub(crate) fn chat_view_release(&mut self) -> bool {
        let Some(id) = self.chat_view.panes.iter().find(|(_, p)| p.sel.as_ref().is_some_and(|s| s.dragging)).map(|(id, _)| id.clone())
        else {
            return false;
        };
        let Some(pane) = self.chat_view.panes.get_mut(&id) else { return true };
        let Some(sel) = pane.sel.as_mut() else { return true };
        sel.dragging = false;
        if sel.moved || sel.clicks > 1 {
            self.chrome_dirty = true;
            return true;
        }
        let at = sel.anchor;
        pane.sel = None;
        self.chrome_dirty = true;
        if let Zone::Bottom(r) = at.zone {
            self.chat_view_caret_to(&id, r, at.col);
        }
        true
    }

    /// 바닥 `row` 줄 `col` 칸이 입력칸이면 claude 커서를 그리로 옮긴다 — 같은 줄은 ←/→ 한 번에, 다른 줄이면
    /// ↑/↓ 를 먼저 보내고 커서가 그 줄에 서면 `pump_chat_caret` 이 ←/→ 로 마무리한다.
    fn chat_view_caret_to(&mut self, id: &str, row: usize, col: usize) {
        let (hist, caret, input) = {
            let drawn = self.chat_view.drawn.borrow();
            let Some(d) = drawn.get(id) else { return };
            let bottom = d.rows.get(d.view.hist..).unwrap_or_default().to_vec();
            let input = crate::screenread::prompt_box(&bottom).map(|pb| pb.rows());
            (d.view.hist, d.caret, input)
        };
        let Some(input) = input.filter(|rows| rows.contains(&row)) else { return };
        let Some((crow, _)) = caret.filter(|(r, _)| (input.start + hist..input.end + hist).contains(r)) else { return };
        let target = hist + row;
        if let Some(pane) = self.chat_view.panes.get_mut(id) {
            pane.caret_goal = Some(CaretGoal { row: target, col, until: Instant::now() + CARET_WAIT, vertical: false });
        }
        if crow == target {
            self.pump_chat_caret(id);
        } else {
            let lines = target as isize - crow as isize;
            self.send_arrows(if lines > 0 { 'B' } else { 'A' }, lines.unsigned_abs());
            if let Some(goal) = self.chat_view.panes.get_mut(id).and_then(|p| p.caret_goal.as_mut()) {
                goal.vertical = true;
            }
        }
    }

    /// 입력칸 누름의 마무리 — 커서가 그 줄에 섰으면 ←/→ 를 보낸다. 늦으면 그만둔다.
    pub(crate) fn pump_chat_caret(&mut self, id: &str) {
        let Some(goal) = self.chat_view.panes.get(id).and_then(|p| p.caret_goal) else { return };
        if Instant::now() > goal.until {
            if let Some(p) = self.chat_view.panes.get_mut(id) {
                p.caret_goal = None;
            }
            return;
        }
        let steps = {
            let drawn = self.chat_view.drawn.borrow();
            let Some(d) = drawn.get(id) else { return };
            let Some((crow, ccol)) = d.caret.filter(|(r, _)| *r == goal.row) else { return };
            d.rows.get(crow).map(|row| caret_steps(row, ccol, goal.col))
        };
        if let Some(p) = self.chat_view.panes.get_mut(id) {
            p.caret_goal = None;
        }
        if let Some(n) = steps.filter(|n| *n != 0) {
            self.send_arrows(if n > 0 { 'C' } else { 'D' }, n.unsigned_abs());
        }
    }

    /// 화살표 `n` 번 — 학생 TUI 가 켠 DECCKM 을 따른다(`forward_key` 와 같은 판정).
    fn send_arrows(&mut self, letter: char, n: usize) {
        let app_cursor = self.ws.lock().ok().and_then(|ws| ws.active().and_then(|p| p.term()).map(|t| t.app_cursor)).unwrap_or(false);
        let one = if app_cursor { format!("\x1bO{letter}") } else { format!("\x1b[{letter}") };
        self.send_bytes(one.repeat(n).as_bytes());
    }

    /// 끄는 손이 본문 위·아래 밖에 있으면 대화를 그쪽으로 굴리고 끝을 따라 옮긴다 — 가만히 쥐고 있어도 계속 간다.
    pub(crate) fn pump_chat_drag_scroll(&mut self) {
        let Some(id) = self.chat_view.panes.iter().find(|(_, p)| p.sel.as_ref().is_some_and(|s| s.dragging && matches!(s.anchor.zone, Zone::Conv(_)))).map(|(id, _)| id.clone())
        else {
            return;
        };
        let Some((rect, cell, view)) = self.chat_view.drawn.borrow().get(&id).map(|d| (d.rect, d.cell, d.view)) else { return };
        let y = self.cursor_px.1;
        let top = rect.1;
        let bottom = rect.1 + view.hist as f32 * cell.1;
        let over = if y < top { (top - y) / cell.1 } else if y > bottom { -((y - bottom) / cell.1) } else { 0.0 };
        if over == 0.0 {
            return;
        }
        let now = Instant::now();
        let Some(pane) = self.chat_view.panes.get_mut(&id) else { return };
        if pane.drag_tick.is_some_and(|t| now.duration_since(t) < DRAG_SCROLL_EVERY) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        pane.drag_tick = Some(now);
        // 멀리 끌수록 빨리 — 한 번에 1줄, 본문에서 두 줄 멀어질 때마다 한 줄 더.
        let lines = 1 + (over.abs() / 2.0) as isize;
        pane.scroll_by(if over > 0.0 { lines } else { -lines });
        if let Some(sel) = pane.sel.as_mut() {
            sel.moved = true;
            let shown = view.total.saturating_sub(view.first).min(view.hist);
            // 다음 프레임의 첫 줄을 미리 잡아 끝을 화면 끝 줄에 둔다.
            let max_first = view.total.saturating_sub(view.hist);
            let first = if over > 0.0 {
                view.first.saturating_sub(lines as usize)
            } else {
                (view.first + lines as usize).min(max_first)
            };
            sel.end = if over > 0.0 {
                Pos { zone: Zone::Conv(first), col: 0 }
            } else {
                let last = (first + shown).min(view.total).saturating_sub(1);
                Pos { zone: Zone::Conv(last), col: view.cols.saturating_sub(1) }
            };
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 대화 선택 끝을 키로 옮긴다(⇧←→ · ⇧⌥←→ · ⇧↑↓). 대화 선택이 없으면 false — 키는 학생 TUI 로 간다.
    pub(crate) fn chat_view_extend(&mut self, id: &str, step: Step) -> bool {
        let Some(pane) = self.chat_view.panes.get_mut(id) else { return false };
        let drawn = self.chat_view.drawn.borrow();
        let Some(d) = drawn.get(id) else { return false };
        let Some(sel) = pane.sel.as_mut().filter(|s| !s.dragging && s.cols == d.view.cols && (s.anchor != s.end || s.clicks > 1)) else {
            return false;
        };
        let cache = pane.cache.borrow();
        let doc = Doc { lines: &cache.lines, bottom: d.rows.get(d.view.hist..).unwrap_or_default() };
        sel.end = doc.step(sel.anchor, sel.end, step);
        // 끝이 화면 밖으로 나가면 따라 굴린다.
        if let Zone::Conv(l) = sel.end.zone {
            let view = d.view;
            let back = pane.back.get();
            if l < view.first {
                pane.back.set(back + (view.first - l));
            } else if l >= view.first + view.hist {
                pane.back.set(back.saturating_sub(l + 1 - (view.first + view.hist)));
            }
        }
        drop(cache);
        drop(drawn);
        self.chrome_dirty = true;
        true
    }

    /// ⌥↑ / ⌥↓ — 지금 맨 위 줄보다 위·아래의 내 말 줄을 맨 위에 세운다. 아래로 더 없으면 맨 아래로.
    pub(crate) fn chat_view_jump_turn(&mut self, id: &str, down: bool) -> bool {
        let Some(first) = self.chat_view.drawn.borrow().get(id).map(|d| d.view.first) else { return false };
        let Some(pane) = self.chat_view.panes.get(id) else { return false };
        let items: Vec<usize> = pane.feed.lock().map_or_else(
            |_| Vec::new(),
            |f| f.conv.items.iter().enumerate().filter(|(_, it)| matches!(it, parse::Item::Bubble { user: true, .. })).map(|(i, _)| i).collect(),
        );
        let lines: Vec<(usize, usize)> = {
            let cache = pane.cache.borrow();
            items.into_iter().filter_map(|i| cache.line_of(i).map(|l| (i, l))).collect()
        };
        let target = if down {
            lines.iter().find(|(_, l)| *l > first)
        } else {
            lines.iter().rev().find(|(_, l)| *l < first)
        };
        match target {
            Some((item, _)) => pane.jump.set(Some(*item)),
            None if down => pane.back.set(0),
            None => {}
        }
        self.chrome_dirty = true;
        true
    }
}

/// 입력칸 누름이 ↑/↓ 뒤 커서가 그 줄에 서기를 기다리는 끝.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CaretGoal {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) until: Instant,
    pub(crate) vertical: bool,
}

const CARET_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// 본문 밖으로 끌고 있을 때 대화를 한 번 더 굴리는 간격.
const DRAG_SCROLL_EVERY: std::time::Duration = std::time::Duration::from_millis(40);

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, width: usize) -> Vec<GridCell> {
        let mut out = Vec::new();
        for ch in text.chars() {
            let mut c = GridCell::blank();
            c.ch = ch;
            out.push(c);
            if unicode_width::UnicodeWidthChar::width(ch) == Some(2) {
                let mut s = GridCell::blank();
                s.ch = '\0';
                out.push(s);
            }
        }
        out.resize(width, GridCell::blank());
        out
    }

    fn line(text: &str, width: usize) -> classic::Line {
        classic::Line { cells: row(text, width), toggle: None, face: None }
    }

    fn sel(a: Pos, b: Pos) -> ChatSel {
        let mut s = ChatSel::at(a, (0.0, 0.0), 20);
        s.end = b;
        s
    }

    fn conv(l: usize, col: usize) -> Pos {
        Pos { zone: Zone::Conv(l), col }
    }

    #[test]
    fn a_selection_scrolled_half_off_screen_is_cut_at_the_visible_edge() {
        let view = View { first: 10, hist: 5, total: 40, bottom: 3, cols: 20 };
        let s = sel(conv(2, 4), conv(12, 6));
        assert_eq!(view.screen(&s), Some(Selection { anchor: (0, 0), end: (6, 2) }));
        let s = sel(conv(12, 6), conv(30, 1));
        assert_eq!(view.screen(&s), Some(Selection { anchor: (6, 2), end: (19, 4) }), "아래로 넘치면 보이는 대화 끝 줄까지");
        assert_eq!(view.screen(&sel(conv(1, 0), conv(3, 0))), None, "통째로 위에 있으면 안 칠한다");
        let s = sel(conv(30, 0), Pos { zone: Zone::Bottom(1), col: 3 });
        assert_eq!(view.screen(&s), Some(Selection { anchor: (0, 5), end: (3, 6) }), "가린 대화 줄에서 바닥까지면 바닥 첫 칸부터");
        assert_eq!(view.pos_at(6, 2), Pos { zone: Zone::Bottom(1), col: 2 });
        let short = View { first: 0, hist: 5, total: 2, bottom: 3, cols: 20 };
        assert_eq!(short.pos_at(4, 3), conv(1, 19), "짧은 대화 아래 빈 줄은 마지막 줄 끝");
    }

    #[test]
    fn text_spans_lines_off_screen_and_the_bottom() {
        let lines = vec![line("첫 줄 글", 12), line("둘째 줄", 12), line("셋째", 12)];
        let bottom = vec![row("❯ 입력 중", 12)];
        let doc = Doc { lines: &lines, bottom: &bottom };
        assert_eq!(doc.text(&sel(conv(0, 3), conv(1, 3))), "줄 글\n둘째");
        assert_eq!(doc.text(&sel(conv(2, 0), Pos { zone: Zone::Bottom(0), col: 5 })), "셋째\n❯ 입력");
    }

    #[test]
    fn shift_option_arrows_walk_words_across_lines() {
        let lines = vec![line("hello world foo", 20), line("다음 줄 bar", 20)];
        let doc = Doc { lines: &lines, bottom: &[] };
        let a = conv(0, 0);
        assert_eq!(doc.step(a, conv(0, 0), Step::Word(true)), conv(0, 4), "hello 끝 글자까지");
        assert_eq!(doc.step(a, conv(0, 4), Step::Word(true)), conv(0, 10), "다음 낱말 world 끝");
        assert_eq!(doc.step(a, conv(0, 14), Step::Word(true)), conv(1, 2), "줄 끝 넘어 다음 줄 첫 낱말 끝 글자(넓은 글자)");
        assert_eq!(doc.step(a, conv(1, 0), Step::Word(false)), conv(0, 14), "줄 맨 앞으로 돌아오면 앞 줄 끝에서 끝난다");
        assert_eq!(doc.step(a, conv(0, 14), Step::Word(false)), conv(0, 11), "foo 앞 — 끝은 그 앞 빈칸");
        let back = conv(0, 14);
        assert_eq!(doc.step(back, conv(0, 6), Step::Word(false)), conv(0, 0), "앵커 앞으로 고르면 낱말 첫 글자가 끝");
        assert_eq!(doc.step(a, conv(1, 0), Step::Char(true)), conv(1, 2), "넓은 글자 뒤 칸은 건너뛴다");
        assert_eq!(doc.step(a, conv(1, 2), Step::Char(false)), conv(1, 0));
        assert_eq!(doc.step(a, conv(0, 3), Step::Line(true)), conv(1, 3));
        assert_eq!(doc.step(a, conv(0, 3), Step::Line(false)), conv(0, 3), "첫 줄 위는 그대로");
    }

    #[test]
    fn double_and_triple_clicks_take_the_word_and_the_line() {
        let lines = vec![line("hello world foo", 20)];
        let doc = Doc { lines: &lines, bottom: &[] };
        assert_eq!(doc.span(conv(0, 7), 2), (conv(0, 6), conv(0, 10)));
        assert_eq!(doc.span(conv(0, 7), 3), (conv(0, 0), conv(0, 19)));
    }

    #[test]
    fn a_click_in_the_input_counts_glyphs_to_the_caret() {
        let r = row("❯ 안녕 hello", 30);
        // 커서는 「hello」 끝(❯·빈칸 2 + 안녕 4 + 빈칸 1 + hello 5 = 12).
        assert_eq!(caret_steps(&r, 12, 2), -8, "글 처음으로 — 안·녕·빈칸·h·e·l·l·o");
        assert_eq!(caret_steps(&r, 12, 4), -7, "녕 앞");
        assert_eq!(caret_steps(&r, 12, 5), -6, "넓은 글자 오른쪽 반을 누르면 그 글자 뒤");
        assert_eq!(caret_steps(&r, 2, 25), 8, "글 끝 너머는 글 끝");
        assert_eq!(caret_steps(&r, 12, 0), -8, "글머리 위는 글 처음");
        let mut ghost = row("❯ hi there", 30);
        ghost.iter_mut().skip(5).for_each(|c| c.dim = true);
        assert_eq!(caret_steps(&ghost, 4, 9), 0, "흐린 제안 글은 입력 글이 아니다");
    }
}
