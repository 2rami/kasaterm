//! 학생 기록(jsonl)을 대화 칸으로 편다. 폰의 `mobile/lib/conversation.dart` 와 같은 규칙이다 —
//! 같은 기록을 두 기기가 다르게 읽으면 「폰에선 보이던 말이 PC 에선 없다」가 된다.
//! claude 의 transcript 와 codex 의 rollout 을 한 칸 목록으로 편다.

use std::collections::HashMap;

use serde_json::Value;

/// 사람 대신 입력칸에 말을 넣은 쪽 — 이름과 무슨 말인지(쪽지·완료 보고·맡긴 일).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sender {
    pub(crate) name: String,
    pub(crate) via: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    /// user 턴이면 사람 말이거나, `from` 이 있으면 사람 대신 다른 칸·카사텀·나쵸가 넣은 말.
    Bubble { user: bool, text: String, from: Option<Sender>, queued: bool, images: usize },
    Thinking(String),
    Tool { name: String, summary: String, result: Option<String>, error: bool },
    /// 슬래시 명령(`/model`)이나 `!` 셸 한 줄.
    Command { name: String, args: String },
    /// 명령의 출력(`<local-command-stdout>`·`<bash-stdout>`).
    Output(String),
    /// 답이 붙은 AskUserQuestion. 답이 오기 전엔 안 그린다 — 그동안은 화면에서 읽은
    /// 선택지 카드가 맡는다.
    Answered { questions: Vec<String>, pairs: Option<Vec<(String, String)>> },
    /// 서브에이전트·워크플로를 띄웠다는 한 줄.
    Launch(String),
    System(String),
    Interrupted,
    /// 꺼내진 예약. 지우면 뒤 칸의 번호가 밀려 도구 결과가 엉뚱한 칸에 붙는다.
    Gone,
}

#[derive(Default)]
pub(crate) struct Conversation {
    pub(crate) items: Vec<Item>,
    by_tool: HashMap<String, usize>,
    queue: Vec<Option<usize>>,
    session: Option<String>,
    last_codex_say: Option<String>,
    last_codex_prompt: Option<String>,
}

impl Conversation {
    /// 기록 조각 하나. 세션이 바뀐 것을 알아채면 false — 부르는 쪽이 처음부터 다시 읽는다.
    /// 파일이 줄어들 때만 꼬리를 다시 읽으므로 `/clear` 뒤 새 파일이 옛 자리보다 길면
    /// 새 파일의 중간부터 읽게 된다. 줄마다 붙은 sessionId 로 그 경우를 가른다.
    pub(crate) fn apply(&mut self, raw: &str) -> bool {
        let events: Vec<Value> = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(Value::is_object)
            .collect();
        if let Some(current) = &self.session {
            if events.iter().any(|e| e["sessionId"].as_str().is_some_and(|sid| sid != current)) {
                return false;
            }
        }
        for e in &events {
            if let Some(sid) = e["sessionId"].as_str() {
                if e["isSidechain"] != Value::Bool(true) {
                    self.session = Some(sid.to_string());
                }
            }
            if e["payload"].is_object() {
                self.codex(e);
            } else {
                self.claude(e);
            }
        }
        true
    }

    fn push(&mut self, item: Item) -> usize {
        self.items.push(item);
        self.items.len() - 1
    }

    // ── claude ──────────────────────────────────────────────────────────────

    fn claude(&mut self, ev: &Value) {
        if ev["isSidechain"] == Value::Bool(true) {
            return;
        }
        let kind = ev["type"].as_str().unwrap_or("");
        match kind {
            "system" => {
                if let Some(text) = system_text(ev) {
                    self.push(Item::System(text));
                }
                return;
            }
            "queue-operation" => {
                self.queue_op(ev);
                return;
            }
            "attachment" => {
                let att = &ev["attachment"];
                if att["type"] == "hook_success" {
                    if let Some(msg) = hook_message(&att["stdout"]) {
                        self.push(Item::System(msg));
                    }
                }
                return;
            }
            "user" | "assistant" => {}
            _ => return,
        }
        let user = kind == "user";
        // 스킬 본문·caveat 처럼 사람이 친 적 없는 주입 — 사람 말풍선으로 새면 안 된다.
        if user && ev["isMeta"] == Value::Bool(true) {
            return;
        }
        let content = &ev["message"]["content"];
        if user && interrupted(content) {
            if !matches!(self.items.last(), Some(Item::Interrupted)) {
                self.push(Item::Interrupted);
            }
            return;
        }
        if let Some(s) = content.as_str() {
            if user {
                self.user_text(s);
            } else if !s.trim().is_empty() {
                self.push(Item::Bubble { user: false, text: s.trim().into(), from: None, queued: false, images: 0 });
            }
            return;
        }
        let Some(blocks) = content.as_array() else { return };
        for block in blocks {
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    let Some(t) = block["text"].as_str().filter(|t| !t.trim().is_empty()) else { continue };
                    if user {
                        self.user_text(t);
                    } else {
                        self.push(Item::Bubble { user: false, text: t.trim().into(), from: None, queued: false, images: 0 });
                    }
                }
                "image" if user => match self.items.last_mut() {
                    Some(Item::Bubble { user: true, from: None, images, .. }) => *images += 1,
                    _ => {
                        self.push(Item::Bubble { user: true, text: String::new(), from: None, queued: false, images: 1 });
                    }
                },
                "thinking" => {
                    if let Some(t) = block["thinking"].as_str().filter(|t| !t.trim().is_empty()) {
                        self.push(Item::Thinking(t.trim().into()));
                    }
                }
                "tool_use" => self.tool_use(block),
                "tool_result" => self.tool_result(block),
                _ => {}
            }
        }
    }

    fn tool_use(&mut self, b: &Value) {
        let name = b["name"].as_str().unwrap_or("tool");
        let input = &b["input"];
        let item = match name {
            "AskUserQuestion" => Item::Answered {
                questions: input["questions"]
                    .as_array()
                    .map(|qs| {
                        qs.iter()
                            .filter(|q| q.is_object())
                            .map(|q| {
                                q["question"].as_str().or(q["header"].as_str()).unwrap_or("질문").to_string()
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                pairs: None,
            },
            "Agent" | "Task" => Item::Launch(
                [input["subagent_type"].as_str(), input["description"].as_str()]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
            ),
            "Workflow" => Item::Launch(format!("워크플로 {}", workflow_name(input).unwrap_or_default()).trim().to_string()),
            _ => Item::Tool { name: name.into(), summary: tool_summary(name, input), result: None, error: false },
        };
        let at = self.push(item);
        if let Some(id) = b["id"].as_str() {
            self.by_tool.insert(id.into(), at);
        }
    }

    fn tool_result(&mut self, b: &Value) {
        let Some(&at) = b["tool_use_id"].as_str().and_then(|id| self.by_tool.get(id)) else { return };
        let text = result_text(&b["content"]);
        match &mut self.items[at] {
            Item::Tool { result, error, .. } => {
                *result = Some(text);
                *error = b["is_error"] == Value::Bool(true);
            }
            Item::Answered { questions, pairs } => *pairs = Some(answered_pairs(&text, questions)),
            _ => {}
        }
    }

    fn user_text(&mut self, raw: &str) {
        if let Some(cmd) = tag(raw, "command-name") {
            let args = tag(raw, "command-args").unwrap_or("").trim().to_string();
            self.push(Item::Command { name: cmd.trim().into(), args });
            return;
        }
        if let Some(bash) = tag(raw, "bash-input") {
            self.push(Item::Command { name: "!".into(), args: bash.trim().into() });
            return;
        }
        let out = match tag(raw, "local-command-stdout") {
            Some(s) => s.to_string(),
            None => [tag(raw, "bash-stdout"), tag(raw, "bash-stderr")]
                .into_iter()
                .flatten()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        if !out.trim().is_empty() {
            self.push(Item::Output(strip_ansi(&out).trim().into()));
            return;
        }
        if raw.contains("<bash-stdout>") || raw.contains("<local-command-stdout>") {
            return;
        }
        let clean = strip_meta(raw);
        if clean.is_empty() || is_injection(&clean) {
            return;
        }
        let item = match relayed(&clean) {
            Some((from, body)) => Item::Bubble { user: true, text: body, from: Some(from), queued: false, images: 0 },
            None => Item::Bubble { user: true, text: clean, from: None, queued: false, images: 0 },
        };
        self.push(item);
    }

    /// 작업 중 보낸 예약은 정식 user 턴이 아니라 큐 조작으로만 남는다. enqueue 를 대기
    /// 말풍선으로 세우고, dequeue 로 꺼내지면 같은 글이 정식 턴으로 다시 오니 걷는다.
    fn queue_op(&mut self, ev: &Value) {
        match ev["operation"].as_str().unwrap_or("") {
            "enqueue" => {
                let clean = ev["content"].as_str().map(strip_meta).unwrap_or_default();
                if clean.is_empty() || is_injection(&clean) {
                    self.queue.push(None);
                    return;
                }
                let at = self.push(Item::Bubble { user: true, text: clean, from: None, queued: true, images: 0 });
                self.queue.push(Some(at));
            }
            op @ ("dequeue" | "popAll") => {
                let n = if op == "popAll" { self.queue.len() } else { 1 };
                for _ in 0..n.min(self.queue.len()) {
                    if let Some(at) = self.queue.remove(0) {
                        self.items[at] = Item::Gone;
                    }
                }
            }
            "remove" => {
                if !self.queue.is_empty() {
                    if let Some(at) = self.queue.remove(0) {
                        if let Item::Bubble { queued, .. } = &mut self.items[at] {
                            *queued = false;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ── codex ───────────────────────────────────────────────────────────────

    fn codex(&mut self, ev: &Value) {
        let p = &ev["payload"];
        let kind = (ev["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or(""));
        match kind {
            // 옛 판은 event_msg 로, 새 판은 item_completed 로 말을 적는다 — 한 판이 둘 다
            // 적기도 해서, 같은 쪽의 같은 글이 이어 오면 한 번만 세운다.
            ("event_msg", "user_message") => self.codex_say(true, p["message"].as_str().unwrap_or("")),
            ("event_msg", "agent_message") => self.codex_say(false, p["message"].as_str().unwrap_or("")),
            ("event_msg", "item_completed") => {
                let item = &p["item"];
                match item["type"].as_str().unwrap_or("") {
                    "UserMessage" => self.codex_say(true, &codex_text(&item["content"])),
                    "AgentMessage" => self.codex_say(false, &codex_text(&item["content"])),
                    _ => {}
                }
            }
            ("event_msg", "agent_reasoning") => {
                let t = p["text"].as_str().unwrap_or("").trim();
                if !t.is_empty() {
                    self.push(Item::Thinking(t.into()));
                }
            }
            ("event_msg", "turn_aborted") => {
                self.push(Item::Interrupted);
            }
            ("response_item", "function_call" | "custom_tool_call") => {
                let raw = if p["arguments"].is_null() { &p["input"] } else { &p["arguments"] };
                let (name, input) = match (p["name"].as_str().unwrap_or("tool"), raw.as_str().and_then(codex_exec)) {
                    ("exec", Some(shell)) => ("shell", shell),
                    (name, _) => (name, codex_input(raw)),
                };
                if name == "spawn_agent" {
                    self.push(Item::Launch(tool_summary(name, &input)));
                    return;
                }
                let at = self.push(Item::Tool { name: name.into(), summary: tool_summary(name, &input), result: None, error: false });
                if let Some(id) = p["call_id"].as_str() {
                    self.by_tool.insert(id.into(), at);
                }
            }
            ("response_item", "function_call_output" | "custom_tool_call_output") => {
                let Some(&at) = p["call_id"].as_str().and_then(|id| self.by_tool.get(id)) else { return };
                let (text, failed) = codex_output(&p["output"]);
                if let Item::Tool { result, error, .. } = &mut self.items[at] {
                    *result = Some(text);
                    *error = failed;
                }
            }
            _ => {}
        }
    }

    fn codex_say(&mut self, user: bool, raw: &str) {
        let t = raw.trim();
        let last = if user { &mut self.last_codex_prompt } else { &mut self.last_codex_say };
        if t.is_empty() || last.as_deref() == Some(t) {
            return;
        }
        *last = Some(t.to_string());
        self.push(Item::Bubble { user, text: t.into(), from: None, queued: false, images: 0 });
    }
}

/// 그릴 줄 하나 — 이어진 도구 호출은 한 묶음(`Tools`), 그 밖은 칸 하나. 한 턴에 도구가
/// 수십 번이라 한 줄씩 세우면 말이 안 보인다. 값은 `Conversation::items` 의 번호다.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    One(usize),
    Tools(Vec<usize>),
}

/// 칸을 그릴 줄로 — 이어진 도구는 한 묶음, 답 안 온 질문과 꺼내진 예약은 뺀다.
pub(crate) fn group_rows(items: &[Item]) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        match it {
            Item::Gone | Item::Answered { pairs: None, .. } => {}
            Item::Tool { .. } => match out.last_mut() {
                Some(Row::Tools(run)) => run.push(i),
                _ => out.push(Row::Tools(vec![i])),
            },
            _ => out.push(Row::One(i)),
        }
    }
    out
}

// ── 글 다듬기(폰·데스크톱 아로나와 같은 규칙) ─────────────────────────────────

const META_BLOCKS: &[(&str, &str)] = &[
    ("<system-reminder>", "</system-reminder>"),
    ("<command-message>", "</command-message>"),
    ("<command-name>", "</command-name>"),
    ("<command-args>", "</command-args>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<task-notification>", "</task-notification>"),
    ("<local-command-caveat>", "</local-command-caveat>"),
];

/// 시스템이 user 턴에 끼워 넣은 블록과 그림 자리표시 줄을 걷는다.
pub(crate) fn strip_meta(text: &str) -> String {
    let mut s = unwrap_pasted(unwrap_plugin_prompt(text));
    for (open, close) in META_BLOCKS {
        while let Some(start) = s.find(open) {
            match s[start + open.len()..].find(close) {
                Some(rel) => s.replace_range(start..start + open.len() + rel + close.len(), ""),
                None => {
                    s.truncate(start);
                    break;
                }
            }
        }
    }
    let kept: Vec<&str> = s
        .split('\n')
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("[Image: source:")
                || t.starts_with("[Image: original ")
                || (t.starts_with("[Image #") && t.ends_with(']')))
        })
        .collect();
    replace_image_marks(&kept.join("\n")).trim().to_string()
}

/// 10-07 전 mod 가 `$.prompt.submit` 으로 넣은 말(tell·거울 입력)을 엔진은 「The <mod> plugin sent a message:」와 뒤 설명으로
/// 감싸 기록한다 — 사람이 보낸 말만 남긴다.
fn unwrap_plugin_prompt(text: &str) -> &str {
    let Some((name, body)) = text.strip_prefix("The ").and_then(|r| r.split_once(" plugin sent a message:")) else {
        return text;
    };
    if name.is_empty() || name.contains(char::is_whitespace) {
        return text;
    }
    let body = body.strip_prefix('\n').or_else(|| body.strip_prefix(' ')).unwrap_or(body);
    body.split("\n\nThis is how Claude Code surfaces a prompt a plugin submits").next().unwrap_or(body)
}

/// 붙여넣은 글은 `<pasted_content id="…">글</pasted_content id="…">` 로 감겨 기록된다 — 긴 tell 도
/// 붙여넣기로 들어가 이렇게 남는다. 감싼 표만 걷고 글은 둔다.
fn unwrap_pasted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<pasted_content").or_else(|| rest.find("</pasted_content")) {
        out.push_str(&rest[..at]);
        match rest[at..].find('>') {
            Some(end) => rest = &rest[at + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `[Image #3]` → `(사진)`.
fn replace_image_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("[Image #") {
        let after = &rest[at + 8..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(']') {
            out.push_str(&rest[..at]);
            out.push_str("(사진)");
            rest = &after[digits + 1..];
        } else {
            out.push_str(&rest[..at + 8]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// 사람이 친 말처럼 보이지만 하네스가 넣은 것 — 압축 요약 이어가기 등.
pub(crate) fn is_injection(text: &str) -> bool {
    let lower = text.trim_start().to_lowercase();
    text.to_lowercase().contains("[request interrupted")
        || lower.strip_prefix("##").is_some_and(|r| r.trim_start().starts_with("context usage"))
        || lower.strip_prefix("caveat:").is_some_and(|r| r.starts_with(char::is_whitespace))
        || lower.starts_with("this session is being continued from a previous conversation")
}

/// 사람이 아니라 다른 칸·카사텀·나쵸가 이 학생 입력칸에 넣은 말이면 보낸 쪽과 본문. 모두 입력칸에
/// 붙여넣어 들어가 기록에는 사람이 친 말과 같은 user 턴으로 남는다 — 표식을 안 읽으면 대화 보기가
/// 남의 말을 사람 말풍선에 담는다(2026-10-08 지적). 터미널 화면은 `screenread` 가 같은 표식을 읽는다.
pub(crate) fn relayed(text: &str) -> Option<(Sender, String)> {
    let sender = |name: &str, via| Sender { name: name.trim().to_string(), via };
    if let Some((name, body)) = teammate_message(text) {
        return Some((sender(&name, "쪽지"), body));
    }
    let t = text.trim();
    // `kasaterm-cli tell` 이 보낸 칸의 학생 이름을 심는다.
    if let Some((name, body)) = t.strip_prefix('⟦').and_then(|r| r.split_once('⟧')) {
        let ok = !name.trim().is_empty() && name.chars().count() <= 24 && !name.contains('\n');
        return ok.then(|| (sender(name, "쪽지"), body.trim().to_string()));
    }
    // 학생의 `done` 을 연 칸에 알리는 줄 — `[완료] 이름(%N) — 요약`(socket.rs `pane_done`).
    for (mark, via) in [("[완료]", "완료 보고"), ("[실패]", "실패 보고")] {
        let Some(rest) = t.strip_prefix(mark) else { continue };
        let rest = rest.trim_start();
        let open = rest.find('(')?;
        let close = open + rest[open..].find(')')?;
        if open == 0 || !rest[open + 1..close].starts_with('%') {
            return None;
        }
        let body = rest[close + 1..].trim_start();
        let body = body.strip_prefix('—').unwrap_or(body).trim();
        return Some((sender(&rest[..open], via), body.to_string()));
    }
    // 나쵸가 일을 맡기며 붙이는 첫 줄은 보고 방법 안내라 걷고, 그 아래 맡긴 글만 남긴다.
    if let Some(rest) = t.strip_prefix("[origin=nacho") {
        let (first, more) = rest.split_once('\n').unwrap_or((rest, ""));
        let body = if more.trim().is_empty() { first.split_once(']').map_or(first, |(_, b)| b) } else { more };
        return Some((sender("나쵸", "맡긴 일"), body.trim().to_string()));
    }
    if let Some(body) = t.strip_prefix("[쪽지 확인 못 함]") {
        return Some((sender("카사텀", "쪽지 확인 못 함"), body.trim().to_string()));
    }
    None
}

/// `<teammate-message teammate_id="…">본문</teammate-message>` 이 턴 전체일 때만.
pub(crate) fn teammate_message(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let rest = t.strip_prefix("<teammate-message")?;
    if !rest.starts_with(|c: char| c.is_whitespace() || c == '>') {
        return None;
    }
    let close = rest.find('>')?;
    let attrs = &rest[..close];
    let body = rest[close + 1..].strip_suffix("</teammate-message>")?;
    let name_at = attrs.find("teammate_id=\"")? + 13;
    let name = &attrs[name_at..name_at + attrs[name_at..].find('"')?];
    let body = body.trim();
    if name.trim().is_empty() || body.is_empty() || body.contains("<teammate-message") {
        return None;
    }
    Some((name.trim().into(), body.into()))
}

/// AskUserQuestion 결과 글(`…answered: "질문"="답". …`)에서 질문↔답.
pub(crate) fn answered_pairs(result: &str, questions: &[String]) -> Vec<(String, String)> {
    let mut found: HashMap<String, String> = HashMap::new();
    let mut rest = result;
    while let Some(q0) = rest.find('"') {
        let after = &rest[q0 + 1..];
        let Some(q1) = after.find('"') else { break };
        let question = &after[..q1];
        let tail = &after[q1 + 1..];
        if let Some(ans) = tail.strip_prefix("=\"") {
            if let Some(a1) = ans.find('"') {
                if !question.is_empty() {
                    found.insert(question.into(), ans[..a1].into());
                }
                rest = &ans[a1 + 1..];
                continue;
            }
        }
        rest = tail;
    }
    questions
        .iter()
        .map(|q| (q.clone(), found.get(q).cloned().unwrap_or_else(|| "—".into())))
        .collect()
}

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 도구 한 줄의 꼬리 — 무엇에 썼는지를 사람 말로.
pub(crate) fn tool_summary(name: &str, input: &Value) -> String {
    let s = |k: &str| -> Option<String> {
        match &input[k] {
            Value::String(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
            Value::Array(parts) if !parts.is_empty() && parts.iter().all(Value::is_string) => {
                let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                // codex 셸은 ["bash","-lc","명령"] — 앞의 해석기는 사람이 읽을 거리가 아니다.
                let joined = if parts.len() >= 3 && parts[1] == "-lc" { parts[parts.len() - 1].to_string() } else { parts.join(" ") };
                Some(joined.trim().to_string())
            }
            _ => None,
        }
    };
    let base = |p: String| p.rsplit('/').find(|s| !s.is_empty()).map(str::to_string).unwrap_or(p);
    let picked = match name {
        "Bash" => s("description").or_else(|| s("command")),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => s("file_path").map(base),
        "Grep" | "Glob" => s("pattern"),
        "WebFetch" => s("url"),
        "WebSearch" | "ToolSearch" => s("query"),
        "TaskCreate" => s("subject"),
        "Skill" => s("skill"),
        _ => s("description")
            .or_else(|| s("command"))
            .or_else(|| s("cmd"))
            .or_else(|| s("file_path").map(base))
            .or_else(|| s("query"))
            .or_else(|| s("url"))
            .or_else(|| input.as_object().and_then(|o| o.values().find_map(|v| v.as_str().map(|v| v.trim().to_string())))),
    };
    let line = picked.unwrap_or_default().split('\n').next().unwrap_or("").to_string();
    if line.chars().count() > 90 {
        format!("{}…", line.chars().take(89).collect::<String>())
    } else {
        line
    }
}

/// `mcp__kasachrome__browser_click` → `browser_click`.
pub(crate) fn tool_label(name: &str) -> &str {
    if name.starts_with("mcp__") {
        name.rsplit("__").next().unwrap_or(name)
    } else {
        name
    }
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)?;
    Some(&s[start..start + end])
}

fn interrupted(content: &Value) -> bool {
    let flat = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    };
    flat.trim_start().starts_with("[Request interrupted by user")
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn system_text(ev: &Value) -> Option<String> {
    match ev["subtype"].as_str()? {
        "compact_boundary" => Some(match ev["compactMetadata"]["preTokens"].as_f64() {
            Some(pre) => format!("대화를 압축했어요 · {}k 토큰", (pre / 1000.0).round() as i64),
            None => "대화를 압축했어요".into(),
        }),
        "api_error" => {
            let mut parts = vec!["API 오류".to_string()];
            if let Some(status) = ev["error"]["status"].as_i64() {
                parts.push(status.to_string());
            }
            if !ev["retryAttempt"].is_null() {
                parts.push(format!("재시도 {}/{}", ev["retryAttempt"], ev["maxRetries"]));
            }
            Some(parts.join(" · "))
        }
        _ => None,
    }
}

fn hook_message(stdout: &Value) -> Option<String> {
    let v: Value = serde_json::from_str(stdout.as_str()?.trim()).ok()?;
    let msg = v["systemMessage"].as_str()?.trim();
    (!msg.is_empty()).then(|| msg.to_string())
}

fn workflow_name(input: &Value) -> Option<String> {
    let script = input["script"].as_str()?;
    let at = script.find("name:")? + 5;
    let rest = script[at..].trim_start();
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let body = &rest[1..];
    Some(body[..body.find(quote)?].to_string())
}

fn codex_text(content: &Value) -> String {
    content
        .as_array()
        .map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

/// codex 의 `exec` 는 인자가 JS 코드다 — `await tools.exec_command({cmd:"…"})`, 여럿이면 `Promise.all([…])`.
/// 사람이 읽을 것은 그 안의 명령이라 꺼내 셸 도구로 편다(코드를 그대로 두면 「const r = await tools…」가 떠
/// 무엇을 했는지 안 보인다, 2026-10-08). 명령이 없으면(다른 도구만 부른 코드) None.
fn codex_exec(js: &str) -> Option<Value> {
    let mut cmds: Vec<String> = Vec::new();
    let mut rest = js;
    while let Some(at) = rest.find("cmd:") {
        rest = rest[at + 4..].trim_start();
        let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\'' | '`')) else { continue };
        let mut out = String::new();
        let mut chars = rest[1..].char_indices();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, e)) => out.push(e),
                    None => break,
                },
                c if c == quote => {
                    end = Some(i + 1 + c.len_utf8());
                    break;
                }
                c => out.push(c),
            }
        }
        let Some(end) = end else { break };
        rest = &rest[end..];
        if !out.trim().is_empty() {
            cmds.push(out.trim().to_string());
        }
    }
    let first = cmds.first()?;
    let command = match cmds.len() {
        1 => first.clone(),
        n => format!("{} 외 {}개", first.lines().next().unwrap_or(""), n - 1),
    };
    Some(serde_json::json!({ "command": command }))
}

fn codex_input(args: &Value) -> Value {
    match args {
        Value::String(s) => {
            if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(s) {
                return v;
            }
            // apply_patch 는 인자가 JSON 이 아니라 패치 글 그대로다.
            let file = ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|m| s.find(m).map(|at| s[at + m.len()..].lines().next().unwrap_or("").trim().to_string()));
            let mut o = serde_json::Map::new();
            if let Some(f) = file {
                o.insert("file_path".into(), Value::String(f));
            }
            o.insert("input".into(), Value::String(s.clone()));
            Value::Object(o)
        }
        Value::Object(_) => args.clone(),
        _ => Value::Object(Default::default()),
    }
}

fn codex_output(out: &Value) -> (String, bool) {
    if let Some(s) = out.as_str() {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(s) {
            let text = match &v["output"] {
                Value::String(t) => t.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            let code = if v["metadata"].is_object() { &v["metadata"]["exit_code"] } else { &v["exit_code"] };
            return (text, code.as_i64().is_some_and(|c| c != 0));
        }
        return (s.to_string(), false);
    }
    (result_text(out), false)
}

// ── 화면에서 읽는 선택지 ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PromptOption {
    /// 화면에 적힌 번호(1부터).
    pub(crate) index: usize,
    pub(crate) label: String,
    pub(crate) current: bool,
    /// 이름 밑에 딸린 설명 줄(질문 창의 선택지 설명). 없으면 빈 글.
    pub(crate) note: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PromptMenu {
    pub(crate) title: String,
    pub(crate) options: Vec<PromptOption>,
    /// 제목 위 창 안의 줄들, 화면 차례 그대로 — 도구 이름·설명·명령 원문(Claude `Bash command`,
    /// 코덱스 `Would you like to run…`·`$ 명령`, 질문 머리). 거울 카드가 TUI 와 같은 정보를 보이는 재료다.
    pub(crate) context: Vec<String>,
}

impl PromptMenu {
    pub(crate) fn cursor(&self) -> usize {
        self.options.iter().position(|o| o.current).unwrap_or(0)
    }
}

/// `❯ 1. 예` 꼴 한 줄 — (고른 표식이 있나, 번호, 이름).
fn option_line(line: &str) -> Option<(bool, usize, String)> {
    let t = line.trim();
    let t = t.strip_prefix(['│', '|']).unwrap_or(t).trim_start();
    let (current, t) = match t.chars().next() {
        Some(c @ ('❯' | '›' | '●')) => (true, t[c.len_utf8()..].trim_start()),
        _ => (false, t),
    };
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = t[digits..].strip_prefix('.')?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let label = rest.trim();
    let label = label.strip_suffix(['│', '|']).unwrap_or(label).trim_end();
    // 이름 뒤 두 칸 넘게 띄운 설명은 이름이 아니다.
    let label = label.split("  ").next().unwrap_or(label).trim();
    if label.is_empty() {
        return None;
    }
    Some((current, t[..digits].parse().ok()?, label.to_string()))
}

/// 고른 표식(❯)이 있어야 살아 있는 선택지다 — 답글 속 번호 목록과 가른다.
pub(crate) fn parse_prompt_menu(lines: &[String]) -> Option<PromptMenu> {
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    let start = last.saturating_sub(13);
    let mut opts: Vec<(PromptOption, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate().take(last + 1).skip(start) {
        if let Some((current, index, label)) = option_line(line) {
            opts.push((PromptOption { index, label, current, note: String::new() }, i));
        }
    }
    if opts.len() < 2 || !opts.iter().any(|(o, _)| o.current) {
        return None;
    }
    let after_last = opts.last()?.1 + 1;
    if lines[after_last.min(lines.len())..].iter().any(|l| {
        let t = l.trim_start();
        let t = t.strip_prefix(['│', '|']).unwrap_or(t).trim_start();
        t.starts_with(['❯', '›']) && option_line(l).is_none()
    }) {
        return None;
    }
    // 흩어진 번호 줄은 메뉴가 아니다. 다만 AskUserQuestion 은 선택지마다 설명 줄이 딸려
    // 벌어지므로, 번호가 1부터 빈틈없이 이어지면 선택지당 서너 줄까지 봐준다.
    let ordered = opts.iter().enumerate().all(|(i, (o, _))| o.index == i + 1);
    let spread = opts.last()?.1 - opts[0].1;
    if spread > if ordered { opts.len() * 4 } else { opts.len() + 2 } {
        return None;
    }
    let first = opts[0].1;
    let mut title = String::new();
    let mut title_at = first;
    for i in (first.saturating_sub(6)..first).rev() {
        let t = lines[i].trim_matches(|c: char| c.is_whitespace() || c == '│' || c == '|');
        if t.is_empty() || t.starts_with(['─', '—', '-', '╭', '╰', '╮', '╯', '>', '❯', '›', '●']) {
            continue;
        }
        title = t.to_string();
        title_at = i;
        break;
    }
    let width = lines.iter().map(|l| l.trim_end().chars().count()).max().unwrap_or(0);
    for k in 0..opts.len() {
        let at = opts[k].1;
        let end = opts.get(k + 1).map_or((at + 5).min(lines.len()), |n| n.1);
        let number_col = lines[at].chars().position(|c| c.is_ascii_digit()).unwrap_or(0);
        let mut more: Vec<String> = Vec::new();
        for line in &lines[at + 1..end] {
            let indent = line.chars().take_while(|c| c.is_whitespace()).count();
            let t = line.trim();
            if t.is_empty() || indent <= number_col || is_rule(t) {
                break;
            }
            more.push(t.to_string());
        }
        if more.is_empty() {
            continue;
        }
        // 이름이 줄 끝까지 찼으면 다음 줄은 접힌 이름의 나머지다(Claude 의 긴 「Yes, and …」).
        if lines[at].trim_end().chars().count() + 4 >= width {
            let o = &mut opts[k].0;
            o.label = format!("{} {}", o.label, more.join(" "));
        } else {
            opts[k].0.note = more.join(" ");
        }
    }
    let mut context: Vec<String> = Vec::new();
    for line in lines[title_at.saturating_sub(16)..title_at].iter().rev() {
        let t = line.trim_matches(|c: char| c.is_whitespace() || c == '│' || c == '|');
        if t.is_empty() || t.chars().all(|c| matches!(c, '╌' | '┄' | '┈' | '·')) {
            continue;
        }
        if is_rule(t) || t.starts_with(['╭', '╰', '•', '⏺', '●', '❯', '›', '>', '⎿', '✻', '✢', '✽']) {
            break;
        }
        context.push(t.to_string());
    }
    context.reverse();
    Some(PromptMenu { title, options: opts.into_iter().map(|(o, _)| o).collect(), context })
}

/// 창을 가르는 가로줄(`────`) — 창 머리 위나 질문 창 아래 칸막이.
fn is_rule(t: &str) -> bool {
    t.chars().filter(|c| matches!(c, '─' | '━')).count() >= 8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(lines: &[&str]) -> Conversation {
        let mut c = Conversation::default();
        assert!(c.apply(&lines.join("\n")));
        c
    }

    #[test]
    fn claude_turns_become_bubbles_tools_and_results() {
        let c = conv(&[
            r#"{"type":"user","sessionId":"s","message":{"content":"안녕"}}"#,
            r#"{"type":"assistant","sessionId":"s","message":{"content":[{"type":"text","text":"네"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls","description":"목록"}}]}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a\nb","is_error":true}]}}"#,
        ]);
        assert_eq!(c.items.len(), 3);
        assert!(matches!(&c.items[0], Item::Bubble { user: true, from: None, .. }));
        assert_eq!(c.items[2], Item::Tool { name: "Bash".into(), summary: "목록".into(), result: Some("a\nb".into()), error: true });
        assert_eq!(group_rows(&c.items), vec![Row::One(0), Row::One(1), Row::Tools(vec![2])]);
    }

    #[test]
    fn meta_injections_and_sidechains_never_reach_the_human_side() {
        let c = conv(&[
            r#"{"type":"user","isMeta":true,"message":{"content":"스킬 본문"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"서브"}}"#,
            r#"{"type":"user","message":{"content":"<system-reminder>x</system-reminder>진짜 말"}}"#,
            r#"{"type":"user","message":{"content":"This session is being continued from a previous conversation"}}"#,
            r#"{"type":"user","message":{"content":"[Request interrupted by user]"}}"#,
        ]);
        assert_eq!(c.items.len(), 2);
        assert!(matches!(&c.items[0], Item::Bubble { text, .. } if text == "진짜 말"));
        assert_eq!(c.items[1], Item::Interrupted);
    }

    #[test]
    fn commands_outputs_and_teammates_get_their_own_shapes() {
        let c = conv(&[
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name><command-args>opus</command-args>"}}"#,
            r#"{"type":"user","message":{"content":"<local-command-stdout>\u001b[1m바꿈\u001b[0m</local-command-stdout>"}}"#,
            r#"{"type":"user","message":{"content":"<teammate-message teammate_id=\"아로나\">끝났어요</teammate-message>"}}"#,
        ]);
        assert_eq!(c.items[0], Item::Command { name: "/model".into(), args: "opus".into() });
        assert_eq!(c.items[1], Item::Output("바꿈".into()));
        assert!(matches!(&c.items[2], Item::Bubble { from: Some(f), text, .. } if f.name == "아로나" && text == "끝났어요"));
    }

    /// tell·완료 보고·나쵸가 맡긴 일은 사람 말풍선이 아니라 보낸 쪽 이름을 단 말이다.
    #[test]
    fn relayed_turns_name_who_put_them_in() {
        let c = conv(&[
            r#"{"type":"user","message":{"content":"⟦아즈사⟧ 폰 판 구워서 올려"}}"#,
            r#"{"type":"user","message":{"content":"\n\n<pasted_content id=\"95ad\">\n⟦유우카⟧ 나쵸 쪽 답\n둘째 줄\n</pasted_content id=\"95ad\">\n"}}"#,
            r#"{"type":"user","message":{"content":"[완료] 코유키(%0) — 카사넷 끝"}}"#,
            r#"{"type":"user","message":{"content":"[origin=nacho task=w1] 보고는 이렇게 한다\n거노 지시 그대로: 고쳐 줘"}}"#,
            r#"{"type":"user","message":{"content":"[완료] 표시만 친 사람 말"}}"#,
            r#"{"type":"user","message":{"content":"사람 말"}}"#,
        ]);
        let who: Vec<Option<(&str, &str, &str)>> = c
            .items
            .iter()
            .map(|i| match i {
                Item::Bubble { from, text, .. } => from.as_ref().map(|f| (f.name.as_str(), f.via, text.as_str())),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(who[0], Some(("아즈사", "쪽지", "폰 판 구워서 올려")));
        assert_eq!(who[1], Some(("유우카", "쪽지", "나쵸 쪽 답\n둘째 줄")));
        assert_eq!(who[2], Some(("코유키", "완료 보고", "카사넷 끝")));
        assert_eq!(who[3], Some(("나쵸", "맡긴 일", "거노 지시 그대로: 고쳐 줘")));
        assert_eq!(who[4], None, "괄호 속 칸 번호가 없으면 사람이 친 글이다");
        assert_eq!(who[5], None);
    }

    #[test]
    fn queued_prompts_leave_when_dequeued_without_shifting_tool_results() {
        let mut c = conv(&[
            r#"{"type":"queue-operation","operation":"enqueue","content":"예약"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#,
            r#"{"type":"queue-operation","operation":"dequeue"}"#,
        ]);
        assert_eq!(c.items[0], Item::Gone);
        assert!(c.apply(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#));
        assert!(matches!(&c.items[1], Item::Tool { summary, result: Some(r), .. } if summary == "b.rs" && r == "ok"));
        assert_eq!(group_rows(&c.items), vec![Row::Tools(vec![1])]);
    }

    #[test]
    fn a_different_session_asks_for_a_fresh_read() {
        let mut c = conv(&[r#"{"type":"user","sessionId":"a","message":{"content":"하나"}}"#]);
        assert!(!c.apply(r#"{"type":"user","sessionId":"b","message":{"content":"둘"}}"#));
        assert_eq!(c.items.len(), 1);
    }

    #[test]
    fn answered_questions_wait_for_their_answer() {
        let mut c = conv(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"q","name":"AskUserQuestion","input":{"questions":[{"question":"색은?"}]}}]}}"#,
        ]);
        assert!(group_rows(&c.items).is_empty());
        c.apply(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"q","content":"User answered: \"색은?\"=\"파랑\"."}]}}"#);
        assert_eq!(c.items[0], Item::Answered { questions: vec!["색은?".into()], pairs: Some(vec![("색은?".into(), "파랑".into())]) });
    }

    #[test]
    fn codex_rollouts_share_the_same_rows() {
        let c = conv(&[
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"고쳐줘"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"text":"고쳐줘"}]}}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"c","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo test\"]}"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"{\"output\":\"fail\",\"metadata\":{\"exit_code\":1}}"}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"고쳤어요"}}"#,
        ]);
        assert_eq!(c.items.len(), 3);
        assert_eq!(c.items[1], Item::Tool { name: "shell".into(), summary: "cargo test".into(), result: Some("fail".into()), error: true });
    }

    #[test]
    fn codex_exec_code_shows_the_commands_inside() {
        let one = conv(&[r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"e","input":"const r = await tools.exec_command({cmd:\"kasaslk thread -w sionic 'https://x' | tail -n 120\",\"workdir\":\"/w\"}); text(r.output)"}}"#]);
        assert_eq!(one.items[0], Item::Tool { name: "shell".into(), summary: "kasaslk thread -w sionic 'https://x' | tail -n 120".into(), result: None, error: false });
        let many = conv(&[r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"e","input":"const rs = await Promise.all([\n  tools.exec_command({cmd:\"git pull --ff-only\"}),\n  tools.exec_command({cmd: 'ls -la'})\n]);"}}"#]);
        assert_eq!(many.items[0], Item::Tool { name: "shell".into(), summary: "git pull --ff-only 외 1개".into(), result: None, error: false });
        let other = conv(&[r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"e","input":"await tools.view_image({path:\"/a.png\"})"}}"#]);
        assert!(matches!(&other.items[0], Item::Tool { name, .. } if name == "exec"));
    }

    #[test]
    fn image_marks_and_tags_are_cleaned() {
        assert_eq!(strip_meta("[Image #1] 이거 봐\n[Image: source: /a.png]"), "(사진) 이거 봐");
        assert_eq!(tool_label("mcp__kasachrome__browser_click"), "browser_click");
        assert!(is_injection("## Context Usage\n..."));
        assert!(!is_injection("그냥 말"));
    }

    #[test]
    fn a_prompt_the_mod_submitted_shows_only_what_the_person_wrote() {
        let raw = "The kasaterm-bridge plugin sent a message:\nUse the Bash tool to run exactly: touch y1.txt\n\nThis is how Claude Code surfaces a prompt a plugin submits between turns — it starts this turn in the user's place. Address the message above.";
        assert_eq!(strip_meta(raw), "Use the Bash tool to run exactly: touch y1.txt");
        assert_eq!(strip_meta("The plan plugin sent a message: is a sentence"), "is a sentence");
        assert_eq!(strip_meta("The big plan plugin sent a message: x"), "The big plan plugin sent a message: x");
    }

    #[test]
    fn prompt_menu_needs_a_live_cursor() {
        let screen = |rows: &[&str]| rows.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let menu = parse_prompt_menu(&screen(&[
            "Do you want to proceed?",
            "❯ 1. Yes",
            "  2. Yes, and don't ask again  설명",
            "  3. No",
        ]))
        .unwrap();
        assert_eq!(menu.title, "Do you want to proceed?");
        assert_eq!(menu.options.len(), 3);
        assert_eq!(menu.options[1].label, "Yes, and don't ask again");
        assert_eq!(menu.cursor(), 0);
        assert!(parse_prompt_menu(&screen(&["1. 첫째", "2. 둘째"])).is_none());
    }

    /// Claude Code 2.1.291·codex 0.160.1 의 실제 창(100칸)에서 뜬 줄.
    #[test]
    fn real_approval_windows_keep_the_tool_input_and_every_choice_word_for_word() {
        let screen = |rows: &[&str]| rows.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let rule = "─".repeat(100);
        let dash = "╌".repeat(100);
        let bash = parse_prompt_menu(&screen(&[
            "⏺ Removing nothing-here.txt file with force flag",
            &rule,
            " Bash command",
            "",
            "   rm -f nothing-here.txt",
            "   Remove nothing-here.txt file with force flag",
            "",
            &dash,
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and always allow access to /private/tmp/yuzu/tui/work from this project",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]))
        .unwrap();
        assert_eq!(bash.title, "Do you want to proceed?");
        assert_eq!(bash.context, ["Bash command", "rm -f nothing-here.txt", "Remove nothing-here.txt file with force flag"]);
        let labels: Vec<&str> = bash.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Yes", "Yes, and always allow access to /private/tmp/yuzu/tui/work from this project", "No"]);
        assert!(bash.options.iter().all(|o| o.note.is_empty()));

        let write = parse_prompt_menu(&screen(&[
            &rule,
            " Create file",
            " note.txt",
            &dash,
            "  1 hi",
            &dash,
            " Do you want to create note.txt?",
            " ❯ 1. Yes",
            "   2. Yes, and switch to accept edits (auto-approve file edits and common file commands) for this",
            "      session (shift+tab)",
            "   3. No",
        ]))
        .unwrap();
        assert_eq!(write.options[1].label, "Yes, and switch to accept edits (auto-approve file edits and common file commands) for this session (shift+tab)");
        assert_eq!(write.context, ["Create file", "note.txt", "1 hi"]);

        let question = parse_prompt_menu(&screen(&[
            &rule,
            " ☐ 색상 선택",
            "",
            "어떤 색을 선호하나요?",
            "",
            "❯ 1. 빨강",
            "     밝고 활기찬 빨간색",
            "  2. 파랑",
            "     침착하고 안정적인 파란색",
            "  3. Type something.",
            &rule,
            "  4. Chat about this",
            "",
            "Enter to select · ↑/↓ to navigate · Esc to cancel",
        ]))
        .unwrap();
        assert_eq!(question.title, "어떤 색을 선호하나요?");
        assert_eq!(question.context, ["☐ 색상 선택"]);
        assert_eq!(question.options.len(), 4);
        assert_eq!(question.options[0].note, "밝고 활기찬 빨간색");
        assert_eq!(question.options[2].note, "");

        let codex = parse_prompt_menu(&screen(&[
            "• Running touch hello.txt",
            "",
            "  Would you like to run the following command?",
            "",
            "  Environment: local",
            "",
            "  Reason: Allow me to create hello.txt in the current workspace?",
            "",
            "  $ touch hello.txt",
            "",
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again for commands that start with `touch hello.txt` (p)",
            "  3. No, and tell Codex what to do differently (esc)",
            "",
            "  Press enter to confirm or esc to cancel",
        ]))
        .unwrap();
        assert_eq!(codex.title, "$ touch hello.txt");
        assert_eq!(codex.context, ["Would you like to run the following command?", "Environment: local", "Reason: Allow me to create hello.txt in the current workspace?"]);
        assert_eq!(codex.options[2].label, "No, and tell Codex what to do differently (esc)");
    }
}
