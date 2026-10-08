//! 대화 칸의 배치와 그리기. 치수는 `docs/design.md` 「학생 대화 보기」에 있다.
//!
//! 배치는 기록이 바뀔 때만 다시 한다(`Layout`). 말풍선 하나를 펴려면 낱말마다 폭을
//! 재야 해서, 프레임마다 하면 긴 대화에서 그 값이 프레임을 먹는다. 말풍선은 한 번 서면
//! 글이 안 바뀌므로 칸 번호로 붙잡아 두고, 줄 목록만 새로 엮는다.

use std::collections::{HashMap, HashSet};

use super::live::{self, ModLive};
use super::parse::{tool_label, Item, Row};
use super::{ChatPane, Feed};
use crate::{gpu, native_controls, theme, MdBlock, MdSpan};

type Rect = (f32, f32, f32, f32);

/// 한 프레임에 대화로 그릴 pane 하나 — 그리는 쪽이 `self` 를 못 빌려 렌더 루프가 미리 뜬다.
pub(crate) struct Slot {
    pub(crate) pane: String,
    pub(crate) rect: Rect,
    /// 말풍선 머리에 붙는 이름 — 학생 이름, 없으면 하네스.
    pub(crate) name: String,
    pub(crate) working: bool,
    pub(crate) needs_you: bool,
    pub(crate) focused: bool,
    /// 다른 기기 학생의 거울. 기록은 그 기기에 있다.
    pub(crate) mirror: bool,
    pub(crate) preedit: String,
    pub(crate) caret_on: bool,
    /// 이 칸에 실제로 깔린 바탕. 거울 칸은 기기색이 섞여 테마 바탕과 다르다 — 채움을 테마 바탕에서
    /// 끌어내면 분홍 칸 위에 푸른 회색 상자가 떠 따로 논다(2026-10-08).
    pub(crate) bg: [u8; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hit {
    /// 도구 묶음·생각·출력 접기. 값은 그 칸의 첫 번호.
    Fold(usize),
    /// 도구 한 줄의 결과 펼치기.
    Tool(usize),
    Copy(usize),
    Send,
    Stop,
    Terminal,
    Composer,
    Bottom,
    Pick(usize),
    /// 화면 선택지 없이 mod 요청만 보일 때의 허락·거절.
    Decide(bool),
    Dismiss,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tone {
    Text,
    Dim,
    Mute,
    Accent,
}

impl Tone {
    fn color(self) -> [u8; 4] {
        match self {
            Tone::Text => theme::text(),
            Tone::Dim => theme::text_dim(),
            Tone::Mute => theme::text_mute(),
            Tone::Accent => theme::accent(),
        }
    }
}

#[derive(Clone, Debug)]
struct Run {
    x: f32,
    y: f32,
    text: String,
    size: f32,
    bold: bool,
    code: bool,
    tone: Tone,
}

#[derive(Clone, Copy, Debug)]
enum DecoKind {
    /// 코드 칸 바탕 + 테두리.
    Code,
    /// 글 사이 `code` 바탕.
    Chip,
    /// 인용 왼쪽 줄.
    Bar,
    Rule,
    /// 표 바깥 테두리.
    Frame,
    /// 표 머리 줄 바탕.
    Shade,
}

#[derive(Clone, Debug)]
struct Deco {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    kind: DecoKind,
}

/// 왼쪽 위 (0,0) 기준으로 편 글 덩어리.
#[derive(Clone, Debug, Default)]
struct Block {
    runs: Vec<Run>,
    decos: Vec<Deco>,
    w: f32,
    h: f32,
}

#[derive(Clone, Debug)]
struct Style {
    size: f32,
    bold: bool,
    code: bool,
    tone: Tone,
}

fn measure(g: &mut gpu::GpuRenderer, text: &str, st: &Style) -> f32 {
    if st.code {
        g.measure_code_text(text, st.size)
    } else {
        g.measure_chrome_text(text, st.size, st.bold)
    }
}

/// 한 줄씩 흘려 쓰는 붓. 같은 꼴이 이어지면 한 조각으로 붙여 그리기 수를 줄인다.
struct Pen<'a> {
    out: &'a mut Block,
    x0: f32,
    right: f32,
    x: f32,
    y: f32,
    line: f32,
    cur: Option<(Run, f32)>,
}

impl Pen<'_> {
    fn flush(&mut self) {
        let Some((run, w)) = self.cur.take() else { return };
        if run.code {
            // 글자 높이에 맞춰 가운데 — 줄 높이로 깔면 줄이 넓은 본문에서 칩이 아래로 처진다.
            self.out.decos.push(Deco { x: run.x - 3.0, y: run.y - 3.0, w: w + 6.0, h: run.size + 7.0, kind: DecoKind::Chip });
        }
        self.out.w = self.out.w.max(run.x + w + if run.code { 3.0 } else { 0.0 });
        self.out.runs.push(run);
    }

    fn newline(&mut self) {
        self.flush();
        self.x = self.x0;
        self.y += self.line;
    }

    fn put(&mut self, text: &str, w: f32, st: &Style) {
        let y_text = self.y + (self.line - st.size) / 2.0 - 1.0;
        if let Some((run, rw)) = self.cur.as_mut() {
            if run.bold == st.bold && run.code == st.code && run.tone == st.tone && run.size == st.size
                && (run.y - y_text).abs() < 0.5 && (run.x + *rw - self.x).abs() < 0.5
            {
                run.text.push_str(text);
                *rw += w;
                self.x += w;
                return;
            }
        }
        self.flush();
        self.cur = Some((Run { x: self.x, y: y_text, text: text.to_string(), size: st.size, bold: st.bold, code: st.code, tone: st.tone }, w));
        self.x += w;
    }

    /// 낱말 단위로 흘린다. 한 낱말이 한 줄보다 길면(경로·주소) 그때만 글자 단위로 자른다.
    fn text(&mut self, g: &mut gpu::GpuRenderer, text: &str, st: &Style) {
        for (pi, part) in text.split('\n').enumerate() {
            if pi > 0 {
                self.newline();
            }
            for token in part.split_inclusive(' ') {
                let word = token.trim_end_matches(' ');
                if !word.is_empty() {
                    let ww = measure(g, word, st);
                    if self.x + ww > self.right && self.x > self.x0 {
                        self.newline();
                    }
                    if ww > self.right - self.x0 {
                        for ch in word.chars() {
                            let mut buf = [0u8; 4];
                            let s = ch.encode_utf8(&mut buf);
                            let cw = measure(g, s, st);
                            if self.x + cw > self.right && self.x > self.x0 {
                                self.newline();
                            }
                            self.put(s, cw, st);
                        }
                    } else {
                        self.put(word, ww, st);
                    }
                }
                if token.len() > word.len() && self.x > self.x0 {
                    let sw = measure(g, " ", st);
                    self.put(" ", sw, st);
                }
            }
        }
    }

    /// 다 쓰고 차지한 높이.
    fn finish(mut self, y0: f32) -> f32 {
        self.flush();
        self.y + self.line - y0
    }
}

fn span_style(span: &MdSpan, base: &Style) -> Style {
    Style {
        size: if span.code { base.size - 1.0 } else { base.size },
        bold: base.bold || span.bold,
        code: span.code,
        tone: if span.link.is_some() { Tone::Accent } else { base.tone },
    }
}

fn flow_spans(g: &mut gpu::GpuRenderer, out: &mut Block, spans: &[MdSpan], x0: f32, y0: f32, max_w: f32, base: &Style, line: f32) -> f32 {
    let mut pen = Pen { out, x0, right: x0 + max_w, x: x0, y: y0, line, cur: None };
    for span in spans {
        pen.text(g, &span.text, &span_style(span, base));
    }
    pen.finish(y0)
}

fn flow_plain(g: &mut gpu::GpuRenderer, out: &mut Block, text: &str, x0: f32, y0: f32, max_w: f32, st: &Style, line: f32) -> f32 {
    let mut pen = Pen { out, x0, right: x0 + max_w, x: x0, y: y0, line, cur: None };
    pen.text(g, text, st);
    pen.finish(y0)
}

/// 본문 줄 높이 — 글 14 에 22.
const LINE: f32 = 22.0;

fn body_style(tone: Tone) -> Style {
    Style { size: 14.0, bold: false, code: false, tone }
}

/// 코드 칸 — 고정폭 12.5, 줄 19, 안 여백 12, 언어가 있으면 머리 줄 24. 줄은 글자 단위로 접는다(코드에 낱말 경계가 없다).
fn code_block(g: &mut gpu::GpuRenderer, out: &mut Block, code: &str, lang: &str, y0: f32, max_w: f32, max_lines: usize) -> f32 {
    let st = Style { size: 12.5, bold: false, code: true, tone: Tone::Text };
    let inner = (max_w - 24.0).max(20.0);
    let mut lines: Vec<String> = Vec::new();
    for raw in code.trim_end_matches('\n').split('\n') {
        let raw = raw.replace('\t', "    ");
        let mut line = String::new();
        let mut w = 0.0;
        for ch in raw.chars() {
            let mut buf = [0u8; 4];
            let cw = measure(g, ch.encode_utf8(&mut buf), &st);
            if w + cw > inner && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                w = 0.0;
            }
            line.push(ch);
            w += cw;
        }
        lines.push(line);
    }
    let hidden = lines.len().saturating_sub(max_lines);
    lines.truncate(max_lines);
    if hidden > 0 {
        lines.push(format!("… {hidden}줄 더"));
    }
    let lang = lang.trim();
    // 머리 줄이 있으면 그 줄이 위 여백을 대신한다 — 따로 더하면 언어 이름과 첫 줄 사이가 휑하다.
    let (head, top) = if lang.is_empty() { (0.0, 12.0) } else { (24.0, 2.0) };
    if head > 0.0 {
        out.runs.push(Run { x: 12.0, y: y0 + 7.0, text: lang.to_string(), size: 11.0, bold: false, code: false, tone: Tone::Mute });
    }
    for (i, line) in lines.iter().enumerate() {
        let tone = if hidden > 0 && i == lines.len() - 1 { Tone::Mute } else { Tone::Text };
        out.runs.push(Run { x: 12.0, y: y0 + head + top + i as f32 * 19.0 + 2.0, text: line.clone(), size: 12.5, bold: false, code: true, tone });
    }
    let h = head + top + lines.len() as f32 * 19.0 + 12.0;
    // Claude 앱처럼 열 폭을 다 쓴다 — 글 폭에 맞추면 칸마다 오른쪽 끝이 들쭉날쭉하다.
    out.decos.push(Deco { x: 0.0, y: y0, w: max_w, h, kind: DecoKind::Code });
    out.w = out.w.max(max_w);
    h
}

/// 표 — 바깥 테두리·칸 선·머리 바탕, 열 폭은 내용 폭에 비례(최소 48), 칸 안에서 접는다.
fn table_block(g: &mut gpu::GpuRenderer, out: &mut Block, head: &[Vec<MdSpan>], rows: &[Vec<Vec<MdSpan>>], y0: f32, max_w: f32, tone: Tone) -> f32 {
    const PAD_X: f32 = 10.0;
    const PAD_Y: f32 = 6.0;
    const TLINE: f32 = 20.0;
    let lines: Vec<&[Vec<MdSpan>]> = std::iter::once(head).chain(rows.iter().map(Vec::as_slice)).filter(|r| !r.is_empty()).collect();
    let cols = lines.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return 0.0;
    }
    let style = |bold: bool| Style { size: 13.0, bold, code: false, tone };
    let mut natural = vec![0.0f32; cols];
    for (ri, row) in lines.iter().enumerate() {
        for (ci, cell) in row.iter().enumerate() {
            let w: f32 = cell.iter().map(|s| measure(g, &s.text, &span_style(s, &style(ri == 0)))).sum();
            natural[ci] = natural[ci].max(w + PAD_X * 2.0);
        }
    }
    let total: f32 = natural.iter().sum();
    let widths: Vec<f32> = if total <= max_w { natural } else { natural.iter().map(|w| (w / total * max_w).max(48.0)).collect() };
    let table_w = widths.iter().sum::<f32>().min(max_w);
    let mut y = y0;
    for (ri, row) in lines.iter().enumerate() {
        let st = style(ri == 0);
        // 머리 바탕은 그 줄의 글·칩보다 먼저 깔려야 한다 — 줄을 편 뒤 자리를 알고 앞에 끼운다.
        let deco_at = out.decos.len();
        let mut row_h = TLINE;
        let mut x = 0.0;
        for (ci, cw) in widths.iter().enumerate() {
            if let Some(cell) = row.get(ci) {
                row_h = row_h.max(flow_spans(g, out, cell, x + PAD_X, y + PAD_Y, (cw - PAD_X * 2.0).max(10.0), &st, TLINE));
            }
            x += cw;
        }
        let h = row_h + PAD_Y * 2.0;
        if ri == 0 {
            out.decos.insert(deco_at, Deco { x: 1.0, y: y + 1.0, w: table_w - 2.0, h: h - 1.0, kind: DecoKind::Shade });
        } else {
            out.decos.push(Deco { x: 0.0, y, w: table_w, h: 1.0, kind: DecoKind::Rule });
        }
        y += h;
    }
    let mut x = 0.0;
    for cw in &widths[..widths.len() - 1] {
        x += cw;
        out.decos.push(Deco { x, y: y0, w: 1.0, h: y - y0, kind: DecoKind::Rule });
    }
    out.decos.push(Deco { x: 0.0, y: y0, w: table_w, h: y - y0, kind: DecoKind::Frame });
    out.w = out.w.max(table_w);
    y - y0
}

/// 답·말풍선 본문 — 마크다운을 문단·제목·목록·인용·코드·표로 편다.
fn markdown_block(g: &mut gpu::GpuRenderer, text: &str, max_w: f32, tone: Tone) -> Block {
    let mut out = Block::default();
    let (blocks, _) = crate::parse_markdown(text);
    let mut y = 0.0;
    let body = body_style(tone);
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            y += 10.0;
        }
        y += match block {
            MdBlock::Heading { level, spans } => {
                let big = *level <= 2;
                let st = Style { size: if big { 17.0 } else { 14.0 }, bold: true, code: false, tone };
                flow_spans(g, &mut out, spans, 0.0, y, max_w, &st, if big { 26.0 } else { LINE })
            }
            MdBlock::Para { spans } => flow_spans(g, &mut out, spans, 0.0, y, max_w, &body, LINE),
            MdBlock::Code { code, lang } => code_block(g, &mut out, code, lang, y, max_w, 40),
            MdBlock::ListItem { depth, marker, spans, task } => {
                let indent = *depth as f32 * 18.0;
                let mark = match task {
                    Some(true) => "☑".to_string(),
                    Some(false) => "☐".to_string(),
                    None => marker.clone(),
                };
                let mw = g.measure_chrome_text(&mark, 14.0, false);
                let lead = mw.max(12.0) + 8.0;
                out.runs.push(Run { x: indent, y: y + 3.0, text: mark, size: 14.0, bold: false, code: false, tone: Tone::Dim });
                flow_spans(g, &mut out, spans, indent + lead, y, (max_w - indent - lead).max(20.0), &body, LINE)
            }
            MdBlock::Quote { spans } | MdBlock::Callout { spans, .. } => {
                let h = flow_spans(g, &mut out, spans, 12.0, y, (max_w - 12.0).max(20.0), &body_style(Tone::Dim), LINE);
                out.decos.push(Deco { x: 0.0, y, w: 2.0, h, kind: DecoKind::Bar });
                h
            }
            MdBlock::Rule => {
                out.decos.push(Deco { x: 0.0, y: y + 10.0, w: max_w, h: 1.0, kind: DecoKind::Rule });
                21.0
            }
            MdBlock::Meta { rows } => {
                let text = rows.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
                flow_plain(g, &mut out, &text, 0.0, y, max_w, &body_style(Tone::Dim), LINE)
            }
            MdBlock::Image { alt, .. } => {
                let text = if alt.is_empty() { "(그림)".to_string() } else { format!("(그림) {alt}") };
                flow_plain(g, &mut out, &text, 0.0, y, max_w, &body_style(Tone::Dim), LINE)
            }
            MdBlock::Table { head, rows, .. } => table_block(g, &mut out, head, rows, y, max_w, tone),
        };
    }
    out.h = y.max(LINE);
    out
}

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Running,
    Ok,
    Failed,
}

#[derive(Clone, Debug)]
enum Kind {
    /// 턴 머리 — `via` 는 사람 대신 넣은 말일 때 이름 뒤에 붙는 「쪽지」·「완료 보고」.
    Name { text: String, via: &'static str },
    /// `mine` 은 사람이 친 말 — 셸 묶음의 명령 줄처럼 점과 함께 묶음 머리에 선다. `relay` 는 사람 대신
    /// 다른 칸·카사텀·나쵸가 넣은 말 — 이름 줄 아래 왼쪽 막대 글.
    Bubble { item: usize, mine: bool, queued: bool, relay: bool },
    /// 사람 쪽 줄(내 말·남이 넣은 말·명령)이 여는 새 묶음의 위 선 — 칸 전체 폭.
    Rule,
    /// 접는 머리 한 줄 — 도구 묶음·생각·출력.
    Fold { key: usize, icon: &'static str, label: String, detail: String, open: bool, failed: usize, live: bool },
    Tool { item: usize, status: Status, name: String, summary: String, open: bool, has_result: bool },
    /// 펼친 결과·생각·출력. 왼쪽으로 18 들여 바탕을 깐다.
    Body { key: usize },
    Command { text: String },
    Answered { key: usize },
    Note { icon: &'static str, text: String },
    /// 맨 아래 「이름 · 지금 하는 일」 — 하는 일은 mod 가 그때그때 알려 그릴 때 채운다.
    Working,
}

#[derive(Clone, Debug)]
struct LRow {
    y: f32,
    h: f32,
    kind: Kind,
}

#[derive(Default)]
pub(crate) struct Layout {
    /// (기록 판, 다시 받은 횟수, 열 폭, 글꼴 판, 펼침 판, 작업 중)
    key: Option<(u64, u64, u32, u32, u64, bool)>,
    /// 기록에서 온 줄 수 — 0 이면 빈 대화 안내를 띄운다.
    shown: usize,
    open_gen: u64,
    rows: Vec<LRow>,
    height: f32,
    /// 칸 번호 → (편 폭, 글 덩어리). 말풍선·결과·답 카드가 여기 산다.
    blocks: HashMap<usize, (u32, Block)>,
    /// 결과 덩어리를 편 때 그 결과의 길이 — 결과가 늦게 오면 다시 편다.
    result_len: HashMap<usize, usize>,
    generation: u64,
}

impl Layout {
    pub(crate) fn invalidate(&mut self) {
        self.open_gen += 1;
    }
}

/// 한 번에 펴 두는 줄 수 상한. 그 앞은 터미널 보기의 스크롤백에 있다.
const ROW_CAP: usize = 300;

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// 이어진 도구 묶음을 사람 말 한 줄로 — 「명령 2개 실행 · 파일 1개 수정」. 아이콘은 첫 갈래의 것.
fn tools_summary(names: &[&str]) -> (&'static str, String) {
    let kind = |name: &str| -> (&'static str, &'static str) {
        match super::parse::tool_label(name) {
            "Bash" | "BashOutput" | "KillShell" | "KillBash" | "Monitor" | "shell" | "exec_command" => ("terminal", "명령"),
            "Read" | "NotebookRead" => ("file-text", "읽기"),
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ("pencil", "수정"),
            "Grep" | "Glob" | "LS" => ("folder", "찾기"),
            "WebFetch" | "WebSearch" => ("globe", "웹"),
            "Agent" | "Task" => ("users", "서브에이전트"),
            "TodoWrite" | "TaskCreate" | "TaskUpdate" => ("square-check", "할 일"),
            _ => ("terminal", "도구"),
        }
    };
    let mut groups: Vec<((&'static str, &'static str), usize)> = Vec::new();
    for name in names {
        let k = kind(name);
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, n)) => *n += 1,
            None => groups.push((k, 1)),
        }
    }
    let words = |(icon, what): (&'static str, &'static str), n: usize| -> String {
        let _ = icon;
        match what {
            "명령" => format!("명령 {n}개 실행"),
            "읽기" => format!("파일 {n}개 읽음"),
            "수정" => format!("파일 {n}개 수정"),
            "찾기" => format!("{n}번 찾음"),
            "웹" => format!("웹 {n}번"),
            "서브에이전트" => format!("서브에이전트 {n}개"),
            "할 일" => "할 일 정리".to_string(),
            _ => format!("도구 {n}개"),
        }
    };
    let icon = groups.first().map(|((icon, _), _)| *icon).unwrap_or("terminal");
    let mut parts: Vec<String> = groups.iter().take(3).map(|(k, n)| words(*k, *n)).collect();
    if groups.len() > 3 {
        parts.push(format!("외 {}", groups.len() - 3));
    }
    (icon, parts.join(" · "))
}

#[allow(clippy::too_many_arguments)]
fn build(g: &mut gpu::GpuRenderer, layout: &mut Layout, feed: &Feed, open: &HashSet<usize>, col_w: f32, name: &str, working: bool) {
    let items = &feed.conv.items;
    let rows = super::parse::group_rows(items);
    let skip = rows.len().saturating_sub(ROW_CAP);
    let bubble_max = (col_w * 0.75).min(560.0).max(80.0);
    let wbits = col_w.to_bits();
    let mut out: Vec<LRow> = Vec::new();
    let mut y = 0.0;
    // 턴 머리(얼굴·이름)는 학생 쪽 첫 줄 앞에 한 번 — 도구·생각이 끼어도 같은 턴이면 다시 안 선다.
    let mut header: Option<String> = None;
    let n_rows = rows.len();
    let push = |out: &mut Vec<LRow>, y: &mut f32, h: f32, gap: f32, kind: Kind| {
        if !out.is_empty() {
            *y += gap;
        }
        out.push(LRow { y: *y, h, kind });
        *y += h;
    };
    // 머리를 세웠으면 다음 줄은 바짝(4), 이어지는 턴이면 보통 간격.
    let head = |out: &mut Vec<LRow>, y: &mut f32, header: &mut Option<String>, who: &str, via: &'static str, gap: f32| -> f32 {
        let key = format!("{who}\u{0}{via}");
        if header.as_deref() == Some(key.as_str()) {
            return gap;
        }
        *header = Some(key);
        // 사람 쪽 머리 바로 아래 답은 같은 묶음이라 바짝 붙인다.
        let after_head = out.last().is_some_and(|r| matches!(r.kind, Kind::Bubble { mine: true, .. } | Kind::Command { .. }));
        push(out, y, 22.0, if after_head { 10.0 } else { 18.0 }, Kind::Name { text: who.to_string(), via });
        4.0
    };
    if skip > 0 {
        push(&mut out, &mut y, 24.0, 0.0, Kind::Note { icon: "info", text: format!("앞의 {skip}개는 터미널 보기에 있어요") });
    }
    for (ri, row) in rows.iter().enumerate().skip(skip) {
        match row {
            Row::Tools(run) => {
                let key = run[0];
                let is_open = open.contains(&key);
                let failed = run.iter().filter(|&&i| matches!(&items[i], Item::Tool { error: true, .. })).count();
                let live = working && ri + 1 == n_rows && run.iter().any(|&i| matches!(&items[i], Item::Tool { result: None, .. }));
                let names: Vec<&str> = run.iter().filter_map(|&i| match &items[i] { Item::Tool { name, .. } => Some(name.as_str()), _ => None }).collect();
                let (icon, label) = tools_summary(&names);
                let gap = head(&mut out, &mut y, &mut header, name, "", 8.0);
                let detail = run.last().and_then(|&i| match &items[i] {
                    Item::Tool { name, summary, .. } => Some(if summary.is_empty() { tool_label(name).to_string() } else { format!("{} · {summary}", tool_label(name)) }),
                    _ => None,
                }).unwrap_or_default();
                push(&mut out, &mut y, 28.0, gap, Kind::Fold { key, icon, label, detail, open: is_open, failed, live });
                if !is_open {
                    continue;
                }
                for &i in run {
                    let Item::Tool { name, summary, result, error } = &items[i] else { continue };
                    let status = match (result, error) {
                        (None, _) => Status::Running,
                        (Some(_), true) => Status::Failed,
                        (Some(_), false) => Status::Ok,
                    };
                    let has_result = result.as_deref().is_some_and(|r| !r.trim().is_empty());
                    let tool_open = has_result && open.contains(&i);
                    push(&mut out, &mut y, 24.0, 0.0, Kind::Tool { item: i, status, name: tool_label(name).to_string(), summary: summary.clone(), open: tool_open, has_result });
                    if tool_open {
                        let result = result.as_deref().unwrap_or("");
                        let fresh = layout.result_len.get(&i) != Some(&result.len());
                        if fresh || layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                            let mut b = Block::default();
                            let text = super::parse::strip_ansi(result);
                            b.h = code_block(g, &mut b, text.trim_end(), "", 0.0, (col_w - 20.0).max(40.0), 14);
                            layout.blocks.insert(i, (wbits, b));
                            layout.result_len.insert(i, result.len());
                        }
                        let h = layout.blocks[&i].1.h;
                        push(&mut out, &mut y, h, 4.0, Kind::Body { key: i });
                    }
                }
            }
            Row::One(i) => {
                let i = *i;
                match &items[i] {
                    Item::Bubble { user, text, from, queued, images } => {
                        let mine = *user && from.is_none();
                        let relay = from.is_some();
                        let gap = if mine || relay {
                            if !out.is_empty() {
                                push(&mut out, &mut y, 1.0, 14.0, Kind::Rule);
                            }
                            header = None;
                            match from {
                                Some(f) => {
                                    push(&mut out, &mut y, 22.0, 8.0, Kind::Name { text: f.name.clone(), via: f.via });
                                    4.0
                                }
                                None => 8.0,
                            }
                        } else {
                            head(&mut out, &mut y, &mut header, name, "", 8.0)
                        };
                        if layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                            let mut body = text.clone();
                            if *images > 0 {
                                let mark = if *images == 1 { "(사진)".to_string() } else { format!("(사진 {images}장)") };
                                body = if body.is_empty() { mark } else { format!("{mark}\n{body}") };
                            }
                            let tone = if *queued { Tone::Dim } else { Tone::Text };
                            // 내 말은 점 뒤(16)부터, 남이 넣은 말은 막대 뒤(12)부터 — 오른쪽 34 는 올리면 서는 복사 자리.
                            let max_w = if mine { col_w - 16.0 - 34.0 } else if relay { col_w - 12.0 - 34.0 } else { col_w };
                            let b = markdown_block(g, &body, max_w, tone);
                            layout.blocks.insert(i, (wbits, b));
                        }
                        let b = &layout.blocks[&i].1;
                        let h = if mine { b.h + 12.0 } else { b.h } + if *queued { 18.0 } else { 0.0 };
                        push(&mut out, &mut y, h, gap, Kind::Bubble { item: i, mine, queued: *queued, relay });
                    }
                    Item::Thinking(text) | Item::Output(text) => {
                        let thinking = matches!(&items[i], Item::Thinking(_));
                        let is_open = open.contains(&i);
                        let (icon, label) = if thinking { ("lightbulb", "생각") } else { ("terminal", "출력") };
                        let gap = head(&mut out, &mut y, &mut header, name, "", 8.0);
                        push(&mut out, &mut y, 28.0, gap, Kind::Fold { key: i, icon, label: label.into(), detail: first_line(text).to_string(), open: is_open, failed: 0, live: false });
                        if is_open {
                            if layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                                let mut b = Block::default();
                                b.h = if thinking {
                                    flow_plain(g, &mut b, text, 0.0, 4.0, (col_w - 20.0).max(40.0), &Style { size: 12.5, bold: false, code: false, tone: Tone::Dim }, 19.0) + 8.0
                                } else {
                                    code_block(g, &mut b, text, "", 0.0, (col_w - 20.0).max(40.0), 40)
                                };
                                layout.blocks.insert(i, (wbits, b));
                            }
                            let h = layout.blocks[&i].1.h;
                            push(&mut out, &mut y, h, 4.0, Kind::Body { key: i });
                        }
                    }
                    Item::Command { name: cmd, args } => {
                        header = None;
                        let text = if cmd == "!" { format!("! {args}") } else { format!("{cmd} {args}").trim().to_string() };
                        if !out.is_empty() {
                            push(&mut out, &mut y, 1.0, 14.0, Kind::Rule);
                        }
                        push(&mut out, &mut y, 26.0, 8.0, Kind::Command { text });
                    }
                    Item::Answered { pairs: Some(pairs), .. } => {
                        header = None;
                        if layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                            let mut b = Block::default();
                            let mut by = 10.0;
                            let inner = (bubble_max - 28.0).max(40.0);
                            for (q, a) in pairs {
                                by += flow_plain(g, &mut b, q, 14.0, by, inner, &Style { size: 11.0, bold: false, code: false, tone: Tone::Dim }, 17.0);
                                by += flow_plain(g, &mut b, a, 14.0, by, inner, &body_style(Tone::Text), LINE) + 4.0;
                            }
                            b.h = by + 6.0;
                            b.w += 14.0;
                            layout.blocks.insert(i, (wbits, b));
                        }
                        let h = layout.blocks[&i].1.h;
                        push(&mut out, &mut y, h, 12.0, Kind::Answered { key: i });
                    }
                    Item::Launch(label) => {
                        push(&mut out, &mut y, 24.0, 8.0, Kind::Note { icon: "users", text: format!("서브에이전트 · {label}") });
                    }
                    Item::System(text) => {
                        push(&mut out, &mut y, 24.0, 8.0, Kind::Note { icon: "info", text: text.clone() });
                    }
                    Item::Interrupted => {
                        push(&mut out, &mut y, 24.0, 8.0, Kind::Note { icon: "octagon-alert", text: "작업을 멈췄어요".into() });
                    }
                    Item::Tool { .. } | Item::Answered { pairs: None, .. } | Item::Gone => {}
                }
            }
        }
    }
    layout.shown = out.len();
    if working {
        push(&mut out, &mut y, 24.0, 12.0, Kind::Working);
    }
    layout.rows = out;
    layout.height = y;
}

/// 입력칸 글을 칸 폭에 접은 줄들과, 캐럿이 앉을 (줄, 그 줄 안 x). 조합 중인 글은 캐럿 자리에
/// 끼워 같이 접는다 — 따로 그리면 줄 끝에서 조합 글자가 칸 밖으로 샌다.
fn composer_lines(g: &mut gpu::GpuRenderer, draft: &str, cursor: usize, preedit: &str, width: f32) -> (Vec<String>, usize, f32, Option<(usize, f32, f32)>) {
    let col = cursor.min(draft.chars().count());
    let (head, tail) = crate::lineedit::split(draft, col);
    let full = format!("{head}{preedit}{tail}");
    let caret_at = head.chars().count() + preedit.chars().count();
    let pre_from = head.chars().count();
    let mut lines: Vec<String> = vec![String::new()];
    let mut w = 0.0;
    let mut caret = (0usize, 0.0f32);
    let mut pre: Option<(usize, f32, f32)> = None;
    for (i, ch) in full.chars().enumerate() {
        if i == caret_at {
            caret = (lines.len() - 1, w);
        }
        if i == pre_from && !preedit.is_empty() {
            pre = Some((lines.len() - 1, w, 0.0));
        }
        if ch == '\n' {
            lines.push(String::new());
            w = 0.0;
            continue;
        }
        let mut buf = [0u8; 4];
        let cw = g.measure_chrome_text(ch.encode_utf8(&mut buf), 13.0, false);
        if w + cw > width && w > 0.0 {
            lines.push(String::new());
            w = 0.0;
        }
        lines.last_mut().unwrap().push(ch);
        w += cw;
        if let Some((_, _, pw)) = pre.as_mut() {
            if i < pre_from + preedit.chars().count() {
                *pw += cw;
            }
        }
    }
    if caret_at >= full.chars().count() {
        caret = (lines.len() - 1, w);
    }
    let caret_line = caret.0;
    (lines, caret_line, caret.1, pre)
}

fn draw_run(g: &mut gpu::GpuRenderer, ox: f32, oy: f32, run: &Run) {
    if run.code {
        g.draw_code_text(ox + run.x, oy + run.y, &run.text, run.size, run.tone.color());
    } else {
        g.draw_text(ox + run.x, oy + run.y, &run.text, gpu::DrawOpts { font_size: run.size, color: run.tone.color(), bold: run.bold, italic: false });
    }
}

fn draw_block(g: &mut gpu::GpuRenderer, ox: f32, oy: f32, b: &Block, fill: [u8; 4], clip: (f32, f32)) {
    let chip_bg = theme::lerp(fill, theme::text(), 0.07);
    let panel_bg = theme::lerp(fill, theme::text(), 0.04);
    let edge = theme::border_w().max(1.0);
    for d in &b.decos {
        if oy + d.y + d.h < clip.0 || oy + d.y > clip.1 {
            continue;
        }
        let (dx, dy) = (ox + d.x, oy + d.y);
        match d.kind {
            DecoKind::Code => {
                g.round_rect_fill(dx, dy, d.w, d.h, theme::radius_md(), panel_bg);
                g.round_rect_stroke(dx, dy, d.w, d.h, theme::radius_md(), edge, theme::border());
            }
            DecoKind::Chip => g.round_rect_fill(dx, dy, d.w, d.h, 4.0, chip_bg),
            DecoKind::Bar => g.rect(dx, dy, d.w, d.h, theme::text_mute()),
            DecoKind::Rule => g.rect(dx, dy, d.w, d.h, theme::border()),
            DecoKind::Shade => g.rect(dx, dy, d.w, d.h, panel_bg),
            DecoKind::Frame => g.round_rect_stroke(dx, dy, d.w, d.h, theme::radius_sm(), edge, theme::border()),
        }
    }
    for run in &b.runs {
        let y = oy + run.y;
        if y + run.size * 1.4 < clip.0 || y > clip.1 {
            continue;
        }
        draw_run(g, ox, oy, run);
    }
}

fn label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str, size: f32, color: [u8; 4], bold: bool) {
    g.draw_text(x, y, text, gpu::DrawOpts { font_size: size, color, bold, italic: false });
}

fn inside(c: (f32, f32), r: Rect) -> bool {
    c.0 >= r.0 && c.0 < r.0 + r.2 && c.1 >= r.1 && c.1 < r.1 + r.3
}

/// 승인·질문 판 원문 한 줄 높이.
const CTX_LINE: f32 = 17.0;
/// 카드에 펴는 원문 줄 상한 — 그 뒤는 「… N줄 더」로 접고 전부는 터미널 보기에서 본다.
const CTX_MAX: usize = 8;

struct CardOption {
    label: String,
    note: String,
    current: bool,
    hit: Hit,
}

/// 입력칸 위 카드 한 장 — 화면에서 읽은 TUI 선택지, 또는 화면이 아직 없을 때 mod 의 승인 요청.
struct Card {
    context: Vec<String>,
    title: String,
    options: Vec<CardOption>,
    /// 거절 선택지 이름 — 그것을 mod 결정으로 보내 입력칸 글이 까닭이 된다.
    reason_hint: Option<String>,
}

impl Card {
    fn height(&self) -> f32 {
        let ctx = self.context.len() as f32 * CTX_LINE + if self.context.is_empty() { 0.0 } else { 6.0 };
        let title = if self.title.is_empty() { 0.0 } else { 26.0 };
        let options: f32 = self.options.iter().map(|o| if o.note.is_empty() { 36.0 } else { 50.0 }).sum();
        12.0 + ctx + title + options + if self.reason_hint.is_some() { 18.0 } else { 0.0 } + 26.0 + 12.0
    }
}

fn clip_lines(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > CTX_MAX {
        let more = lines.len() - (CTX_MAX - 1);
        lines.truncate(CTX_MAX - 1);
        lines.push(format!("… {more}줄 더"));
    }
    lines
}

fn card_of(menu: Option<&super::parse::PromptMenu>, live: &ModLive) -> Option<Card> {
    if let Some(m) = menu {
        let options = m
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| CardOption { label: format!("{}. {}", o.index, o.label), note: o.note.clone(), current: o.current, hit: Hit::Pick(i) })
            .collect();
        return Some(Card {
            context: clip_lines(m.context.clone()),
            title: m.title.clone(),
            options,
            reason_hint: (0..m.options.len())
                .find(|&i| live::decision_for(m, live, i).is_some_and(|(allow, _)| !allow))
                .map(|i| m.options[i].label.clone()),
        });
    }
    // 질문은 허락·거절로 답하는 것이 아니다 — 화면의 선택지를 기다린다.
    let p = live.permissions.first().filter(|p| live.live && p.tool != "AskUserQuestion")?;
    let mut context = vec![tool_label(&p.tool).to_string()];
    context.extend(live::input_lines(p));
    Some(Card {
        context: clip_lines(context),
        title: "허락할까요?".into(),
        options: vec![
            CardOption { label: "허락".into(), note: String::new(), current: false, hit: Hit::Decide(true) },
            CardOption { label: "거절".into(), note: String::new(), current: false, hit: Hit::Decide(false) },
        ],
        reason_hint: Some("거절".into()),
    })
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, cursor: (f32, f32), slot: &Slot, pane: &mut ChatPane, feed: Option<&Feed>) -> Vec<(Hit, Rect)> {
    let mut hits: Vec<(Hit, Rect)> = Vec::new();
    let (x, y, w, h) = slot.rect;
    if w < 60.0 || h < 80.0 {
        return hits;
    }
    // PC 는 명령 묶음 보기와 같은 판 — 가운데 읽기 열 없이 칸 폭을 다 쓰고 글은 왼쪽 20 에서 선다
    // (2026-10-08 「셸 대화형과 모양 비슷하게, 양옆 여백 없이」).
    let col_x = x + 20.0;
    let col_w = (w - 40.0).max(40.0);
    g.push_clip(x, y, w, h);

    // ── 입력 띠 — 위 선, 글 줄, 그 아래 단추 줄 32 ─────────────────────────
    let inner_w = col_w.max(20.0);
    let (lines, caret_line, caret_x, pre) = composer_lines(g, &pane.draft, pane.cursor, &slot.preedit, inner_w);
    let shown = lines.len().clamp(1, 6);
    let first = (caret_line + 1).saturating_sub(shown).min(lines.len().saturating_sub(shown));
    let text_h = shown as f32 * 20.0;
    let composer_h = 1.0 + 8.0 + text_h + 2.0 + 32.0 + 6.0;
    let comp_y = y + h - composer_h;

    // ── 선택지 판·기다림 줄 ─────────────────────────────────────────────
    let mod_live = pane.mod_live();
    pane.live_painted = pane.live.lock().ok().map(|l| l.clone());
    let working = slot.working || (mod_live.live && (mod_live.turn_open || mod_live.compacting));
    let needs_you = slot.needs_you || !mod_live.permissions.is_empty() || mod_live.question;
    let card = card_of(pane.menu.as_ref(), &mod_live);
    let card_h = match &card {
        Some(c) => c.height(),
        None if needs_you => 40.0,
        None => 0.0,
    };
    let list_y = y + 8.0;
    let list_bottom = comp_y - 6.0 - card_h - if card_h > 0.0 { 8.0 } else { 0.0 };
    let list_h = (list_bottom - list_y).max(0.0);

    // ── 대화 목록 ─────────────────────────────────────────────────────────
    if let Some(f) = feed {
        if pane.layout.generation != f.generation {
            pane.layout.blocks.clear();
            pane.layout.result_len.clear();
            pane.open.clear();
            pane.layout.generation = f.generation;
        }
        let key = (f.version, f.generation, col_w.to_bits(), theme::ui_font_gen(), pane.layout.open_gen, working);
        if pane.layout.key != Some(key) {
            build(g, &mut pane.layout, f, &pane.open, col_w, &slot.name, working);
            pane.layout.key = Some(key);
        }
    }
    let empty: Option<(&str, String)> = match feed {
        None if pane.layout.key.is_none() => Some(("대화를 불러오고 있어요", String::new())),
        None => None,
        Some(f) if !f.loaded => Some(("대화를 불러오고 있어요", String::new())),
        Some(f) if f.error.is_some() => Some(("대화를 못 읽었어요", f.error.clone().unwrap_or_default())),
        Some(f) if f.missing || pane.layout.shown == 0 => Some((
            "아직 대화가 없어요",
            if f.missing {
                "이 창에 묶인 대화 기록이 아직 없어요. 학생이 첫 답을 하면 여기에 떠요.".into()
            } else {
                "말을 걸면 여기에 떠요.".into()
            },
        )),
        Some(_) => None,
    };
    // 셸 묶음처럼 위에서부터 쌓고 입력 띠는 마지막 말 바로 아래에 세운다 — 칸이 차면 바닥에 붙는다.
    let slack = if empty.is_none() { (list_h - pane.layout.height - 16.0).max(0.0) } else { 0.0 };
    let comp_y = comp_y - slack;
    let list_bottom = list_bottom - slack;
    let list_h = list_h - slack;
    if let Some((title, body)) = &empty {
        let tw = g.measure_chrome_text(title, 15.0, true);
        let cy = list_y + (list_h / 2.0 - 44.0).max(0.0);
        label(g, col_x + (col_w - tw) / 2.0, cy, title, 15.0, theme::text(), true);
        let mut b = Block::default();
        let bw = col_w.min(380.0);
        let body_st = Style { size: 13.0, bold: false, code: false, tone: Tone::Dim };
        let bh = if body.is_empty() { 0.0 } else { flow_plain(g, &mut b, body, 0.0, 0.0, bw, &body_st, 20.0) };
        // 줄마다 가운데로 — 왼쪽 맞춤이면 둘째 줄이 제목 아래에서 어긋나 보인다.
        let mut ends: Vec<(f32, f32)> = Vec::new();
        for r in &b.runs {
            let right = r.x + g.measure_chrome_text(&r.text, r.size, r.bold);
            match ends.iter_mut().find(|(y, _)| (*y - r.y).abs() < 0.5) {
                Some(line) => line.1 = line.1.max(right),
                None => ends.push((r.y, right)),
            }
        }
        for r in &mut b.runs {
            if let Some((_, right)) = ends.iter().find(|(y, _)| (*y - r.y).abs() < 0.5) {
                r.x += (bw - right) / 2.0;
            }
        }
        let bx = col_x + (col_w - bw) / 2.0;
        draw_block(g, bx, cy + 28.0, &b, slot.bg, (f32::MIN, f32::MAX));
        let btn = (col_x + (col_w - 110.0) / 2.0, cy + 40.0 + bh, 110.0, 26.0);
        native_controls::text_button(g, btn, cursor, "터미널로 보기", native_controls::Style::default());
        hits.push((Hit::Terminal, btn));
        pane.scroll_max = 0.0;
        pane.scroll = 0.0;
    } else {
        let layout = &pane.layout;
        let content_h = layout.height;
        pane.scroll_max = (content_h + 16.0 - list_h).max(0.0);
        pane.scroll = pane.scroll.clamp(0.0, pane.scroll_max);
        let top = list_y + list_h - 8.0 - content_h + pane.scroll;
        g.push_clip(x, list_y, w, list_h);
        let clip = (list_y, list_bottom);
        let in_list = |c: (f32, f32)| inside(c, (x, list_y, w, list_h));
        for row in &layout.rows {
            let ry = top + row.y;
            if ry + row.h < list_y || ry > list_bottom {
                continue;
            }
            match &row.kind {
                Kind::Name { text, via } => {
                    // 얼굴 그림은 자르기를 안 타고 위에 얹힌다 — 줄이 목록 안에 다 들어올 때만 그린다.
                    let face = ry + 3.0 >= list_y && ry + 19.0 <= list_bottom && crate::sprites::draw_student_face(g, text, col_x, ry + 3.0, 16.0);
                    let nx = col_x + if face { 22.0 } else { 0.0 };
                    let t = crate::info::fit_text(g, text, (col_x + col_w - nx).max(0.0), 12.0, true);
                    label(g, nx, ry + 4.0, &t, 12.0, theme::text_dim(), true);
                    if !via.is_empty() {
                        let vx = nx + g.measure_chrome_text(&t, 12.0, true) + 6.0;
                        label(g, vx, ry + 4.0, &format!("· {via}"), 12.0, theme::text_mute(), false);
                    }
                }
                Kind::Rule => g.rect(x, ry, w, 1.0, theme::with_alpha(theme::border(), 140)),
                Kind::Bubble { item, relay: true, .. } => {
                    let Some((_, b)) = layout.blocks.get(item) else { continue };
                    g.rect(col_x, ry, 2.0, row.h, theme::border());
                    draw_block(g, col_x + 12.0, ry, b, slot.bg, clip);
                    let r = (col_x, ry, col_w, row.h);
                    let cr = (col_x + col_w - 26.0, ry, 26.0, 26.0);
                    if in_list(cursor) && inside(cursor, r) {
                        native_controls::icon_button(g, cr, cursor, "copy", native_controls::Style::default());
                        hits.push((Hit::Copy(*item), cr));
                    }
                }
                Kind::Bubble { item, mine, queued, relay: false } => {
                    let Some((_, b)) = layout.blocks.get(item) else { continue };
                    if *mine {
                        let dot = if *queued { theme::attention() } else { theme::accent() };
                        crate::circle_rect(g, col_x, ry + 6.0 + (LINE - 8.0) / 2.0, 8.0, dot);
                        draw_block(g, col_x + 16.0, ry + 6.0, b, slot.bg, clip);
                        if *queued {
                            label(g, col_x + 16.0, ry + row.h - 15.0, "예약 · 지금 일이 끝나면 읽어요", 11.0, theme::text_mute(), false);
                        }
                        // 올리면 오른쪽에 복사 — 글 고르기가 없어 이것이 꺼내는 길이다.
                        let r = (col_x, ry, col_w, row.h);
                        let cr = (col_x + col_w - 26.0, ry + 3.0, 26.0, 26.0);
                        if in_list(cursor) && inside(cursor, r) {
                            native_controls::icon_button(g, cr, cursor, "copy", native_controls::Style::default());
                            hits.push((Hit::Copy(*item), cr));
                        }
                    } else {
                        draw_block(g, col_x, ry, b, slot.bg, clip);
                        let r = (col_x, ry, col_w, row.h);
                        let cr = (col_x + col_w - 26.0, ry, 26.0, 26.0);
                        if in_list(cursor) && inside(cursor, r) {
                            native_controls::icon_button(g, cr, cursor, "copy", native_controls::Style::default());
                            hits.push((Hit::Copy(*item), cr));
                        }
                    }
                }
                Kind::Fold { key, icon, label: name, detail, open, failed, live } => {
                    let r = (col_x, ry, col_w, row.h);
                    let hover = inside(cursor, r) && in_list(cursor);
                    g.hover_pointer |= hover;
                    let mid = ry + row.h / 2.0;
                    g.queue_icon(icon, col_x, mid - 6.5, 13.0, theme::text_mute());
                    let mut tx = col_x + 20.0;
                    let main = if hover { theme::text() } else { theme::text_dim() };
                    let n = crate::info::fit_text(g, name, (col_w - 40.0).max(0.0), 12.5, false);
                    label(g, tx, mid - 7.5, &n, 12.5, main, false);
                    tx += g.measure_chrome_text(&n, 12.5, false) + 8.0;
                    if *live {
                        let t = "도는 중";
                        label(g, tx, mid - 7.0, t, 12.0, theme::accent(), false);
                        tx += g.measure_chrome_text(t, 12.0, false) + 8.0;
                    }
                    if *failed > 0 {
                        let t = format!("실패 {failed}");
                        label(g, tx, mid - 7.0, &t, 12.0, theme::danger(), false);
                        tx += g.measure_chrome_text(&t, 12.0, false) + 8.0;
                    }
                    let d = crate::info::fit_text(g, detail, (col_x + col_w - 20.0 - tx).max(0.0), 12.0, false);
                    if !d.is_empty() {
                        label(g, tx, mid - 7.0, &d, 12.0, theme::text_mute(), false);
                        tx += g.measure_chrome_text(&d, 12.0, false) + 6.0;
                    }
                    g.queue_icon(if *open { "chevron-down" } else { "chevron-right" }, tx, mid - 6.0, 12.0, theme::text_mute());
                    hits.push((Hit::Fold(*key), r));
                }
                Kind::Tool { item, status, name, summary, open, has_result } => {
                    let r = (col_x + 20.0, ry, col_w - 20.0, row.h);
                    let mid = ry + row.h / 2.0;
                    if *has_result && inside(cursor, r) && in_list(cursor) {
                        g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::lerp(slot.bg, theme::text(), 0.08));
                        g.hover_pointer = true;
                    }
                    let (icon, color) = match status {
                        Status::Running => ("chevron-right", theme::accent()),
                        Status::Ok => ("check", theme::success()),
                        Status::Failed => ("x", theme::danger()),
                    };
                    g.queue_icon(icon, r.0 + 4.0, mid - 6.0, 12.0, color);
                    let nx = r.0 + 22.0;
                    let nm = crate::info::fit_text(g, name, (r.2 * 0.4).max(0.0), 12.0, false);
                    label(g, nx, mid - 7.0, &nm, 12.0, theme::text(), false);
                    let sx = nx + g.measure_chrome_text(&nm, 12.0, false) + 8.0;
                    let right = r.0 + r.2 - if *has_result { 22.0 } else { 6.0 };
                    let sm = crate::info::fit_text(g, summary, (right - sx).max(0.0), 12.0, false);
                    label(g, sx, mid - 7.0, &sm, 12.0, theme::text_dim(), false);
                    if *has_result {
                        g.queue_icon(if *open { "chevron-up" } else { "chevron-down" }, r.0 + r.2 - 18.0, mid - 6.0, 12.0, theme::text_mute());
                        hits.push((Hit::Tool(*item), r));
                    }
                }
                Kind::Body { key } => {
                    if let Some((_, b)) = layout.blocks.get(key) {
                        draw_block(g, col_x + 20.0, ry, b, slot.bg, clip);
                    }
                }
                Kind::Command { text } => {
                    crate::circle_rect(g, col_x, ry + (row.h - 8.0) / 2.0, 8.0, theme::text_mute());
                    g.push_clip(col_x + 16.0, ry, (col_w - 16.0).max(0.0), row.h);
                    g.draw_code_text(col_x + 16.0, ry + (row.h - 12.0) / 2.0, text, 12.0, theme::text_dim());
                    g.pop_clip();
                }
                Kind::Answered { key } => {
                    if let Some((_, b)) = layout.blocks.get(key) {
                        let r = (col_x, ry, (b.w + 14.0).min(col_w), row.h);
                        g.round_rect_stroke(r.0, r.1, r.2, r.3, 12.0, theme::border_w().max(1.0), theme::border());
                        draw_block(g, col_x, ry, b, slot.bg, clip);
                    }
                }
                Kind::Working => {
                    let text = format!("{} · {}", slot.name, mod_live.doing().unwrap_or_else(|| "작업 중".into()));
                    let t = crate::info::fit_text(g, &text, (col_w - 24.0).max(0.0), 12.0, false);
                    g.queue_icon("sparkles", col_x, ry + 5.5, 13.0, theme::accent());
                    label(g, col_x + 20.0, ry + 5.0, &t, 12.0, theme::text_dim(), false);
                }
                Kind::Note { icon, text } => {
                    let t = crate::info::fit_text(g, text, (col_w - 24.0).max(0.0), 11.0, false);
                    g.queue_icon(icon, col_x, ry + 6.0, 12.0, theme::text_mute());
                    label(g, col_x + 18.0, ry + 5.5, &t, 11.0, theme::text_mute(), false);
                }
            }
        }
        g.pop_clip();
        if pane.scroll > 240.0 {
            let r = (col_x + col_w - 30.0, list_bottom - 34.0, 26.0, 26.0);
            g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_sm(), slot.bg);
            g.round_rect_stroke(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::border_w().max(1.0), theme::border());
            native_controls::icon_button(g, r, cursor, "chevron-down", native_controls::Style::default());
            hits.push((Hit::Bottom, r));
        }
    }

    // ── 선택지 판 ─────────────────────────────────────────────────────────
    let card_y = comp_y - 6.0 - card_h;
    if let Some(c) = &card {
        let line = if needs_you { theme::attention() } else { theme::accent() };
        g.round_rect_stroke(col_x, card_y, col_w, card_h, theme::radius_md(), theme::border_w().max(1.0), line);
        let mut cy = card_y + 12.0;
        // TUI 창 안의 줄 그대로 — 머리(도구·질문 이름)는 굵게, 원문은 고정폭으로.
        for (i, text) in c.context.iter().enumerate() {
            if i == 0 {
                let t = crate::info::fit_text(g, text, col_w - 24.0, 13.0, true);
                label(g, col_x + 12.0, cy + 1.0, &t, 13.0, theme::text(), true);
            } else {
                g.push_clip(col_x + 12.0, cy, col_w - 24.0, CTX_LINE);
                g.draw_code_text(col_x + 12.0, cy + 2.5, text, 12.0, theme::text_dim());
                g.pop_clip();
            }
            cy += CTX_LINE;
        }
        if !c.context.is_empty() {
            cy += 6.0;
        }
        if !c.title.is_empty() {
            let t = crate::info::fit_text(g, &c.title, col_w - 24.0, 13.0, c.context.is_empty());
            label(g, col_x + 12.0, cy + 4.0, &t, 13.0, theme::text(), c.context.is_empty());
            cy += 26.0;
        }
        for o in &c.options {
            let oh = if o.note.is_empty() { 30.0 } else { 44.0 };
            let r = (col_x + 12.0, cy, col_w - 24.0, oh);
            let hover = inside(cursor, r);
            let edge = if o.current { theme::accent() } else if hover { theme::text_dim() } else { theme::border() };
            g.round_rect_stroke(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::border_w().max(1.0), edge);
            let t = crate::info::fit_text(g, &o.label, r.2 - 24.0, 13.0, o.current);
            label(g, r.0 + 12.0, r.1 + 7.5, &t, 13.0, if o.current { theme::accent() } else { theme::text() }, o.current);
            if !o.note.is_empty() {
                let n = crate::info::fit_text(g, &o.note, r.2 - 24.0, 11.0, false);
                label(g, r.0 + 12.0, r.1 + 25.0, &n, 11.0, theme::text_mute(), false);
            }
            g.hover_pointer |= hover;
            hits.push((o.hit.clone(), r));
            cy += oh + 6.0;
        }
        if let Some(deny) = &c.reason_hint {
            let t = crate::info::fit_text(g, &format!("「{deny}」는 입력칸에 쓴 글을 까닭으로 함께 보내요"), col_w - 24.0, 11.0, false);
            label(g, col_x + 12.0, cy + 1.0, &t, 11.0, theme::text_mute(), false);
            cy += 18.0;
        }
        let esc = (col_x + 12.0, cy, 90.0, 26.0);
        native_controls::text_button(g, esc, cursor, "취소 esc", native_controls::Style::default());
        hits.push((Hit::Dismiss, esc));
        let term = (col_x + col_w - 12.0 - 120.0, cy, 120.0, 26.0);
        native_controls::text_button(g, term, cursor, "터미널에서 보기", native_controls::Style::default());
        hits.push((Hit::Terminal, term));
    } else if needs_you {
        let mid = card_y + card_h / 2.0 - 4.0;
        g.queue_icon("message-square-warning", col_x + 2.0, mid - 7.0, 14.0, theme::attention());
        let bw = 110.0;
        let t = crate::info::fit_text(g, "답을 기다려요 · 고를 것은 터미널 보기에서 확인해요", (col_w - bw - 34.0).max(0.0), 13.0, false);
        label(g, col_x + 22.0, mid - 7.5, &t, 13.0, theme::text(), false);
        let r = (col_x + col_w - bw, mid - 13.0, bw, 26.0);
        native_controls::text_button(g, r, cursor, "터미널로 보기", native_controls::Style::default());
        hits.push((Hit::Terminal, r));
    }

    // ── 입력 띠 — 셸 묶음의 입력 칸처럼 위 선 하나, 상자 없이 ─────────────────
    let box_r = (x, comp_y, w, composer_h);
    g.rect(x, comp_y, w, 1.0, theme::with_alpha(theme::border(), 140));
    let tx = col_x;
    let ty = comp_y + 1.0 + 8.0 + 3.0;
    if pane.draft.is_empty() && slot.preedit.is_empty() {
        let hint = if slot.mirror { "다른 기기 학생에게 보내기" } else { "메시지 보내기" };
        label(g, tx, ty, hint, 13.0, theme::text_mute(), false);
    }
    g.push_clip(x, comp_y + 1.0, w, 8.0 + text_h + 2.0);
    for (i, line) in lines.iter().enumerate().skip(first).take(shown) {
        label(g, tx, ty + (i - first) as f32 * 20.0, line, 13.0, theme::text(), false);
    }
    if let Some((pl, px, pw)) = pre {
        if pl >= first && pl < first + shown && pw > 0.0 {
            g.rect(tx + px, ty + (pl - first) as f32 * 20.0 + 16.0, pw, 1.0, theme::text());
        }
    }
    if slot.focused && slot.caret_on && caret_line >= first && caret_line < first + shown {
        g.rect(tx + caret_x, ty + (caret_line - first) as f32 * 20.0 - 1.5, 1.5, 17.0, theme::accent());
    }
    g.pop_clip();

    // 띠 아래 줄 — 왼쪽 터미널로 보기, 가운데 안내, 오른쪽 멈추기·보내기 원.
    let bmid = comp_y + 1.0 + 8.0 + text_h + 2.0 + 16.0;
    let ready = !pane.draft.trim().is_empty();
    let send = (col_x + col_w - 28.0, bmid - 14.0, 28.0, 28.0);
    if ready {
        g.round_rect_fill(send.0, send.1, send.2, send.3, 14.0, theme::accent());
        g.queue_icon("arrow-up", send.0 + 7.0, send.1 + 7.0, 14.0, slot.bg);
        g.hover_pointer |= inside(cursor, send);
        hits.push((Hit::Send, send));
    } else {
        g.round_rect_stroke(send.0, send.1, send.2, send.3, 14.0, theme::border_w().max(1.0), theme::border());
        g.queue_icon("arrow-up", send.0 + 7.0, send.1 + 7.0, 14.0, theme::text_mute());
    }
    let mut hint_right = send.0 - 10.0;
    if working {
        let stop = (send.0 - 6.0 - 28.0, send.1, 28.0, 28.0);
        let hover = inside(cursor, stop);
        g.round_rect_stroke(stop.0, stop.1, stop.2, stop.3, 14.0, theme::border_w().max(1.0), if hover { theme::text_dim() } else { theme::border() });
        g.queue_icon("square", stop.0 + 8.0, stop.1 + 8.0, 12.0, theme::text_dim());
        g.hover_pointer |= hover;
        hits.push((Hit::Stop, stop));
        hint_right = stop.0 - 10.0;
    }
    let term_label = "터미널로 보기";
    let term_w = g.measure_chrome_text(term_label, 11.5, false) + 34.0;
    let term = (col_x - 8.0, bmid - 13.0, term_w, 26.0);
    let term_hover = inside(cursor, term);
    if term_hover {
        g.round_rect_fill(term.0, term.1, term.2, term.3, theme::radius_sm(), theme::lerp(slot.bg, theme::text(), 0.08));
        g.hover_pointer = true;
    }
    g.queue_icon("terminal", term.0 + 8.0, bmid - 6.5, 13.0, theme::text_dim());
    label(g, term.0 + 26.0, bmid - 7.0, term_label, 11.5, if term_hover { theme::text() } else { theme::text_dim() }, false);
    hits.push((Hit::Terminal, term));
    let hint_x = term.0 + term.2 + 10.0;
    let failed = pane.send_error.lock().ok().and_then(|e| e.clone());
    let (hint, tone) = match &failed {
        Some(e) => (format!("못 보냈어요 · {e}"), theme::danger()),
        None => ("Enter 보내기 · ⇧Enter 줄바꿈".to_string(), theme::text_mute()),
    };
    let hint = crate::info::fit_text(g, &hint, (hint_right - hint_x).max(0.0), 11.0, false);
    label(g, hint_x, bmid - 6.5, &hint, 11.0, tone, false);
    // 단추가 먼저 — 판정은 처음 맞는 것이 이긴다(`chat_view.rs` 클릭).
    hits.push((Hit::Composer, box_r));

    g.pop_clip();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_cap_keeps_the_newest_turns() {
        assert!(ROW_CAP >= 100);
        assert_eq!(first_line("\n  첫 줄 \n둘째"), "첫 줄");
    }
}
