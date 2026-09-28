//! What each local agent pane is working on, sent to the account workspace as a task.
//!
//! The request text is the last typed prompt in that pane's own transcript, read through the
//! same backend the board uses — never the board's 100-character preview. The pane identity is
//! checked before and after the read so a pane that was re-bound meanwhile cannot lend its
//! prompt to another request. States stop at "awaiting verification": completion needs trusted
//! check evidence, which this layer does not have and must not invent.

use super::*;
use kasa_socket::backend::{Backend as _, ConversationTurn};
use serde_json::{json, Value};
use std::time::Duration;

const OBSERVE_EVERY: Duration = Duration::from_secs(10);
const MAX_PANES_PER_TICK: usize = 4;
const MAX_PROMPT: usize = 16 * 1024;

pub(super) struct Observation {
    pub(super) request_id: String,
    pub(super) prompt: String,
    pub(super) work_revision: String,
    pub(super) state: &'static str,
    pub(super) step: String,
}

impl Observation {
    pub(super) fn body(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "request_id": self.request_id,
            "prompt": self.prompt,
            "work_revision": self.work_revision,
            "required_checks": [],
            "state": self.state,
            "step": self.step,
        }))
        .unwrap_or_default()
    }
}

/// Pure assembly of one observation. `known` says whether this request was already seen in
/// progress: an idle pane only moves a request we watched working into verification, so a
/// prompt that was answered long ago never becomes a task by itself.
pub(super) fn observation(
    before: &Value,
    after: &Value,
    turns: &[ConversationTurn],
    mood: &str,
    what: &str,
    known: &dyn Fn(&str) -> bool,
) -> Option<Observation> {
    if before != after {
        return None;
    }
    let field = |name: &str| after.get(name).and_then(Value::as_str).filter(|s| !s.is_empty());
    let (machine, surface_key, session) = (field("machine_id")?, field("surface_key")?, field("session_id")?);
    let prompt = turns.iter().rev().find(|turn| turn.role == "user")?.text.trim();
    if prompt.is_empty()
        || prompt.len() > MAX_PROMPT
        || prompt.chars().any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return None;
    }
    let prompt_hash = kasa_mcp::relay_auth::token_hash(prompt);
    let request_id = kasa_mcp::relay_auth::token_hash(&format!("{machine}\0{surface_key}\0{session}\0{prompt_hash}"));
    let work_revision = kasa_mcp::relay_auth::token_hash(&format!("{session}\0{prompt_hash}"));
    let (state, step) = match mood {
        "busy" => ("working", if what.is_empty() { "진행 중".to_string() } else { what.to_string() }),
        "wait" | "error" => ("blocked", if what.is_empty() { "답변 필요".to_string() } else { what.to_string() }),
        _ if known(&request_id) => ("awaiting_verification", "검증 근거 확인 중".to_string()),
        _ => return None,
    };
    Some(Observation { request_id, prompt: prompt.to_string(), work_revision, state, step: step.chars().take(200).collect() })
}

impl App {
    pub(crate) fn workspace_observe_tick(&mut self) {
        let Some(expected) = self.board_scene.chat.observe_ready(OBSERVE_EVERY) else {
            return;
        };
        let Some(backend) = self.socket_backend.clone() else {
            return;
        };
        let mut panes: Vec<String> = self.pane_activity.keys().cloned().collect();
        panes.sort();
        let mut sent = 0;
        for pane in panes {
            if sent >= MAX_PANES_PER_TICK {
                break;
            }
            if kasa_mcp::remote::is_remote_pane(&pane) {
                continue;
            }
            let Some(activity) = self.pane_activity.get(&pane) else {
                continue;
            };
            let (mood, what) = crate::chrome::pet_state_of(&activity.state, activity.bg_active, activity.compact_pct, &activity.intent);
            let Ok(before) = backend.collab_pane_identity(&pane) else {
                continue;
            };
            let Ok(turns) = backend.transcript_tail(&pane, 12) else {
                continue;
            };
            let Ok(after) = backend.collab_pane_identity(&pane) else {
                continue;
            };
            let chat = &self.board_scene.chat;
            let Some(found) = observation(&before, &after, &turns, mood, &what, &|id| chat.observe_known(id)) else {
                continue;
            };
            if !self.board_scene.chat.observe_changed(&found) {
                continue;
            }
            self.board_scene.chat.observe_send(expected.clone(), found.body());
            sent += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(session: &str) -> Value {
        json!({"machine_id":"m1","surface_key":"sk-1","surface_id":"%3","session_id":session,"instance_id":"i1"})
    }
    fn turn(role: &str, text: &str) -> ConversationTurn {
        ConversationTurn { role: role.into(), text: text.into() }
    }

    #[test]
    fn prompt_comes_from_the_transcript_and_identity_must_hold_across_the_read() {
        let turns = vec![turn("user", "브랜치 그래프를 그려 주세요"), turn("assistant", "그리는 중입니다")];
        let found = observation(&identity("s1"), &identity("s1"), &turns, "busy", "파일 읽는 중", &|_| false).unwrap();
        assert_eq!(found.prompt, "브랜치 그래프를 그려 주세요");
        assert_eq!((found.state, found.step.as_str()), ("working", "파일 읽는 중"));
        assert!(observation(&identity("s1"), &identity("s2"), &turns, "busy", "", &|_| false).is_none(), "pane re-bound during the read");
        assert!(observation(&identity(""), &identity(""), &turns, "busy", "", &|_| false).is_none(), "no session, no request identity");
        assert!(observation(&identity("s1"), &identity("s1"), &[turn("assistant", "답만 있음")], "busy", "", &|_| false).is_none());
        let long = "가".repeat(MAX_PROMPT / 3 + 1);
        assert!(observation(&identity("s1"), &identity("s1"), &[turn("user", &long)], "busy", "", &|_| false).is_none(), "oversized prompts are not truncated into a false original");
    }

    #[test]
    fn request_identity_follows_machine_surface_session_and_prompt_only() {
        let turns = vec![turn("user", "같은 요청")];
        let a = observation(&identity("s1"), &identity("s1"), &turns, "busy", "a", &|_| false).unwrap();
        let b = observation(&identity("s1"), &identity("s1"), &turns, "wait", "b", &|_| false).unwrap();
        assert_eq!(a.request_id, b.request_id, "state changes keep the request");
        let other_session = observation(&identity("s2"), &identity("s2"), &turns, "busy", "a", &|_| false).unwrap();
        assert_ne!(a.request_id, other_session.request_id);
        let other_prompt = observation(&identity("s1"), &identity("s1"), &[turn("user", "다른 요청")], "busy", "a", &|_| false).unwrap();
        assert_ne!(a.request_id, other_prompt.request_id);
        assert!(a.request_id.bytes().all(|b| b.is_ascii_hexdigit()) && a.request_id.len() <= 128);
    }

    #[test]
    fn idle_only_moves_a_watched_request_and_never_claims_completion() {
        let turns = vec![turn("user", "예전 요청")];
        assert!(observation(&identity("s1"), &identity("s1"), &turns, "idle", "", &|_| false).is_none());
        let found = observation(&identity("s1"), &identity("s1"), &turns, "idle", "", &|_| true).unwrap();
        assert_eq!(found.state, "awaiting_verification");
        assert!(!found.body().windows(8).any(|w| w == b"verified"));
        let blocked = observation(&identity("s1"), &identity("s1"), &turns, "error", "빌드 실패", &|_| false).unwrap();
        assert_eq!((blocked.state, blocked.step.as_str()), ("blocked", "빌드 실패"));
    }
}
