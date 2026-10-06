//! 대화 보기 위에 덧붙이는 연결 mod 의 지금 — 일 상태·도는 도구·열린 승인 요청. 기록 파일은 메시지가
//! 끝나야 써지니 「무엇을 하는 중」과 「무엇을 묻는 중」은 여기서 받는다. 원본이 이 기기면 바로 읽고,
//! 거울이면 원본의 `/term/mod-live` 에 매달린다(`docs/mirror-render.md`).

use std::sync::{Mutex, Weak};

use serde_json::{json, Value};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Permission {
    pub(crate) id: String,
    pub(crate) tool: String,
    pub(crate) input: Value,
    pub(crate) preview: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModLive {
    pub(crate) live: bool,
    pub(crate) seq: u64,
    pub(crate) session: String,
    pub(crate) turn_open: bool,
    pub(crate) compacting: bool,
    pub(crate) question: bool,
    /// 도는 도구(도구, 이름표). 서브에이전트 것도 함께.
    pub(crate) tools: Vec<(String, String)>,
    /// 아직 답 없는 승인 요청. 자리에서 답한 것은 원본이 닫아 여기 안 온다. 도구 호출이 승인을 감싸므로
    /// 같은 id 의 도구가 `tools` 에 함께 있다.
    pub(crate) permissions: Vec<Permission>,
}

impl ModLive {
    pub(crate) fn from_json(v: &Value) -> Self {
        let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let tools: Vec<(String, String)> = v["tools"]
            .as_array()
            .map(|a| a.iter().map(|t| (text(t, "tool"), text(t, "label"))).collect())
            .unwrap_or_default();
        let permissions = v["permissions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| Permission {
                        id: text(p, "id"),
                        tool: text(p, "tool"),
                        input: p.get("input").cloned().unwrap_or(Value::Null),
                        preview: text(p, "preview"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            live: v["live"].as_bool().unwrap_or(false),
            seq: v["seq"].as_u64().unwrap_or(0),
            session: text(v, "session"),
            turn_open: v["turn_open"].as_bool().unwrap_or(false),
            compacting: v["compacting"].as_bool().unwrap_or(false),
            question: !v["question"].is_null() && v.get("question").is_some(),
            tools,
            permissions,
        }
    }

    /// 작업 줄에 붙일 지금 하는 일. mod 칸이 아니면 None — 그때는 옛 판정(「작업 중」)만 쓴다.
    pub(crate) fn doing(&self) -> Option<String> {
        if !self.live {
            return None;
        }
        if self.compacting {
            return Some("대화 압축 중".into());
        }
        if let Some(p) = self.permissions.first() {
            return Some(match p.tool.as_str() {
                "AskUserQuestion" => "답 기다림".into(),
                "ExitPlanMode" => "계획 승인 기다림".into(),
                tool => format!("승인 기다림 · {}", super::parse::tool_label(tool)),
            });
        }
        if self.question {
            return Some("답 기다림".into());
        }
        let (tool, label) = self.tools.last()?;
        let name = super::parse::tool_label(tool);
        Some(if label.is_empty() { name.to_string() } else { format!("{name} · {label}") })
    }
}

/// 화면 선택지 `i` 를 mod 결정으로 보낼 수 있으면 (허락인가, 그 요청). 승인 창의 맨 앞 「Yes」와 맨 끝 「No」만
/// 그렇다 — 나머지(「다시 묻지 않기」·계획 승인·질문)는 TUI 가 하는 일을 그대로 하도록 화면 키로 고른다.
pub(crate) fn decision_for<'a>(menu: &super::parse::PromptMenu, live: &'a ModLive, i: usize) -> Option<(bool, &'a Permission)> {
    let p = live.permissions.first().filter(|_| live.live)?;
    if matches!(p.tool.as_str(), "ExitPlanMode" | "AskUserQuestion") || menu.options.first()?.label != "Yes" {
        return None;
    }
    let label = menu.options.get(i)?.label.as_str();
    if i == 0 {
        return Some((true, p));
    }
    (i + 1 == menu.options.len() && (label == "No" || label.starts_with("No,"))).then_some((false, p))
}

/// 승인 요청의 입력 원문을 카드에 펼 줄로 — Bash 는 명령 전부, 파일 도구는 경로와 바뀔 내용, 그 밖엔 JSON.
pub(crate) fn input_lines(p: &Permission) -> Vec<String> {
    let s = |k: &str| p.input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut out: Vec<String> = Vec::new();
    match p.tool.as_str() {
        "Bash" => {
            out.extend(s("command").lines().map(str::to_string));
            let d = s("description");
            if !d.is_empty() {
                out.push(d);
            }
        }
        "Write" => {
            out.push(s("file_path"));
            out.extend(s("content").lines().map(str::to_string));
        }
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            out.push(s("file_path"));
            out.extend(s("old_string").lines().map(|l| format!("- {l}")));
            out.extend(s("new_string").lines().map(|l| format!("+ {l}")));
        }
        "ExitPlanMode" => out.extend(s("plan").lines().map(str::to_string)),
        _ => match &p.input {
            Value::Null => out.push(p.preview.clone()),
            v => out.extend(serde_json::to_string_pretty(v).unwrap_or_default().lines().map(str::to_string)),
        },
    }
    out.retain(|l| !l.trim().is_empty());
    out
}

/// 원본 기기의 mod 사실에 매달린다. 칸이 닫히면(`Weak` 가 풀리면) 끝난다. 원본이 이 창구를 모르면(옛 판) 그만둔다.
pub(crate) fn watch_remote(live: Weak<Mutex<ModLive>>, base: String, surface: String, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut seq: Option<u64> = None;
        loop {
            if live.strong_count() == 0 {
                return;
            }
            let wait = seq.map(|s| format!("&seq={s}&wait_ms=8000")).unwrap_or_default();
            let path = format!("/term/mod-live?surface={}{wait}", kasa_mcp::remote::urlencode(&surface));
            match kasa_mcp::remote::remote_get_json(&base, &path) {
                Ok(v) => {
                    let next = ModLive::from_json(&v);
                    seq = Some(next.seq);
                    let Some(live) = live.upgrade() else { return };
                    let changed = live.lock().map(|mut l| std::mem::replace(&mut *l, next.clone()) != next).unwrap_or(false);
                    if changed {
                        let _ = proxy.send_event(UserEvent::Redraw);
                    }
                }
                Err(e) if e.to_string().contains("404") => return,
                Err(_) => {
                    seq = None;
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        }
    });
}

/// 승인 요청에 답한다 — 원격 승인 계약과 같은 요청 id 로 그 칸의 mod 에 닿는다.
pub(crate) fn decide(remote: Option<&(String, String)>, surface: &str, session: &str, id: &str, allow: bool, message: &str) -> Result<(), String> {
    let decision = if allow { "allow" } else { "deny" };
    match remote {
        None => kasa_mcp::claude_mod::decide(id, surface, session, decision, message, "desk-chat").map_err(str::to_string),
        Some((base, rid)) => {
            let body = json!({"id": id, "surface": rid, "session": session, "decision": decision, "message": message, "by": "desk-mirror"});
            let v = kasa_mcp::remote::remote_post_json(base, "/term/mod-decide", &body).map_err(|e| e.to_string())?;
            if v["ok"].as_bool() == Some(true) {
                Ok(())
            } else {
                Err(v["error"].as_str().unwrap_or("unknown").to_string())
            }
        }
    }
}

/// 원본 기기의 `/transcript-raw` 한 조각 — (raw, 다음 offset, 처음부터인가). 묶인 기록이 없으면 None.
pub(crate) fn remote_chunk(base: &str, surface: &str, offset: u64) -> anyhow::Result<Option<(String, u64, bool)>> {
    let path = format!("/transcript-raw?surface={}&offset={offset}", kasa_mcp::remote::urlencode(surface));
    let v = kasa_mcp::remote::remote_get_json(base, &path)?;
    if v["ok"].as_bool() != Some(true) {
        return Ok(None);
    }
    Ok(Some((
        v["raw"].as_str().unwrap_or("").to_string(),
        v["offset"].as_u64().unwrap_or(offset),
        v["reset"].as_bool().unwrap_or(false),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_request_wins_the_working_line_and_spreads_its_raw_input() {
        // 도구 호출이 승인을 감싼다 — 묻는 동안 같은 id 의 도구가 함께 돈다(2.1.291 실측).
        let v = json!({
            "live": true, "seq": 4, "session": "s", "turn_open": true, "question": null,
            "tools": [{"id": "t2", "tool": "Write", "label": "y2.txt"}],
            "permissions": [{"id": "t2", "tool": "Write", "input": {"file_path": "/a", "content": "x\ny"}}],
        });
        let live = ModLive::from_json(&v);
        assert_eq!(live.permissions.len(), 1);
        assert!(!live.question);
        assert_eq!(live.doing().as_deref(), Some("승인 기다림 · Write"));
        assert_eq!(input_lines(&live.permissions[0]), ["/a", "x", "y"]);
        assert_eq!(ModLive::from_json(&json!({"live": false})).doing(), None);
    }
}
