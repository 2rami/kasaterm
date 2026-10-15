//! PC 대화 보기의 대화 줄 — claude classic 렌더러(`CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1`) 화면을 기록으로
//! 다시 늘어놓는다. 새로 디자인하지 않는다: 요소·순서·글리프·색·들여쓰기는 classic 칸(claude 2.1.299 +
//! 카사텀 student-face mod)이 내는 그대로이고, 하는 일은 칸 폭에 맞춰 접어 한 줄기로 쌓는 것뿐이다.
//! 고칠 때는 같은 대화를 classic 칸과 나란히 찍어 대 본다(docs/design.md 「PC 대화 보기」).

use super::parse::{Conversation, Item, Row};
use crate::{GridCell, MdAlign, MdBlock, MdSpan};
use kasa_bridge::screen::Color;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use unicode_width::UnicodeWidthChar;

/// 줄 맨 앞에 얹는 학생 얼굴(student-face mod 의 그림 자리). 칸에는 빈칸만 둔다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Face {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub(crate) cells: Vec<GridCell>,
    /// 누르면 펼치고 접는 줄 — 그 묶음의 열쇠(첫 칸 번호).
    pub(crate) toggle: Option<usize>,
    pub(crate) face: Option<Face>,
}

/// 칸마다 달라지는 것 — 학생 얼굴이 있나, 턴 끝 줄의 학생 색.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Look {
    pub(crate) face: bool,
    pub(crate) student: Option<[u8; 4]>,
    /// 진짜 화면에서 본 하네스 배너 줄 — 있으면 기록으로 짓는 배너 대신 그대로 세운다(첫 말 전 새 세션도).
    pub(crate) banner: Option<Vec<Vec<GridCell>>>,
}

/// classic 이 쓰는 색. 카사텀이 굽는 claude 테마(`socket::write_claude_custom_theme`)가 덮는 자리는 같은
/// 팔레트 함수에서, 안 덮는 자리(내 말 띠·글 속 코드·로고·diff)는 claude 어두운 바탕값 그대로다.
struct Ink {
    text: Col,
    dim: Col,
    ok: Col,
    err: Col,
    band: Col,
    code: Col,
    logo: Col,
    logo_bg: Col,
    add_bg: Col,
    add_mark: Col,
    del_bg: Col,
    del_mark: Col,
}

/// 격자 색(`Color`)의 복사되는 꼴 — 글자 하나마다 꾸밈을 들고 다닌다.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Col {
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl Col {
    pub(super) fn color(self) -> Color {
        match self {
            Col::Idx(i) => Color::Idx(i),
            Col::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

pub(super) fn rgb(c: [u8; 4]) -> Col {
    Col::Rgb(c[0], c[1], c[2])
}

impl Ink {
    fn now() -> Self {
        Self {
            text: rgb(crate::theme::text()),
            dim: rgb(crate::theme::text_mute()),
            ok: rgb(crate::theme::success()),
            err: rgb(crate::theme::danger()),
            band: Col::Idx(237),
            code: Col::Idx(153),
            logo: Col::Idx(174),
            logo_bg: Col::Idx(16),
            add_bg: Col::Idx(22),
            add_mark: Col::Idx(77),
            del_bg: Col::Idx(52),
            del_mark: Col::Idx(203),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct St {
    pub(super) fg: Option<Col>,
    pub(super) bg: Option<Col>,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) dim: bool,
    pub(super) underline: bool,
}

impl St {
    pub(super) fn fg(c: Col) -> Self {
        Self { fg: Some(c), ..Default::default() }
    }
    pub(super) fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub(super) fn on(mut self, bg: Col) -> Self {
        self.bg = Some(bg);
        self
    }
    pub(super) fn cell(&self, ch: char) -> GridCell {
        let mut c = GridCell::blank();
        c.ch = ch;
        if let Some(fg) = self.fg {
            c.fg = fg.color();
        }
        if let Some(bg) = self.bg {
            c.bg = bg.color();
        }
        c.bold = self.bold;
        c.italic = self.italic;
        c.dim = self.dim;
        c.underline = self.underline;
        c
    }
}

pub(super) type Span = (String, St);

pub(super) fn plain(text: impl Into<String>, st: St) -> Vec<Span> {
    vec![(text.into(), st)]
}

/// 한 줄을 칸 폭으로 짓는다 — 넓은 글자는 다음 칸에 `'\0'` 자리지기를 둔다(격자와 같은 약속).
struct Pen {
    width: usize,
    lines: Vec<Vec<GridCell>>,
    cur: Vec<GridCell>,
    col: usize,
    /// 지금 줄의 글이 시작하는 칸 — 머리(첫 줄)나 들여쓰기(다음 줄) 바로 뒤.
    start: usize,
    cont: Vec<GridCell>,
    fill: Option<Col>,
}

impl Pen {
    fn new(width: usize, first: Vec<GridCell>, cont: Vec<GridCell>, fill: Option<Col>) -> Self {
        let col = first.len();
        Self { width: width.max(4), lines: Vec::new(), cur: first, col, start: col, cont, fill }
    }

    fn put(&mut self, ch: char, st: &St) {
        let w = ch.width().unwrap_or(0);
        if w == 0 {
            return;
        }
        if self.col + w > self.width {
            self.newline();
        }
        self.cur.push(st.cell(ch));
        if w == 2 {
            let mut spacer = st.cell('\0');
            spacer.ch = '\0';
            self.cur.push(spacer);
        }
        self.col += w;
    }

    fn newline(&mut self) {
        let done = std::mem::replace(&mut self.cur, self.cont.clone());
        self.lines.push(done);
        self.col = self.cont.len();
        self.start = self.col;
    }

    /// 낱말 단위로 접는다(Ink 의 wrap 과 같이 빈칸에서). 칸보다 긴 낱말은 글자로 끊는다.
    fn flow(&mut self, spans: &[Span]) {
        let mut words: Vec<Vec<(char, St)>> = Vec::new();
        let mut word: Vec<(char, St)> = Vec::new();
        for (text, st) in spans {
            for ch in text.chars() {
                if ch == '\n' {
                    if !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    words.push(vec![('\n', *st)]);
                } else if ch == ' ' {
                    word.push((ch, *st));
                    words.push(std::mem::take(&mut word));
                } else {
                    word.push((ch, *st));
                }
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        for w in words {
            if w.len() == 1 && w[0].0 == '\n' {
                self.newline();
                continue;
            }
            let body: usize = w.iter().filter(|(c, _)| *c != ' ').map(|(c, _)| c.width().unwrap_or(0)).sum();
            let room = self.width.saturating_sub(self.col);
            if body > room && self.col > self.start && body <= self.width.saturating_sub(self.cont.len()) {
                self.newline();
            }
            for (ch, st) in w {
                if ch == ' ' {
                    // 줄 끝에 걸린 빈칸은 줄바꿈이 되고, 접힌 줄 머리의 빈칸은 버린다.
                    if self.col + 1 > self.width {
                        self.newline();
                        continue;
                    }
                    if self.col == self.start && !self.lines.is_empty() {
                        continue;
                    }
                }
                self.put(ch, &st);
            }
        }
    }

    fn finish(mut self) -> Vec<Vec<GridCell>> {
        self.lines.push(std::mem::take(&mut self.cur));
        let width = self.width;
        let fill = self.fill;
        self.lines
            .into_iter()
            .map(|mut row| {
                // 접힌 줄 끝의 빈칸도 셀로 남지 않게 다듬는다 — 띠 색은 아래에서 다시 깐다.
                while row.len() > width {
                    row.pop();
                }
                let mut pad = GridCell::blank();
                if let Some(bg) = fill {
                    pad.bg = bg.color();
                }
                row.resize(width, pad);
                row
            })
            .collect()
    }
}

pub(super) fn cells(text: &str, st: St) -> Vec<GridCell> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if w == 0 {
            continue;
        }
        out.push(st.cell(ch));
        if w == 2 {
            out.push(st.cell('\0'));
        }
    }
    out
}

pub(super) fn blanks(n: usize, st: St) -> Vec<GridCell> {
    vec![st.cell(' '); n]
}

pub(super) fn text_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// `first` 줄 머리 + 나머지는 `indent` 칸 들여 접는다.
pub(super) fn wrap(width: usize, first: Vec<GridCell>, indent: usize, spans: &[Span], fill: Option<Col>) -> Vec<Vec<GridCell>> {
    let cont = match fill {
        Some(bg) => blanks(indent, St::default().on(bg)),
        None => blanks(indent, St::default()),
    };
    let mut pen = Pen::new(width, first, cont, fill);
    pen.flow(spans);
    pen.finish()
}

pub(super) fn line(cells: Vec<Vec<GridCell>>) -> Vec<Line> {
    cells.into_iter().map(|cells| Line { cells, toggle: None, face: None }).collect()
}

fn blank_line(width: usize) -> Line {
    Line { cells: vec![GridCell::blank(); width], toggle: None, face: None }
}

/// `코드`·**굵게** 를 classic 처럼 풀어 쓴다 — 사람 말 안의 백틱 코드는 색만 바꾸고 백틱은 감춘다.
fn md_spans(spans: &[MdSpan], base: St, ink: &Ink) -> Vec<Span> {
    spans
        .iter()
        .map(|s| {
            let mut st = base;
            if s.bold {
                st.bold = true;
            }
            if s.italic {
                st.italic = true;
            }
            if s.code {
                st.fg = Some(ink.code);
            }
            if s.link.is_some() {
                st.underline = true;
            }
            (s.text.clone(), st)
        })
        .collect()
}

fn inline_code(text: &str, base: St, ink: &Ink) -> Vec<Span> {
    let mut out = Vec::new();
    let mut code = false;
    for (i, part) in text.split('`').enumerate() {
        if i > 0 {
            code = !code;
        }
        if part.is_empty() {
            continue;
        }
        let st = if code { St { fg: Some(ink.code), ..base } } else { base };
        out.push((part.to_string(), st));
    }
    if text.matches('`').count() % 2 == 1 {
        return plain(text, base);
    }
    out
}

/// 답 한 칸의 마크다운을 classic 처럼 — 문단·표·코드 사이에 빈 줄, 목록 항목 사이엔 없음.
fn markdown(text: &str, width: usize, indent: usize, ink: &Ink) -> Vec<Vec<GridCell>> {
    let (blocks, _) = crate::parse_markdown(text);
    let body = St::default();
    let mut out: Vec<Vec<GridCell>> = Vec::new();
    let mut prev_list = false;
    let pad = |n: usize| blanks(n, St::default());
    for block in &blocks {
        let list = matches!(block, MdBlock::ListItem { .. });
        if !out.is_empty() && !(list && prev_list) {
            out.push(vec![GridCell::blank(); width]);
        }
        prev_list = list;
        match block {
            MdBlock::Heading { level, spans } => {
                let mut st = body.bold();
                if *level == 1 {
                    st.italic = true;
                    st.underline = true;
                }
                out.extend(wrap(width, pad(indent), indent, &md_spans(spans, st, ink), None));
            }
            MdBlock::Para { spans } => out.extend(wrap(width, pad(indent), indent, &md_spans(spans, body, ink), None)),
            MdBlock::ListItem { depth, marker, spans, task } => {
                let mark = match task {
                    Some(true) => "☒".to_string(),
                    Some(false) => "☐".to_string(),
                    // 문서 칸은 글머리를 「•」로 바꾸지만 classic 은 원문의 「-」 그대로다.
                    None if marker == "•" => "-".to_string(),
                    None => marker.clone(),
                };
                let lead = indent + *depth as usize * 2;
                let mut first = pad(lead);
                first.extend(cells(&format!("{mark} "), body));
                let hang = lead + text_width(&mark) + 1;
                out.extend(wrap(width, first, hang, &md_spans(spans, body, ink), None));
            }
            MdBlock::Code { code, .. } => {
                for raw in code.trim_end_matches('\n').split('\n') {
                    let raw = raw.replace('\t', "    ");
                    out.extend(wrap(width, pad(indent), indent, &plain(raw, body), None));
                }
            }
            MdBlock::Quote { spans } | MdBlock::Callout { spans, .. } => {
                let mut first = pad(indent);
                first.extend(cells("▎ ", St::fg(ink.dim)));
                let st = St { italic: true, fg: Some(ink.dim), ..Default::default() };
                out.extend(wrap(width, first, indent + 2, &md_spans(spans, st, ink), None));
            }
            MdBlock::Rule => {
                let mut row = pad(indent);
                row.extend(cells(&"─".repeat(width.saturating_sub(indent).min(80)), St::fg(ink.dim)));
                row.resize(width, GridCell::blank());
                out.push(row);
            }
            MdBlock::Meta { rows } => {
                for (k, v) in rows {
                    out.extend(wrap(width, pad(indent), indent, &plain(format!("{k}: {v}"), body), None));
                }
            }
            MdBlock::Image { alt, .. } => {
                let text = if alt.is_empty() { "[Image]".to_string() } else { format!("[Image: {alt}]") };
                out.extend(wrap(width, pad(indent), indent, &plain(text, body), None));
            }
            MdBlock::Table { head, rows, align } => out.extend(table(width, indent, head, rows, align, ink)),
        }
    }
    out
}

pub(super) fn cell_text(spans: &[MdSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect::<String>().replace('\n', " ")
}

/// classic 표 — 줄마다 가름선이 서는 상자. 칸 폭보다 넓으면 Claude Code 처럼 칸마다 글을 접는다. 예전엔
/// 오른쪽을 그대로 잘라, 미니 유우카 칸 거울에서 셋째 칸이 통째로 사라지고 글이 중간에 끊겼다(2026-10-15).
/// 칸 나누기·머리글 가운데·짧은 칸 세로 가운데는 2.1.300 을 90칸에서 잰 꼴이다.
fn table(width: usize, indent: usize, head: &[Vec<MdSpan>], rows: &[Vec<Vec<MdSpan>>], align: &[MdAlign], ink: &Ink) -> Vec<Vec<GridCell>> {
    let mut all: Vec<&[Vec<MdSpan>]> = Vec::new();
    if !head.is_empty() {
        all.push(head);
    }
    all.extend(rows.iter().map(Vec::as_slice));
    let ncol = all.iter().map(|r| r.len()).max().unwrap_or(0);
    if ncol == 0 {
        return Vec::new();
    }
    let mut ideal = vec![1usize; ncol];
    let mut least = vec![TABLE_MIN_COL; ncol];
    for r in &all {
        for (i, c) in r.iter().enumerate() {
            let text = cell_text(c);
            ideal[i] = ideal[i].max(text_width(&text));
            least[i] = least[i].max(text.split_whitespace().map(text_width).max().unwrap_or(0));
        }
    }
    let room = width.saturating_sub(indent + 3 * ncol + 1 + TABLE_MARGIN);
    let widths = column_widths(&ideal, &least, room);
    let rule = |l: &str, m: &str, r: &str| -> Vec<GridCell> {
        let mut s = String::from(l);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(if i + 1 == ncol { r } else { m });
        }
        let mut row = blanks(indent, St::default());
        row.extend(cells(&s, St::default()));
        row
    };
    let data = |r: &[Vec<MdSpan>], head: bool| -> Vec<Vec<GridCell>> {
        let cols: Vec<Vec<Vec<GridCell>>> = (0..ncol)
            .map(|i| {
                let spans = r.get(i).cloned().unwrap_or_default();
                let text: Vec<Span> = md_spans(&spans, St::default(), ink).into_iter().map(|(t, st)| (t.replace('\n', " "), st)).collect();
                let side = if head { MdAlign::Center } else { align.get(i).copied().unwrap_or(MdAlign::Left) };
                wrap(widths[i], Vec::new(), 0, &text, None).into_iter().map(|line| placed(line, widths[i], side)).collect()
            })
            .collect();
        let height = cols.iter().map(Vec::len).max().unwrap_or(1);
        (0..height)
            .map(|y| {
                let mut row = blanks(indent, St::default());
                row.extend(cells("│", St::default()));
                for (i, lines) in cols.iter().enumerate() {
                    let top = (height - lines.len()) / 2;
                    row.extend(blanks(1, St::default()));
                    match y.checked_sub(top).and_then(|k| lines.get(k)) {
                        Some(line) => row.extend(line.iter().cloned()),
                        None => row.extend(blanks(widths[i], St::default())),
                    }
                    row.extend(blanks(1, St::default()));
                    row.extend(cells("│", St::default()));
                }
                row
            })
            .collect()
    };
    let mut out = vec![rule("┌", "┬", "┐")];
    for (n, r) in all.iter().enumerate() {
        if n > 0 {
            out.push(rule("├", "┼", "┤"));
        }
        out.extend(data(r, n == 0 && !head.is_empty()));
    }
    out.push(rule("└", "┴", "┘"));
    for row in &mut out {
        row.truncate(width);
        row.resize(width, GridCell::blank());
    }
    out
}

/// 접는 표의 칸 하나가 받는 가장 좁은 폭 — `#` 같은 한 글자 칸도 세 칸은 받는다.
pub(super) const TABLE_MIN_COL: usize = 3;
/// 접는 표가 오른쪽에 남기는 칸.
const TABLE_MARGIN: usize = 3;

/// 칸마다 폭. 다 들어가면 글 폭 그대로, 아니면 가장 긴 낱말 폭을 먼저 주고 남는 자리를 더 원하는 만큼 나눈다.
pub(super) fn column_widths(ideal: &[usize], least: &[usize], room: usize) -> Vec<usize> {
    if ideal.iter().sum::<usize>() <= room {
        return ideal.to_vec();
    }
    let floor: usize = least.iter().sum();
    if floor >= room {
        return least.to_vec();
    }
    let spare = room - floor;
    let want: Vec<usize> = ideal.iter().zip(least).map(|(i, l)| i.saturating_sub(*l)).collect();
    let total = want.iter().sum::<usize>().max(1);
    let mut widths: Vec<usize> = least.iter().zip(&want).map(|(l, w)| l + spare * w / total).collect();
    let mut order: Vec<usize> = (0..widths.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(spare * want[i] % total));
    let mut left = room.saturating_sub(widths.iter().sum());
    for i in order.into_iter().cycle().take(widths.len() * 2) {
        if left == 0 {
            break;
        }
        if widths[i] < ideal[i] {
            widths[i] += 1;
            left -= 1;
        }
    }
    widths
}

/// 접힌 한 줄을 칸 폭 `w` 에 맞춰 놓는다.
fn placed(mut line: Vec<GridCell>, w: usize, side: MdAlign) -> Vec<GridCell> {
    while line.last().is_some_and(|c| c.ch == ' ') {
        line.pop();
    }
    line.truncate(w);
    let gap = w - line.len();
    let (l, r) = match side {
        MdAlign::Right => (gap, 0),
        MdAlign::Center => (gap / 2, gap - gap / 2),
        MdAlign::Left => (0, gap),
    };
    let mut out = blanks(l, St::default());
    out.extend(line);
    out.extend(blanks(r, St::default()));
    out
}

// ── 도구 ────────────────────────────────────────────────────────────────────

/// classic 이 한 줄로 접는 읽기 도구 — 「Read 2 files, listed 1 directory」.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quiet {
    Read,
    List,
    Search,
}

fn quiet_kind(name: &str, input: Option<&serde_json::Value>) -> Option<Quiet> {
    match name {
        "Read" | "NotebookRead" => Some(Quiet::Read),
        "Grep" | "Glob" => Some(Quiet::Search),
        "LS" => Some(Quiet::List),
        "Bash" => {
            let cmd = input.and_then(|i| i["command"].as_str()).unwrap_or("").trim();
            if cmd.contains(['|', '>', ';']) || cmd.contains("&&") {
                return None;
            }
            match cmd.split_whitespace().next().unwrap_or("") {
                "cat" | "head" | "tail" | "less" | "more" | "bat" | "wc" | "stat" | "file" => Some(Quiet::Read),
                "ls" | "tree" => Some(Quiet::List),
                "grep" | "rg" | "ag" | "find" | "fd" => Some(Quiet::Search),
                _ => None,
            }
        }
        _ => None,
    }
}

fn quiet_line(kinds: &[Quiet], running: bool, ink: &Ink) -> Vec<Span> {
    let count = |k: Quiet| kinds.iter().filter(|x| **x == k).count();
    let dim = St::fg(ink.dim);
    let num = dim.bold();
    let mut parts: Vec<(&str, usize, &str, &str)> = Vec::new();
    for (k, verb, one, many) in [
        (Quiet::Read, if running { "reading" } else { "read" }, "file", "files"),
        (Quiet::List, if running { "listing" } else { "listed" }, "directory", "directories"),
        (Quiet::Search, if running { "searching for" } else { "searched for" }, "pattern", "patterns"),
    ] {
        let n = count(k);
        if n > 0 {
            parts.push((verb, n, one, many));
        }
    }
    let mut out: Vec<Span> = Vec::new();
    for (i, (verb, n, one, many)) in parts.iter().enumerate() {
        if i > 0 {
            out.push((", ".into(), dim));
        }
        let verb = if i == 0 {
            let mut c = verb.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        } else {
            verb.to_string()
        };
        out.push((format!("{verb} "), dim));
        out.push((n.to_string(), num));
        out.push((format!(" {}", if *n == 1 { one } else { many }), dim));
    }
    out.push((if running { "…".into() } else { " (ctrl+o to expand)".into() }, dim));
    out
}

fn tool_title(name: &str) -> String {
    match name {
        "Edit" | "MultiEdit" => "Update".into(),
        "WebFetch" => "Fetch".into(),
        "WebSearch" => "Web Search".into(),
        "TodoWrite" => "Update Todos".into(),
        "exec_command" | "shell" => "Bash".into(),
        _ => match name.strip_prefix("mcp__") {
            Some(rest) => {
                let mut it = rest.splitn(2, "__");
                let server = it.next().unwrap_or("");
                let tool = it.next().unwrap_or("");
                format!("{server} - {tool} (MCP)")
            }
            None => name.to_string(),
        },
    }
}

pub(super) fn shorten_path(path: &str, cwd: Option<&str>) -> String {
    match cwd.and_then(|c| path.strip_prefix(c)).and_then(|p| p.strip_prefix('/')) {
        Some(rel) if !rel.is_empty() => rel.to_string(),
        _ => path.to_string(),
    }
}

fn tool_arg(name: &str, summary: &str, input: Option<&serde_json::Value>, cwd: Option<&str>) -> String {
    let s = |k: &str| input.and_then(|i| i[k].as_str()).map(str::trim).filter(|v| !v.is_empty());
    match name {
        "Bash" | "exec_command" | "shell" => s("command").or(s("cmd")).map(|c| {
            let mut lines = c.lines();
            let first = lines.next().unwrap_or("").to_string();
            if lines.next().is_some() { format!("{first} …") } else { first }
        }),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => s("file_path").map(|p| shorten_path(p, cwd)),
        "Grep" => s("pattern").map(|p| match s("path") {
            Some(path) => format!("pattern: \"{p}\", path: \"{}\"", shorten_path(path, cwd)),
            None => format!("pattern: \"{p}\""),
        }),
        "Glob" => s("pattern").map(|p| format!("pattern: \"{p}\"")),
        _ => None,
    }
    .unwrap_or_else(|| summary.to_string())
}

/// `  ⎿  ` 머리 + 다음 줄부터 다섯 칸 들여 쓴 결과 줄들. 접힌 동안은 세 줄과 「… +N lines」.
fn result_lines(width: usize, text: &[Span], open: bool, ink: &Ink) -> Vec<Vec<GridCell>> {
    const SHOWN: usize = 3;
    const OPEN_CAP: usize = 400;
    let mut first = cells("  ⎿", St::fg(ink.dim));
    first.extend(blanks(2, St::default()));
    let mut lines: Vec<Vec<Span>> = vec![Vec::new()];
    for (t, st) in text {
        for (i, part) in t.split('\n').enumerate() {
            if i > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().push((part.to_string(), *st));
            }
        }
    }
    while lines.len() > 1 && lines.last().is_some_and(|l| l.iter().all(|(t, _)| t.trim().is_empty())) {
        lines.pop();
    }
    let total = lines.len();
    let keep = if open { total.min(OPEN_CAP) } else { total.min(SHOWN) };
    let mut out = Vec::new();
    for (i, spans) in lines.into_iter().take(keep).enumerate() {
        let head = if i == 0 { first.clone() } else { blanks(5, St::default()) };
        out.extend(wrap(width, head, 5, &spans, None));
    }
    if total > keep {
        let st = St { dim: true, ..Default::default() };
        let hint = if open { format!("… +{} lines", total - keep) } else { format!("… +{} lines (ctrl+o to expand)", total - keep) };
        out.extend(wrap(width, blanks(5, St::default()), 5, &plain(hint, st), None));
    }
    out
}

fn numbered(width: usize, body: &str, total: usize, open: bool, ink: &Ink) -> Vec<Vec<GridCell>> {
    let shown = if open { 400 } else { 10 };
    let digits = total.max(1).to_string().len().max(2);
    let mut out = Vec::new();
    for (i, l) in body.lines().take(shown).enumerate() {
        let mut head = blanks(5, St::default());
        head.extend(cells(&format!("{:>digits$} ", i + 1), St { dim: true, fg: Some(ink.text), ..Default::default() }));
        let hang = head.len();
        out.extend(wrap(width, head, hang, &plain(l.replace('\t', "    "), St::fg(ink.text)), None));
    }
    if total > shown {
        let st = St { dim: true, ..Default::default() };
        out.extend(wrap(width, blanks(5, St::default()), 5, &plain(format!("… +{} lines (ctrl+o to expand)", total - shown), st), None));
    }
    out
}

/// Edit 의 diff — 앞뒤 같은 줄은 문맥(세 줄까지), 가운데 지운 줄·넣은 줄. 줄 번호는 기록에 없어 안 단다.
fn diff_lines(width: usize, old: &str, new: &str, ink: &Ink) -> (usize, usize, Vec<Vec<GridCell>>) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let removed = &a[pre..a.len() - suf];
    let added = &b[pre..b.len() - suf];
    let mut out = Vec::new();
    let ctx = |out: &mut Vec<Vec<GridCell>>, l: &str| {
        let mut head = blanks(5, St::default());
        head.extend(cells("  ", St::default()));
        out.extend(wrap(width, head, 7, &plain(l, St::fg(ink.text)), None));
    };
    for l in &a[pre.saturating_sub(3)..pre] {
        ctx(&mut out, l);
    }
    for (mark, list, bg, fg) in [("-", removed, ink.del_bg, ink.del_mark), ("+", added, ink.add_bg, ink.add_mark)] {
        for l in list.iter().take(40) {
            let mut head = blanks(5, St::default());
            head.extend(cells(&format!("{mark} "), St::fg(fg).on(bg)));
            let mut rows = wrap(width, head, 7, &plain(*l, St::fg(ink.text).on(bg)), None);
            for row in &mut rows {
                let end = row.iter().rposition(|c| c.ch != ' ').map_or(7, |i| i + 1);
                for c in row.iter_mut().take(end).skip(5) {
                    c.bg = bg.color();
                }
            }
            out.extend(rows);
        }
    }
    for l in b[b.len() - suf..].iter().take(3) {
        ctx(&mut out, l);
    }
    (added.len(), removed.len(), out)
}

/// 기록의 `structuredPatch` 그대로 — 줄 번호(문맥·넣은 줄은 새 번호, 지운 줄은 옛 번호) + 표지 + 글.
fn patch_lines(width: usize, patch: &serde_json::Value, ink: &Ink) -> (usize, usize, Vec<Vec<GridCell>>) {
    let hunks = patch.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let biggest = hunks
        .iter()
        .map(|h| h["oldStart"].as_u64().unwrap_or(1) + h["oldLines"].as_u64().unwrap_or(0) + h["newStart"].as_u64().unwrap_or(1) + h["newLines"].as_u64().unwrap_or(0))
        .max()
        .unwrap_or(1);
    let digits = biggest.to_string().len() + 1;
    let (mut added, mut removed) = (0, 0);
    let mut out = Vec::new();
    for (n, h) in hunks.iter().enumerate() {
        if n > 0 {
            out.extend(wrap(width, blanks(5, St::default()), 5, &plain("...", St { dim: true, ..Default::default() }), None));
        }
        let (mut old_no, mut new_no) = (h["oldStart"].as_u64().unwrap_or(1), h["newStart"].as_u64().unwrap_or(1));
        for l in h["lines"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let l = l.as_str().unwrap_or("");
            let (mark, body) = l.split_at(l.chars().next().map_or(0, char::len_utf8));
            let (no, head_st, body_st) = match mark {
                "+" => {
                    added += 1;
                    new_no += 1;
                    (new_no - 1, St::fg(ink.add_mark).on(ink.add_bg), St::fg(ink.text).on(ink.add_bg))
                }
                "-" => {
                    removed += 1;
                    old_no += 1;
                    (old_no - 1, St::fg(ink.del_mark).on(ink.del_bg), St::fg(ink.text).on(ink.del_bg))
                }
                _ => {
                    old_no += 1;
                    new_no += 1;
                    (new_no - 1, St { dim: true, fg: Some(ink.text), ..Default::default() }, St::fg(ink.text))
                }
            };
            let mut head = blanks(5, St::default());
            head.extend(cells(&format!("{no:>digits$} "), head_st));
            head.extend(cells(if mark == "+" || mark == "-" { mark } else { " " }, if mark == "+" || mark == "-" { head_st } else { St::default() }));
            let hang = head.len();
            let mut rows = wrap(width, head, hang, &plain(body.replace('\t', "    "), body_st), None);
            // classic 의 diff 띠는 칸 오른쪽 일곱 칸 앞까지 깔린다.
            if mark == "+" || mark == "-" {
                let bg = if mark == "+" { ink.add_bg } else { ink.del_bg };
                for row in &mut rows {
                    for c in row.iter_mut().take(width.saturating_sub(7)).skip(5) {
                        c.bg = bg.color();
                    }
                }
            }
            out.extend(rows);
        }
    }
    (added, removed, out)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// 도구 한 칸 — `⏺ Name(arg)` 와 그 아래 `⎿` 결과.
fn tool_block(width: usize, item: usize, conv: &Conversation, open: bool, ink: &Ink) -> Vec<Line> {
    let Item::Tool { name, summary, result, error } = &conv.items[item] else { return Vec::new() };
    let input = conv.tool_inputs.get(&item);
    let cwd = conv.meta.cwd.as_deref();
    let dot = match (result, error) {
        (None, _) => ink.dim,
        (Some(_), true) => ink.err,
        (Some(_), false) => ink.ok,
    };
    let mut first = cells("⏺", St::fg(dot));
    first.push(GridCell::blank());
    let title = vec![
        (tool_title(name), St::default().bold()),
        (format!("({})", tool_arg(name, summary, input, cwd)), St::default()),
    ];
    let mut rows = wrap(width, first, 2, &title, None);
    let s = |k: &str| input.and_then(|i| i[k].as_str());
    match (name.as_str(), result) {
        (_, None) => rows.extend(result_lines(width, &plain("Running…", St::fg(ink.dim)), open, ink)),
        ("Write", Some(_)) if !*error => {
            let body = s("content").unwrap_or("");
            let total = input.and_then(|i| i["content_lines"].as_u64()).map(|n| n as usize).unwrap_or_else(|| body.lines().count());
            let path = s("file_path").map(|p| shorten_path(p, cwd)).unwrap_or_default();
            let st = St::default();
            let head = vec![("Wrote ".to_string(), st), (total.to_string(), st.bold()), (" lines to ".to_string(), st), (path, st.bold())];
            rows.extend(result_lines(width, &head, true, ink));
            rows.extend(numbered(width, body, total, open, ink));
        }
        ("Edit" | "MultiEdit", Some(_)) if !*error => {
            let (added, removed, diff) = match conv.tool_patches.get(&item) {
                Some(patch) => patch_lines(width, patch, ink),
                None => diff_lines(width, s("old_string").unwrap_or(""), s("new_string").unwrap_or(""), ink),
            };
            let what = match (added, removed) {
                (a, 0) => format!("Added {}", plural(a, "line", "lines")),
                (0, r) => format!("Removed {}", plural(r, "line", "lines")),
                (a, r) => format!("Added {}, removed {}", plural(a, "line", "lines"), plural(r, "line", "lines")),
            };
            rows.extend(result_lines(width, &plain(what, St::default()), true, ink));
            rows.extend(diff);
        }
        (_, Some(text)) => {
            let text = super::parse::strip_ansi(text);
            let body = if text.trim().is_empty() {
                plain(if matches!(name.as_str(), "Bash" | "shell" | "exec_command") { "(No output)" } else { "(No content)" }, St::fg(ink.dim))
            } else {
                plain(text.trim_end().to_string(), if *error { St::fg(ink.err) } else { St::default() })
            };
            rows.extend(result_lines(width, &body, open, ink));
        }
    }
    let mut out = line(rows);
    if let Some(head) = out.first_mut() {
        head.toggle = Some(item);
    }
    out
}

fn took(ms: u64) -> String {
    let s = (ms as f64 / 1000.0).round() as u64;
    let (h, m) = (s / 3600, (s % 3600) / 60);
    let mut parts: Vec<String> = Vec::new();
    if h > 0 {
        parts.push(format!("{h}시간"));
    }
    if m > 0 {
        parts.push(format!("{m}분"));
    }
    if h > 0 || m > 0 {
        if s % 60 > 0 {
            parts.push(format!("{}초", s % 60));
        }
    } else {
        parts.push(format!("{s}초"));
    }
    format!("{} 걸려 끝났어요", parts.join(" "))
}

fn banner(width: usize, conv: &Conversation, ink: &Ink) -> Vec<Line> {
    let logo = St::fg(ink.logo);
    let on = logo.on(ink.logo_bg);
    let dim = St::fg(ink.dim);
    let version = conv.meta.version.as_deref().map(|v| format!("v{}", v.trim_start_matches('v'))).unwrap_or_default();
    let model = conv.meta.model.as_deref().map(model_label).unwrap_or_default();
    let cwd = conv.meta.cwd.clone().unwrap_or_default();
    let mut a = cells(" ▐", logo);
    a.extend(cells("▛███▛█", on));
    a.extend(blanks(3, St::default()));
    a.extend(cells("Claude Code", St::default().bold()));
    a.push(GridCell::blank());
    a.extend(cells(&version, dim));
    let mut b = cells("▝▜", logo);
    b.extend(cells("█████", on));
    b.extend(cells("█▀", logo));
    b.extend(blanks(2, St::default()));
    b.extend(cells(&model, dim));
    let mut c = cells(" ▝▝   ▝▝ ", logo);
    c.extend(blanks(2, St::default()));
    c.extend(cells(&cwd, dim));
    [a, b, c]
        .into_iter()
        .map(|mut row| {
            row.truncate(width);
            row.resize(width, GridCell::blank());
            Line { cells: row, toggle: None, face: None }
        })
        .collect()
}

/// 진짜 화면에서 옮긴 배너 줄을 이 칸 폭으로.
pub(super) fn screen_banner(rows: &[Vec<GridCell>], width: usize) -> Vec<Line> {
    rows.iter()
        .map(|r| {
            let mut cells = r.clone();
            cells.truncate(width);
            cells.resize(width, GridCell::blank());
            Line { cells, toggle: None, face: None }
        })
        .collect()
}

pub(super) fn banner_text(rows: &[Vec<GridCell>]) -> Vec<String> {
    rows.iter().map(|r| r.iter().map(|c| c.ch).collect()).collect()
}

/// `claude-opus-5-5` → `Opus 5.5`.
fn model_label(id: &str) -> String {
    let id = id.trim_start_matches("claude-");
    let mut parts = id.split('-');
    let family = parts.next().unwrap_or("");
    let family = family.chars().next().map(|f| f.to_uppercase().collect::<String>() + &family[f.len_utf8()..]).unwrap_or_default();
    let nums: Vec<&str> = parts.take_while(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() <= 2).collect();
    if nums.is_empty() { family } else { format!("{family} {}", nums.join(".")) }
}

// ── 묶음과 쌓기 ─────────────────────────────────────────────────────────────

/// 그릴 한 덩이 — 앞에 빈 줄을 두나와 그 줄들.
pub(super) struct Block {
    key: usize,
    gap: bool,
    lines: Vec<Line>,
}

/// 덩이를 차례로 모은다 — 서명이 지난번과 같은 덩이는 편 줄을 그대로 쓰고, 이번에 안 나온 덩이는 버린다.
pub(super) struct Shelf<'a> {
    out: Vec<Block>,
    seen: HashSet<usize>,
    cache: &'a mut HashMap<usize, (u64, Vec<Line>)>,
}

impl<'a> Shelf<'a> {
    pub(super) fn new(cache: &'a mut HashMap<usize, (u64, Vec<Line>)>) -> Self {
        Self { out: Vec::new(), seen: HashSet::new(), cache }
    }

    pub(super) fn emit(&mut self, key: usize, gap: bool, sig: u64, make: impl FnOnce() -> Vec<Line>) {
        self.seen.insert(key);
        let lines = match self.cache.get(&key) {
            Some((s, lines)) if *s == sig => lines.clone(),
            _ => {
                let lines = make();
                self.cache.insert(key, (sig, lines.clone()));
                lines
            }
        };
        self.out.push(Block { key, gap, lines });
    }

    pub(super) fn done(self) -> Vec<Block> {
        let seen = self.seen;
        self.cache.retain(|k, _| seen.contains(k));
        self.out
    }
}

/// 칸마다 펴 둔 덩이. 기록이 덧붙어도 이미 편 덩이는 서명이 같으면 다시 안 편다.
#[derive(Default)]
pub(crate) struct Cache {
    width: usize,
    look: Option<Look>,
    blocks: HashMap<usize, (u64, Vec<Line>)>,
    /// 지금 쌓인 줄 전부와 칸 번호 → 그 칸이 시작하는 줄.
    pub(crate) lines: Vec<Line>,
    pub(crate) starts: Vec<(usize, usize)>,
    stamp: Option<(u64, u64, u64)>,
}

pub(super) fn sig_of(conv: &Conversation, items: &[usize], extra: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    extra.hash(&mut h);
    for &i in items {
        i.hash(&mut h);
        match &conv.items[i] {
            Item::Bubble { user, text, queued, images, from } => (0u8, user, text.len(), queued, images, from.is_some()).hash(&mut h),
            Item::Tool { result, error, summary, .. } => (1u8, result.as_ref().map(String::len), error, summary.len()).hash(&mut h),
            Item::Answered { pairs, .. } => (2u8, pairs.as_ref().map(Vec::len)).hash(&mut h),
            Item::Output(t) | Item::System(t) | Item::Launch(t) | Item::Thinking(t) => (3u8, t.len()).hash(&mut h),
            Item::Command { name, args } => (4u8, name.len(), args.len()).hash(&mut h),
            Item::Duration(ms, at) => (5u8, ms, at).hash(&mut h),
            Item::Interrupted | Item::Gone | Item::Compacted => 6u8.hash(&mut h),
        }
    }
    h.finish()
}

impl Cache {
    /// 기록 판·펼침·폭이 바뀌었을 때만 다시 쌓는다. 쌓인 줄은 `lines`.
    pub(crate) fn build(&mut self, conv: &Conversation, version: u64, width: usize, open: &HashSet<usize>, look: &Look) {
        let mut oh = std::collections::hash_map::DefaultHasher::new();
        let mut keys: Vec<&usize> = open.iter().collect();
        keys.sort();
        keys.hash(&mut oh);
        let stamp = (version, width as u64, oh.finish());
        if self.stamp == Some(stamp) && self.look.as_ref() == Some(look) {
            return;
        }
        if self.width != width || self.look.as_ref() != Some(look) {
            self.blocks.clear();
        }
        self.width = width;
        self.look = Some(look.clone());
        self.stamp = Some(stamp);
        let blocks = if conv.codex {
            super::codex::blocks(conv, width, open, look, &mut self.blocks)
        } else {
            blocks(conv, width, open, look, &Ink::now(), &mut self.blocks)
        };
        self.lines.clear();
        self.starts.clear();
        for b in blocks {
            if b.gap && !self.lines.is_empty() {
                self.lines.push(blank_line(width));
            }
            self.starts.push((b.key, self.lines.len()));
            self.lines.extend(b.lines);
        }
        if !self.lines.is_empty() {
            self.lines.push(blank_line(width));
        }
    }

    /// 그 칸(또는 그 뒤 첫 칸)이 시작하는 줄.
    pub(crate) fn line_of(&self, item: usize) -> Option<usize> {
        self.starts.iter().find(|(k, _)| *k != usize::MAX && *k >= item).map(|(_, l)| *l)
    }
}

fn blocks(
    conv: &Conversation,
    width: usize,
    open: &HashSet<usize>,
    look: &Look,
    ink: &Ink,
    cache: &mut HashMap<usize, (u64, Vec<Line>)>,
) -> Vec<Block> {
    let mut shelf = Shelf::new(cache);
    match &look.banner {
        Some(rows) => {
            let sig = sig_of(conv, &[], ("screen-banner", width, banner_text(rows)));
            shelf.emit(usize::MAX, false, sig, || screen_banner(rows, width));
        }
        None if !conv.items.is_empty() => {
            let sig = sig_of(conv, &[], (&conv.meta.version, &conv.meta.model, &conv.meta.cwd));
            shelf.emit(usize::MAX, false, sig, || banner(width, conv, ink));
        }
        None => {}
    }
    // 턴의 첫 답은 큰 얼굴(4×2), 같은 턴 도구 뒤 답·턴 끝 줄은 작은 얼굴(2×1) — student-face mod 와 같다.
    let mut answered = false;
    let mut prev_system = false;
    for row in super::parse::group_rows(&conv.items) {
        match row {
            Row::Tools(run) => {
                prev_system = false;
                let mut i = 0;
                while i < run.len() {
                    let kind = |at: usize| match &conv.items[at] {
                        Item::Tool { name, .. } => quiet_kind(name, conv.tool_inputs.get(&at)),
                        _ => None,
                    };
                    let start = run[i];
                    if kind(start).is_some() && !open.contains(&start) {
                        let mut j = i;
                        while j < run.len() && kind(run[j]).is_some() {
                            j += 1;
                        }
                        let seg: Vec<usize> = run[i..j].to_vec();
                        let sig = sig_of(conv, &seg, ("quiet", width));
                        shelf.emit(start, true, sig, || {
                            let kinds: Vec<Quiet> = seg.iter().filter_map(|&at| kind(at)).collect();
                            let running = seg.iter().any(|&at| matches!(&conv.items[at], Item::Tool { result: None, .. }));
                            let mut l = line(wrap(width, blanks(2, St::default()), 2, &quiet_line(&kinds, running, ink), None));
                            if let Some(h) = l.first_mut() {
                                h.toggle = Some(start);
                            }
                            l
                        });
                        i = j;
                        continue;
                    }
                    // 펼친 읽기 묶음은 칸마다 따로 — 묶음 열쇠(첫 칸)를 다시 누르면 접힌다.
                    if kind(start).is_some() {
                        let mut j = i;
                        while j < run.len() && kind(run[j]).is_some() {
                            let at = run[j];
                            let sig = sig_of(conv, &[at], ("tool-open", width, start));
                            shelf.emit(at, true, sig, || {
                                let mut l = tool_block(width, at, conv, true, ink);
                                if let Some(h) = l.first_mut() {
                                    h.toggle = Some(start);
                                }
                                l
                            });
                            j += 1;
                        }
                        i = j;
                        continue;
                    }
                    let is_open = open.contains(&start);
                    let sig = sig_of(conv, &[start], ("tool", width, is_open));
                    shelf.emit(start, true, sig, || tool_block(width, start, conv, is_open, ink));
                    i += 1;
                }
            }
            Row::One(i) => {
                let item = &conv.items[i];
                let system = matches!(item, Item::System(_));
                let gap = match item {
                    Item::Duration(..) => false,
                    Item::System(_) => !prev_system,
                    _ => true,
                };
                prev_system = system;
                match item {
                    Item::Thinking(_) | Item::Gone => continue,
                    Item::Bubble { user: true, .. } | Item::Command { .. } => answered = false,
                    _ => {}
                }
                let big = matches!(item, Item::Bubble { user: false, .. }) && !answered && look.face;
                if matches!(item, Item::Bubble { user: false, .. }) {
                    answered = true;
                }
                let sig = sig_of(conv, &[i], ("one", width, big));
                shelf.emit(i, gap, sig, || one(width, i, conv, big, look, ink));
            }
        }
    }
    shelf.done()
}

/// 내 말 띠 — 행 전폭이 같은 바탕이라 카사텀의 프롬프트 띠 꾸밈(`restyle_user_prompt_row`)이 그대로 얹힌다.
fn prompt(width: usize, text: &str, ink: &Ink) -> Vec<Line> {
    let base = St::fg(ink.text).on(ink.band);
    let mut first = cells("❯", St::fg(ink.dim).on(ink.band));
    first.push(St::default().on(ink.band).cell(' '));
    line(wrap(width, first, 2, &inline_code(text, base, ink), Some(ink.band)))
}

fn one(width: usize, i: usize, conv: &Conversation, big: bool, look: &Look, ink: &Ink) -> Vec<Line> {
    let dim = St::fg(ink.dim);
    let elbow = |text: &str, st: St| -> Vec<Line> {
        let mut first = cells("  ⎿", dim);
        first.extend(blanks(2, St::default()));
        line(wrap(width, first, 5, &plain(text, st), None))
    };
    match &conv.items[i] {
        Item::Bubble { user: true, text, from, images, .. } => {
            let mut body = match from {
                Some(_) => conv.relayed_raw.get(&i).cloned().unwrap_or_else(|| text.clone()),
                None => text.clone(),
            };
            for n in 1..=*images {
                if !body.is_empty() {
                    body.push(' ');
                }
                body.push_str(&format!("[Image #{n}]"));
            }
            prompt(width, &body, ink)
        }
        Item::Bubble { user: false, text, .. } => {
            let face = look.face.then_some(if big { Face { cols: 4, rows: 2 } } else { Face { cols: 2, rows: 1 } });
            let indent = face.map_or(2, |f| f.cols as usize + 1);
            let mut rows = markdown(text, width, indent, ink);
            if rows.is_empty() {
                rows.push(vec![GridCell::blank(); width]);
            }
            match face {
                Some(f) => {
                    while rows.len() < f.rows as usize {
                        rows.push(vec![GridCell::blank(); width]);
                    }
                }
                None => {
                    if let Some(first) = rows.first_mut() {
                        first[0] = St::fg(ink.text).cell('⏺');
                    }
                }
            }
            let mut out = line(rows);
            if let Some(head) = out.first_mut() {
                head.face = face;
            }
            out
        }
        Item::Command { name, args } => {
            let text = if name == "!" { format!("! {args}") } else { format!("{name} {args}").trim().to_string() };
            prompt(width, &text, ink)
        }
        Item::Output(text) => elbow(text, dim),
        Item::System(text) => elbow(text, dim),
        Item::Launch(text) => {
            let mut first = cells("⏺", St::fg(ink.ok));
            first.push(GridCell::blank());
            line(wrap(width, first, 2, &plain(text, St::default()), None))
        }
        Item::Answered { pairs: Some(pairs), .. } => {
            let mut first = cells("⏺", St::fg(ink.text));
            first.push(GridCell::blank());
            let mut rows = wrap(width, first, 2, &plain("User answered Claude's questions:", St::default()), None);
            for (n, (q, a)) in pairs.iter().enumerate() {
                let head = if n == 0 {
                    let mut h = cells("  ⎿", dim);
                    h.extend(blanks(2, St::default()));
                    h
                } else {
                    blanks(5, St::default())
                };
                let spans = vec![(format!("· {q} → "), dim), (a.clone(), St::default())];
                rows.extend(wrap(width, head, 7, &spans, None));
            }
            line(rows)
        }
        Item::Answered { pairs: None, .. } | Item::Thinking(_) | Item::Gone | Item::Tool { .. } => Vec::new(),
        Item::Interrupted => {
            let mut first = cells("  ⎿", dim);
            first.extend(blanks(2, St::default()));
            let spans = vec![("Interrupted".to_string(), St::fg(ink.err)), (" · What should Claude do instead?".to_string(), dim)];
            line(wrap(width, first, 5, &spans, None))
        }
        Item::Compacted => line(wrap(width, Vec::new(), 2, &plain("✻ Conversation compacted (ctrl+o for history)", dim), None)),
        Item::Duration(ms, _) => {
            let color = look.student.map(rgb).unwrap_or(ink.dim);
            if look.face {
                let mut out = line(wrap(width, blanks(3, St::default()), 3, &plain(took(*ms), St::fg(color)), None));
                if let Some(h) = out.first_mut() {
                    h.face = Some(Face { cols: 2, rows: 1 });
                }
                out
            } else {
                let mut first = cells("◆", St::fg(color));
                first.push(GridCell::blank());
                line(wrap(width, first, 2, &plain(took(*ms), St::fg(color)), None))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.cells.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    fn conv(lines: &[&str]) -> Conversation {
        let mut c = Conversation::default();
        assert!(c.apply(&lines.join("\n")));
        c
    }

    fn build(c: &Conversation, width: usize, face: bool) -> Vec<String> {
        let mut cache = Cache::default();
        cache.build(c, 1, width, &HashSet::new(), &Look { face, student: None, banner: None });
        cache.lines.iter().map(text).collect()
    }

    #[test]
    fn a_turn_lays_out_like_the_classic_screen() {
        let c = conv(&[
            r#"{"type":"user","sessionId":"s","version":"2.1.299","cwd":"/w","message":{"content":"Bash 로 `cat list.txt` 실행해"}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"model":"claude-opus-5-5","content":[{"type":"thinking","thinking":"짧게"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"seq 1 5"}}]}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"1\n2\n3\n4\n5"}]}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/w/a.txt"}}]}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"x"}]}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"text","text":"끝났어요.\n\n- 하나\n- 둘"}]}}"#,
            r#"{"type":"system","sessionId":"s","subtype":"turn_duration","durationMs":15200}"#,
        ]);
        let got = build(&c, 60, false);
        let want = [
            " ▐▛███▛█   Claude Code v2.1.299",
            "▝▜██████▀  Opus 5.5",
            " ▝▝   ▝▝   /w",
            "",
            "❯ Bash 로 cat list.txt 실행해",
            "",
            "⏺ Bash(seq 1 5)",
            "  ⎿  1",
            "     2",
            "     3",
            "     … +2 lines (ctrl+o to expand)",
            "",
            "  Read 1 file (ctrl+o to expand)",
            "",
            "⏺ 끝났어요.",
            "",
            "  - 하나",
            "  - 둘",
            "◆ 15초 걸려 끝났어요",
            "",
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn faces_take_the_bullet_and_push_the_text_in() {
        let c = conv(&[
            r#"{"type":"user","sessionId":"s","message":{"content":"안녕"}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"text","text":"네"}]}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"make"}}]}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":""}]}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"text","text":"됐어요"}]}}"#,
        ]);
        let mut cache = Cache::default();
        cache.build(&c, 1, 40, &HashSet::new(), &Look { face: true, student: None, banner: None });
        let faces: Vec<(String, Option<Face>)> = cache.lines.iter().map(|l| (text(l), l.face)).filter(|(_, f)| f.is_some()).collect();
        assert_eq!(faces, vec![
            ("     네".to_string(), Some(Face { cols: 4, rows: 2 })),
            ("   됐어요".to_string(), Some(Face { cols: 2, rows: 1 })),
        ]);
        let all: Vec<String> = cache.lines.iter().map(text).collect();
        assert!(all.contains(&"  ⎿  (No output)".to_string()), "{all:?}");
    }

    #[test]
    fn prompt_rows_carry_one_background_across_the_whole_width() {
        let c = conv(&[r#"{"type":"user","sessionId":"s","message":{"content":"가나다라마바사아자차카타파하 가나다라마바사아자차카타파하"}}"#]);
        let mut cache = Cache::default();
        cache.build(&c, 1, 30, &HashSet::new(), &Look { face: false, student: None, banner: None });
        let band: Vec<&Line> = cache.lines.iter().filter(|l| l.cells[0].bg == Color::Idx(237)).collect();
        assert_eq!(band.len(), 2);
        for l in band {
            assert!(crate::screenread::band_bg(&l.cells).is_some());
            assert_eq!(l.cells.len(), 30);
        }
    }


    /// 칸보다 넓은 표는 칸마다 접힌다 — 오른쪽 칸이 잘려 나가지 않고, 머리글은 가운데, 짧은 칸은 세로 가운데.
    #[test]
    fn a_table_wider_than_the_pane_wraps_inside_its_cells() {
        let md = "| 과제 | 스웜 | 나쵸 |\n|---|---|---|\n| 홈 탭 | 성공 2걸음, 30초 (첫 시도는 안전장치에 걸려 멈춤) | 성공 4걸음 |";
        let got: Vec<String> = markdown(md, 40, 2, &Ink::now())
            .iter()
            .map(|r| r.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(got, vec![
            "  ┌──────┬─────────────────┬────────┐",
            "  │ 과제 │      스웜       │  나쵸  │",
            "  ├──────┼─────────────────┼────────┤",
            "  │      │ 성공 2걸음,     │        │",
            "  │ 홈   │ 30초 (첫 시도는 │ 성공   │",
            "  │ 탭   │ 안전장치에 걸려 │ 4걸음  │",
            "  │      │ 멈춤)           │        │",
            "  └──────┴─────────────────┴────────┘",
        ]);
    }

    #[test]
    fn tables_are_boxed_with_a_rule_between_rows() {
        let rows = markdown("| 이름 | 확장자 |\n|---|---|\n| a | txt |\n| b | txt |", 40, 3, &Ink::now());
        let got: Vec<String> = rows
            .iter()
            .map(|r| r.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(got, vec![
            "   ┌──────┬────────┐",
            "   │ 이름 │ 확장자 │",
            "   ├──────┼────────┤",
            "   │ a    │ txt    │",
            "   ├──────┼────────┤",
            "   │ b    │ txt    │",
            "   └──────┴────────┘",
        ]);
    }

    #[test]
    fn clicking_a_folded_read_group_opens_each_tool() {
        let c = conv(&[
            r#"{"type":"user","sessionId":"s","message":{"content":"읽어"}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"/x/a.rs"}},{"type":"tool_use","id":"b","name":"Grep","input":{"pattern":"fn"}}]}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"1"},{"type":"tool_result","tool_use_id":"b","content":"2"}]}}"#,
        ]);
        let got = build(&c, 60, false);
        assert!(got.contains(&"  Read 1 file, searched for 1 pattern (ctrl+o to expand)".to_string()), "{got:?}");
        let mut cache = Cache::default();
        let key = cache_key(&c);
        cache.build(&c, 1, 60, &HashSet::from([key]), &Look { face: false, student: None, banner: None });
        let open: Vec<String> = cache.lines.iter().map(text).collect();
        assert!(open.contains(&"⏺ Read(/x/a.rs)".to_string()), "{open:?}");
        assert!(open.contains(&"⏺ Grep(pattern: \"fn\")".to_string()), "{open:?}");
    }

    fn cache_key(c: &Conversation) -> usize {
        c.items.iter().position(|i| matches!(i, Item::Tool { .. })).unwrap()
    }

    #[test]
    fn edits_show_the_numbered_patch_like_classic() {
        let c = conv(&[
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"tool_use","id":"e","name":"Edit","input":{"file_path":"/w/notes.md","old_string":"- 하나","new_string":"- 하나\n- 둘"}}]}}"#,
            r#"{"type":"user","sessionId":"s","cwd":"/w","toolUseResult":{"structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":3,"lines":[" # 메모"," - 하나","+- 둘"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e","content":"ok"}]}}"#,
        ]);
        let got = build(&c, 50, false);
        let at = got.iter().position(|l| l == "⏺ Update(notes.md)").expect("title");
        assert_eq!(&got[at + 1..at + 5], &["  ⎿  Added 1 line", "      1  # 메모", "      2  - 하나", "      3 +- 둘"]);
    }

    #[test]
    fn model_ids_read_like_the_banner() {
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-5-5-20261001"), "Sonnet 5.5");
        assert_eq!(took(67_000), "1분 7초 걸려 끝났어요");
        assert_eq!(took(8_000), "8초 걸려 끝났어요");
    }
}
