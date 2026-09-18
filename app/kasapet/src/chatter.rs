//! 펫이 먼저 거는 말 — 아무도 묻지 않아도 몇 초에 한 줄씩 툭 던진다.
//!
//! 유리 바(`ask.rs`)는 사람이 물어야 답하지만 이쪽은 사람을 기다리지 않는다. 그래서
//! 값을 재는 자리가 다르다 — 말할 때마다 나쵸를 부르면 10초 간격으로 하루 8640번이 되고,
//! 그 한 번이 판과 화면을 통째로 싣는다. 한 번에 여러 줄을 받아 하나씩 풀면 사람이 보는
//! 간격은 그대로면서 부르는 횟수만 준다(2026-09-18 지시).

use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// 한 번 부를 때 받아 두는 줄 수.
const LINES: u32 = 5;
/// 모델이 막혔거나 할 말이 없을 때 다음에 부르기까지. 곧바로 다시 부르면 끊긴 동안
/// 몇 초마다 헛걸음을 하게 된다.
const REST: Duration = Duration::from_secs(90);
/// 답을 이만큼까지 기다린다 — 사람이 기다리는 답이 아니라서 넉넉히 둔다.
const PATIENCE: Duration = Duration::from_secs(45);

#[derive(Default)]
pub struct Client {
    pending: Option<Receiver<Vec<String>>>,
    queue: VecDeque<String>,
    spoke_at: Option<Instant>,
    rest_until: Option<Instant>,
    /// 지금 쥔 줄들이 어느 창을 보고 지은 것인지. 창이 바뀌면 남은 줄은 버린다 —
    /// 다른 창을 보는 사람에게 앞 창 이야기를 계속하는 꼴이 된다.
    subject: String,
}

impl Client {
    /// 말할 때가 됐으면 한 줄 준다. 줄이 떨어지면 뒤에서 다시 받아 둔다.
    ///
    /// `quiet` 는 지금 끼어들면 안 되는 자리라는 뜻이다(사람이 답을 읽는 중, 묻는 중).
    /// 그동안에도 받아는 둔다 — 조용한 사이에 재료까지 안 모으면, 방해가 걷힌 뒤 사람은
    /// 모델을 기다리는 시간만큼 또 조용한 펫을 본다.
    pub fn poll(&mut self, service: &Path, pane: &str, gap: Duration, quiet: bool) -> Option<String> {
        self.receive();
        if pane != self.subject {
            self.subject = pane.to_string();
            self.queue.clear();
        }
        if quiet {
            // 방해가 걷힌 자리부터 한 텀을 센다. 이것을 앞당기면 사람이 답을 다 읽은
            // 순간 잡담이 곧바로 그 위를 덮는다.
            self.spoke_at = Some(Instant::now());
            self.refill(service, pane);
            return None;
        }
        let line = self.due(gap).then(|| self.queue.pop_front()).flatten();
        if line.is_some() {
            self.spoke_at = Some(Instant::now());
        }
        self.refill(service, pane);
        line
    }

    fn due(&self, gap: Duration) -> bool {
        self.spoke_at.is_none_or(|at| at.elapsed() >= gap)
    }

    fn receive(&mut self) {
        let Some(receiver) = self.pending.as_ref() else { return };
        match receiver.try_recv() {
            Ok(lines) => {
                self.pending = None;
                if lines.is_empty() {
                    self.rest_until = Some(Instant::now() + REST);
                }
                self.queue.extend(lines);
            }
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.rest_until = Some(Instant::now() + REST);
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn refill(&mut self, service: &Path, pane: &str) {
        if !self.queue.is_empty() || self.pending.is_some() || pane.is_empty() {
            return;
        }
        if self.rest_until.is_some_and(|until| Instant::now() < until) {
            return;
        }
        self.rest_until = None;
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        let (service, pane) = (service.to_path_buf(), pane.to_string());
        std::thread::spawn(move || {
            let _ = sender.send(fetch(&service, &pane).unwrap_or_default());
        });
    }
}

fn fetch(service: &PathBuf, pane: &str) -> Result<Vec<String>, ()> {
    let port = crate::journal::service(service)?;
    let body = json!({ "pane": pane, "count": LINES });
    let value = crate::journal::request_within(port, "POST", "/api/pet-chatter", Some(&body), PATIENCE)?;
    Ok(value
        .get("lines")
        .and_then(|lines| lines.as_array())
        .map(|lines| {
            lines.iter()
                .filter_map(|line| line.as_str())
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| line.chars().take(200).collect())
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stocked(lines: &[&str]) -> Client {
        Client {
            queue: lines.iter().map(|line| line.to_string()).collect(),
            subject: "%4".into(),
            ..Client::default()
        }
    }

    const GAP: Duration = Duration::from_secs(10);

    #[test]
    fn the_first_line_comes_at_once_and_the_next_waits_out_the_gap() {
        let mut client = stocked(&["첫 줄", "둘째 줄"]);
        // 서비스 기술자가 없는 자리라 다시 받아 오지는 못한다 — 쥔 줄만 푼다.
        let service = Path::new("/nonexistent/service.json");
        assert_eq!(client.poll(service, "%4", GAP, false).as_deref(), Some("첫 줄"));
        assert_eq!(client.poll(service, "%4", GAP, false), None, "10초가 안 지났으면 조용하다");
        client.spoke_at = Some(Instant::now() - GAP);
        assert_eq!(client.poll(service, "%4", GAP, false).as_deref(), Some("둘째 줄"));
    }

    #[test]
    fn moving_to_another_window_drops_the_old_windows_leftovers() {
        let mut client = stocked(&["앞 창 이야기", "앞 창 이야기 둘"]);
        let service = Path::new("/nonexistent/service.json");
        assert_eq!(client.poll(service, "%9", GAP, false), None);
        assert!(client.queue.is_empty());
    }

    #[test]
    fn a_quiet_turn_holds_the_line_but_still_stocks_up() {
        let mut client = stocked(&["줄"]);
        let service = Path::new("/nonexistent/service.json");
        assert_eq!(client.poll(service, "%4", GAP, true), None, "끼어들면 안 되는 자리다");
        assert_eq!(client.queue.len(), 1, "쥔 줄은 버리지 않는다");
        assert_eq!(client.poll(service, "%4", GAP, false), None, "방금 조용했으니 한 텀은 센다");
        client.spoke_at = Some(Instant::now() - GAP);
        assert_eq!(client.poll(service, "%4", GAP, false).as_deref(), Some("줄"));
    }

    #[test]
    fn a_quiet_turn_with_nothing_in_hand_asks_ahead_of_time() {
        let mut client = Client { subject: "%4".into(), ..Client::default() };
        assert_eq!(client.poll(Path::new("/nonexistent/service.json"), "%4", GAP, true), None);
        assert!(client.pending.is_some(), "조용한 사이에 재료를 모아 둔다");
    }

    #[test]
    fn an_empty_answer_rests_instead_of_asking_again_at_once() {
        let mut client = Client { subject: "%4".into(), ..Client::default() };
        let (sender, receiver) = mpsc::channel();
        client.pending = Some(receiver);
        sender.send(Vec::new()).unwrap();
        assert_eq!(client.poll(Path::new("/nonexistent/service.json"), "%4", GAP, false), None);
        assert!(client.rest_until.is_some_and(|until| until > Instant::now()));
        assert!(client.pending.is_none(), "쉬는 동안에는 다시 부르지 않는다");
    }

    #[test]
    fn a_nameless_window_never_starts_a_request() {
        let mut client = Client::default();
        assert_eq!(client.poll(Path::new("/nonexistent/service.json"), "", GAP, false), None);
        assert!(client.pending.is_none());
    }
}
