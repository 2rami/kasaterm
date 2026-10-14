use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use super::TurnState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnSignal {
    Working,
    Completed(u64),
    Stopped,
}

impl TurnSignal {
    pub fn state(self) -> TurnState {
        match self {
            Self::Working => TurnState::Working,
            Self::Completed(_) | Self::Stopped => TurnState::Idle,
        }
    }

    pub fn completion(self) -> Option<u64> {
        match self {
            Self::Completed(stamp) => Some(stamp),
            _ => None,
        }
    }
}

fn stamp(line: &str) -> u64 {
    // This is an in-process notification identity, never an authentication digest.
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hash);
    hash.finish()
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

pub fn turn_signal_from_tail(tail: &str) -> Option<TurnSignal> {
    let mut codex_activity = false;
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let kind = v["type"].as_str().unwrap_or("");
        if v["payload"].is_object() {
            let p = &v["payload"];
            match (kind, p["type"].as_str().unwrap_or("")) {
                ("event_msg", "task_started") => return Some(TurnSignal::Working),
                ("event_msg", "task_complete") => return Some(TurnSignal::Completed(stamp(line))),
                ("event_msg", "turn_aborted") | ("session_meta", _) => return Some(TurnSignal::Stopped),
                ("response_item", _) => codex_activity = true,
                ("event_msg", "token_count" | "thread_settings_applied") => {}
                ("event_msg", _) => codex_activity = true,
                _ => {}
            }
            continue;
        }
        let Some(m) = v.get("message") else { continue };
        match kind {
            "assistant" => return Some(match m["stop_reason"].as_str() {
                Some("tool_use") | None => TurnSignal::Working,
                Some("end_turn") if v["isApiErrorMessage"] != true => TurnSignal::Completed(stamp(line)),
                _ => TurnSignal::Stopped,
            }),
            "user" => {
                if v["isMeta"] == true { continue; }
                let text = content_text(&m["content"]);
                if text.contains("<command-name>") || text.contains("<local-command-stdout>") { continue; }
                return Some(if text.trim_start().starts_with("[Request interrupted") {
                    TurnSignal::Stopped
                } else { TurnSignal::Working });
            }
            _ => {}
        }
    }
    codex_activity.then_some(TurnSignal::Working)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    pub id: String,
    pub prompt: String,
    pub blocking: bool,
}

#[derive(Clone)]
struct Asked {
    id: String,
    prompts: Vec<String>,
    answered: BTreeSet<usize>,
    blocking: bool,
    accepted: bool,
}

#[derive(Clone, Default)]
pub struct Questions {
    asked: Vec<Asked>,
}

impl Questions {
    pub fn pending(&self) -> Vec<PendingQuestion> {
        self.asked.iter().filter(|q| q.accepted).map(|q| PendingQuestion {
            id: q.id.clone(),
            prompt: q.prompts.iter().enumerate().filter(|(i, _)| !q.answered.contains(i))
                .map(|(_, p)| p.as_str()).collect::<Vec<_>>().join(" · ").chars().take(240).collect(),
            blocking: q.blocking,
        }).collect()
    }

    pub fn update_tail(&mut self, tail: &str) {
        // Replaying the bounded tail is idempotent. Keeping outstanding calls lets
        // an async question survive a long-running tool pushing it outside the tail.
        for line in tail.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let p = &v["payload"];
            match (v["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or("")) {
                ("response_item", "function_call") => {
                    let name = p["name"].as_str().unwrap_or("").rsplit('.').next().unwrap_or("");
                    if !matches!(name, "request_user_input" | "request_user_input_async") { continue; }
                    let Some(id) = p["call_id"].as_str().filter(|id| !id.is_empty()) else { continue };
                    if self.asked.iter().any(|q| q.id == id) { continue; }
                    let args = p["arguments"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| p["arguments"].clone());
                    let Some(questions) = args["questions"].as_array() else { continue };
                    if questions.is_empty() || questions.len() > 64 { continue; }
                    let prompts = questions.iter().map(|q| {
                        q["question"].as_str().or_else(|| q["title"].as_str()).unwrap_or("답을 기다려요")
                            .chars().take(240).collect()
                    }).collect();
                    if self.asked.len() == 64 { self.asked.remove(0); }
                    let blocking = name == "request_user_input";
                    self.asked.push(Asked { id: id.into(), prompts, answered: BTreeSet::new(), blocking, accepted: blocking });
                }
                ("response_item", "function_call_output") => {
                    let Some(index) = self.asked.iter().position(|q| p["call_id"] == q.id) else { continue };
                    let result = p["output"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| p["output"].clone());
                    if !self.asked[index].blocking && result["accepted"] == true {
                        self.asked[index].accepted = true;
                    } else {
                        self.asked.remove(index);
                    }
                }
                ("event_msg", "turn_aborted") => self.asked.clear(),
                ("event_msg", "task_complete") => self.asked.retain(|q| !q.blocking),
                ("event_msg", "user_message") => self.user_turn(&content_text(&p["message"])),
                ("response_item", "message") if p["role"] == "user" => self.user_turn(&content_text(&p["content"])),
                _ => {}
            }
        }
    }

    // A person who types an ordinary message instead of answering the question card has moved
    // on, and Codex stops showing the card — keeping the async question would leave the pane
    // "waiting for an answer" forever (2026-10-15: a Codex pane sat orange over a question asked
    // turns earlier, with no card left to open). Messages Codex injects itself (`<…>` blocks,
    // the reply envelope included) are not that.
    fn user_turn(&mut self, text: &str) {
        let text = text.trim();
        if text.starts_with('<') {
            self.answer_envelope(text);
        } else if !text.is_empty() {
            self.asked.retain(|q| q.blocking);
        }
    }

    fn answer_envelope(&mut self, text: &str) {
        let Some(json) = text.trim().strip_prefix("<send_user_message_question_reply>")
            .and_then(|s| s.trim().strip_suffix("</send_user_message_question_reply>")) else { return };
        let Ok(replies) = serde_json::from_str::<Vec<Value>>(json) else { return };
        for reply in replies {
            let Some(item) = reply["questionItemId"].as_str().and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok()) else { continue };
            if item.len() != 3 || item[0] != "request_user_input_async" { continue; }
            let Some(index) = item[2].as_u64().and_then(|i| usize::try_from(i).ok()) else { continue };
            if let Some(q) = self.asked.iter_mut().find(|q| item[1] == q.id) {
                if index < q.prompts.len() { q.answered.insert(index); }
            }
        }
        self.asked.retain(|q| q.answered.len() < q.prompts.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, asynchronous: bool) -> String {
        json!({"type":"response_item","payload":{"type":"function_call","call_id":id,
            "name":if asynchronous { "request_user_input_async" } else { "request_user_input" },
            "arguments":json!({"questions":[{"title":"어느 기기인가요?"},{"question":"어떤 셸인가요?"}]}).to_string()}}).to_string()
    }
    fn output(id: &str, value: Value) -> String {
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":id,"output":value.to_string()}}).to_string()
    }
    fn reply(id: &str, index: usize) -> String {
        let body = json!([{"questionItemId":json!(["request_user_input_async",id,index]).to_string(),"answer":"선택"}]);
        json!({"type":"event_msg","payload":{"type":"user_message","message":format!("<send_user_message_question_reply>\n{body}\n</send_user_message_question_reply>")}}).to_string()
    }

    #[test]
    fn a_closed_turn_is_not_necessarily_a_completed_response() {
        for line in [
            json!({"type":"event_msg","payload":{"type":"turn_aborted"}}),
            json!({"type":"session_meta","payload":{}}),
            json!({"type":"user","message":{"content":"[Request interrupted by user]"}}),
            json!({"type":"assistant","message":{"stop_reason":"max_tokens"}}),
            json!({"type":"assistant","isApiErrorMessage":true,"message":{"stop_reason":"end_turn"}}),
        ] {
            assert_eq!(turn_signal_from_tail(&line.to_string()), Some(TurnSignal::Stopped));
        }
        for line in [json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"1"}}),
            json!({"type":"assistant","uuid":"1","message":{"stop_reason":"end_turn"}})] {
            assert!(matches!(turn_signal_from_tail(&line.to_string()), Some(TurnSignal::Completed(_))));
        }
    }

    #[test]
    fn metadata_cannot_create_a_second_completion() {
        let done = r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"1"}}"#;
        let later = format!("{done}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\"}}}}\n{{\"type\":");
        assert_eq!(turn_signal_from_tail(done), turn_signal_from_tail(&later));
    }

    #[test]
    fn blocking_questions_end_only_with_their_own_result() {
        let mut questions = Questions::default();
        questions.update_tail(&call("a", false));
        assert_eq!(questions.pending().len(), 1);
        assert!(questions.pending()[0].blocking);
        questions.update_tail(&output("another", json!({"answers":{}})));
        assert_eq!(questions.pending().len(), 1);
        questions.update_tail(&output("a", json!({"answers":{}})));
        assert!(questions.pending().is_empty());
    }

    #[test]
    fn async_acceptance_is_not_an_answer_and_survives_completion_and_tail_rollover() {
        let mut questions = Questions::default();
        questions.update_tail(&call("a", true));
        assert!(questions.pending().is_empty());
        questions.update_tail(&output("a", json!({"accepted":true})));
        assert_eq!(questions.pending().len(), 1);
        assert!(!questions.pending()[0].blocking);
        questions.update_tail(r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"1"}}"#);
        questions.update_tail(r#"{"type":"event_msg","payload":{"type":"token_count"}}"#);
        assert_eq!(questions.pending().len(), 1);
        questions.update_tail(&reply("another", 0));
        assert_eq!(questions.pending().len(), 1);
        questions.update_tail(&reply("a", 0));
        assert_eq!(questions.pending()[0].prompt, "어떤 셸인가요?");
        questions.update_tail(&reply("a", 1));
        assert!(questions.pending().is_empty());
    }

    #[test]
    fn an_ordinary_message_moves_past_open_async_questions() {
        let typed = |text: &str| json!({"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":text}]}}).to_string();
        let mut questions = Questions::default();
        questions.update_tail(&[call("a", true), output("a", json!({"accepted":true}))].join("\n"));
        questions.update_tail(r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"1"}}"#);
        questions.update_tail(&typed("<environment_context>\n  <cwd>/tmp</cwd>\n</environment_context>"));
        assert_eq!(questions.pending().len(), 1, "an injected context block is not the person moving on");
        questions.update_tail(&call("b", false));
        questions.update_tail(&typed("다른 일 해 줘"));
        assert_eq!(questions.pending().len(), 1, "only the async question is dropped");
        assert!(questions.pending()[0].blocking);
    }

    #[test]
    fn replayed_tails_and_rejected_async_calls_do_not_resurrect_questions() {
        let tail = [call("a", true), output("a", json!({"accepted":true})), reply("a", 0), reply("a", 1)].join("\n");
        let mut questions = Questions::default();
        for _ in 0..3 { questions.update_tail(&tail); assert!(questions.pending().is_empty()); }
        questions.update_tail(&[call("b", true), output("b", json!({"error":"unsupported"}))].join("\n"));
        assert!(questions.pending().is_empty());
        questions.update_tail(&call("c", false));
        questions.update_tail(r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}"#);
        assert!(questions.pending().is_empty());
    }
}
