//! 마지막 턴에서 사람을 부를 때 쓰는 재료 — 스스로 깨어날 예약, 학생이 마지막에 한 말 전문, 승인을 기다리는 도구.
//! 알림 판정·말(`crate::ring`)이 쓴다. 꼬리는 최신 것부터 읽는다.

use serde_json::Value;

fn lines_newest_first(tail: &str) -> impl Iterator<Item = Value> + '_ {
    tail.lines().rev().filter_map(|line| serde_json::from_str(line).ok())
}

fn blocks(v: &Value) -> &[Value] {
    v.pointer("/message/content").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// 사람(또는 하네스가 사람 자리에서) 넣은 말 — 턴의 시작이다. 도구 결과만 든 줄은 턴 안이다.
fn opens_turn(v: &Value) -> bool {
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    match v.pointer("/message/content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(items)) => items.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("text")),
        _ => false,
    }
}

/// 마지막 턴이 스스로 깨어날 예약을 걸고 끝났나 — `ScheduleWakeup`(멈춤이 아닌 것)·`CronCreate`. 깨어나면 새 턴이
/// 서므로 예약은 그 턴 안에서만 본다. 예전 턴의 예약까지 보면 예약을 다 쓴 뒤의 진짜 끝을 못 울린다.
pub fn continues(tail: &str) -> bool {
    for v in lines_newest_first(tail) {
        if opens_turn(&v) {
            return false;
        }
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let scheduled = blocks(&v).iter().any(|b| {
            let name = b.get("name").and_then(Value::as_str);
            b.get("type").and_then(Value::as_str) == Some("tool_use")
                && match name {
                    Some("ScheduleWakeup") => b.pointer("/input/stop").and_then(Value::as_bool) != Some(true),
                    Some("CronCreate") => true,
                    _ => false,
                }
        });
        if scheduled {
            return true;
        }
    }
    false
}

/// 학생이 마지막에 한 말 전문(Claude 의 마지막 글 덩이들, Codex 의 `agent_message`). 판의 `last_reply` 는 120자로
/// 잘려 첫 문장이 끊기므로 알림은 이것을 쓴다.
pub fn last_words(tail: &str) -> Option<String> {
    for v in lines_newest_first(tail) {
        if let Some(p) = v.get("payload").filter(|_| v.get("type").and_then(Value::as_str) == Some("event_msg")) {
            if p.get("type").and_then(Value::as_str) == Some("agent_message") {
                let said = p.get("message").and_then(Value::as_str).unwrap_or("").trim();
                if !said.is_empty() {
                    return Some(said.to_string());
                }
            }
            continue;
        }
        if opens_turn(&v) {
            return None;
        }
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let said: Vec<&str> = blocks(&v)
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        if !said.is_empty() {
            return Some(said.join("\n"));
        }
    }
    None
}

/// 결과가 아직 없는 마지막 도구 호출 — 승인을 기다리는 칸이면 그것이 승인을 구하는 일이다. (도구 이름, 입력).
pub fn pending_tool(tail: &str) -> Option<(String, Value)> {
    let mut answered = std::collections::HashSet::new();
    for v in lines_newest_first(tail) {
        for b in blocks(&v) {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                        answered.insert(id.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                    if !answered.contains(id) {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        return Some((name, b.get("input").cloned().unwrap_or(Value::Null)));
                    }
                }
                _ => {}
            }
        }
        if opens_turn(&v) {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
    }

    fn said(text: &str) -> String {
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}).to_string()
    }

    fn tool(id: &str, name: &str, input: Value) -> String {
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
            .to_string()
    }

    fn result(id: &str) -> String {
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
            .to_string()
    }

    #[test]
    fn a_wakeup_in_the_last_turn_means_the_student_will_continue() {
        let wake = tool("t1", "ScheduleWakeup", serde_json::json!({"delaySeconds": 600, "prompt": "/loop"}));
        let tail = [prompt("돌려 줘"), wake.clone(), result("t1"), said("10분 뒤에 볼게요.")].join("\n");
        assert!(continues(&tail));
        let stop = tool("t2", "ScheduleWakeup", serde_json::json!({"stop": true}));
        assert!(!continues(&[prompt("그만"), stop, result("t2"), said("멈췄어요.")].join("\n")));
        let woke = [prompt("돌려 줘"), wake, result("t1"), said("10분 뒤에 볼게요."), prompt("/loop"), said("다 됐어요.")].join("\n");
        assert!(!continues(&woke), "깨어난 턴은 예약을 다 쓴 것이다");
        let cron = tool("t3", "CronCreate", serde_json::json!({"cron": "*/5 * * * *"}));
        assert!(continues(&[prompt("5분마다"), cron, result("t3")].join("\n")));
    }

    #[test]
    fn last_words_are_the_whole_final_message() {
        let long = "가".repeat(300);
        let tail = [prompt("해 줘"), said("먼저 볼게요."), tool("t1", "Bash", serde_json::json!({})), result("t1"), said(&long)].join("\n");
        assert_eq!(last_words(&tail).as_deref(), Some(long.as_str()));
        assert_eq!(last_words(&[said("지난 턴"), prompt("새 일")].join("\n")), None, "이번 턴에 한 말이 아직 없다");
        let codex = serde_json::json!({"timestamp": "t", "type": "event_msg", "payload": {"type": "agent_message", "message": "끝냈어요"}});
        assert_eq!(last_words(&codex.to_string()).as_deref(), Some("끝냈어요"));
    }

    #[test]
    fn the_pending_tool_is_the_last_call_without_a_result() {
        let tail = [
            prompt("고쳐 줘"),
            tool("t1", "Read", serde_json::json!({"file_path": "/a"})),
            result("t1"),
            tool("t2", "Bash", serde_json::json!({"command": "cargo test"})),
        ]
        .join("\n");
        let (name, input) = pending_tool(&tail).unwrap();
        assert_eq!((name.as_str(), input["command"].as_str()), ("Bash", Some("cargo test")));
        assert!(pending_tool(&[prompt("x"), tool("t1", "Read", serde_json::json!({})), result("t1")].join("\n")).is_none());
    }

    fn notice(id: &str, extra: &str) -> String {
        let text = format!("<task-notification>\n<task-id>{id}</task-id>\n<summary>s</summary>\n{extra}</task-notification>");
        prompt(&text)
    }

    fn launched(id: &str, text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": text}]}})
            .to_string()
    }

    #[test]
    fn a_background_subagent_and_a_live_monitor_keep_the_pane_busy() {
        let agent = tool("t1", "Agent", serde_json::json!({"description": "조사", "run_in_background": true}));
        let start = [prompt("조사해 줘"), agent, launched("t1", "Async agent launched successfully.\nagentId: a25d14 (internal)")];
        let busy = crate::transcript::snapshot_from_tail("%1", &start.join("\n"), true);
        assert_eq!(busy.background, vec!["조사".to_string()], "띄웠다는 결과가 와도 끝난 게 아니다");
        let done = [start.join("\n"), notice("a25d14", "")].join("\n");
        assert!(crate::transcript::snapshot_from_tail("%1", &done, true).background.is_empty());

        let monitor = tool("t2", "Monitor", serde_json::json!({"command": "tail -f log", "description": "로그"}));
        let watch = [prompt("지켜봐"), monitor, launched("t2", "Monitor started (task bx1, persistent)")].join("\n");
        let evented = [watch.clone(), notice("bx1", "<event>line</event>\n")].join("\n");
        assert_eq!(crate::transcript::snapshot_from_tail("%1", &evented, true).background, vec!["로그".to_string()]);
        let ended = [watch, notice("bx1", "<status>killed</status>\n")].join("\n");
        assert!(crate::transcript::snapshot_from_tail("%1", &ended, true).background.is_empty());
    }
}
