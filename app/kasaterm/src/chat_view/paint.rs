//! 대화 칸의 배치와 그리기. 치수는 `docs/design.md` 「학생 대화 보기」에 있다.
//!
//! 배치는 기록이 바뀔 때만 다시 한다(`Layout`). 말풍선 하나를 펴려면 낱말마다 폭을
//! 재야 해서, 프레임마다 하면 긴 대화에서 그 값이 프레임을 먹는다. 말풍선은 한 번 서면
//! 글이 안 바뀌므로 칸 번호로 붙잡아 두고, 줄 목록만 새로 엮는다.

use std::collections::{HashMap, HashSet};

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
    /// 코드 칸 바탕.
    Code,
    /// 글 사이 `code` 바탕.
    Chip,
    /// 인용 왼쪽 줄.
    Bar,
    Rule,
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
            self.out.decos.push(Deco { x: run.x - 3.0, y: run.y - 1.0, w: w + 6.0, h: self.line - 2.0, kind: DecoKind::Chip });
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

fn body_style(tone: Tone) -> Style {
    Style { size: 12.0, bold: false, code: false, tone }
}

/// 코드 칸 — 고정폭 11, 줄 16, 안 여백 8. 줄은 글자 단위로 접는다(코드에 낱말 경계가 없다).
fn code_block(g: &mut gpu::GpuRenderer, out: &mut Block, code: &str, y0: f32, max_w: f32, max_lines: usize) -> f32 {
    let st = Style { size: 11.0, bold: false, code: true, tone: Tone::Text };
    let inner = (max_w - 16.0).max(20.0);
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
    let mut widest: f32 = 0.0;
    for (i, line) in lines.iter().enumerate() {
        let w = measure(g, line, &st);
        widest = widest.max(w);
        let tone = if hidden > 0 && i == lines.len() - 1 { Tone::Mute } else { Tone::Text };
        out.runs.push(Run { x: 8.0, y: y0 + 6.0 + i as f32 * 16.0 + 2.0, text: line.clone(), size: 11.0, bold: false, code: true, tone });
    }
    let h = lines.len() as f32 * 16.0 + 12.0;
    let w = (widest + 16.0).min(max_w);
    out.decos.push(Deco { x: 0.0, y: y0, w, h, kind: DecoKind::Code });
    out.w = out.w.max(w);
    h
}

/// 말풍선 본문 — 마크다운을 문단·제목·목록·인용·코드로 편다. 표는 칸을 `│` 로 이은 줄.
fn markdown_block(g: &mut gpu::GpuRenderer, text: &str, max_w: f32, tone: Tone) -> Block {
    let mut out = Block::default();
    let (blocks, _) = crate::parse_markdown(text);
    let mut y = 0.0;
    let body = body_style(tone);
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            y += 6.0;
        }
        y += match block {
            MdBlock::Heading { level, spans } => {
                let big = *level <= 2;
                let st = Style { size: if big { 14.0 } else { 12.0 }, bold: true, code: false, tone };
                flow_spans(g, &mut out, spans, 0.0, y, max_w, &st, if big { 22.0 } else { 18.0 })
            }
            MdBlock::Para { spans } => flow_spans(g, &mut out, spans, 0.0, y, max_w, &body, 18.0),
            MdBlock::Code { code, .. } => code_block(g, &mut out, code, y, max_w, 40),
            MdBlock::ListItem { depth, marker, spans, task } => {
                let indent = *depth as f32 * 14.0;
                let mark = match task {
                    Some(true) => "☑".to_string(),
                    Some(false) => "☐".to_string(),
                    None => marker.clone(),
                };
                let mw = g.measure_chrome_text(&mark, 12.0, false);
                let lead = mw.max(10.0) + 6.0;
                out.runs.push(Run { x: indent, y: y + 2.0, text: mark, size: 12.0, bold: false, code: false, tone: Tone::Dim });
                flow_spans(g, &mut out, spans, indent + lead, y, (max_w - indent - lead).max(20.0), &body, 18.0)
            }
            MdBlock::Quote { spans } | MdBlock::Callout { spans, .. } => {
                let h = flow_spans(g, &mut out, spans, 10.0, y, (max_w - 10.0).max(20.0), &body_style(Tone::Dim), 18.0);
                out.decos.push(Deco { x: 0.0, y, w: 2.0, h, kind: DecoKind::Bar });
                h
            }
            MdBlock::Rule => {
                out.decos.push(Deco { x: 0.0, y: y + 6.0, w: max_w, h: 1.0, kind: DecoKind::Rule });
                13.0
            }
            MdBlock::Meta { rows } => {
                let text = rows.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
                flow_plain(g, &mut out, &text, 0.0, y, max_w, &body_style(Tone::Dim), 18.0)
            }
            MdBlock::Image { alt, .. } => {
                let text = if alt.is_empty() { "(그림)".to_string() } else { format!("(그림) {alt}") };
                flow_plain(g, &mut out, &text, 0.0, y, max_w, &body_style(Tone::Dim), 18.0)
            }
            MdBlock::Table { head, rows, .. } => {
                let mut h = 0.0;
                for (ri, row) in std::iter::once(head).chain(rows.iter()).enumerate() {
                    if row.is_empty() {
                        continue;
                    }
                    let mut spans: Vec<MdSpan> = Vec::new();
                    for (ci, cell) in row.iter().enumerate() {
                        if ci > 0 {
                            spans.push(MdSpan { text: "  │  ".into(), bold: false, italic: false, code: false, strike: false, link: None });
                        }
                        spans.extend(cell.iter().cloned());
                    }
                    let st = Style { size: 12.0, bold: ri == 0, code: false, tone };
                    h += flow_spans(g, &mut out, &spans, 0.0, y + h, max_w, &st, 18.0);
                }
                h
            }
        };
    }
    out.h = y.max(18.0);
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
    Name { text: String },
    Bubble { item: usize, mine: bool, queued: bool },
    /// 접는 머리 한 줄 — 도구 묶음·생각·출력.
    Fold { key: usize, icon: &'static str, label: String, detail: String, open: bool, failed: usize, live: bool },
    Tool { item: usize, status: Status, name: String, summary: String, open: bool, has_result: bool },
    /// 펼친 결과·생각·출력. 왼쪽으로 18 들여 바탕을 깐다.
    Body { key: usize },
    Command { text: String },
    Answered { key: usize },
    Note { icon: &'static str, text: String },
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

#[allow(clippy::too_many_arguments)]
fn build(g: &mut gpu::GpuRenderer, layout: &mut Layout, feed: &Feed, open: &HashSet<usize>, col_w: f32, name: &str, working: bool) {
    let items = &feed.conv.items;
    let rows = super::parse::group_rows(items);
    let skip = rows.len().saturating_sub(ROW_CAP);
    let bubble_max = (col_w * 0.8).min(560.0).max(80.0);
    let wbits = col_w.to_bits();
    let mut out: Vec<LRow> = Vec::new();
    let mut y = 0.0;
    let mut last_speaker: Option<String> = None;
    let n_rows = rows.len();
    let push = |out: &mut Vec<LRow>, y: &mut f32, h: f32, gap: f32, kind: Kind| {
        if !out.is_empty() {
            *y += gap;
        }
        out.push(LRow { y: *y, h, kind });
        *y += h;
    };
    if skip > 0 {
        push(&mut out, &mut y, 22.0, 0.0, Kind::Note { icon: "info", text: format!("앞의 {skip}개는 터미널 보기에 있어요") });
    }
    for (ri, row) in rows.iter().enumerate().skip(skip) {
        match row {
            Row::Tools(run) => {
                last_speaker = None;
                let key = run[0];
                let is_open = open.contains(&key);
                let failed = run.iter().filter(|&&i| matches!(&items[i], Item::Tool { error: true, .. })).count();
                let live = working && ri + 1 == n_rows && run.iter().any(|&i| matches!(&items[i], Item::Tool { result: None, .. }));
                let detail = run.last().and_then(|&i| match &items[i] {
                    Item::Tool { name, summary, .. } => Some(if summary.is_empty() { tool_label(name).to_string() } else { format!("{} · {summary}", tool_label(name)) }),
                    _ => None,
                }).unwrap_or_default();
                push(&mut out, &mut y, 26.0, 6.0, Kind::Fold { key, icon: "terminal", label: format!("도구 {}개", run.len()), detail, open: is_open, failed, live });
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
                    push(&mut out, &mut y, 22.0, 0.0, Kind::Tool { item: i, status, name: tool_label(name).to_string(), summary: summary.clone(), open: tool_open, has_result });
                    if tool_open {
                        let result = result.as_deref().unwrap_or("");
                        let fresh = layout.result_len.get(&i) != Some(&result.len());
                        if fresh || layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                            let mut b = Block::default();
                            let text = super::parse::strip_ansi(result);
                            b.h = code_block(g, &mut b, text.trim_end(), 0.0, (col_w - 18.0).max(40.0), 14);
                            layout.blocks.insert(i, (wbits, b));
                            layout.result_len.insert(i, result.len());
                        }
                        let h = layout.blocks[&i].1.h;
                        push(&mut out, &mut y, h, 2.0, Kind::Body { key: i });
                    }
                }
            }
            Row::One(i) => {
                let i = *i;
                match &items[i] {
                    Item::Bubble { user, text, from, queued, images } => {
                        let mine = *user && from.is_none();
                        let speaker = if mine { String::new() } else { from.clone().unwrap_or_else(|| name.to_string()) };
                        let new_speaker = last_speaker.as_deref() != Some(speaker.as_str());
                        if new_speaker && !mine {
                            push(&mut out, &mut y, 18.0, 14.0, Kind::Name { text: speaker.clone() });
                        }
                        let gap = if new_speaker && mine { 14.0 } else if new_speaker { 2.0 } else { 6.0 };
                        last_speaker = Some(speaker);
                        if layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                            let mut body = text.clone();
                            if *images > 0 {
                                let mark = if *images == 1 { "(사진)".to_string() } else { format!("(사진 {images}장)") };
                                body = if body.is_empty() { mark } else { format!("{mark}\n{body}") };
                            }
                            let tone = if *queued { Tone::Dim } else { Tone::Text };
                            let b = markdown_block(g, &body, bubble_max - 24.0, tone);
                            layout.blocks.insert(i, (wbits, b));
                        }
                        let b = &layout.blocks[&i].1;
                        let h = b.h + 16.0 + if *queued { 16.0 } else { 0.0 };
                        push(&mut out, &mut y, h, gap, Kind::Bubble { item: i, mine, queued: *queued });
                    }
                    Item::Thinking(text) | Item::Output(text) => {
                        last_speaker = None;
                        let thinking = matches!(&items[i], Item::Thinking(_));
                        let is_open = open.contains(&i);
                        let (icon, label) = if thinking { ("lightbulb", "생각") } else { ("terminal", "출력") };
                        push(&mut out, &mut y, 22.0, 6.0, Kind::Fold { key: i, icon, label: label.into(), detail: first_line(text).to_string(), open: is_open, failed: 0, live: false });
                        if is_open {
                            if layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                                let mut b = Block::default();
                                b.h = if thinking {
                                    flow_plain(g, &mut b, text, 8.0, 6.0, (col_w - 34.0).max(40.0), &Style { size: 11.0, bold: false, code: false, tone: Tone::Dim }, 16.0) + 12.0
                                } else {
                                    code_block(g, &mut b, text, 0.0, (col_w - 18.0).max(40.0), 40)
                                };
                                layout.blocks.insert(i, (wbits, b));
                            }
                            let h = layout.blocks[&i].1.h;
                            push(&mut out, &mut y, h, 2.0, Kind::Body { key: i });
                        }
                    }
                    Item::Command { name: cmd, args } => {
                        last_speaker = None;
                        let text = if cmd == "!" { format!("! {args}") } else { format!("{cmd} {args}").trim().to_string() };
                        push(&mut out, &mut y, 22.0, 10.0, Kind::Command { text });
                    }
                    Item::Answered { pairs: Some(pairs), .. } => {
                        last_speaker = None;
                        if layout.blocks.get(&i).is_none_or(|(w, _)| *w != wbits) {
                            let mut b = Block::default();
                            let mut by = 8.0;
                            let inner = (bubble_max - 24.0).max(40.0);
                            for (q, a) in pairs {
                                by += flow_plain(g, &mut b, q, 12.0, by, inner, &Style { size: 10.5, bold: false, code: false, tone: Tone::Dim }, 16.0);
                                by += flow_plain(g, &mut b, a, 12.0, by, inner, &body_style(Tone::Text), 18.0) + 4.0;
                            }
                            b.h = by + 4.0;
                            b.w = b.w + 12.0;
                            layout.blocks.insert(i, (wbits, b));
                        }
                        let h = layout.blocks[&i].1.h;
                        push(&mut out, &mut y, h, 10.0, Kind::Answered { key: i });
                    }
                    Item::Launch(label) => {
                        last_speaker = None;
                        push(&mut out, &mut y, 22.0, 6.0, Kind::Note { icon: "users", text: format!("서브에이전트 · {label}") });
                    }
                    Item::System(text) => {
                        last_speaker = None;
                        push(&mut out, &mut y, 22.0, 6.0, Kind::Note { icon: "info", text: text.clone() });
                    }
                    Item::Interrupted => {
                        last_speaker = None;
                        push(&mut out, &mut y, 22.0, 6.0, Kind::Note { icon: "octagon-alert", text: "작업을 멈췄어요".into() });
                    }
                    Item::Tool { .. } | Item::Answered { pairs: None, .. } | Item::Gone => {}
                }
            }
        }
    }
    layout.shown = out.len();
    if working {
        push(&mut out, &mut y, 22.0, 10.0, Kind::Note { icon: "sparkles", text: format!("{name} · 작업 중") });
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
        let cw = g.measure_chrome_text(ch.encode_utf8(&mut buf), 12.0, false);
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
    let code_bg = theme::lerp(fill, theme::text(), 0.07);
    for d in &b.decos {
        if oy + d.y + d.h < clip.0 || oy + d.y > clip.1 {
            continue;
        }
        match d.kind {
            DecoKind::Code => g.round_rect_fill(ox + d.x, oy + d.y, d.w, d.h, theme::radius_sm(), code_bg),
            DecoKind::Chip => g.round_rect_fill(ox + d.x, oy + d.y, d.w, d.h, 3.0, code_bg),
            DecoKind::Bar => g.rect(ox + d.x, oy + d.y, d.w, d.h, theme::text_mute()),
            DecoKind::Rule => g.rect(ox + d.x, oy + d.y, d.w, d.h, theme::border()),
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

/// 말풍선 바탕. 말한 쪽 아래 모서리만 3 으로 좁혀 누가 한 말인지 꼬리처럼 읽힌다.
fn bubble(g: &mut gpu::GpuRenderer, r: Rect, fill: [u8; 4], mine: bool) {
    let radius = 10.0_f32.min(r.3 / 2.0);
    g.round_rect_fill(r.0, r.1, r.2, r.3, radius, fill);
    let x = if mine { r.0 + r.2 - radius } else { r.0 };
    g.round_rect_fill(x, r.1 + r.3 - radius, radius, radius, 3.0_f32.min(radius / 2.0), fill);
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, cursor: (f32, f32), slot: &Slot, pane: &mut ChatPane, feed: Option<&Feed>) -> Vec<(Hit, Rect)> {
    let mut hits: Vec<(Hit, Rect)> = Vec::new();
    let (x, y, w, h) = slot.rect;
    if w < 60.0 || h < 80.0 {
        return hits;
    }
    let pad = if w >= 480.0 { 16.0 } else { 10.0 };
    let col_w = (w - pad * 2.0).min(800.0).max(40.0);
    let col_x = x + ((w - col_w) / 2.0).floor();
    g.push_clip(x, y, w, h);

    // ── 입력칸(아래 고정) ────────────────────────────────────────────────
    let inner_w = (col_w - 20.0).max(20.0);
    let (lines, caret_line, caret_x, pre) = composer_lines(g, &pane.draft, pane.cursor, &slot.preedit, inner_w);
    let shown = lines.len().clamp(1, 6);
    let first = (caret_line + 1).saturating_sub(shown).min(lines.len().saturating_sub(shown));
    let input_h = 40.0 + (shown - 1) as f32 * 18.0;
    let composer_h = 10.0 + input_h + 8.0 + 26.0 + 10.0;
    let comp_y = y + h - composer_h;

    // ── 선택지 카드·기다림 줄 ───────────────────────────────────────────
    let menu = pane.menu.clone().filter(|_| !slot.mirror);
    let card_h = match &menu {
        Some(m) => 10.0 + if m.title.is_empty() { 0.0 } else { 24.0 } + m.options.len() as f32 * 32.0 + 26.0 + 10.0,
        None if slot.needs_you => 40.0,
        None => 0.0,
    };
    let list_y = y + 8.0;
    let list_bottom = comp_y - card_h - if card_h > 0.0 { 8.0 } else { 0.0 };
    let list_h = (list_bottom - list_y).max(0.0);

    // ── 대화 목록 ─────────────────────────────────────────────────────────
    if let Some(f) = feed {
        if pane.layout.generation != f.generation {
            pane.layout.blocks.clear();
            pane.layout.result_len.clear();
            pane.open.clear();
            pane.layout.generation = f.generation;
        }
        let key = (f.version, f.generation, col_w.to_bits(), theme::ui_font_gen(), pane.layout.open_gen, slot.working);
        if pane.layout.key != Some(key) {
            build(g, &mut pane.layout, f, &pane.open, col_w, &slot.name, slot.working);
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
            if slot.mirror {
                "다른 기기 학생의 거울이라 대화 기록은 그 기기에 있어요. 터미널 보기로 보세요.".into()
            } else if f.missing {
                "이 창에 묶인 대화 기록이 아직 없어요. 학생이 첫 답을 하면 여기에 떠요.".into()
            } else {
                "말을 걸면 여기에 떠요.".into()
            },
        )),
        Some(_) => None,
    };
    if let Some((title, body)) = &empty {
        let tw = g.measure_chrome_text(title, 14.0, true);
        let cy = list_y + (list_h / 2.0 - 40.0).max(0.0);
        label(g, col_x + (col_w - tw) / 2.0, cy, title, 14.0, theme::text(), true);
        let mut b = Block::default();
        let bw = col_w.min(360.0);
        let bh = if body.is_empty() { 0.0 } else { flow_plain(g, &mut b, body, 0.0, 0.0, bw, &body_style(Tone::Dim), 18.0) };
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
        draw_block(g, bx, cy + 26.0, &b, theme::pane_bg(), (f32::MIN, f32::MAX));
        let btn = (col_x + (col_w - 110.0) / 2.0, cy + 34.0 + bh, 110.0, 26.0);
        native_controls::text_button(g, btn, cursor, "터미널로 보기", native_controls::Style::default());
        hits.push((Hit::Terminal, btn));
        pane.scroll_max = 0.0;
        pane.scroll = 0.0;
    } else {
        let layout = &pane.layout;
        let content_h = layout.height;
        pane.scroll_max = (content_h + 16.0 - list_h).max(0.0);
        pane.scroll = pane.scroll.clamp(0.0, pane.scroll_max);
        // 아래에 붙여 쌓는다 — 새 말이 입력칸 바로 위에 선다(대화 앱과 같은 자리).
        let top = list_y + list_h - 8.0 - content_h + pane.scroll;
        g.push_clip(x, list_y, w, list_h);
        let mine_fill = theme::lerp(theme::pane_bg(), theme::accent(), 0.22);
        let their_fill = theme::lerp(theme::pane_bg(), theme::text(), 0.07);
        let clip = (list_y, list_bottom);
        for row in &layout.rows {
            let ry = top + row.y;
            if ry + row.h < list_y || ry > list_bottom {
                continue;
            }
            match &row.kind {
                Kind::Name { text } => {
                    let t = crate::info::fit_text(g, text, col_w, 10.5, false);
                    label(g, col_x + 2.0, ry + 3.0, &t, 10.5, theme::text_dim(), false);
                }
                Kind::Bubble { item, mine, queued } => {
                    let Some((_, b)) = layout.blocks.get(item) else { continue };
                    let bw = (b.w + 24.0).max(36.0);
                    let bh = row.h - if *queued { 16.0 } else { 0.0 };
                    let bx = if *mine { col_x + col_w - bw } else { col_x };
                    let fill = if *queued {
                        theme::lerp(theme::pane_bg(), theme::attention(), 0.12)
                    } else if *mine {
                        mine_fill
                    } else {
                        their_fill
                    };
                    let r = (bx, ry, bw, bh);
                    bubble(g, r, fill, *mine);
                    draw_block(g, bx + 12.0, ry + 8.0, b, fill, clip);
                    if *queued {
                        let t = "예약 · 지금 일이 끝나면 읽어요";
                        let tw = g.measure_chrome_text(t, 10.5, false);
                        label(g, bx + bw - tw, ry + bh + 2.0, t, 10.5, theme::text_mute(), false);
                    }
                    // 올리면 옆에 복사 — 말풍선 안 글은 고르기가 없어 이것이 꺼내는 길이다.
                    let hover = inside(cursor, r) && inside(cursor, (x, list_y, w, list_h));
                    let cr = if *mine { (bx - 28.0, ry, 26.0, 26.0) } else { (bx + bw + 2.0, ry, 26.0, 26.0) };
                    let room = cr.0 >= col_x - pad + 2.0 && cr.0 + cr.2 <= col_x + col_w + pad - 2.0;
                    if room && (hover || inside(cursor, cr)) {
                        native_controls::icon_button(g, cr, cursor, "copy", native_controls::Style::default());
                        hits.push((Hit::Copy(*item), cr));
                    }
                }
                Kind::Fold { key, icon, label: name, detail, open, failed, live } => {
                    let r = (col_x, ry, col_w, row.h);
                    let hover = inside(cursor, r);
                    if hover {
                        g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::surface_hover());
                        g.hover_pointer = true;
                    }
                    let mid = ry + row.h / 2.0;
                    g.queue_icon(if *open { "chevron-down" } else { "chevron-right" }, col_x + 4.0, mid - 6.0, 12.0, theme::text_mute());
                    g.queue_icon(icon, col_x + 20.0, mid - 6.0, 12.0, theme::text_dim());
                    let mut tx = col_x + 38.0;
                    label(g, tx, mid - 6.5, name, 11.0, theme::text_dim(), false);
                    tx += g.measure_chrome_text(name, 11.0, false) + 8.0;
                    if *live {
                        let t = "도는 중";
                        label(g, tx, mid - 6.5, t, 11.0, theme::accent(), false);
                        tx += g.measure_chrome_text(t, 11.0, false) + 8.0;
                    }
                    if *failed > 0 {
                        let t = format!("실패 {failed}");
                        label(g, tx, mid - 6.5, &t, 11.0, theme::danger(), false);
                        tx += g.measure_chrome_text(&t, 11.0, false) + 8.0;
                    }
                    let d = crate::info::fit_text(g, detail, (col_x + col_w - 8.0 - tx).max(0.0), 11.0, false);
                    label(g, tx, mid - 6.5, &d, 11.0, theme::text_mute(), false);
                    hits.push((Hit::Fold(*key), r));
                }
                Kind::Tool { item, status, name, summary, open, has_result } => {
                    let r = (col_x + 18.0, ry, col_w - 18.0, row.h);
                    let mid = ry + row.h / 2.0;
                    if *has_result && inside(cursor, r) {
                        g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::surface_hover());
                        g.hover_pointer = true;
                    }
                    let (icon, color) = match status {
                        Status::Running => ("chevron-right", theme::accent()),
                        Status::Ok => ("check", theme::success()),
                        Status::Failed => ("x", theme::danger()),
                    };
                    g.queue_icon(icon, r.0 + 4.0, mid - 6.0, 12.0, color);
                    let nx = r.0 + 22.0;
                    let nm = crate::info::fit_text(g, name, (r.2 * 0.4).max(0.0), 11.0, false);
                    label(g, nx, mid - 6.5, &nm, 11.0, theme::text(), false);
                    let sx = nx + g.measure_chrome_text(&nm, 11.0, false) + 8.0;
                    let right = r.0 + r.2 - if *has_result { 22.0 } else { 6.0 };
                    let s = crate::info::fit_text(g, summary, (right - sx).max(0.0), 11.0, false);
                    label(g, sx, mid - 6.5, &s, 11.0, theme::text_dim(), false);
                    if *has_result {
                        g.queue_icon(if *open { "chevron-up" } else { "chevron-down" }, r.0 + r.2 - 18.0, mid - 6.0, 12.0, theme::text_mute());
                        hits.push((Hit::Tool(*item), r));
                    }
                }
                Kind::Body { key } => {
                    if let Some((_, b)) = layout.blocks.get(key) {
                        draw_block(g, col_x + 18.0, ry, b, theme::pane_bg(), clip);
                    }
                }
                Kind::Command { text } => {
                    let tw = g.measure_code_text(text, 11.0).min(col_w - 16.0);
                    let r = (col_x + col_w - tw - 16.0, ry, tw + 16.0, row.h);
                    g.round_rect_stroke(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::border_w().max(1.0), theme::border());
                    g.push_clip(r.0, r.1, r.2, r.3);
                    g.draw_code_text(r.0 + 8.0, ry + 5.0, text, 11.0, theme::text_dim());
                    g.pop_clip();
                }
                Kind::Answered { key } => {
                    if let Some((_, b)) = layout.blocks.get(key) {
                        let r = (col_x, ry, (b.w + 12.0).min(col_w), row.h);
                        g.round_rect_stroke(r.0, r.1, r.2, r.3, 10.0, theme::border_w().max(1.0), theme::border());
                        draw_block(g, col_x, ry, b, theme::pane_bg(), clip);
                    }
                }
                Kind::Note { icon, text } => {
                    let t = crate::info::fit_text(g, text, (col_w - 24.0).max(0.0), 10.5, false);
                    let tw = g.measure_chrome_text(&t, 10.5, false) + 18.0;
                    let nx = col_x + (col_w - tw) / 2.0;
                    g.queue_icon(icon, nx, ry + 5.0, 12.0, theme::text_mute());
                    label(g, nx + 18.0, ry + 4.5, &t, 10.5, theme::text_mute(), false);
                }
            }
        }
        g.pop_clip();
        if pane.scroll > 240.0 {
            let r = (col_x + col_w - 30.0, list_bottom - 34.0, 26.0, 26.0);
            g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::pane_bg());
            g.round_rect_stroke(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::border_w().max(1.0), theme::border());
            native_controls::icon_button(g, r, cursor, "chevron-down", native_controls::Style::default());
            hits.push((Hit::Bottom, r));
        }
    }

    // ── 선택지 카드 ───────────────────────────────────────────────────────
    let card_y = comp_y - card_h;
    if let Some(m) = &menu {
        let line = if slot.needs_you { theme::attention() } else { theme::accent() };
        g.round_rect_stroke(col_x, card_y, col_w, card_h, theme::radius_md(), theme::border_w().max(1.0), line);
        let mut cy = card_y + 10.0;
        if !m.title.is_empty() {
            let t = crate::info::fit_text(g, &m.title, col_w - 20.0, 12.0, true);
            label(g, col_x + 10.0, cy + 3.0, &t, 12.0, theme::text(), true);
            cy += 24.0;
        }
        for (i, o) in m.options.iter().enumerate() {
            let r = (col_x + 10.0, cy, col_w - 20.0, 26.0);
            let hover = inside(cursor, r);
            let edge = if o.current { theme::accent() } else if hover { theme::text_dim() } else { theme::border() };
            g.round_rect_stroke(r.0, r.1, r.2, r.3, theme::radius_sm(), theme::border_w().max(1.0), edge);
            let t = crate::info::fit_text(g, &format!("{}. {}", o.index, o.label), r.2 - 20.0, 12.0, o.current);
            label(g, r.0 + 10.0, r.1 + 6.5, &t, 12.0, if o.current { theme::accent() } else { theme::text() }, o.current);
            g.hover_pointer |= hover;
            hits.push((Hit::Pick(i), r));
            cy += 32.0;
        }
        let esc = (col_x + 10.0, cy, 90.0, 26.0);
        native_controls::text_button(g, esc, cursor, "취소 esc", native_controls::Style::default());
        hits.push((Hit::Dismiss, esc));
        let term = (col_x + col_w - 10.0 - 120.0, cy, 120.0, 26.0);
        native_controls::text_button(g, term, cursor, "터미널에서 보기", native_controls::Style::default());
        hits.push((Hit::Terminal, term));
    } else if slot.needs_you {
        let mid = card_y + card_h / 2.0 - 4.0;
        g.queue_icon("message-square-warning", col_x + 2.0, mid - 7.0, 14.0, theme::attention());
        let bw = 110.0;
        let t = crate::info::fit_text(g, "답을 기다려요 · 고를 것은 터미널 보기에서 확인해요", (col_w - bw - 34.0).max(0.0), 12.0, false);
        label(g, col_x + 22.0, mid - 7.0, &t, 12.0, theme::text(), false);
        let r = (col_x + col_w - bw, mid - 13.0, bw, 26.0);
        native_controls::text_button(g, r, cursor, "터미널로 보기", native_controls::Style::default());
        hits.push((Hit::Terminal, r));
    }

    // ── 입력칸 ────────────────────────────────────────────────────────────
    g.rect(x, comp_y, w, 1.0, theme::border());
    let box_r = (col_x, comp_y + 10.0, col_w, input_h);
    let edge = if slot.focused { theme::accent() } else { theme::border() };
    g.round_rect_stroke(box_r.0, box_r.1, box_r.2, box_r.3, theme::radius_sm(), theme::border_w().max(1.0), edge);
    hits.push((Hit::Composer, box_r));
    let tx = box_r.0 + 10.0;
    let ty = box_r.1 + 11.0;
    if pane.draft.is_empty() && slot.preedit.is_empty() {
        let hint = if slot.mirror { "다른 기기 학생에게 보내기" } else { "메시지 보내기" };
        label(g, tx, ty, hint, 12.0, theme::text_mute(), false);
    }
    g.push_clip(box_r.0 + 2.0, box_r.1 + 2.0, box_r.2 - 4.0, box_r.3 - 4.0);
    for (i, line) in lines.iter().enumerate().skip(first).take(shown) {
        label(g, tx, ty + (i - first) as f32 * 18.0, line, 12.0, theme::text(), false);
    }
    if let Some((pl, px, pw)) = pre {
        if pl >= first && pl < first + shown && pw > 0.0 {
            g.rect(tx + px, ty + (pl - first) as f32 * 18.0 + 15.0, pw, 1.0, theme::text());
        }
    }
    if slot.focused && slot.caret_on && caret_line >= first && caret_line < first + shown {
        g.rect(tx + caret_x, ty + (caret_line - first) as f32 * 18.0 - 1.0, 1.5, 16.0, theme::accent());
    }
    g.pop_clip();

    let by = box_r.1 + input_h + 8.0;
    let term_w = g.measure_chrome_text("터미널로 보기", 12.0, false) + 20.0;
    let term = (col_x, by, term_w, 26.0);
    native_controls::text_button(g, term, cursor, "터미널로 보기", native_controls::Style::default());
    hits.push((Hit::Terminal, term));
    let ready = !pane.draft.trim().is_empty();
    let send_w = 76.0;
    let send = (col_x + col_w - send_w, by, send_w, 26.0);
    native_controls::text_button(g, send, cursor, "보내기", native_controls::Style { primary: ready, enabled: ready, ..Default::default() });
    if ready {
        hits.push((Hit::Send, send));
    }
    let mut hint_right = send.0 - 8.0;
    if slot.working {
        let stop = (send.0 - 6.0 - 84.0, by, 84.0, 26.0);
        native_controls::text_button(g, stop, cursor, "멈추기 esc", native_controls::Style::default());
        hits.push((Hit::Stop, stop));
        hint_right = stop.0 - 8.0;
    }
    let hint_x = term.0 + term.2 + 10.0;
    let hint = crate::info::fit_text(g, "Enter 보내기 · ⇧Enter 줄바꿈", (hint_right - hint_x).max(0.0), 10.5, false);
    label(g, hint_x, by + 7.0, &hint, 10.5, theme::text_mute(), false);

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
