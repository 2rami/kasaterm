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
        self.update_selected_at(panes, now, |_| true)
    }

    fn settled_deadline(&self) -> Option<Instant> {
        self.panes.values().filter_map(|seen| seen.pending.and_then(|(_, at)| at.map(|at| at + COMPLETION_SETTLE))).min()
    }

    fn update_selected_at(&mut self, panes: &HashMap<String, Resolved>, now: Instant, fresh: impl Fn(&String) -> bool) -> Vec<(String, Vec<Transition>)> {
        self.panes.retain(|id, _| panes.contains_key(id));
        let mut events = Vec::new();
        for (id, next) in panes {
            if !fresh(id) { continue; }
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

/// 원격 행의 `response_end` 를 판정 모양으로. 증거가 없으면(구판 원본·끊긴 캐시) 출처도 없어
/// 관찰자가 완료 자격을 주지 않는다 — 원격의 쉼만으로는 완료를 추측하지 않는다.
pub(crate) fn remote_fact(row: &serde_json::Value, now: Instant) -> Resolved {
    let end = row.get("response_end").filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<kasa_socket::backend::ResponseEnd>(v.clone()).ok())
        .filter(|end| !end.source.is_empty()
            && end.session_id.as_deref().is_some_and(|sid| !sid.is_empty())
            && row.get("id").and_then(|v| v.as_str()) == Some(end.surface_id.as_str())
            && row.get("state_fresh").and_then(|v| v.as_bool()) != Some(false));
    let Some(end) = end else {
        return Resolved { state: AgentState::Unknown, source: None, completion: None, questions: Vec::new(),
            reason: "remote", bg_active: false, has_error: false, intent: String::new(), since: now };
    };
    Resolved {
        state: if end.idle { AgentState::Idle } else { AgentState::Working },
        source: Some(format!("{}:{}", end.session_id.unwrap_or_default(), end.source)),
        completion: end.completion.and_then(|key| u64::from_str_radix(&key, 16).ok()),
        questions: end.questions.into_iter()
            .map(|id| crate::transcript::PendingQuestion { id, prompt: String::new(), blocking: false })
            .collect(),
        reason: "remote response end",
        bg_active: end.background,
        has_error: end.error,
        intent: String::new(),
        since: now,
    }
}

/// 다른 기기 칸의 응답 완료를 보는 관찰자 — 열린 거울과 원격 목록이 함께 쓴다. 원격 사실은 폴링으로만
/// 바뀌므로 새로 읽은 표본에서만 판정하고, 2초 유지 만기에는 캐시를 다시 쓰지 않고 새로 읽어 확인한다.
#[derive(Default)]
pub(crate) struct RemoteObserver {
    inner: Observer,
    generation: Option<u64>,
    samples: HashMap<String, String>,
    /// 새 표본을 청한 뒤 다시 깨어 볼 때. 표본이 오면 걷는다.
    wake: Option<Instant>,
}

/// 새 표본을 청한 뒤 그것이 캐시에 앉았는지 다시 볼 간격.
const RESAMPLE_WAKE: Duration = Duration::from_millis(300);

impl RemoteObserver {
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.wake.or_else(|| self.inner.settled_deadline())
    }

    pub(crate) fn fresh(&self, generation: u64) -> bool {
        self.generation != Some(generation)
    }

    /// 같은 표본으로는 판정하지 않는다. 유지 만기가 지났으면 새로 읽기를 한 번 청하고(`true`),
    /// 그 표본이 올 때까지 잠깐씩 깨어 본다.
    pub(crate) fn wants_sample(&mut self, now: Instant) -> bool {
        if let Some(at) = self.wake {
            if at <= now {
                self.wake = Some(now + RESAMPLE_WAKE);
            }
            return false;
        }
        let due = self.inner.settled_deadline().is_some_and(|at| at <= now);
        if due {
            self.wake = Some(now + RESAMPLE_WAKE);
        }
        due
    }

    /// `rows` 는 (원격 칸 열쇠, 행). 새 정상 종료가 2초 유지된 칸과 그 종료 열쇠만 돌려준다.
    pub(crate) fn update<'a>(
        &mut self,
        generation: u64,
        rows: impl IntoIterator<Item = (String, String, &'a serde_json::Value)>,
        now: Instant,
    ) -> Vec<(String, u64)> {
        self.generation = Some(generation);
        self.wake = None;
        let mut samples = HashMap::new();
        let mut fresh = HashSet::new();
        let panes: HashMap<String, Resolved> = rows.into_iter().map(|(key, sample, row)| {
            if self.samples.get(&key) != Some(&sample) { fresh.insert(key.clone()); }
            samples.insert(key.clone(), sample);
            (key, remote_fact(row, now))
        }).collect();
        self.samples = samples;
        self.inner.update_selected_at(&panes, now, |key| fresh.contains(key)).into_iter()
            .flat_map(|(key, events)| events.into_iter().filter_map(move |event| match event {
                Transition::TurnDone { key: done } => Some((key.clone(), done)),
                _ => None,
            }))
            .collect()
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

    fn remote(idle: bool, completion: Option<&str>, session: &str) -> serde_json::Value {
        serde_json::json!({"id": "%3", "status": if idle { "idle" } else { "working" },
            "response_end": {"surface_id": "%3", "session_id": session, "source": format!("src-{session}"),
                "completion": completion, "idle": idle, "error": false, "background": false, "questions": []}})
    }

    /// 원격 표본 둘을 2초 넘게 띄워 먹인다 — 세대가 바뀔 때만 판정하는 펌프와 같다.
    fn sample(observer: &mut RemoteObserver, generation: &mut u64, at: &mut Instant, row: &serde_json::Value) -> Vec<u64> {
        let mut out = Vec::new();
        for _ in 0..2 {
            *generation += 1;
            *at += Duration::from_millis(2100);
            out.extend(observer.update(*generation, [("mini\u{1f}key".to_string(), generation.to_string(), row)], *at).into_iter().map(|(_, k)| k));
        }
        out
    }

    #[test]
    fn a_remote_completion_needs_its_evidence_a_fresh_sample_and_the_same_session() {
        let (mut observer, mut generation, mut at) = (RemoteObserver::default(), 0, Instant::now());
        // 첫 관측의 지난 종료는 새 소식이 아니다.
        assert!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("0a"), "s1")).is_empty());
        assert!(sample(&mut observer, &mut generation, &mut at, &remote(false, None, "s1")).is_empty());
        assert_eq!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("0b"), "s1")), [0x0b]);
        assert!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("0b"), "s1")).is_empty());
        // 세션이 바뀌면 그 세션의 첫 관측이다.
        assert!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("0c"), "s2")).is_empty());
        // 증거를 안 싣는 구판·끊긴 캐시의 쉼은 완료가 아니고, 다시 실려도 첫 관측이다.
        let old = serde_json::json!({"id": "%3", "status": "idle"});
        assert!(sample(&mut observer, &mut generation, &mut at, &old).is_empty());
        assert!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("0d"), "s2")).is_empty());
        // 기기가 끊겨 행이 빠졌다가 돌아온 것도 첫 관측이다.
        generation += 1;
        observer.update(generation, std::iter::empty(), at);
        assert!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("0e"), "s2")).is_empty());
    }

    #[test]
    fn remote_errors_questions_and_background_work_are_not_completion() {
        for field in ["error", "background", "questions"] {
            let (mut observer, mut generation, mut at) = (RemoteObserver::default(), 0, Instant::now());
            sample(&mut observer, &mut generation, &mut at, &remote(false, None, "s1"));
            let mut done = remote(true, Some("01"), "s1");
            done["response_end"][field] = if field == "questions" { serde_json::json!(["call-a"]) } else { true.into() };
            assert!(sample(&mut observer, &mut generation, &mut at, &done).is_empty(), "{field}");
            assert!(sample(&mut observer, &mut generation, &mut at, &remote(true, Some("01"), "s1")).is_empty(), "{field}");
        }
    }

    #[test]
    fn a_remote_completion_settles_only_on_a_new_sample() {
        let (mut observer, mut generation, mut at) = (RemoteObserver::default(), 0, Instant::now());
        observer.update(generation, [("k".to_string(), generation.to_string(), &remote(false, None, "s1"))], at);
        generation += 1;
        let row = remote(true, Some("01"), "s1");
        assert!(observer.update(generation, [("k".to_string(), generation.to_string(), &row)], at).is_empty());
        assert!(!observer.fresh(generation), "같은 세대는 다시 판정하지 않는다");
        assert!(!observer.wants_sample(at + Duration::from_secs(1)));
        assert!(observer.wants_sample(at + COMPLETION_SETTLE), "만기에는 새로 읽기를 청한다");
        assert!(!observer.wants_sample(at + COMPLETION_SETTLE), "한 번만 청한다");
        assert!(observer.deadline().is_some(), "표본이 올 때까지 다시 깨어 본다");
        generation += 1;
        assert_eq!(observer.update(generation, [("k".to_string(), generation.to_string(), &row)], at + COMPLETION_SETTLE), [("k".to_string(), 1)]);
        assert_eq!(observer.deadline(), None);
    }

    #[test]
    fn another_machine_refresh_cannot_settle_a_cached_completion() {
        let mut observer = RemoteObserver::default();
        let now = Instant::now();
        let working = remote(false, None, "s1");
        let done = remote(true, Some("0a"), "s1");
        observer.update(1, [("a".into(), "a1".into(), &working), ("b".into(), "b1".into(), &working)], now);
        observer.update(2, [("a".into(), "a2".into(), &done), ("b".into(), "b1".into(), &working)], now);
        assert!(observer.update(3, [("a".into(), "a2".into(), &done), ("b".into(), "b2".into(), &working)],
            now + COMPLETION_SETTLE).is_empty());
        assert!(observer.wants_sample(now + COMPLETION_SETTLE));
        assert_eq!(observer.update(4, [("a".into(), "a3".into(), &done), ("b".into(), "b2".into(), &working)],
            now + COMPLETION_SETTLE), [("a".into(), 10)]);
    }

    #[test]
    fn remote_evidence_must_match_the_row_and_have_a_session() {
        let now = Instant::now();
        for field in ["surface_id", "session_id", "state_fresh"] {
            let mut row = remote(true, Some("01"), "s1");
            match field {
                "state_fresh" => row[field] = false.into(),
                "session_id" => row["response_end"][field] = serde_json::Value::Null,
                _ => row["response_end"][field] = "%other".into(),
            }
            assert!(remote_fact(&row, now).source.is_none(), "{field}");
        }
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
