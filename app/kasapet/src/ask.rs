//! 머리 위 유리 바가 묻는 곳 — 지금 보고 있는 창이든 모든 기기든 한 마디 묻고, 답은 말풍선으로 받는다.
//!
//! 대화창(`chat.rs`)과 갈라 둔 이유는 오가는 것의 모양이 다르기 때문이다. 저쪽은
//! 이어지는 대화라 기록을 쌓고 페이지를 넘기지만, 이쪽은 한 번 묻고 한 번 받는 것이
//! 전부다 — 답을 받으면 그 자리에서 끝이고 다음 질문은 앞의 것을 모른다.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

/// 서버가 답으로 준 것. `actions` 는 「이 창 맥미니로 이사해줘」처럼 말 한 마디로
/// 벌어진 일을 사람이 확인할 수 있게 한 줄씩 적은 것이다.
pub struct Answer {
    pub text: String,
    pub actions: Vec<String>,
}

impl Answer {
    /// 바에 찍을 한두 줄. 답 다음에 벌어진 일을 붙이되, 답이 이미 그 말을 하고 있으면
    /// 안 붙인다 — 「앞으로 가져옴 / 앞으로 가져옴」처럼 같은 줄이 두 번 뜨면 사람은
    /// 둘째 줄에서 새 정보를 찾다가 없다는 것을 알게 된다.
    pub fn line(&self) -> String {
        let fresh: Vec<&str> = self.actions.iter()
            .map(String::as_str)
            .filter(|action| !self.text.contains(*action))
            .collect();
        if fresh.is_empty() {
            return self.text.clone();
        }
        format!("{}\n{}", self.text, fresh.join(" · "))
    }
}

#[derive(Default)]
pub struct Client {
    pending: Option<Receiver<Result<Answer, ()>>>,
}

impl Client {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// 묻는다. 이미 묻고 있으면 아무 일도 안 한다 — 답이 둘 오면 어느 것이 이 질문의
    /// 답인지 바가 알 방법이 없다.
    pub fn ask(&mut self, service: PathBuf, text: String, pane: String) -> bool {
        if self.busy() || text.trim().is_empty() {
            return false;
        }
        let text: String = text.trim().chars().take(2000).collect();
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(run(&service, &text, &pane));
        });
        true
    }

    pub fn poll(&mut self) -> Option<Result<Answer, ()>> {
        match self.pending.as_ref()?.try_recv() {
            Ok(result) => {
                self.pending = None;
                Some(result)
            }
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some(Err(()))
            }
            Err(TryRecvError::Empty) => None,
        }
    }

    /// 바를 닫으면 기다리던 답은 버린다. 스레드는 제 할 일을 마치고 조용히 끝난다.
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

fn run(service: &Path, text: &str, pane: &str) -> Result<Answer, ()> {
    let port = crate::journal::service(service)?;
    let body = json!({ "text": text, "pane": pane });
    // 서버가 판을 다 읽고 창을 옮기는 일까지 하고 답하므로 장부 조회보다 한참 오래 걸린다
    // (모델 대기 35초 + kasaterm-cli 네 번).
    let value = crate::journal::request_within(port, "POST", "/api/ask", Some(&body), Duration::from_secs(50))?;
    parse(&value)
}

fn parse(value: &Value) -> Result<Answer, ()> {
    let text = value["answer"].as_str().filter(|t| !t.trim().is_empty()).ok_or(())?;
    let actions = value["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let kind = row["kind"].as_str()?;
            let detail = row["detail"].as_str().unwrap_or("").trim();
            let what = if detail.is_empty() { kind } else { detail };
            let what: String = what.chars().take(80).collect();
            Some(if row["ok"].as_bool() == Some(true) { what } else { format!("{what} — 실패") })
        })
        .take(3)
        .collect();
    // 모든 기기 요약은 기계마다 한 단락이라 한 창 답보다 서너 배 길다. 말풍선이 받는다.
    Ok(Answer { text: text.chars().take(1500).collect(), actions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_without_actions_is_just_the_answer() {
        let answer = parse(&json!({"answer": "아즈사가 파일을 고치는 중이에요."})).unwrap();
        assert!(answer.actions.is_empty());
        assert_eq!(answer.line(), "아즈사가 파일을 고치는 중이에요.");
    }

    /// 벌어진 일은 성패까지 같이 적는다 — 「이사해줘」라고 말했는데 조용히 실패하면
    /// 사람은 옮겨진 줄 알고 그 창을 찾아 헤맨다.
    #[test]
    fn actions_carry_their_outcome_into_the_line() {
        let answer = parse(&json!({
            "answer": "옮겼어요.",
            "actions": [
                {"kind": "migrate_pane", "ok": true, "detail": "맥미니로 이사"},
                {"kind": "migrate_pane", "ok": false, "detail": "붙을 기계가 없음"}
            ]
        }))
        .unwrap();
        assert_eq!(answer.line(), "옮겼어요.\n맥미니로 이사 · 붙을 기계가 없음 — 실패");
    }

    /// 답이 이미 한 말은 아래 줄에 또 적지 않는다(2026-09-14 실호출: 「앞으로 가져와
    /// 줘」가 답과 벌어진 일 양쪽에 「앞으로 가져옴」으로 왔다).
    #[test]
    fn an_action_the_answer_already_said_is_not_repeated() {
        let answer = parse(&json!({
            "answer": "앞으로 가져옴",
            "actions": [{"kind": "focus_pane", "ok": true, "detail": "앞으로 가져옴"}]
        }))
        .unwrap();
        assert_eq!(answer.line(), "앞으로 가져옴");
    }

    /// 답이 비어 있으면 성공으로 치지 않는다. 빈 바를 띄워 두면 사람은 답을 기다리며
    /// 계속 바라보게 된다.
    #[test]
    fn an_empty_answer_is_a_failure() {
        assert!(parse(&json!({"answer": "  "})).is_err());
        assert!(parse(&json!({"actions": []})).is_err());
    }

    #[test]
    fn asking_twice_before_the_answer_lands_is_refused() {
        let mut client = Client::default();
        let (_sender, receiver) = mpsc::channel();
        client.pending = Some(receiver);
        assert!(!client.ask(PathBuf::from("/없다.json"), "질문".into(), "%1".into()));
        client.cancel();
        assert!(!client.busy());
    }

    #[test]
    fn a_blank_question_is_never_sent() {
        let mut client = Client::default();
        assert!(!client.ask(PathBuf::from("/없다.json"), "   ".into(), "%1".into()));
        assert!(!client.busy());
    }
}
