//! 코덱스 칸의 대화 줄 — 코덱스 기본(인라인) TUI(0.162)가 스크롤백에 내는 화면을 기록(rollout)으로 다시
//! 늘어놓는다. claude 꼴(`classic.rs`)을 덮어씌우지 않는다: 요소·글리프·색은 코덱스 칸 그대로이고, 하는 일은
//! 칸 폭에 맞춰 쌓기뿐이다. 명령은 rollout 의 `CommandExecution`(명령·해석·출력·종료 코드)으로 편다 —
//! 코덱스 TUI 가 「Ran …」·「Explored」를 그리는 재료와 같다.

use super::classic::{banner_text, blanks, cells, line, plain, screen_banner, shorten_path, sig_of, text_width, wrap, Block, Col, Line, Look, Shelf, Span, St};
use super::parse::{Conversation, Item, Row};
use crate::{GridCell, MdBlock, MdSpan};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// 코덱스 TUI 의 색 — 테마와 무관하게 코덱스가 고정으로 내는 값(0.162 실측).
const BLUE: Col = Col::Rgb(99, 168, 248);
const CMD: Col = Col::Rgb(137, 180, 250);
const STR: Col = Col::Rgb(166, 227, 161);
const ARG: Col = Col::Rgb(205, 214, 244);
const HEAD: Col = Col::Rgb(249, 226, 175);
/// 사람 말 칸의 바탕 자리표 — 코덱스는 입력칸과 같은 바탕을 깔고, 그 값은 터미널 바탕에 따라 달라 화면을
/// 지을 때 진짜 입력칸 바탕으로 바꾼다(`chat_view::chat_frame`).
pub(super) const PROMPT_FILL: Col = Col::Rgb(63, 69, 77);
const OK: Col = Col::Idx(2);
const FAIL: Col = Col::Idx(1);

fn dim() -> St {
    St { dim: true, ..Default::default() }
}

fn pad(row: &mut Vec<GridCell>, width: usize) {
    row.truncate(width);
    row.resize(width, GridCell::blank());
}

fn md_spans(spans: &[MdSpan], base: St, cwd: Option<&str>) -> Vec<Span> {
    let mut out = Vec::new();
    for s in spans {
        let mut st = base;
        st.bold |= s.bold;
        st.italic |= s.italic;
        if s.code {
            st.fg = Some(STR);
        }
        out.push((s.text.clone(), st));
        // 코덱스는 고리를 「글 (경로)」로 푼다.
        if let Some(dest) = s.link.as_deref().filter(|d| *d != s.text) {
            out.push((" (".into(), base));
            out.push((shorten_path(dest, cwd), St { fg: Some(STR), ..base }));
            out.push((")".into(), base));
        }
    }
    out
}

/// 답 한 칸 — 첫 줄 앞에 `• `, 나머지는 두 칸 들여. 문단·표 사이 빈 줄, 목록은 `• `.
fn markdown(text: &str, width: usize, cwd: Option<&str>) -> Vec<Vec<GridCell>> {
    let (blocks, _) = crate::parse_markdown(text);
    let mut out: Vec<Vec<GridCell>> = Vec::new();
    let mut prev_list = false;
    let indent = 2;
    for block in &blocks {
        let list = matches!(block, MdBlock::ListItem { .. });
        if !out.is_empty() && !(list && prev_list) {
            out.push(vec![GridCell::blank(); width]);
        }
        prev_list = list;
        match block {
            MdBlock::Heading { spans, .. } => out.extend(wrap(width, blanks(indent, St::default()), indent, &md_spans(spans, St::default().bold(), cwd), None)),
            MdBlock::Para { spans } => out.extend(wrap(width, blanks(indent, St::default()), indent, &md_spans(spans, St::default(), cwd), None)),
            MdBlock::ListItem { depth, marker, spans, .. } => {
                let mark = if marker == "•" || marker == "-" { "•".to_string() } else { marker.clone() };
                let lead = indent + *depth as usize * 2;
                let mut first = blanks(lead, St::default());
                first.extend(cells(&format!("{mark} "), St::default()));
                out.extend(wrap(width, first, lead + text_width(&mark) + 1, &md_spans(spans, St::default(), cwd), None));
            }
            MdBlock::Code { code, .. } => {
                for raw in code.trim_end_matches('\n').split('\n') {
                    out.extend(wrap(width, blanks(indent + 2, St::default()), indent + 2, &plain(raw.replace('\t', "    "), St::default()), None));
                }
            }
            MdBlock::Quote { spans } | MdBlock::Callout { spans, .. } => {
                let mut first = blanks(indent, St::default());
                first.extend(cells("> ", dim()));
                out.extend(wrap(width, first, indent + 2, &md_spans(spans, dim(), cwd), None));
            }
            MdBlock::Rule => {
                let mut row = blanks(indent, St::default());
                row.extend(cells(&"─".repeat(width.saturating_sub(indent).min(80)), dim()));
                pad(&mut row, width);
                out.push(row);
            }
            MdBlock::Meta { rows } => {
                for (k, v) in rows {
                    out.extend(wrap(width, blanks(indent, St::default()), indent, &plain(format!("{k}: {v}"), St::default()), None));
                }
            }
            MdBlock::Image { alt, .. } => out.extend(wrap(width, blanks(indent, St::default()), indent, &plain(format!("[image {alt}]"), dim()), None)),
            MdBlock::Table { head, rows, .. } => out.extend(table(width, indent, head, rows, cwd)),
        }
    }
    if let Some(first) = out.first_mut() {
        let bullet = cells("• ", dim());
        first.splice(0..bullet.len().min(first.len()), bullet);
    }
    out
}

/// 코덱스 표 — 세로줄 없이 칸 사이 두 칸, 머리 밑 `━`, 줄 사이 `─`(흐리게). 칸 폭은 글 폭 + 앞뒤 한 칸.
fn table(width: usize, indent: usize, head: &[Vec<MdSpan>], rows: &[Vec<Vec<MdSpan>>], cwd: Option<&str>) -> Vec<Vec<GridCell>> {
    let mut all: Vec<&[Vec<MdSpan>]> = Vec::new();
    if !head.is_empty() {
        all.push(head);
    }
    all.extend(rows.iter().map(Vec::as_slice));
    let ncol = all.iter().map(|r| r.len()).max().unwrap_or(0);
    if ncol == 0 {
        return Vec::new();
    }
    let span_w = |spans: &[MdSpan]| text_width(&md_spans(spans, St::default(), cwd).iter().map(|(t, _)| t.as_str()).collect::<String>());
    let mut widths = vec![1usize; ncol];
    for r in &all {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(span_w(c));
        }
    }
    let rule = |ch: &str| {
        let mut row = blanks(indent, St::default());
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                row.extend(blanks(2, St::default()));
            }
            row.extend(cells(&ch.repeat(w + 2), dim()));
        }
        pad(&mut row, width);
        row
    };
    let data = |r: &[Vec<MdSpan>], base: St| {
        let mut row = blanks(indent, St::default());
        for i in 0..ncol {
            if i > 0 {
                row.extend(blanks(2, St::default()));
            }
            let spans = r.get(i).map(Vec::as_slice).unwrap_or(&[]);
            row.push(GridCell::blank());
            for (t, st) in md_spans(spans, base, cwd) {
                row.extend(cells(&t.replace('\n', " "), st));
            }
            row.extend(blanks(widths[i] + 1 - span_w(spans), St::default()));
        }
        pad(&mut row, width);
        row
    };
    let mut out = Vec::new();
    for (n, r) in all.iter().enumerate() {
        let header = n == 0 && !head.is_empty();
        if n > 0 {
            out.push(rule(if n == 1 && !head.is_empty() { "━" } else { "─" }));
        }
        out.push(data(r, if header { St::fg(HEAD) } else { St::default() }));
    }
    out
}

/// 명령 한 줄을 코덱스처럼 칠한다 — 첫 낱말 파랑, 따옴표 글 초록, 나머지 옅은 흰색.
fn highlight(cmd: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut first = true;
    let mut chars = cmd.chars().peekable();
    let mut word = String::new();
    let flush = |out: &mut Vec<Span>, word: &mut String, first: &mut bool| {
        if word.is_empty() {
            return;
        }
        let col = if *first { CMD } else { ARG };
        out.push((std::mem::take(word), St::fg(col)));
        *first = false;
    };
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                flush(&mut out, &mut word, &mut first);
                let mut s = String::from(c);
                for d in chars.by_ref() {
                    s.push(d);
                    if d == c {
                        break;
                    }
                }
                out.push((s, St::fg(STR)));
                first = false;
            }
            ' ' => {
                flush(&mut out, &mut word, &mut first);
                out.push((" ".into(), St::fg(ARG)));
            }
            _ => word.push(c),
        }
    }
    flush(&mut out, &mut word, &mut first);
    out
}

/// `└` 아래 출력 — 접힌 동안은 끝 세 줄과 「+ N lines (⌃t to expand)」.
fn output_lines(width: usize, text: &str, total: usize, open: bool) -> Vec<Vec<GridCell>> {
    let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    if text.trim().is_empty() {
        let mut head = cells("  └ ", dim());
        head.extend(cells("(no output)", dim()));
        pad(&mut head, width);
        return vec![head];
    }
    let total = total.max(lines.len());
    let shown: Vec<&str> = if open { lines.iter().take(400).copied().collect() } else { lines[lines.len().saturating_sub(3)..].to_vec() };
    let mut out = Vec::new();
    for (i, l) in shown.iter().enumerate() {
        let head = if i == 0 { cells("  └ ", dim()) } else { blanks(4, St::default()) };
        out.extend(wrap(width, head, 4, &plain(l.replace('\t', "    "), dim()), None));
    }
    if total > shown.len() {
        let hint = if open { format!("+ {} lines", total - shown.len()) } else { format!("+ {} lines (⌃t to expand)", total - shown.len()) };
        out.extend(wrap(width, blanks(4, St::default()), 4, &plain(hint, dim()), None));
    }
    out
}

/// `CommandExecution` 의 해석 — 읽기·찾기·목록 줄. 모르는 명령이 하나라도 있으면 None(「Ran」으로 그린다).
fn explore_lines(input: &Value) -> Option<Vec<(String, String)>> {
    let parsed = input["parsed_cmd"].as_array()?;
    if parsed.is_empty() {
        return None;
    }
    parsed
        .iter()
        .map(|p| {
            let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
            match p["type"].as_str()? {
                "read" => Some(("Read".into(), if s("name").is_empty() { s("path") } else { s("name") })),
                "search" => Some(("Search".into(), match (s("query"), s("path")) {
                    (q, p) if !p.is_empty() => format!("{q} in {p}"),
                    (q, _) => q,
                })),
                "list_files" => Some(("List".into(), s("path"))),
                _ => None,
            }
        })
        .collect()
}

fn ran(width: usize, item: usize, conv: &Conversation, open: bool) -> Vec<Line> {
    let Item::Tool { summary, result, error, .. } = &conv.items[item] else { return Vec::new() };
    let input = conv.tool_inputs.get(&item);
    let (dot, verb) = match (result, error) {
        (None, _) => (dim(), "Running"),
        (Some(_), true) => (St::fg(FAIL).bold(), "Ran"),
        (Some(_), false) => (St::fg(OK).bold(), "Ran"),
    };
    let mut first = cells("•", dot);
    first.push(GridCell::blank());
    let mut spans: Vec<Span> = vec![(format!("{verb} "), St::default().bold())];
    spans.extend(highlight(summary.lines().next().unwrap_or("")));
    let mut rows = wrap(width, first, 4, &spans, None);
    if let Some(text) = result {
        let total = input.and_then(|i| i["output_lines"].as_u64()).map_or(0, |n| n as usize);
        rows.extend(output_lines(width, &super::parse::strip_ansi(text), total, open));
    }
    let mut out = line(rows);
    if let Some(h) = out.first_mut() {
        h.toggle = Some(item);
    }
    out
}

fn explored(width: usize, seg: &[usize], conv: &Conversation, open: bool) -> Vec<Line> {
    let running = seg.iter().any(|&at| matches!(&conv.items[at], Item::Tool { result: None, .. }));
    let mut first = cells("•", dim());
    first.push(GridCell::blank());
    let mut rows = wrap(width, first, 2, &plain(if running { "Exploring" } else { "Explored" }, St::default().bold()), None);
    // 이어진 읽기는 한 줄로 — 「Read a.rs, b.rs」.
    let mut merged: Vec<(String, String)> = Vec::new();
    for &at in seg {
        for (verb, target) in conv.tool_inputs.get(&at).and_then(explore_lines).unwrap_or_default() {
            match merged.last_mut() {
                Some((v, t)) if *v == verb && verb == "Read" => {
                    t.push_str(", ");
                    t.push_str(&target);
                }
                _ => merged.push((verb, target)),
            }
        }
    }
    for (n, (verb, target)) in merged.iter().enumerate() {
        let head = if n == 0 { cells("  └ ", dim()) } else { blanks(4, St::default()) };
        rows.extend(wrap(width, head, 4, &[(verb.clone(), St::fg(BLUE)), (format!(" {target}"), St::default())], None));
    }
    if !open {
        rows.extend(wrap(width, blanks(4, St::default()), 4, &plain("+ Show details", dim()), None));
    }
    let mut out = line(rows);
    if let Some(h) = out.first_mut() {
        h.toggle = seg.first().copied();
    }
    if open {
        for &at in seg {
            out.push(Line { cells: vec![GridCell::blank(); width], toggle: None, face: None });
            out.extend(ran(width, at, conv, true));
        }
    }
    out
}

/// 그 밖의 도구(apply_patch·MCP·옛 rollout 의 셸 호출) — 「• Called 이름(요약)」 + 출력.
fn called(width: usize, item: usize, conv: &Conversation, open: bool) -> Vec<Line> {
    let Item::Tool { name, summary, result, error } = &conv.items[item] else { return Vec::new() };
    if matches!(name.as_str(), "shell" | "exec_command" | "exec" | "local_shell") {
        return ran(width, item, conv, open);
    }
    let dot = match (result, error) {
        (None, _) => dim(),
        (Some(_), true) => St::fg(FAIL).bold(),
        (Some(_), false) => St::fg(OK).bold(),
    };
    let mut first = cells("•", dot);
    first.push(GridCell::blank());
    let (verb, what) = if name == "apply_patch" { ("Edited", summary.clone()) } else { ("Called", format!("{name}({summary})")) };
    let mut rows = wrap(width, first, 4, &[(format!("{verb} "), St::default().bold()), (what, St::default())], None);
    if let Some(text) = result {
        rows.extend(output_lines(width, &super::parse::strip_ansi(text), 0, open));
    }
    let mut out = line(rows);
    if let Some(h) = out.first_mut() {
        h.toggle = Some(item);
    }
    out
}

fn worked(ms: u64, at: Option<u64>) -> String {
    let s = ms / 1000;
    let took = match (s / 3600, (s % 3600) / 60, s % 60) {
        (0, 0, sec) => format!("{sec}s"),
        (0, m, sec) => format!("{m}m {sec:02}s"),
        (h, m, sec) => format!("{h}h {m:02}m {sec:02}s"),
    };
    match at.and_then(crate::info_prompts::local_tm) {
        Some(tm) => format!("Worked for {took} • {:02}:{:02}", tm.tm_hour, tm.tm_min),
        None => format!("Worked for {took}"),
    }
}

fn banner(width: usize, conv: &Conversation) -> Vec<Line> {
    let version = conv.meta.version.as_deref().map(|v| format!(" (v{})", v.trim_start_matches('v'))).unwrap_or_default();
    let mut a = blanks(2, St::default());
    a.extend(cells(">_ ", St::fg(BLUE)));
    a.extend(cells("OpenAI Codex", St::default().bold()));
    a.extend(cells(&version, dim()));
    let mut b = blanks(5, St::default());
    b.extend(cells(conv.meta.cwd.as_deref().unwrap_or(""), dim()));
    let mut rows = vec![vec![GridCell::blank(); width], a, b];
    rows.iter_mut().for_each(|r| pad(r, width));
    line(rows)
}

fn one(width: usize, i: usize, conv: &Conversation) -> Vec<Line> {
    let cwd = conv.meta.cwd.as_deref();
    match &conv.items[i] {
        Item::Bubble { user: true, text, images, .. } => {
            let mut body = text.clone();
            for _ in 0..*images {
                body.push_str(" [image]");
            }
            let fill = St::default().on(PROMPT_FILL);
            let first = cells("› ", St { bold: true, dim: true, ..fill });
            // 코덱스는 사람 말 칸 위아래에 바탕 깐 빈 줄을 한 줄씩 둔다.
            let edge = || blanks(width, fill);
            let mut rows = vec![edge()];
            rows.extend(wrap(width, first, 2, &plain(body.trim(), fill), Some(PROMPT_FILL)));
            rows.push(edge());
            line(rows)
        }
        Item::Bubble { user: false, text, .. } => line(markdown(text, width, cwd)),
        Item::Duration(ms, at) => line(wrap(width, blanks(2, St::default()), 2, &plain(worked(*ms, *at), dim()), None)),
        Item::Interrupted => line(wrap(
            width,
            Vec::new(),
            2,
            &[
                ("■ ".to_string(), St::fg(FAIL)),
                ("Conversation interrupted - tell the model what to do differently.".to_string(), St::fg(FAIL)),
            ],
            None,
        )),
        Item::Compacted => line(wrap(width, cells("• ", dim()), 2, &plain("Context compacted", dim()), None)),
        Item::Launch(text) | Item::System(text) | Item::Output(text) => line(wrap(width, cells("• ", dim()), 2, &plain(text, dim()), None)),
        Item::Command { name, args } => line(wrap(width, cells("› ", St::default().bold()), 2, &plain(format!("{name} {args}").trim(), St::default()), None)),
        Item::Answered { pairs: Some(pairs), .. } => {
            let rows = pairs
                .iter()
                .flat_map(|(q, a)| wrap(width, cells("• ", dim()), 2, &[(format!("{q} → "), dim()), (a.clone(), St::default())], None))
                .collect();
            line(rows)
        }
        Item::Answered { pairs: None, .. } | Item::Thinking(_) | Item::Gone | Item::Tool { .. } => Vec::new(),
    }
}

pub(super) fn blocks(conv: &Conversation, width: usize, open: &HashSet<usize>, look: &Look, cache: &mut HashMap<usize, (u64, Vec<Line>)>) -> Vec<Block> {
    let mut shelf = Shelf::new(cache);
    match &look.banner {
        Some(rows) => {
            let sig = sig_of(conv, &[], ("codex-screen-banner", width, banner_text(rows)));
            shelf.emit(usize::MAX, false, sig, || {
                let mut out = vec![Line { cells: vec![GridCell::blank(); width], toggle: None, face: None }];
                out.extend(screen_banner(rows, width));
                out
            });
        }
        None if !conv.items.is_empty() => {
            let sig = sig_of(conv, &[], ("codex-banner", width, &conv.meta.version, &conv.meta.cwd));
            shelf.emit(usize::MAX, false, sig, || banner(width, conv));
        }
        None => {}
    }
    // 새 rollout 은 명령마다 `CommandExecution` 을 싣고, 그 명령들을 띄운 `exec` 호출은 따로 남는다 — 둘을 다 그리면 겹친다.
    let commands = conv.items.iter().any(|it| matches!(it, Item::Tool { name, .. } if name == "codex_command"));
    for row in super::parse::group_rows(&conv.items) {
        match row {
            Row::Tools(run) => {
                let run: Vec<usize> = run
                    .into_iter()
                    .filter(|&at| match &conv.items[at] {
                        Item::Tool { name, .. } => !commands || !matches!(name.as_str(), "shell" | "exec_command" | "exec" | "local_shell"),
                        _ => false,
                    })
                    .collect();
                let explore = |at: usize| matches!(&conv.items[at], Item::Tool { name, .. } if name == "codex_command") && conv.tool_inputs.get(&at).and_then(explore_lines).is_some();
                let mut i = 0;
                while i < run.len() {
                    let start = run[i];
                    if explore(start) {
                        let mut j = i;
                        while j < run.len() && explore(run[j]) {
                            j += 1;
                        }
                        let seg: Vec<usize> = run[i..j].to_vec();
                        let is_open = open.contains(&start);
                        let sig = sig_of(conv, &seg, ("explored", width, is_open));
                        shelf.emit(start, true, sig, || explored(width, &seg, conv, is_open));
                        i = j;
                        continue;
                    }
                    let is_open = open.contains(&start);
                    let sig = sig_of(conv, &[start], ("codex-tool", width, is_open));
                    shelf.emit(start, true, sig, || match &conv.items[start] {
                        Item::Tool { name, .. } if name == "codex_command" => ran(width, start, conv, is_open),
                        _ => called(width, start, conv, is_open),
                    });
                    i += 1;
                }
            }
            Row::One(i) => {
                if matches!(conv.items[i], Item::Thinking(_) | Item::Gone) {
                    continue;
                }
                let sig = sig_of(conv, &[i], ("codex-one", width));
                shelf.emit(i, true, sig, || one(width, i, conv));
            }
        }
    }
    shelf.done()
}

#[cfg(test)]
mod tests {
    use super::super::classic::Cache;
    use super::*;

    fn text(line: &Line) -> String {
        line.cells.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn a_codex_turn_lays_out_like_the_inline_screen() {
        let lines = [
            r#"{"type":"session_meta","payload":{"cli_version":"0.162.1","cwd":"/w"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"run things"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"하겠습니다."}]}}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c1","input":"tools.exec_command({cmd:\"printf 1\"})"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","printf \"%s\\n\" 1 2 3 4 5"],"parsed_cmd":[{"type":"unknown","cmd":"printf"}],"status":"completed","aggregated_output":"1\n2\n3\n4\n5\n","exit_code":0}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","cat list.txt"],"parsed_cmd":[{"type":"read","cmd":"cat list.txt","name":"list.txt","path":"list.txt"}],"status":"completed","aggregated_output":"a\n","exit_code":0}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","touch x"],"parsed_cmd":[{"type":"unknown","cmd":"touch x"}],"status":"completed","aggregated_output":"","exit_code":0}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"| 명령 | 출력 |\n|---|---|\n| `cat` | a |\n\n- 끝"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_complete","duration_ms":13144}}"#,
        ];
        let mut conv = Conversation::default();
        assert!(conv.apply(&lines.join("\n")));
        let mut cache = Cache::default();
        cache.build(&conv, 1, 50, &HashSet::new(), &super::super::classic::Look { face: false, student: None, banner: None });
        let got: Vec<String> = cache.lines.iter().map(text).collect();
        let want = [
            "",
            "  >_ OpenAI Codex (v0.162.1)",
            "     /w",
            "",
            "",
            "› run things",
            "",
            "",
            "• 하겠습니다.",
            "",
            "• Ran printf \"%s\\n\" 1 2 3 4 5",
            "  └ 3",
            "    4",
            "    5",
            "    + 2 lines (⌃t to expand)",
            "",
            "• Explored",
            "  └ Read list.txt",
            "    + Show details",
            "",
            "• Ran touch x",
            "  └ (no output)",
            "",
            "•  명령    출력",
            "  ━━━━━━  ━━━━━━",
            "   cat     a",
            "",
            "  • 끝",
            "",
            "  Worked for 13s",
            "",
        ];
        assert_eq!(got, want);
    }
}
