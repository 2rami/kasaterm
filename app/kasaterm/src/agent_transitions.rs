//! pane 상태가 바뀔 때 무엇을 알릴 것인가 — 순수 함수.
//!
//! 알림·토스트·펄스는 전부 **전이**에서 나온다. 화면 스캔이 깜빡일 때마다 토스트가 뜨던
//! 시절(2026-07)에 완료 토스트를 통째로 걷어냈는데, 이제 상태가 깜빡이지 않으므로 전이
//! 하나 = 알림 하나로 다시 세운다. 훅(Stop·Notification)이 먼저 알려 온 것과 겹치면
//! `notify_desktop` 의 8초 dedup 키(`done:`·`approval:`·`error:`)가 한 번으로 접는다.

use crate::agent_state::{AgentState, WaitKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// 일하던 pane 이 멈췄다(승인 대기에서 풀린 것은 아니다 — 그건 끝난 게 아니라 뚫린 것).
    TurnDone,
    /// 사람 차례가 됐다, 또는 기다리는 이유의 종류가 바뀌었다.
    Waiting { kind: WaitKind, reason: String },
    /// 하네스가 오류로 멈췄다(라벨이 바뀌면 다시 한 번).
    Error { label: String },
    /// 오류에서 벗어났다 — 표시만 걷는다.
    Recovered,
    CompactStart,
    CompactEnd,
}

/// `prev` 가 None 이면 이 pane 을 처음 본 것 — 아무것도 알리지 않는다(앱이 방금 켜졌다).
pub(crate) fn transitions(prev: Option<&AgentState>, next: &AgentState) -> Vec<Transition> {
    let Some(prev) = prev else { return Vec::new() };
    if prev == next {
        return Vec::new();
    }
    let mut out = Vec::new();
    match (prev, next) {
        (AgentState::Compacting, AgentState::Idle) | (AgentState::Working, AgentState::Idle) => {
            out.push(Transition::TurnDone);
        }
        _ => {}
    }
    if matches!(prev, AgentState::Compacting) && !matches!(next, AgentState::Compacting) {
        out.push(Transition::CompactEnd);
    }
    if matches!(next, AgentState::Compacting) && !matches!(prev, AgentState::Compacting) {
        out.push(Transition::CompactStart);
    }
    match (prev, next) {
        (AgentState::Waiting { kind: a, .. }, AgentState::Waiting { kind: b, reason }) if a != b => {
            out.push(Transition::Waiting { kind: *b, reason: reason.clone() });
        }
        (AgentState::Waiting { .. }, AgentState::Waiting { .. }) => {}
        (_, AgentState::Waiting { kind, reason }) => {
            out.push(Transition::Waiting { kind: *kind, reason: reason.clone() });
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
    fn finishing_work_is_done_but_getting_unstuck_is_not() {
        assert_eq!(transitions(Some(&AgentState::Working), &AgentState::Idle), vec![Transition::TurnDone]);
        assert_eq!(
            transitions(Some(&AgentState::Compacting), &AgentState::Idle),
            vec![Transition::TurnDone, Transition::CompactEnd]
        );
        assert!(transitions(Some(&waiting(WaitKind::Permission)), &AgentState::Idle).is_empty());
        assert!(transitions(Some(&waiting(WaitKind::Permission)), &AgentState::Working).is_empty());
    }

    #[test]
    fn waiting_fires_once_per_kind() {
        let got = transitions(Some(&AgentState::Working), &waiting(WaitKind::Permission));
        assert_eq!(got, vec![Transition::Waiting { kind: WaitKind::Permission, reason: "x".into() }]);
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
    fn compacting_brackets_are_reported() {
        assert_eq!(transitions(Some(&AgentState::Working), &AgentState::Compacting), vec![Transition::CompactStart]);
        assert_eq!(transitions(Some(&AgentState::Compacting), &AgentState::Working), vec![Transition::CompactEnd]);
    }
}
