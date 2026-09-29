//! 받는 곳 판정 — 나쵸 판 입력칸의 글이 누구에게 갈지.
//!
//! 코드가 확실한 것(`@이름`·명령·고른 학생 + 「그거·다시」·글 속 이름)을 먼저 거르고,
//! 뜻을 알아야 하는 것만 관문의 Jev 판정에 묻는다. 판정은 제안이다 — 보내기는 사람이
//! 누를 때만 일어난다. 규칙·문턱·실측은 `docs/boards.md` 「받는 곳 판정」.
//!
//! 사이드바(받는 학생 줄 강조)는 `RouteHighlight` 만 읽는다.

use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 이 확률 이상이면 하나로 세우고, 아래면 위 셋을 사람이 고르게 한다.
pub(crate) const SURE: f32 = 0.75;
/// 어절이 끝났거나 오래 묻지 않았을 때는 곧, 아니면 손이 멈추기를 기다린다.
const ASK_SOON: Duration = Duration::from_millis(60);
const ASK_PAUSE: Duration = Duration::from_millis(320);
/// 쉬지 않고 쳐도 이만큼 지나면 한 번은 묻는다 — 치는 동안 받는 곳이 바뀌어 보이게.
const ASK_EVERY: Duration = Duration::from_millis(900);
const MIN_CHARS: usize = 2;
const MIN_ASK_CHARS: usize = 4;
const PICK_COUNT: usize = 3;

/// 받을 학생 — board 스냅샷의 pane 주소에서 온다. 사이드바는 `machine_id`·`surface_id`
/// 로 자기 줄을 찾고, 옛 판 기계라 주소가 어긋나면 `name` 으로 맞춘다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StudentRef {
    pub(crate) machine_id: String,
    pub(crate) machine_label: String,
    pub(crate) surface_id: String,
    pub(crate) name: String,
    pub(crate) local: bool,
}

/// Jev 에 넘기는 학생 문맥. 마지막 보고가 없으면 틀린다(제목만으로는 「투혼 … 돌려」가 새 일로 갔다).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) student: StudentRef,
    pub(crate) title: String,
    pub(crate) latest: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RouteTarget {
    Student(StudentRef),
    NewTask,
    Nacho,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RouteBasis {
    Mention,
    Command,
    Selected,
    LastSent,
    NamedInText,
    Jev { probability: f32, latency_ms: u64 },
    /// 후보 칩에서 사람이 골랐다.
    Picked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HoldReason {
    TooShort,
    UnknownName(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RouteState {
    Empty,
    Hold(HoldReason),
    Thinking,
    Decided { target: RouteTarget, basis: RouteBasis, alternatives: Vec<(RouteTarget, f32)> },
    /// 확신이 문턱 아래 — 확률 높은 순. 첫째가 기본 선택이다.
    Pick { options: Vec<(RouteTarget, f32)>, latency_ms: Option<u64> },
    Failed(String),
}

impl RouteState {
    /// 보내기 단추가 쓸 받는 곳 — 고르기 상태면 첫째.
    pub(crate) fn target(&self) -> Option<&RouteTarget> {
        match self {
            Self::Decided { target, .. } => Some(target),
            Self::Pick { options, .. } => options.first().map(|(target, _)| target),
            _ => None,
        }
    }
}

/// 사이드바가 읽는 강조. `tentative` 면 Jev 확신이 문턱 아래라 「여기로?」로 그린다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteHighlight {
    pub(crate) student: StudentRef,
    pub(crate) tentative: bool,
}

pub(crate) struct RouteContext<'a> {
    pub(crate) candidates: &'a [Candidate],
    pub(crate) selected: Option<&'a StudentRef>,
    pub(crate) last_sent: Option<&'a StudentRef>,
}

const DEICTIC: [&str; 13] = ["그거", "이거", "저거", "그것", "이것", "그건", "이건", "다시", "계속", "마저", "이어서", "아까", "방금"];
/// 이름 뒤에 붙어도 이름으로 읽는 토씨. 없으면 「케이스」가 케이로 간다.
const PARTICLES: [&str; 17] = ["에게", "한테", "이한테", "이랑", "께", "가", "이", "는", "은", "도", "를", "을", "의", "랑", "아", "야", "만"];

fn is_hangul(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c) || ('\u{3131}'..='\u{318E}').contains(&c)
}

/// 글 속에 이름이 낱말로 서 있나 — 앞은 글머리나 한글 아닌 글자, 뒤는 끝·한글 아닌 글자·토씨.
fn names_word(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    text.match_indices(name).any(|(at, _)| {
        let before_ok = text[..at].chars().next_back().map_or(true, |c| !is_hangul(c));
        let rest = &text[at + name.len()..];
        let after_ok = match rest.chars().next() {
            None => true,
            Some(c) if !is_hangul(c) => true,
            Some(_) => PARTICLES.iter().any(|p| {
                rest.starts_with(p) && rest[p.len()..].chars().next().map_or(true, |c| !is_hangul(c))
            }),
        };
        before_ok && after_ok
    })
}

fn student(c: &Candidate) -> RouteTarget {
    RouteTarget::Student(c.student.clone())
}

fn even_pick(hits: &[&Candidate]) -> RouteState {
    let p = 1.0 / hits.len() as f32;
    RouteState::Pick { options: hits.iter().map(|c| (student(c), p)).collect(), latency_ms: None }
}

/// 코드 판정. 확실하면 상태를, 뜻을 알아야 하면 `None`(Jev 로 넘김)을 준다.
/// `text` 는 확정된 글만 — IME 조합 중인 글자는 부르는 쪽이 빼고 넘긴다.
pub(crate) fn code_route(text: &str, ctx: &RouteContext) -> Option<RouteState> {
    let t = text.trim();
    let chars = t.chars().count();
    if chars < MIN_CHARS {
        return Some(RouteState::Empty);
    }
    if let Some(rest) = t.strip_prefix('@') {
        let token: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        let hits: Vec<&Candidate> = ctx.candidates.iter().filter(|c| !token.is_empty() && c.student.name.starts_with(&token)).collect();
        return Some(match hits.len() {
            0 => RouteState::Hold(HoldReason::UnknownName(token)),
            1 => RouteState::Decided { target: student(hits[0]), basis: RouteBasis::Mention, alternatives: Vec::new() },
            _ => even_pick(&hits),
        });
    }
    if t.starts_with("/새") || t.starts_with("새 일:") {
        return Some(RouteState::Decided { target: RouteTarget::NewTask, basis: RouteBasis::Command, alternatives: Vec::new() });
    }
    if t.starts_with("/나쵸") || t.starts_with("나쵸,") {
        return Some(RouteState::Decided { target: RouteTarget::Nacho, basis: RouteBasis::Command, alternatives: Vec::new() });
    }
    if DEICTIC.iter().any(|d| t.starts_with(d)) {
        if let Some(s) = ctx.selected {
            return Some(RouteState::Decided { target: RouteTarget::Student(s.clone()), basis: RouteBasis::Selected, alternatives: Vec::new() });
        }
        if let Some(s) = ctx.last_sent {
            return Some(RouteState::Decided { target: RouteTarget::Student(s.clone()), basis: RouteBasis::LastSent, alternatives: Vec::new() });
        }
    }
    let named: Vec<&Candidate> = ctx.candidates.iter().filter(|c| names_word(t, &c.student.name)).collect();
    let distinct = named.iter().map(|c| c.student.name.as_str()).collect::<std::collections::BTreeSet<_>>().len();
    if distinct == 1 {
        // 같은 이름이 두 기계에 있으면 어느 쪽인지는 사람이 고른다.
        return Some(if named.len() == 1 {
            RouteState::Decided { target: student(named[0]), basis: RouteBasis::NamedInText, alternatives: Vec::new() }
        } else {
            even_pick(&named)
        });
    }
    if chars < MIN_ASK_CHARS {
        return Some(RouteState::Hold(HoldReason::TooShort));
    }
    None
}

/// 보낼 본문 — 받는 곳을 가리키려고 친 머리(`@이름`·`/새`·`/나쵸`)는 떼고, 학생에게 가는 글이
/// `/새`·`새 일:` 로 시작하면 새 일이라 창 이름이 될 제목을 함께 준다(답·후속 말에는 제목이 없다).
pub(crate) fn outgoing(text: &str) -> (String, Option<String>) {
    let mut body = text.trim();
    if let Some(rest) = body.strip_prefix('@') {
        body = rest.split_once(char::is_whitespace).map_or("", |(_, rest)| rest).trim_start();
    }
    if let Some(rest) = body.strip_prefix("/나쵸") {
        return (rest.trim().to_owned(), None);
    }
    let fresh = body.strip_prefix("/새").or_else(|| body.strip_prefix("새 일:"));
    match fresh {
        Some(rest) => {
            let rest = rest.trim();
            let title: String = rest.lines().next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
            (rest.to_owned(), (!title.is_empty()).then_some(title))
        }
        None => (body.to_owned(), None),
    }
}

/// 관문 판정 창구에 보낼 본문. 학생 id 는 목록 순번이라 답을 같은 목록으로 되돌려 읽는다.
pub(crate) fn request_body(text: &str, candidates: &[Candidate]) -> Value {
    json!({
        "message": text,
        "students": candidates.iter().enumerate().map(|(i, c)| json!({
            "id": format!("s{i}"),
            "name": c.student.name,
            "title": c.title,
            "latest": c.latest,
            "status": c.status,
        })).collect::<Vec<_>>(),
    })
}

/// 관문 답(`{"probabilities":{"s0":…,"new_task":…,"ask_nacho":…},"latency_ms":…}`)을 상태로.
/// 보낸 적 없는 보기가 섞였거나 모양이 틀리면 실패로 — 추측해서 세우지 않는다.
pub(crate) fn from_answer(answer: &Value, candidates: &[Candidate]) -> RouteState {
    let Some(probs) = answer["probabilities"].as_object() else {
        return RouteState::Failed("판정 답의 모양이 달라요".into());
    };
    let latency_ms = answer["latency_ms"].as_u64().unwrap_or(0);
    let mut options = Vec::with_capacity(probs.len());
    for (key, p) in probs {
        let Some(p) = p.as_f64().filter(|p| (0.0..=1.0).contains(p)) else {
            return RouteState::Failed("판정 답의 확률이 틀려요".into());
        };
        let target = match key.as_str() {
            "new_task" => RouteTarget::NewTask,
            "ask_nacho" => RouteTarget::Nacho,
            id => match id.strip_prefix('s').and_then(|n| n.parse::<usize>().ok()).and_then(|n| candidates.get(n)) {
                Some(c) => student(c),
                None => return RouteState::Failed("판정 답에 없는 학생이 있어요".into()),
            },
        };
        options.push((target, p as f32));
    }
    if options.is_empty() {
        return RouteState::Failed("판정 답이 비었어요".into());
    }
    options.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (target, probability) = options[0].clone();
    if probability >= SURE {
        let alternatives = options.into_iter().skip(1).take(PICK_COUNT - 1).collect();
        RouteState::Decided { target, basis: RouteBasis::Jev { probability, latency_ms }, alternatives }
    } else {
        options.truncate(PICK_COUNT);
        RouteState::Pick { options, latency_ms: Some(latency_ms) }
    }
}

/// 언제 물을지 — 어절 끝이면 곧, 오래 안 물었으면 곧, 아니면 손이 멈춘 뒤.
pub(crate) fn ask_delay(text: &str, since_last_ask: Option<Duration>) -> Duration {
    let word_end = text.chars().next_back().is_some_and(char::is_whitespace);
    let overdue = since_last_ask.map_or(true, |d| d >= ASK_EVERY);
    if word_end || overdue { ASK_SOON } else { ASK_PAUSE }
}

/// 관문에 보낼 판정 하나. 답은 같은 `seq`·`candidates` 로 `Router::answer` 에 되돌린다.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RouteRequest {
    pub(crate) seq: u64,
    pub(crate) text: String,
    pub(crate) candidates: Vec<Candidate>,
}

/// 입력칸 하나의 판정 상태. 네트워크는 모른다 — `due` 가 물을 것을 내주고 `answer` 로 받는다.
#[derive(Debug)]
pub(crate) struct Router {
    seq: u64,
    applied: u64,
    pending: bool,
    last_ask: Option<Instant>,
    scheduled: Option<(Instant, RouteRequest)>,
    state: RouteState,
}

impl Default for Router {
    fn default() -> Self {
        Self { seq: 0, applied: 0, pending: false, last_ask: None, scheduled: None, state: RouteState::Empty }
    }
}

impl Router {
    pub(crate) fn state(&self) -> &RouteState {
        &self.state
    }

    /// 앞 받는 곳을 세워 둔 채 다시 보는 중인가 — 「· 다시 보는 중」만 붙인다.
    pub(crate) fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn highlight(&self) -> Option<RouteHighlight> {
        let tentative = matches!(self.state, RouteState::Pick { .. });
        match self.state.target()? {
            RouteTarget::Student(student) => Some(RouteHighlight { student: student.clone(), tentative }),
            _ => None,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.seq += 1;
        self.applied = self.seq;
        self.pending = false;
        self.scheduled = None;
        self.state = RouteState::Empty;
    }

    /// 후보 칩을 눌렀다 — 다음 글자가 바뀌기 전까지 그 받는 곳으로 선다.
    pub(crate) fn choose(&mut self, target: RouteTarget) {
        self.seq += 1;
        self.applied = self.seq;
        self.pending = false;
        self.scheduled = None;
        self.state = RouteState::Decided { target, basis: RouteBasis::Picked, alternatives: Vec::new() };
    }

    /// 글이 바뀔 때마다. 코드가 정하면 바로 서고, 아니면 물을 때를 잡는다.
    pub(crate) fn update(&mut self, text: &str, ctx: &RouteContext, now: Instant) {
        self.seq += 1;
        if let Some(state) = code_route(text, ctx) {
            self.applied = self.seq;
            self.pending = false;
            self.scheduled = None;
            self.state = state;
            return;
        }
        // 이미 받는 곳이 서 있으면 그대로 두고 「다시 보는 중」만 — 칠 때마다 줄이 깜빡이지 않게.
        if self.state.target().is_some() {
            self.pending = true;
        } else {
            self.state = RouteState::Thinking;
        }
        let delay = ask_delay(text, self.last_ask.map(|at| now.saturating_duration_since(at)));
        let request = RouteRequest { seq: self.seq, text: text.trim().to_owned(), candidates: ctx.candidates.to_vec() };
        self.scheduled = Some((now + delay, request));
    }

    /// 물을 때가 됐으면 판정 하나를 내준다(한 번만).
    pub(crate) fn due(&mut self, now: Instant) -> Option<RouteRequest> {
        if self.scheduled.as_ref().is_some_and(|(at, _)| *at <= now) {
            self.last_ask = Some(now);
            return self.scheduled.take().map(|(_, request)| request);
        }
        None
    }

    /// 다음 판정까지 남은 시간 — 부르는 쪽이 그때 다시 깨도록.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Duration> {
        self.scheduled.as_ref().map(|(at, _)| at.saturating_duration_since(now))
    }

    /// 판정 답. 치는 동안에도 보이게 앞 글에 대한 답도 싣되, 더 새 판단이 이미 섰으면 버린다.
    pub(crate) fn answer(&mut self, request: &RouteRequest, answer: Result<Value, String>) {
        if request.seq < self.applied {
            return;
        }
        self.applied = request.seq;
        self.pending = request.seq < self.seq;
        self.state = match answer {
            Ok(value) => from_answer(&value, &request.candidates),
            Err(message) => RouteState::Failed(message),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, machine: &str, surface: &str, title: &str) -> Candidate {
        Candidate {
            student: StudentRef {
                machine_id: machine.into(),
                machine_label: machine.into(),
                surface_id: surface.into(),
                name: name.into(),
                local: machine == "m1",
            },
            title: title.into(),
            latest: String::new(),
            status: "waiting".into(),
        }
    }

    fn roster() -> Vec<Candidate> {
        vec![cand("유우카", "m1", "%3", "미러링 방식 변경"), cand("케이", "m1", "%5", "계정 설정"), cand("코하루", "m1", "%7", "Pluto 스타 AI")]
    }

    fn ctx<'a>(c: &'a [Candidate], selected: Option<&'a StudentRef>, last: Option<&'a StudentRef>) -> RouteContext<'a> {
        RouteContext { candidates: c, selected, last_sent: last }
    }

    fn decided_name(state: &RouteState) -> Option<(&str, &RouteBasis)> {
        match state {
            RouteState::Decided { target: RouteTarget::Student(s), basis, .. } => Some((s.name.as_str(), basis)),
            _ => None,
        }
    }

    #[test]
    fn mention_prefix_and_ambiguous_prefix() {
        let c = roster();
        let s = code_route("@유우 다시 봐줘", &ctx(&c, None, None)).unwrap();
        assert_eq!(decided_name(&s), Some(("유우카", &RouteBasis::Mention)));
        assert_eq!(code_route("@아로나 해줘", &ctx(&c, None, None)), Some(RouteState::Hold(HoldReason::UnknownName("아로나".into()))));
        let two = vec![cand("유우카", "m1", "%3", ""), cand("유우카", "m2", "%9", "")];
        assert!(matches!(code_route("@유우카 봐줘", &ctx(&two, None, None)), Some(RouteState::Pick { .. })));
    }

    #[test]
    fn commands_and_short_text() {
        let c = roster();
        assert!(matches!(code_route("/새 알림 소리 설정", &ctx(&c, None, None)), Some(RouteState::Decided { target: RouteTarget::NewTask, .. })));
        assert!(matches!(code_route("나쵸, 오늘 뭐 끝났어", &ctx(&c, None, None)), Some(RouteState::Decided { target: RouteTarget::Nacho, .. })));
        assert_eq!(code_route(" 가", &ctx(&c, None, None)), Some(RouteState::Empty));
        assert_eq!(code_route("빌드", &ctx(&c, None, None)), Some(RouteState::Hold(HoldReason::TooShort)));
    }

    #[test]
    fn deictic_goes_to_selected_then_last_sent() {
        let c = roster();
        let kei = c[1].student.clone();
        let koharu = c[2].student.clone();
        let s = code_route("그거 다시 해봐", &ctx(&c, Some(&kei), Some(&koharu))).unwrap();
        assert_eq!(decided_name(&s), Some(("케이", &RouteBasis::Selected)));
        let s = code_route("그거 다시 해봐", &ctx(&c, None, Some(&koharu))).unwrap();
        assert_eq!(decided_name(&s), Some(("코하루", &RouteBasis::LastSent)));
        // 고른 학생이 있어도 가리키는 말이 아니면 Jev 로 — 고른 학생에 끌려가지 않게.
        assert_eq!(code_route("투혼에서 저그 풀러시 상대로 돌려", &ctx(&c, Some(&kei), None)), None);
    }

    #[test]
    fn name_in_text_needs_a_word_boundary() {
        let c = roster();
        let s = code_route("유우카한테 미러링 다시 봐 달라고 해", &ctx(&c, None, None)).unwrap();
        assert_eq!(decided_name(&s), Some(("유우카", &RouteBasis::NamedInText)));
        assert!(decided_name(&code_route("하단바는 케이가 보고 있지?", &ctx(&c, None, None)).unwrap()).is_some());
        // 「케이스」의 케이는 이름이 아니다.
        assert_eq!(code_route("테스트 케이스 하나 더 추가해", &ctx(&c, None, None)), None);
        // 이름이 둘이면 코드가 못 정한다.
        assert_eq!(code_route("유우카랑 케이 둘 다 멈춰", &ctx(&c, None, None)), None);
    }

    #[test]
    fn answer_threshold_and_validation() {
        let c = roster();
        let sure = from_answer(&json!({"probabilities":{"s0":0.96,"s1":0.02,"new_task":0.01,"ask_nacho":0.01},"latency_ms":240}), &c);
        match &sure {
            RouteState::Decided { target: RouteTarget::Student(s), basis: RouteBasis::Jev { latency_ms: 240, .. }, alternatives } => {
                assert_eq!(s.name, "유우카");
                assert_eq!(alternatives.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        let unsure = from_answer(&json!({"probabilities":{"s2":0.65,"new_task":0.29,"ask_nacho":0.05,"s0":0.01},"latency_ms":347}), &c);
        match unsure {
            RouteState::Pick { options, latency_ms: Some(347) } => {
                assert_eq!(options.len(), 3);
                assert_eq!(options[0].0, RouteTarget::Student(c[2].student.clone()));
                assert_eq!(options[1].0, RouteTarget::NewTask);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(from_answer(&json!({"probabilities":{"s9":1.0}}), &c), RouteState::Failed(_)));
        assert!(matches!(from_answer(&json!({"probabilities":{"s0":1.5}}), &c), RouteState::Failed(_)));
        assert!(matches!(from_answer(&json!({"choice":"s0"}), &c), RouteState::Failed(_)));
    }

    #[test]
    fn outgoing_strips_routing_marks_and_titles_only_new_work() {
        assert_eq!(outgoing("@유우 다시 봐줘"), ("다시 봐줘".into(), None));
        assert_eq!(outgoing("@케이 /새 로그인 화면 정리\n세부는 이렇게"), ("로그인 화면 정리\n세부는 이렇게".into(), Some("로그인 화면 정리".into())));
        assert_eq!(outgoing("/새 알림 소리 설정"), ("알림 소리 설정".into(), Some("알림 소리 설정".into())));
        assert_eq!(outgoing("/나쵸 오늘 뭐 끝났어"), ("오늘 뭐 끝났어".into(), None));
        assert_eq!(outgoing("미러링 다시 봐줘"), ("미러링 다시 봐줘".into(), None));
        assert_eq!(outgoing("@케이").0, "");
    }

    #[test]
    fn ask_timing() {
        assert_eq!(ask_delay("투혼에서 ", Some(Duration::from_millis(100))), ASK_SOON);
        assert_eq!(ask_delay("투혼에서", Some(Duration::from_millis(100))), ASK_PAUSE);
        assert_eq!(ask_delay("투혼에서", Some(Duration::from_millis(950))), ASK_SOON);
        assert_eq!(ask_delay("투혼에서", None), ASK_SOON);
    }

    #[test]
    fn router_keeps_target_while_rejudging_and_drops_stale_answers() {
        let c = roster();
        let t0 = Instant::now();
        let mut r = Router::default();
        r.update("미러링에서 회색 추천", &ctx(&c, None, None), t0);
        assert_eq!(*r.state(), RouteState::Thinking);
        let first = r.due(t0 + Duration::from_secs(1)).unwrap();
        assert!(r.due(t0 + Duration::from_secs(1)).is_none());
        r.update("미러링에서 회색 추천 문구", &ctx(&c, None, None), t0 + Duration::from_millis(1100));
        let second = r.due(t0 + Duration::from_secs(2)).unwrap();
        // 늦게 온 앞 글의 답도 아직 더 새 답이 없으면 싣는다 — 치는 동안 보이게.
        r.answer(&first, Ok(json!({"probabilities":{"s0":0.9,"new_task":0.1},"latency_ms":300})));
        assert_eq!(r.highlight().map(|h| h.student.name), Some("유우카".into()));
        assert!(r.pending());
        r.answer(&second, Ok(json!({"probabilities":{"s1":0.6,"s0":0.4},"latency_ms":200})));
        let h = r.highlight().unwrap();
        assert_eq!((h.student.name.as_str(), h.tentative), ("케이", true));
        assert!(!r.pending());
        // 새 답이 선 뒤에 온 옛 답은 버린다.
        r.answer(&first, Ok(json!({"probabilities":{"s2":1.0}})));
        assert_eq!(r.highlight().unwrap().student.name, "케이");
    }

    #[test]
    fn picked_chip_wins_over_an_answer_that_was_already_in_flight() {
        let c = roster();
        let t0 = Instant::now();
        let mut r = Router::default();
        r.update("슬랙 알림 소리 켜는 설정", &ctx(&c, None, None), t0);
        let asked = r.due(t0 + Duration::from_secs(1)).unwrap();
        r.choose(RouteTarget::NewTask);
        r.answer(&asked, Ok(json!({"probabilities":{"s1":1.0}})));
        assert!(matches!(r.state(), RouteState::Decided { target: RouteTarget::NewTask, basis: RouteBasis::Picked, .. }));
        assert!(r.highlight().is_none());
    }

    #[test]
    fn code_decision_overrides_inflight_answer_and_failures_surface() {
        let c = roster();
        let t0 = Instant::now();
        let mut r = Router::default();
        r.update("미러링에서 회색 추천", &ctx(&c, None, None), t0);
        let asked = r.due(t0 + Duration::from_secs(1)).unwrap();
        r.update("@케이 미러링에서 회색 추천", &ctx(&c, None, None), t0 + Duration::from_secs(1));
        r.answer(&asked, Ok(json!({"probabilities":{"s0":1.0}})));
        assert_eq!(r.highlight().unwrap().student.name, "케이");
        r.update("슬랙 알림 소리 켜는 설정", &ctx(&c, None, None), t0 + Duration::from_secs(2));
        let asked = r.due(t0 + Duration::from_secs(3)).unwrap();
        r.answer(&asked, Err("판정 창구에 닿지 못했어요".into()));
        assert_eq!(*r.state(), RouteState::Failed("판정 창구에 닿지 못했어요".into()));
        assert!(r.highlight().is_none());
    }
}
