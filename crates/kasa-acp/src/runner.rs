//! 칸 안에서 ACP 학생 하나를 돌릴 때의 순수한 몫 — 이벤트를 화면 줄로 펴고, 턴의 결과를 모은다.
//!
//! 입출력은 실행 파일(`src/bin/kasa-acp.rs`)이 붙인다. 이 몫을 따로 둔 이유: 디스패처가 결과
//! 파일 하나로 수확을 판정하므로, 무엇을 학생의 「답」으로 볼지가 시험으로 고정돼 있어야 한다.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::event::{Event, PermissionRequest, PromptOutcome, ToolOutput, ToolStatus};

const OUTPUT_LINES: usize = 6;

/// 화면에 낼 것. 말 조각은 줄을 끊지 않고 이어 붙이고, 나머지는 제 줄을 차지한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Text(String),
    Block(String),
}

#[derive(Default)]
struct ToolSeen {
    kind: Option<String>,
    title: Option<String>,
    command: Option<String>,
    shown: bool,
    output: String,
    finished: bool,
}

impl ToolSeen {
    fn label(&self, id: &str) -> String {
        self.command.clone().or_else(|| self.title.clone()).unwrap_or_else(|| id.to_owned())
    }
}

/// 세션 하나의 화면·결과 상태. 컨텍스트 사용량은 턴을 넘어 이어지고, 말·도구는 턴마다 새로 센다.
#[derive(Default)]
pub struct SessionView {
    turn: u32,
    text: String,
    /// 마지막 도구 호출 뒤의 말. 학생이 일을 마치고 하는 보고가 여기 온다.
    tail: String,
    context: Option<(u64, u64)>,
    cost_usd: Option<f64>,
    tools: HashMap<String, ToolSeen>,
    /// 이 세션이 고친 파일. 디스패처가 같은 파일에 다른 학생을 붙이지 않는 근거라 턴을 넘어 쌓는다.
    files: BTreeSet<String>,
}

impl SessionView {
    pub fn begin_turn(&mut self) {
        self.turn += 1;
        self.text.clear();
        self.tail.clear();
        self.tools.clear();
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }

    pub fn observe(&mut self, ev: &Event) -> Vec<Line> {
        match ev {
            Event::Message { text, .. } => {
                self.text.push_str(text);
                self.tail.push_str(text);
                vec![Line::Text(text.clone())]
            }
            Event::ToolCall(call) => {
                self.tail.clear();
                let seen = self.tools.entry(call.id.clone()).or_default();
                seen.kind.clone_from(&call.kind);
                if is_edit(seen.kind.as_deref()) {
                    self.files.extend(call.locations.iter().cloned());
                }
                if !call.title.is_empty() {
                    seen.title = Some(call.title.clone());
                }
                if call.command.is_some() {
                    seen.command.clone_from(&call.command);
                }
                show_header(&call.id, seen).into_iter().collect()
            }
            Event::ToolCallUpdate(update) => {
                let seen = self.tools.entry(update.id.clone()).or_default();
                if is_edit(seen.kind.as_deref()) {
                    self.files.extend(update.locations.iter().cloned());
                }
                if update.title.is_some() {
                    seen.title.clone_from(&update.title);
                }
                if update.command.is_some() {
                    seen.command.clone_from(&update.command);
                }
                match &update.output {
                    Some(ToolOutput::Full(full)) => full.clone_into(&mut seen.output),
                    Some(ToolOutput::Delta(delta)) => seen.output.push_str(delta),
                    None => {}
                }
                let mut lines: Vec<Line> = show_header(&update.id, seen).into_iter().collect();
                let ended = matches!(update.status, Some(ToolStatus::Completed | ToolStatus::Failed));
                if ended && !seen.finished {
                    seen.finished = true;
                    if !seen.shown {
                        seen.shown = true;
                        lines.push(Line::Block(format!("▸ {}", seen.label(&update.id))));
                    }
                    lines.extend(output_snippet(&seen.output).into_iter().map(Line::Block));
                    if update.status == Some(ToolStatus::Failed) {
                        let code = update.exit_code.map(|c| format!(" (exit {c})")).unwrap_or_default();
                        lines.push(Line::Block(format!("  ! failed{code}")));
                    }
                }
                lines
            }
            Event::Plan { entries, .. } => {
                let mut out = String::from("plan:");
                for e in entries {
                    let mark = match e.status.as_str() {
                        "completed" => "x",
                        "in_progress" => "~",
                        _ => " ",
                    };
                    out.push_str(&format!("\n  [{mark}] {}", e.content));
                }
                vec![Line::Block(out)]
            }
            Event::Usage { used, size, cost_usd, .. } => {
                self.context = Some((*used, *size));
                if cost_usd.is_some() {
                    self.cost_usd = *cost_usd;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// 학생의 답 — 마지막 도구 호출 뒤의 말, 그게 비면 이번 턴의 말 전부.
    pub fn answer(&self) -> &str {
        let tail = self.tail.trim();
        if tail.is_empty() { self.text.trim() } else { tail }
    }

    pub fn context_percent(&self) -> Option<f64> {
        let (used, size) = self.context?;
        (size > 0).then(|| used as f64 * 100.0 / size as f64)
    }

    pub fn footer(&self, outcome: &PromptOutcome) -> String {
        let mut parts = vec![outcome.stop.as_str().to_owned()];
        if let Some(pct) = self.context_percent() {
            parts.push(format!("context {pct:.1}%"));
        }
        if let Some(cost) = self.cost_usd {
            parts.push(format!("${cost:.2}"));
        }
        format!("── {}", parts.join(" · "))
    }

    /// 디스패처가 읽는 상태. `state` 는 `working`(턴 도는 중)·`idle`(다음 지시 대기)·`exited`(runner 끝).
    /// `stop`·`answer` 는 마지막으로 끝난 턴의 것이다.
    pub fn status_json(&self, harness: &str, session_id: &str, state: &str, stop: Option<&str>, at: f64) -> Value {
        json!({
            "harness": harness,
            "session_id": session_id,
            "state": state,
            "turn": self.turn,
            "stop": stop,
            "answer": self.answer(),
            "files": self.files,
            "context_used": self.context.map(|c| c.0),
            "context_size": self.context.map(|c| c.1),
            "cost_usd": self.cost_usd,
            "at": at,
        })
    }
}

fn is_edit(kind: Option<&str>) -> bool {
    matches!(kind, Some("edit" | "delete" | "move"))
}

fn show_header(id: &str, seen: &mut ToolSeen) -> Option<Line> {
    if seen.shown || seen.command.is_none() {
        return None;
    }
    seen.shown = true;
    Some(Line::Block(format!("▸ {}", seen.label(id))))
}

fn output_snippet(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = lines.iter().take(OUTPUT_LINES).map(|l| format!("  │ {l}")).collect();
    if lines.len() > OUTPUT_LINES {
        out.push(format!("  │ … {} more lines", lines.len() - OUTPUT_LINES));
    }
    out
}

/// 사람에게 물을 글과, 번호(1부터) 순서의 선택지 id.
pub fn permission_prompt(req: &PermissionRequest) -> (String, Vec<String>) {
    let what = req.command.clone().or_else(|| req.title.clone()).unwrap_or_else(|| "tool call".into());
    let mut text = format!("? permission: {what}\n ");
    let mut ids = Vec::new();
    for (n, o) in req.options.iter().enumerate() {
        text.push_str(&format!(" [{}] {}", n + 1, o.name));
        ids.push(o.id.clone());
    }
    (text, ids)
}

/// 반쯤 쓴 파일을 디스패처가 읽지 않게 임시 파일에 쓰고 옮긴다.
pub fn write_status(path: &Path, status: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, format!("{status}\n"))?;
    std::fs::rename(&tmp, path)
}

/// inbox 파일에 놓인 다음 지시를 가져가고 파일을 지운다. 비었거나 없으면 `None`.
///
/// 지시를 칸 입력으로 넣지 않는 이유: 터미널의 정규 모드는 한 줄을 MAX_CANON(macOS 1024바이트)까지만
/// 받아 긴 한글 브리프가 잘리고, 여러 줄은 줄마다 다른 지시가 된다. 보내는 쪽은 임시 파일에 쓰고
/// 옮겨서(`write_status` 와 같은 꼴) 반쯤 쓴 지시를 읽히지 않는다.
pub fn take_inbox(path: &Path) -> io::Result<Option<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    std::fs::remove_file(path)?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::event::{PermissionKind, PermissionOption, StopReason, ToolCall, ToolCallUpdate};
    use crate::rpc::RequestId;

    fn msg(text: &str) -> Event {
        Event::Message { session_id: "s".into(), text: text.into() }
    }

    fn call(id: &str, title: &str, command: Option<&str>) -> Event {
        Event::ToolCall(ToolCall {
            session_id: "s".into(),
            id: id.into(),
            name: None,
            title: title.into(),
            kind: None,
            status: ToolStatus::Pending,
            command: command.map(Into::into),
            locations: Vec::new(),
        })
    }

    fn update(id: &str, status: Option<ToolStatus>, command: Option<&str>, output: Option<ToolOutput>) -> Event {
        Event::ToolCallUpdate(ToolCallUpdate {
            session_id: "s".into(),
            id: id.into(),
            status,
            title: None,
            command: command.map(Into::into),
            output,
            exit_code: None,
            locations: Vec::new(),
        })
    }

    fn blocks(lines: &[Line]) -> Vec<&str> {
        lines
            .iter()
            .filter_map(|l| match l {
                Line::Block(b) => Some(b.as_str()),
                Line::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn answer_is_what_the_student_said_after_the_last_tool() {
        let mut v = SessionView::default();
        v.begin_turn();
        v.observe(&msg("I'll create it. "));
        v.observe(&call("t1", "ls", Some("ls")));
        v.observe(&msg("Done: notes.txt has 1 line.\n"));
        assert_eq!(v.answer(), "Done: notes.txt has 1 line.");

        v.begin_turn();
        v.observe(&msg("Created notes.txt."));
        v.observe(&call("t2", "wc", Some("wc -l notes.txt")));
        assert_eq!(v.answer(), "Created notes.txt.", "falls back to the whole turn when nothing follows the tool");
    }

    #[test]
    fn codex_tool_header_once_then_delta_output_on_completion() {
        let mut v = SessionView::default();
        v.begin_turn();
        assert_eq!(blocks(&v.observe(&call("t1", "ls -la", Some("ls -la")))), ["▸ ls -la"]);
        assert!(v.observe(&update("t1", None, None, Some(ToolOutput::Delta("a\n".into())))).is_empty());
        let done = v.observe(&update("t1", Some(ToolStatus::Completed), None, Some(ToolOutput::Delta("b\n".into()))));
        assert_eq!(blocks(&done), ["  │ a", "  │ b"]);
        assert!(v.observe(&update("t1", Some(ToolStatus::Completed), None, None)).is_empty());
    }

    #[test]
    fn claude_placeholder_title_waits_for_the_command() {
        let mut v = SessionView::default();
        v.begin_turn();
        assert!(v.observe(&call("t1", "Terminal", None)).is_empty());
        assert_eq!(blocks(&v.observe(&update("t1", None, Some("wc -l notes.txt"), None))), ["▸ wc -l notes.txt"]);
        let done = v.observe(&update("t1", Some(ToolStatus::Completed), None, Some(ToolOutput::Full("1 notes.txt".into()))));
        assert_eq!(blocks(&done), ["  │ 1 notes.txt"]);
    }

    #[test]
    fn commandless_tool_gets_its_header_at_completion_and_failure_is_marked() {
        let mut v = SessionView::default();
        v.begin_turn();
        v.observe(&call("t1", "Write notes.txt", None));
        let Event::ToolCallUpdate(mut failed) = update("t1", Some(ToolStatus::Failed), None, None) else {
            unreachable!()
        };
        failed.exit_code = Some(2);
        assert_eq!(blocks(&v.observe(&Event::ToolCallUpdate(failed))), ["▸ Write notes.txt", "  ! failed (exit 2)"]);
    }

    #[test]
    fn long_output_is_cut() {
        let mut v = SessionView::default();
        v.begin_turn();
        let out: String = (1..=10).map(|n| format!("{n}\n")).collect();
        let lines = v.observe(&update("t", Some(ToolStatus::Completed), Some("seq 10"), Some(ToolOutput::Full(out))));
        let b = blocks(&lines);
        assert_eq!(b.first(), Some(&"▸ seq 10"));
        assert_eq!(b.len(), 1 + OUTPUT_LINES + 1);
        assert_eq!(b.last(), Some(&"  │ … 4 more lines"));
    }

    #[test]
    fn context_survives_turns_and_lands_in_the_outcome() {
        let mut v = SessionView::default();
        v.begin_turn();
        v.observe(&Event::Usage { session_id: "s".into(), used: 48_000, size: 1_000_000, cost_usd: Some(0.3) });
        v.observe(&Event::Usage { session_id: "s".into(), used: 48_001, size: 1_000_000, cost_usd: None });
        v.begin_turn();
        v.observe(&msg("ok"));
        let outcome = PromptOutcome {
            stop: StopReason::EndTurn,
            input_tokens: None,
            output_tokens: None,
            cached_read_tokens: None,
            total_tokens: None,
        };
        assert_eq!(v.footer(&outcome), "── end_turn · context 4.8% · $0.30");
        let j = v.status_json("claude", "sid", "idle", Some("end_turn"), 12.5);
        assert_eq!(
            j,
            json!({ "harness": "claude", "session_id": "sid", "state": "idle", "turn": 2, "stop": "end_turn",
                    "answer": "ok", "files": [], "context_used": 48001, "context_size": 1000000,
                    "cost_usd": 0.3, "at": 12.5 })
        );
    }

    #[test]
    fn permission_prompt_numbers_the_options() {
        let req = PermissionRequest {
            id: RequestId::for_test(json!(1)),
            session_id: "s".into(),
            tool_call_id: None,
            title: Some("Run rm".into()),
            command: Some("rm -rf build".into()),
            options: vec![
                PermissionOption { id: "a".into(), name: "Allow".into(), kind: PermissionKind::AllowOnce },
                PermissionOption { id: "r".into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
            ],
        };
        let (text, ids) = permission_prompt(&req);
        assert_eq!(text, "? permission: rm -rf build\n  [1] Allow [2] Reject");
        assert_eq!(ids, ["a", "r"]);
    }

    #[test]
    fn edited_files_accumulate_across_turns_but_reads_do_not_count() {
        let mut v = SessionView::default();
        v.begin_turn();
        let Event::ToolCall(mut write) = call("w1", "Write", None) else { unreachable!() };
        write.kind = Some("edit".into());
        v.observe(&Event::ToolCall(write));
        let Event::ToolCallUpdate(mut located) = update("w1", None, None, None) else { unreachable!() };
        located.locations = vec!["/w/a.rs".into()];
        v.observe(&Event::ToolCallUpdate(located));
        let Event::ToolCall(mut read) = call("r1", "Read", None) else { unreachable!() };
        read.kind = Some("read".into());
        read.locations = vec!["/w/b.rs".into()];
        v.observe(&Event::ToolCall(read));
        v.begin_turn();
        assert_eq!(v.status_json("c", "s", "working", None, 0.0)["files"], json!(["/w/a.rs"]));
    }

    #[test]
    fn inbox_is_taken_once() {
        let dir = std::env::temp_dir().join(format!("kasa-acp-inbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let inbox = dir.join("next.txt");
        assert_eq!(take_inbox(&inbox).expect("missing is fine"), None);
        std::fs::write(&inbox, "first line\nsecond line\n").expect("write");
        assert_eq!(take_inbox(&inbox).expect("read").as_deref(), Some("first line\nsecond line"));
        assert!(!inbox.exists());
        std::fs::write(&inbox, "  \n").expect("write");
        assert_eq!(take_inbox(&inbox).expect("blank"), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn status_file_is_replaced_whole() {
        let dir = std::env::temp_dir().join(format!("kasa-acp-status-{}", std::process::id()));
        let path = dir.join("nested/task.json");
        write_status(&path, &json!({ "turn": 1 })).expect("write");
        write_status(&path, &json!({ "turn": 2 })).expect("rewrite");
        let read: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(read["turn"], 2);
        assert!(!dir.join("nested/task.json.tmp").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
