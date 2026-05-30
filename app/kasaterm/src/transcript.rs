//! claude-code transcript(jsonl) → 협업 board 활동 추출.
//!
//! claude code는 세션을 `~/.claude/projects/<cwd>/<session>.jsonl`에
//! 실시간으로 append한다. `type:"assistant"` 줄의 `message.content[]`에
//! `tool_use`(name+input) 블록이 들어있어, 그걸 읽으면 그 pane이 "지금
//! 무슨 파일을 읽고/고치고, 무슨 명령을 돌리는지"를 — claude가 announce를
//! 직접 호출하지 않아도 — 알 수 있다. thinking 블록은 항상 redact(빈
//! 문자열)이라 무시한다.
//!
//! 이 모듈은 순수 파싱 함수만 둔다. 파일 tail/상태는 socket.rs의 watcher가
//! 들고 이 함수들을 호출한다.

use agent_socket::backend::PaneActivity;
use std::collections::VecDeque;

/// board의 intent에 보여줄 최근 도구 사용 개수.
pub const RECENT_MAX: usize = 4;

/// transcript에서 뽑은 한 번의 도구 사용.
#[derive(Clone, Debug)]
pub struct ToolEvent {
    /// intent 표시용 라벨, 예: "Read auth.ts", "Bash cargo build".
    pub label: String,
    /// 충돌 신호용 파일 절대경로. Edit/Write 계열(=실제로 고치는 중)만
    /// `Some` — Read/Grep은 intent엔 보이되 "claimed"는 아니므로 `None`.
    pub file: Option<String>,
}

/// jsonl 한 줄에서 tool_use 이벤트들을 뽑는다. assistant 줄이 아니거나
/// 파싱 실패면 빈 Vec(불완전한 마지막 줄도 안전).
pub fn parse_line(line: &str) -> Vec<ToolEvent> {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }
    let content = match v.pointer("/message/content").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return Vec::new(),
    };
    content
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .map(|b| {
            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
            tool_event(name, b.get("input"))
        })
        .collect()
}

fn tool_event(name: &str, input: Option<&serde_json::Value>) -> ToolEvent {
    let get = |k: &str| input.and_then(|i| i.get(k)).and_then(|v| v.as_str());
    match name {
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = get("file_path").or_else(|| get("notebook_path")).unwrap_or("");
            let is_edit = matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit");
            ToolEvent {
                label: format!("{name} {}", basename(path)),
                file: (is_edit && !path.is_empty()).then(|| path.to_string()),
            }
        }
        "Bash" => {
            let cmd = get("command").unwrap_or("");
            ToolEvent {
                label: format!("Bash {}", cmd.chars().take(40).collect::<String>()),
                file: None,
            }
        }
        "Grep" | "Glob" => ToolEvent {
            label: format!("{name} {}", get("pattern").or_else(|| get("path")).unwrap_or("")),
            file: None,
        },
        other => ToolEvent { label: other.to_string(), file: None },
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 최근 도구 사용 링버퍼 + idle 여부 → PaneActivity.
/// - intent: 최근 도구들을 시간순으로 `"Read auth.ts → Bash cargo build"`.
/// - files: Edit/Write 대상(충돌 신호), 중복 제거.
/// - status: idle | building(빌드/테스트 류 Bash가 최근) | working.
pub fn build_activity(surface_id: &str, recent: &VecDeque<ToolEvent>, idle: bool) -> PaneActivity {
    let intent = recent
        .iter()
        .map(|e| e.label.as_str())
        .collect::<Vec<_>>()
        .join(" → ");
    let mut files: Vec<String> = Vec::new();
    for e in recent {
        if let Some(f) = &e.file {
            if !files.contains(f) {
                files.push(f.clone());
            }
        }
    }
    let status = if idle {
        "idle"
    } else if recent.back().map_or(false, |e| {
        let l = e.label.to_lowercase();
        l.starts_with("bash")
            && (l.contains("build") || l.contains("test") || l.contains("cargo") || l.contains("compile"))
    }) {
        "building"
    } else {
        "working"
    };
    PaneActivity {
        surface_id: surface_id.to_string(),
        intent: if intent.is_empty() { "active".into() } else { intent },
        status: status.into(),
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_label_no_file_claim() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/b/auth.ts"}}]}}"#;
        let ev = parse_line(line);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].label, "Read auth.ts");
        assert_eq!(ev[0].file, None, "Read는 충돌 신호 아님");
    }

    #[test]
    fn edit_marks_file_claim() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/auth.ts"}}]}}"#;
        let ev = parse_line(line);
        assert_eq!(ev[0].file.as_deref(), Some("/a/auth.ts"));
        assert_eq!(ev[0].label, "Edit auth.ts");
    }

    #[test]
    fn bash_label_truncated() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build --release -p kasaterm"}}]}}"#;
        let ev = parse_line(line);
        assert!(ev[0].label.starts_with("Bash cargo build"));
        assert!(ev[0].label.len() <= "Bash ".len() + 40);
    }

    #[test]
    fn non_assistant_and_garbage_ignored() {
        assert!(parse_line(r#"{"type":"user","message":{"content":[]}}"#).is_empty());
        assert!(parse_line("not json").is_empty());
        assert!(parse_line("").is_empty());
    }

    #[test]
    fn multiple_tool_uses_in_one_line() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Read","input":{"file_path":"/x.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(parse_line(line).len(), 2);
    }

    #[test]
    fn activity_intent_files_status() {
        let mut r = VecDeque::new();
        r.push_back(ToolEvent { label: "Read auth.ts".into(), file: None });
        r.push_back(ToolEvent { label: "Edit auth.ts".into(), file: Some("/a/auth.ts".into()) });
        r.push_back(ToolEvent { label: "Bash cargo build".into(), file: None });
        let a = build_activity("%1", &r, false);
        assert_eq!(a.intent, "Read auth.ts → Edit auth.ts → Bash cargo build");
        assert_eq!(a.files, vec!["/a/auth.ts"]);
        assert_eq!(a.status, "building");
        let idle = build_activity("%1", &r, true);
        assert_eq!(idle.status, "idle");
    }
}
