//! Quiet output and tab switches are not proof of completion. Notification history
//! therefore belongs to each PTY/session and requires a stable, normal end record.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::agent_state::{AgentState, Resolved, WaitKind};

const COMPLETION_SETTLE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// 같은 세션에서 새로 확인한 정상 응답 종료만 알린다.
    TurnDone { key: u64 },
    /// 사람 차례가 됐다, 또는 기다리는 이유의 종류가 바뀌었다.
    Waiting { kind: WaitKind, reason: String, key: Option<String> },
    /// 하네스가 오류로 멈췄다(라벨이 바뀌면 다시 한 번).
    Error { label: String },
    /// 오류에서 벗어났다 — 표시만 걷는다.
    Recovered,
    CompactStart,
    CompactEnd,
}

/// 하네스가 스스로 다시 시도하는 오류(`screenread::connection_trouble_in` 의 연결·서버 쪽 라벨).
/// 대개 몇 초 안에 풀리므로 바로 알리면 알림이 소음이 된다 — 부르는 쪽이 붙들었다가 오래
/// 가면 그때 알린다. 한도·잔액처럼 사람이 손써야 풀리는 것은 여기 없다(바로 알린다).
pub(crate) fn is_transient_trouble(label: &str) -> bool {
    matches!(label, "연결 끊김" | "오프라인" | "응답 없음" | "재시도 중" | "서버 혼잡" | "API 오류")
}

/// `prev` 가 None 이면 이 pane 을 처음 본 것 — 아무것도 알리지 않는다(앱이 방금 켜졌다).
pub(crate) fn transitions(prev: Option<&AgentState>, next: &AgentState) -> Vec<Transition> {
    let Some(prev) = prev else { return Vec::new() };
    if prev == next {
        return Vec::new();
    }
    let mut out = Vec::new();
    if matches!(prev, AgentState::Compacting) && !matches!(next, AgentState::Compacting) {
        out.push(Transition::CompactEnd);
    }
    if matches!(next, AgentState::Compacting) && !matches!(prev, AgentState::Compacting) {
        out.push(Transition::CompactStart);
    }
    match (prev, next) {
        (AgentState::Waiting { kind: a, .. }, AgentState::Waiting { kind: b, reason }) if a != b => {
            out.push(Transition::Waiting { kind: *b, reason: reason.clone(), key: None });
        }
        (AgentState::Waiting { .. }, AgentState::Waiting { .. }) => {}
        (_, AgentState::Waiting { kind, reason }) => {
            out.push(Transition::Waiting { kind: *kind, reason: reason.clone(), key: None });
        }
        _ => {}
    }
    match (prev, next) {
        (AgentState::Error { label: a }, AgentState::Error { label: b }) => {
            if a != b {
                out.push(Transition::Error { label: b.clone() });
            }
        }
        (AgentState::Error { .. }, _) => out.push(Transition::Recovered),
        (_, AgentState::Error { label }) => out.push(Transition::Error { label: label.clone() }),
        _ => {}
    }
    out
}

#[derive(Default)]
pub(crate) struct Observer {
    panes: HashMap<String, Seen>,
}

struct Seen {
    source: Option<String>,
    state: AgentState,
    completion: Option<u64>,
    pending: Option<(u64, Option<Instant>)>,
    questions: HashSet<String>,
}

impl Observer {
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        self.panes.values().filter_map(|seen| seen.pending.map(|(_, since)| {
            since.map(|at| at + COMPLETION_SETTLE).unwrap_or(now + Duration::from_millis(300))
                .max(now + Duration::from_millis(50))
        })).min()
    }

    pub(crate) fn update(&mut self, panes: &HashMap<String, Resolved>) -> Vec<(String, Vec<Transition>)> {
        self.update_at(panes, Instant::now())
    }

    fn update_at(&mut self, panes: &HashMap<String, Resolved>, now: Instant) -> Vec<(String, Vec<Transition>)> {
        self.panes.retain(|id, _| panes.contains_key(id));
        let mut events = Vec::new();
        for (id, next) in panes {
            let same = self.panes.get(id).filter(|old| old.source == next.source);
            let mut pending = same.and_then(|old| old.pending);
            let mut out = transitions(same.map(|old| &old.state), &next.state);
            if !next.questions.is_empty() {
                out.retain(|event| !matches!(event, Transition::Waiting { kind: WaitKind::Question, .. }));
            }
            for question in &next.questions {
                if same.is_none_or(|old| !old.questions.contains(&question.id)) {
                    out.push(Transition::Waiting { kind: WaitKind::Question,
                        reason: question.prompt.clone(), key: Some(question.id.clone()) });
                }
            }
            if let Some(old) = same {
                let idle = matches!(next.state, AgentState::Idle | AgentState::Waiting { kind: WaitKind::Idle, .. });
                let eligible = next.source.is_some() && !next.has_error && !next.bg_active && next.questions.is_empty();
                if !eligible || pending.is_some_and(|(key, _)| next.completion != Some(key)) {
                    pending = None;
                }
                if eligible {
                    if let Some(key) = next.completion.filter(|key| Some(*key) != old.completion) {
                        pending = Some((key, None));
                    }
                    // A Stop hook may immediately continue the turn. A normal end
                    // must remain current before it can announce a finished response.
                    if let Some((key, since)) = pending.as_mut() {
                        if !idle {
                            *since = None;
                        } else if now.saturating_duration_since(*since.get_or_insert(now)) >= COMPLETION_SETTLE {
                            out.push(Transition::TurnDone { key: *key });
                            pending = None;
                        }
                    }
                }
            }
            // Retain the last seen completion across the next working turn. A stale
            // record reappearing after a quiet timeout is not a new response.
            let completion = next.completion.or_else(|| same.and_then(|old| old.completion));
            self.panes.insert(id.clone(), Seen { source: next.source.clone(), state: next.state.clone(),
                completion, pending, questions: next.questions.iter().map(|q| q.id.clone()).collect() });
            if !out.is_empty() { events.push((id.clone(), out)); }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(kind: WaitKind) -> AgentState {
        AgentState::Waiting { kind, reason: "x".into() }
    }

    #[test]
    fn first_sight_and_no_change_are_silent() {
        assert!(transitions(None, &AgentState::Idle).is_empty());
        assert!(transitions(Some(&AgentState::Working), &AgentState::Working).is_empty());
        assert!(transitions(Some(&AgentState::Unknown), &AgentState::Idle).is_empty());
    }

    #[test]
    fn stopping_work_without_a_completion_record_is_silent() {
        assert!(transitions(Some(&AgentState::Working), &AgentState::Idle).is_empty());
        assert_eq!(
            transitions(Some(&AgentState::Compacting), &AgentState::Idle),
            vec![Transition::CompactEnd]
        );
        assert!(transitions(Some(&waiting(WaitKind::Permission)), &AgentState::Idle).is_empty());
        assert!(transitions(Some(&waiting(WaitKind::Permission)), &AgentState::Working).is_empty());
    }

    #[test]
    fn waiting_fires_once_per_kind() {
        let got = transitions(Some(&AgentState::Working), &waiting(WaitKind::Permission));
        assert_eq!(got, vec![Transition::Waiting { kind: WaitKind::Permission, reason: "x".into(), key: None }]);
        let same = AgentState::Waiting { kind: WaitKind::Permission, reason: "y".into() };
        assert!(transitions(Some(&waiting(WaitKind::Permission)), &same).is_empty(), "이유 글자만 달라진 건 새 소식이 아니다");
        let got = transitions(Some(&waiting(WaitKind::Permission)), &waiting(WaitKind::Question));
        assert!(matches!(got.as_slice(), [Transition::Waiting { kind: WaitKind::Question, .. }]));
    }

    #[test]
    fn errors_fire_on_entry_and_on_a_new_label_only() {
        let e1 = AgentState::Error { label: "API 오류".into() };
        let e2 = AgentState::Error { label: "연결 끊김".into() };
        assert_eq!(transitions(Some(&AgentState::Working), &e1), vec![Transition::Error { label: "API 오류".into() }]);
        assert!(transitions(Some(&e1), &e1).is_empty());
        assert_eq!(transitions(Some(&e1), &e2), vec![Transition::Error { label: "연결 끊김".into() }]);
        assert_eq!(transitions(Some(&e1), &AgentState::Idle), vec![Transition::Recovered]);
    }

    #[test]
    fn only_self_retrying_troubles_are_held() {
        for label in ["연결 끊김", "재시도 중", "서버 혼잡", "API 오류"] {
            assert!(is_transient_trouble(label), "{label}");
        }
        for label in ["한도 소진", "잔액 부족", ""] {
            assert!(!is_transient_trouble(label), "{label} 는 사람이 손써야 풀린다");
        }
    }

    #[test]
    fn compacting_brackets_are_reported() {
        assert_eq!(transitions(Some(&AgentState::Working), &AgentState::Compacting), vec![Transition::CompactStart]);
        assert_eq!(transitions(Some(&AgentState::Compacting), &AgentState::Working), vec![Transition::CompactEnd]);
    }

    fn fact(state: AgentState, completion: Option<u64>) -> Resolved {
        Resolved { state, source: Some("session-a".into()), completion, questions: Vec::new(),
            reason: "fixture", bg_active: false, has_error: false, intent: String::new(), since: std::time::Instant::now() }
    }

    fn step(observer: &mut Observer, value: Resolved) -> Vec<Transition> {
        let panes = HashMap::from([("%a".into(), value)]);
        let now = Instant::now();
        observer.update_at(&panes, now).into_iter()
            .chain(observer.update_at(&panes, now + Duration::from_secs(2)))
            .flat_map(|(_, e)| e).collect()
    }

    #[test]
    fn only_a_new_normal_end_not_quiet_or_interruption_notifies() {
        let mut observer = Observer::default();
        assert!(step(&mut observer, fact(AgentState::Working, None)).is_empty());
        assert!(step(&mut observer, fact(AgentState::Idle, None)).is_empty());
        assert_eq!(step(&mut observer, fact(AgentState::Idle, Some(1))), [Transition::TurnDone { key: 1 }]);
        assert!(step(&mut observer, fact(AgentState::Working, None)).is_empty());
        assert!(step(&mut observer, fact(AgentState::Idle, Some(1))).is_empty());
        assert_eq!(step(&mut observer, fact(AgentState::Idle, Some(2))), [Transition::TurnDone { key: 2 }]);
    }

    #[test]
    fn startup_session_replacement_and_hidden_tabs_do_not_mix() {
        let mut observer = Observer::default();
        assert!(step(&mut observer, fact(AgentState::Idle, Some(1))).is_empty());
        let mut other = fact(AgentState::Idle, Some(2));
        other.source = Some("session-b".into());
        assert!(step(&mut observer, other).is_empty());
        let mut panes = HashMap::from([
            ("%a".into(), fact(AgentState::Working, None)),
            ("%hidden".into(), fact(AgentState::Working, None)),
        ]);
        assert!(observer.update(&panes).is_empty());
        panes.insert("%hidden".into(), fact(AgentState::Idle, Some(3)));
        let now = Instant::now();
        assert!(observer.update_at(&panes, now).is_empty());
        assert_eq!(observer.update_at(&panes, now + Duration::from_secs(2)), [("%hidden".into(), vec![Transition::TurnDone { key: 3 }])]);
        assert!(observer.update(&panes).is_empty());
    }

    #[test]
    fn errors_background_work_and_pending_questions_are_not_completion() {
        for case in 0..3 {
            let mut observer = Observer::default();
            step(&mut observer, fact(AgentState::Working, None));
            let mut done = fact(AgentState::Idle, Some(1));
            done.has_error = case == 0;
            done.bg_active = case == 1;
            if case == 2 { done.questions.push(crate::transcript::PendingQuestion {
                id: "q".into(), prompt: "선택".into(), blocking: false,
            }); }
            assert!(!step(&mut observer, done).iter().any(|e| matches!(e, Transition::TurnDone { .. })));
            assert!(step(&mut observer, fact(AgentState::Idle, Some(1))).is_empty());
        }
    }

    #[test]
    fn each_async_question_notifies_once_even_while_the_agent_keeps_working() {
        let mut observer = Observer::default();
        step(&mut observer, fact(AgentState::Working, None));
        let mut asking = fact(AgentState::Working, None);
        asking.questions.push(crate::transcript::PendingQuestion {
            id: "call-a".into(), prompt: "어느 기기인가요?".into(), blocking: false,
        });
        let first = step(&mut observer, asking.clone());
        assert!(matches!(first.as_slice(), [Transition::Waiting { kind: WaitKind::Question, key: Some(key), .. }] if key == "call-a"));
        assert!(step(&mut observer, asking.clone()).is_empty());
        asking.state = waiting(WaitKind::Question);
        assert!(step(&mut observer, asking.clone()).is_empty());
        asking.questions[0].id = "call-b".into();
        let second = step(&mut observer, asking);
        assert!(matches!(second.as_slice(), [Transition::Waiting { key: Some(key), .. }] if key == "call-b"));
    }

    #[test]
    fn a_stop_that_continues_before_settling_never_announces_completion() {
        let mut observer = Observer::default();
        let mut panes = HashMap::from([("%a".into(), fact(AgentState::Working, None))]);
        let now = Instant::now();
        observer.update_at(&panes, now);
        panes.insert("%a".into(), fact(AgentState::Idle, Some(1)));
        assert!(observer.update_at(&panes, now + Duration::from_millis(100)).is_empty());
        panes.insert("%a".into(), fact(AgentState::Working, None));
        assert!(observer.update_at(&panes, now + Duration::from_secs(1)).is_empty());
        panes.insert("%a".into(), fact(AgentState::Idle, Some(1)));
        assert!(observer.update_at(&panes, now + Duration::from_secs(4)).is_empty());
        assert_eq!(observer.next_deadline(), None);
    }

    #[test]
    fn the_completion_deadline_wakes_an_otherwise_quiet_window() {
        let mut observer = Observer::default();
        let mut panes = HashMap::from([("%a".into(), fact(AgentState::Working, None))]);
        let now = Instant::now();
        observer.update_at(&panes, now);
        assert_eq!(observer.next_deadline(), None);
        panes.insert("%a".into(), fact(AgentState::Idle, Some(1)));
        observer.update_at(&panes, now);
        assert_eq!(observer.next_deadline(), Some(now + COMPLETION_SETTLE));
        observer.update_at(&panes, now + COMPLETION_SETTLE);
        assert_eq!(observer.next_deadline(), None);
    }
}
