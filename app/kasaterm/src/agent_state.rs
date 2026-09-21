//! pane 의 에이전트가 지금 무엇을 하는가 — **한 곳의 판정**.
//!
//! 전에는 화면 글자(스피너 글리프·승인 프롬프트·`▰▰▱ %`·연결 끊김 문구)를 읽어 정했다.
//! 하네스 UI 가 바뀔 때마다 깨졌다 — Windows 의 `*` 스피너, reduce-motion 의 `●`, codex 의
//! 장식 점자, `✻ Brewed for …` 완료 줄, 입력창 아래 서브에이전트 목록의 `●`. 게다가 헤더
//! 바·사이드바·보드가 저마다 다른 블렌드를 써서 시로코 하나를 두고 「idle」과 「working」이
//! 동시에 떴다(2026-09-17). 여기서는 **하네스가 스스로 알린 것**만 재료로 쓴다:
//!
//! - 훅 턴 경계(UserPromptSubmit 이 열고 Stop 이 닫는다) · PreCompact / SessionStart(compact)
//! - 기록(transcript) 턴 경계 `transcript::turn_state_from_tail` 와 마지막 오류
//! - Notification 훅의 attention(승인·질문·방치)
//! - claude 명부 `agents --json` 의 idle|busy|waiting
//! - PTY 출력 박동(살아 있나만) · Enter 직후 브리지 · codex·agy 전용 화면 승인 폴백
//!
//! 화면은 이제 그림 자리(스프라이트 앵커)와 압축 % 장식에만 쓴다. `resolve` 는 순수 함수라
//! 표로 시험하고, `StateHub` 가 재료를 모아 GUI 틱과 보드 빌더 양쪽에 같은 답을 준다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::state::{HookActivity, HookTurn, TurnObservation, TURN_STALE};
use crate::stream::AttentionFlag;
use crate::transcript::{HarnessError, TurnState};

/// 사람이 답해야 하는 이유의 종류. Notification 훅의 `permission_prompt` ·
/// `elicitation*`/`agent_needs_input` · `idle_prompt` 에 대응한다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitKind {
    Permission,
    Question,
    Idle,
}

impl WaitKind {
    pub(crate) fn parse(kind: &str) -> Option<Self> {
        match kind {
            "permission" => Some(Self::Permission),
            "question" => Some(Self::Question),
            "idle" => Some(Self::Idle),
            _ => None,
        }
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Idle => "idle",
        }
    }
    /// 이유가 비어 왔을 때 펫·토스트에 쓸 기본 문구.
    pub(crate) fn default_reason(self) -> &'static str {
        match self {
            Self::Permission => "승인 대기",
            Self::Question => "답 기다림",
            Self::Idle => "다음 지시 기다림",
        }
    }
}

/// `PaneState` 가 아닌 것은 그 이름이 BSP pane 구조체라서다.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum AgentState {
    /// 재료가 없다 — 원격 거울에 사실이 안 실렸거나, 지원 안 하는 하네스.
    #[default]
    Unknown,
    Idle,
    Working,
    Compacting,
    Waiting { kind: WaitKind, reason: String },
    Error { label: String },
}

impl AgentState {
    pub(crate) fn is_busy(&self) -> bool {
        matches!(self, Self::Working | Self::Compacting)
    }
    /// 사람 손이 있어야 풀리는가 — 승인·질문뿐이다. 60초 방치(idle_prompt)는 「답을 마치고
    /// 다음 지시를 기다림」이라 보드에는 waiting/idle 로 실리지만 주황 깜빡임·알림·tell 거부는
    /// 안 받는다(2026-09-18 「선택하는 거 아닌데 왜 주황색 깜빡임」).
    pub(crate) fn needs_you(&self) -> bool {
        matches!(self, Self::Waiting { kind: WaitKind::Permission | WaitKind::Question, .. })
    }
    /// 같은 상태인가(이유·라벨 글자는 무시) — 전이 관찰과 `since` 계산용.
    pub(crate) fn same_kind(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Waiting { kind: a, .. }, Self::Waiting { kind: b, .. }) => a == b,
            (Self::Error { .. }, Self::Error { .. }) => true,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
    /// 보드 `status` 칸 낱말. 소비자가 정확 일치로 비교하므로 넷뿐이다.
    pub(crate) fn board_word(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle | Self::Error { .. } => "idle",
            Self::Working | Self::Compacting => "working",
            Self::Waiting { .. } => "waiting",
        }
    }
    /// GUI `PaneStatusView.status` 낱말(compacting 은 헤더 바 모양이 다르다).
    pub(crate) fn view_word(&self) -> &'static str {
        match self {
            Self::Unknown | Self::Idle | Self::Error { .. } => "idle",
            Self::Working => "working",
            Self::Compacting => "compacting",
            Self::Waiting { .. } => "waiting",
        }
    }
}

/// 명부(`claude agents --json`)가 말하는 것.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Official {
    Idle,
    Busy,
    Waiting { error: bool },
}

/// 판정 재료 — 화면 셀은 없다. 시각은 「지금으로부터 얼마 전」으로 담아 시험이 시계를
/// 안 만진다.
#[derive(Clone, Debug, Default)]
pub(crate) struct Evidence {
    /// 다른 기기의 거울 pane — 그쪽 판이 준 낱말(`working`·`idle`·`waiting`·`unknown`)과 사유.
    /// (보드 낱말, 기다리는 이유, 그 기다림의 종류). 종류는 옛 판 기계에서 안 온다.
    pub remote: Option<(String, Option<String>, Option<WaitKind>)>,
    /// None = 셸(에이전트가 안 돈다).
    pub harness: Option<kasa_pty::AgentKind>,
    /// 훅이 알린 턴 경계와 그 나이.
    pub hook_turn: Option<(HookTurn, Duration)>,
    /// 마지막으로 어떤 훅이든 이 pane 에서 온 지.
    pub hook_beat: Option<Duration>,
    /// PreCompact 이후 지난 시간(SessionStart(compact) 가 지운다).
    pub compact_age: Option<Duration>,
    /// 기록: 턴 상태·mtime 나이·있음·마지막 오류.
    pub transcript_turn: Option<TurnState>,
    pub transcript_age: Option<Duration>,
    pub transcript_present: bool,
    pub transcript_error: Option<HarnessError>,
    /// attention 훅: 종류·이유·나이.
    pub attention: Option<(WaitKind, String, Option<Duration>)>,
    pub official: Option<Official>,
    /// PTY 가 최근에 바이트를 내고 있나(스피너 재그리기 포함) — 살아 있나만 말한다.
    pub heartbeat: bool,
    /// 사람이 Enter 를 친 지.
    pub last_submit_age: Option<Duration>,
    /// 화면이 지금 보여 주는 것 — 훅·기록을 **대조**하는 둘째 눈(2026-09-18 「둘 다 확인해서
    /// 정확하게」). 정본이 아니라, 정본이 없거나 정본과 어긋날 때만 판정을 바꾼다. None 은 그
    /// pane 의 격자를 못 본 것(원격·아직 스캔 전) — 그때는 화면 규칙이 전부 쉰다.
    pub screen: Option<ScreenSigns>,
    /// 훅·기록이 본 뒤 작업(서브에이전트·백그라운드).
    pub bg_active: bool,
    pub intent: String,
}

/// GUI 스캔이 화면 격자에서 읽어 오는 표식들. 셀은 안 들고 온다 — 있다/없다와 짧은 라벨뿐.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScreenSigns {
    /// 살아 있는 스피너 행(`find_claude_spinner`: 첫 글리프·줄임표·경과시간 괄호·아래에
    /// 대화 마커 없음)이 보인다.
    pub spinner: bool,
    /// 승인 위젯(`rows_show_approval_prompt`)이 떠 있다 — 라벨은 위젯 종류.
    pub approval: Option<String>,
    /// 끊김 문구(`find_connection_trouble`)가 화면 아래 몇 줄 안에 있다.
    pub trouble: Option<&'static str>,
}

/// 판정 결과.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Resolved {
    pub state: AgentState,
    /// 보드 `status_reason` — 판정의 근거 한 마디.
    pub reason: &'static str,
    pub bg_active: bool,
    /// 미니맵 삼각형 — Error 상태이거나, 도는 중에 도구가 실패한 것이 마지막 활동일 때.
    pub has_error: bool,
    pub intent: String,
    /// 이 상태에 들어온 때. 「14분째」의 기준점.
    pub since: Instant,
}

/// Enter 직후 이만큼은 훅·기록이 아직 안 왔어도 열린 턴으로 본다 — UserPromptSubmit 이
/// 몇 ms, 기록의 첫 줄이 한 틱 뒤에 오는 사이를 잇는다. PTY 입력이라 화면 판독이 아니다.
pub(crate) const SUBMIT_BRIDGE: Duration = Duration::from_secs(4);
/// PreCompact 뒤 이만큼 지나면 압축은 끝난 것으로 본다(SessionStart(compact) 를 놓쳐도).
const COMPACT_MAX: Duration = Duration::from_secs(600);
/// 열린 턴인데 화면에 스피너가 없고 출력 박동도 없는 채 이만큼 지나면 닫힌 것으로 본다 —
/// 닫는 줄을 못 남긴 턴(API 오류·빈 답)을 `TURN_STALE`(90초)보다 훨씬 일찍 접는다. 승인 위젯이
/// 떠 있으면 스피너가 없는 게 정상이라 이 시계를 안 돌린다.
const QUIET_CLOSE: Duration = Duration::from_secs(6);
/// agy 는 턴 경계를 안 남긴다 — 기록이 이만큼 안에 자랐으면 도는 중.
const AGY_ACTIVE: Duration = Duration::from_secs(15);

/// 다른 기계가 보낸 낱말을 되살린다. `kind` 는 그쪽 보드의 `attention_kind` — 없으면
/// 승인으로 친다. 방치(`idle`)를 승인으로 잘못 보는 편이 그 반대보다 덜 위험해서가
/// 아니라, 옛 판 기계는 그 칸을 안 보내기 때문이다. 보내 주면 그대로 가른다.
fn remote_state(word: &str, reason: Option<&str>, kind: Option<WaitKind>) -> AgentState {
    match word {
        "working" => AgentState::Working,
        "idle" => AgentState::Idle,
        "waiting" => AgentState::Waiting {
            kind: kind.unwrap_or(WaitKind::Permission),
            reason: reason.unwrap_or("").to_string(),
        },
        _ => AgentState::Unknown,
    }
}

/// 턴이 열려 있나 — 훅과 기록 중 **더 새로운** 경계가 정하고, 그 위에 staleness 를 건다.
/// 둘 다 없으면 명부·브리지·agy 폴백.
fn turn_open(e: &Evidence) -> Option<&'static str> {
    // Enter 브리지는 **그 Enter 뒤에 아무 신호도 안 온 동안**만 잇는다 — 훅이나 기록이 그
    // 뒤에 말했으면(열렸든 닫혔든) 그쪽이 정본이다. Enter 3초 뒤 Stop 이 닫았는데 브리지가
    // 1초를 더 「일하는 중」으로 붙잡던 것(2026-09-18 리그 실측).
    let bridge = e.last_submit_age.is_some_and(|submit| {
        submit < SUBMIT_BRIDGE
            && [e.hook_turn.map(|(_, at)| at), e.transcript_age]
                .into_iter()
                .flatten()
                .all(|signal| signal > submit)
    });
    let hook = e.hook_turn;
    let transcript = e.transcript_turn.zip(e.transcript_age);
    let newest = match (hook, transcript) {
        (Some((h, ha)), Some((t, ta))) => {
            if ha <= ta {
                Some((h == HookTurn::Open, "hook turn open", ha))
            } else {
                Some((t == TurnState::Working, "transcript turn open", ta))
            }
        }
        (Some((h, ha)), None) => Some((h == HookTurn::Open, "hook turn open", ha)),
        (None, Some((t, ta))) => Some((t == TurnState::Working, "transcript turn open", ta)),
        (None, None) => None,
    };
    match newest {
        Some((true, reason, _)) => {
            // 마지막 신호(훅 박동·훅 경계·기록 mtime) 중 가장 새것이 낡았고 박동도 없으면
            // 닫는 줄을 못 남긴 턴이다.
            let freshest = [e.hook_beat, e.hook_turn.map(|(_, a)| a), e.transcript_age]
                .into_iter()
                .flatten()
                .min();
            let stale = freshest.is_none_or(|age| age >= TURN_STALE) && !e.heartbeat;
            // 화면 대조: 스피너도 박동도 승인 위젯도 없이 몇 초 조용하면 90초를 안 기다린다.
            let quiet = e.screen.as_ref().is_some_and(|s| !s.spinner && s.approval.is_none())
                && !e.heartbeat
                && freshest.is_some_and(|age| age >= QUIET_CLOSE);
            if stale {
                bridge.then_some("enter bridge")
            } else if quiet {
                None
            } else {
                Some(reason)
            }
        }
        Some((false, _, _)) => {
            if bridge {
                Some("enter bridge")
            } else if e.screen.as_ref().is_some_and(|s| s.spinner) && e.heartbeat {
                // 훅·기록은 닫혔다는데 화면은 살아 돈다 — 훅이 죽었거나(설정 오류) 기록이
                // 밀린 것이다. 둘 다 확인하기로 했으니 도는 쪽을 믿는다.
                Some("screen spinner")
            } else {
                None
            }
        }
        None if bridge => Some("enter bridge"),
        None => match e.harness {
            Some(kasa_pty::AgentKind::Agy) => {
                let fresh = e.transcript_age.is_some_and(|age| age < AGY_ACTIVE);
                (fresh || e.heartbeat).then_some("agy fresh")
            }
            _ => {
                if matches!(e.official, Some(Official::Busy)) {
                    Some("official busy")
                } else if e.screen.as_ref().is_some_and(|s| s.spinner) && e.heartbeat {
                    // 훅도 기록도 명부도 없는 하네스(gemini·hermes…) — 화면 스피너가 유일하다.
                    // 박동까지 요구하는 이유: 스크롤백에 굳은 스피너 글자로 영영 working 이 되지 않게.
                    Some("screen spinner")
                } else {
                    None
                }
            }
        },
    }
}

/// attention 이 아직 유효한가 — 그보다 새로운 Stop·기록 성장·훅 박동은 사람이 답했다는 뜻.
fn attention_live(e: &Evidence, kind: WaitKind, age: Option<Duration>) -> bool {
    let Some(age) = age else { return true };
    let newer = |other: Option<Duration>| other.is_some_and(|o| o < age);
    match kind {
        WaitKind::Permission | WaitKind::Question => {
            let closed = e
                .hook_turn
                .is_some_and(|(turn, at)| turn == HookTurn::Closed && at < age);
            !(closed || newer(e.transcript_age) || newer(e.hook_beat))
        }
        WaitKind::Idle => {
            let reopened = e
                .hook_turn
                .is_some_and(|(turn, at)| turn == HookTurn::Open && at < age)
                || (e.transcript_turn == Some(TurnState::Working) && newer(e.transcript_age));
            !reopened
        }
    }
}

/// 순수 판정. 우선순위는 위에서 아래 — 첫 줄이 맞으면 거기서 끝.
pub(crate) fn resolve(e: &Evidence) -> (AgentState, &'static str) {
    if let Some((word, reason, kind)) = &e.remote {
        return (remote_state(word, reason.as_deref(), *kind), "remote");
    }
    if e.harness.is_none() {
        return (AgentState::Idle, "shell");
    }
    // 화면이 살아 돈다(스피너 + 출력 박동) = 사람이 이미 답했다. 훅 attention 표식은 Stop·기록
    // 성장이 와야 풀리는데 그 사이 몇 초를 「대기」로 남기던 것을 화면이 먼저 푼다.
    let screen_running = e.screen.as_ref().is_some_and(|s| s.spinner) && e.heartbeat;
    if let Some((kind, reason, age)) = &e.attention {
        if matches!(kind, WaitKind::Permission | WaitKind::Question)
            && attention_live(e, *kind, *age)
            && !screen_running
        {
            let reason = if reason.trim().is_empty() { kind.default_reason().to_string() } else { reason.clone() };
            return (AgentState::Waiting { kind: *kind, reason }, "hook attention");
        }
    }
    if let Some((reason, false)) = e.screen.as_ref().and_then(|s| s.approval.as_ref().map(|r| (r, s.spinner))) {
        // 승인 위젯이 떠 있으면 하네스가 무엇이든 사람 차례다. claude 는 보통 attention 훅이
        // 먼저 오지만, 훅이 안 걸린 세션(옛 --settings)이나 훅이 죽은 날엔 이게 유일한 눈이다
        // (2026-09-18 실측: 따옴표 버그로 턴 훅이 하루 동안 안 닿았다). 스피너가 같이 보이면
        // 위젯 글자는 본문 인용이다 — 진짜 위젯이 떠 있는 동안 스피너는 안 돈다.
        return (
            AgentState::Waiting { kind: WaitKind::Permission, reason: reason.clone() },
            "screen approval",
        );
    }
    if matches!(e.official, Some(Official::Waiting { error: false })) {
        let signal_newer = e.hook_beat.is_some_and(|a| a < Duration::from_secs(5))
            || e.transcript_age.is_some_and(|a| a < Duration::from_secs(5));
        if !signal_newer {
            return (
                AgentState::Waiting { kind: WaitKind::Permission, reason: "승인 대기".into() },
                "official waiting",
            );
        }
    }
    if let Some(age) = e.compact_age {
        let closed_after = e
            .hook_turn
            .is_some_and(|(turn, at)| turn == HookTurn::Closed && at < age);
        if age < COMPACT_MAX && !closed_after {
            return (AgentState::Compacting, "precompact hook");
        }
    }
    if let Some(reason) = turn_open(e) {
        return (AgentState::Working, reason);
    }
    if let Some(err) = &e.transcript_error {
        if err.hard {
            return (AgentState::Error { label: err.label.clone() }, "harness error");
        }
    }
    if let Some(label) = e.screen.as_ref().and_then(|s| s.trouble) {
        // 끊김 문구가 화면 아래에 있다 — 기록엔 안 남는 종류(재시도 중·오프라인)도 잡는다.
        return (AgentState::Error { label: label.to_string() }, "screen trouble");
    }
    if matches!(e.official, Some(Official::Waiting { error: true })) {
        return (AgentState::Error { label: "오류 복구 대기".into() }, "official error");
    }
    if let Some((WaitKind::Idle, reason, age)) = &e.attention {
        if attention_live(e, WaitKind::Idle, *age) {
            let reason = if reason.trim().is_empty() { WaitKind::Idle.default_reason().to_string() } else { reason.clone() };
            return (AgentState::Waiting { kind: WaitKind::Idle, reason }, "idle prompt");
        }
    }
    if e.transcript_present || e.hook_turn.is_some() {
        return (AgentState::Idle, "turn closed");
    }
    (AgentState::Unknown, "no evidence")
}

/// 훅·기록·명부·박동을 모으는 자리. GUI 틱과 소켓의 보드 빌더가 같은 캐시를 읽는다.
#[derive(Default)]
pub(crate) struct StateHub {
    pub attention: Arc<Mutex<HashMap<String, AttentionFlag>>>,
    pub hook_activity: Arc<Mutex<HashMap<String, HookActivity>>>,
    /// pane → 결속된 기록 파일. `PtyBackend` 가 같은 맵을 쓴다(bind hook·discover 가 채움).
    pub bound: Arc<Mutex<HashMap<String, PathBuf>>>,
    pub turn_hooks: Mutex<HashMap<String, (HookTurn, Instant)>>,
    pub hook_beat: Mutex<HashMap<String, Instant>>,
    pub compact: Mutex<HashMap<String, Instant>>,
    pub perm_mode: Mutex<HashMap<String, String>>,
    /// pane 의 화면 표식 — GUI 스캔(`refresh_pane_activity`)이 틱마다 채운다.
    pub screen: Mutex<HashMap<String, ScreenSigns>>,
    transcript: Mutex<HashMap<String, TurnObservation>>,
    resolved: Mutex<(Option<Instant>, HashMap<String, Resolved>)>,
}

/// 같은 틱 안에서 두 번 읽어도 파일은 한 번만 본다.
const REFRESH_MEMO: Duration = Duration::from_millis(250);
/// 기록 꼬리 창 — 턴 경계·오류·의도는 끝 몇 줄에 있다.
const TAIL_WINDOW: u64 = 32 * 1024;

impl StateHub {
    // ── 훅이 부르는 쓰기 ──────────────────────────────────────────────
    pub(crate) fn beat(&self, surface: &str) {
        self.hook_beat.lock().unwrap().insert(surface.to_string(), Instant::now());
        self.invalidate();
    }
    /// 재료가 바뀌었다 — 다음 `refresh` 는 메모를 건너뛰고 다시 판정한다. 훅 직후 보드가
    /// 옛 판정을 읽던 한 박자(최대 250ms, 관측 주기까지 겹치면 2초)를 없앤다.
    pub(crate) fn invalidate(&self) {
        self.resolved.lock().unwrap().0 = None;
    }
    /// `surface.turn` 의 phase 하나를 적는다. 모르는 phase 는 무시.
    pub(crate) fn turn(&self, surface: &str, phase: &str, permission_mode: Option<&str>) {
        let now = Instant::now();
        let key = surface.to_string();
        match phase {
            "start" => {
                self.turn_hooks.lock().unwrap().insert(key.clone(), (HookTurn::Open, now));
                self.compact.lock().unwrap().remove(surface);
                let mut attention = self.attention.lock().unwrap();
                if attention.get(surface).is_some_and(|f| f.kind == "idle") {
                    attention.remove(surface);
                }
            }
            "end" => {
                self.turn_hooks.lock().unwrap().insert(key.clone(), (HookTurn::Closed, now));
                self.compact.lock().unwrap().remove(surface);
            }
            "compact_start" => {
                self.compact.lock().unwrap().insert(key.clone(), now);
            }
            "compact_end" => {
                self.compact.lock().unwrap().remove(surface);
            }
            "reset" => {
                self.turn_hooks.lock().unwrap().remove(surface);
                self.compact.lock().unwrap().remove(surface);
                self.attention.lock().unwrap().remove(surface);
                self.perm_mode.lock().unwrap().remove(surface);
            }
            _ => return,
        }
        if let Some(mode) = permission_mode.filter(|m| !m.is_empty()) {
            self.perm_mode.lock().unwrap().insert(key.clone(), mode.to_string());
        }
        self.hook_beat.lock().unwrap().insert(key, now);
        self.invalidate();
    }
    pub(crate) fn permission_mode(&self, surface: &str) -> Option<String> {
        self.perm_mode.lock().unwrap().get(surface).cloned()
    }
    // ── 읽기 ──────────────────────────────────────────────────────────
    pub(crate) fn state(&self, surface: &str) -> AgentState {
        self.resolved
            .lock()
            .unwrap()
            .1
            .get(surface)
            .map(|r| r.state.clone())
            .unwrap_or_default()
    }
    pub(crate) fn resolved(&self, surface: &str) -> Option<Resolved> {
        self.resolved.lock().unwrap().1.get(surface).cloned()
    }
    /// 이사·정지 게이트용 — 모르는 것은 일하는 중으로 친다(턴을 자르는 쪽이 더 나쁘다).
    pub(crate) fn is_working(&self, surface: &str) -> bool {
        matches!(self.state(surface), AgentState::Working | AgentState::Compacting | AgentState::Unknown)
    }

    /// 재료를 모아 판정을 갱신한다. 250ms 안에 또 부르면 그대로 둔다.
    pub(crate) fn refresh(&self) {
        let now = Instant::now();
        {
            let memo = self.resolved.lock().unwrap();
            if memo.0.is_some_and(|at| now.duration_since(at) < REFRESH_MEMO) {
                return;
            }
        }
        let live = kasa_pty::live_sessions();
        // 닫힌 pane 의 재료는 여기서 잊는다 — 훅은 닫힘을 알려 주지 않는다.
        self.turn_hooks.lock().unwrap().retain(|id, _| live.contains(id));
        for map in [&self.hook_beat, &self.compact] {
            map.lock().unwrap().retain(|id, _| live.contains(id));
        }
        self.perm_mode.lock().unwrap().retain(|id, _| live.contains(id));
        self.screen.lock().unwrap().retain(|id, _| live.contains(id));
        let official = crate::socket::agents_status_cached();
        let official_errors = crate::socket::agents_error_sids_cached();
        let bound: HashMap<String, PathBuf> = match self.bound.try_lock() {
            Ok(b) => b.clone(),
            // 보드 빌더가 512KB 를 읽는 동안 잡고 있으면 GUI 틱이 그 뒤에 서면 안 된다 —
            // 지난 관측의 경로를 그대로 쓴다.
            Err(_) => self.transcript.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.path.clone())).collect(),
        };
        let mut next: HashMap<String, Resolved> = HashMap::with_capacity(live.len());
        let previous = std::mem::take(&mut self.resolved.lock().unwrap().1);
        let mut transcript = self.transcript.lock().unwrap();
        transcript.retain(|id, _| live.contains(id));
        for id in &live {
            let session = kasa_pty::lookup_session(id);
            let harness = session.as_ref().and_then(|p| p.active_agent());
            let mut evidence = Evidence { harness, ..Default::default() };
            if kasa_mcp::remote::is_remote_pane(id) {
                let facts = kasa_mcp::remote::cached_pane(id);
                let word = facts
                    .as_ref()
                    .and_then(|v| v.get("status").and_then(|s| s.as_str()))
                    .unwrap_or("unknown")
                    .to_string();
                let why = facts
                    .as_ref()
                    .and_then(|v| v.get("waiting_for").and_then(|s| s.as_str()))
                    .map(str::to_string);
                let kind = facts
                    .as_ref()
                    .and_then(|v| v.get("attention_kind").and_then(|s| s.as_str()))
                    .and_then(WaitKind::parse);
                evidence.remote = Some((word, why, kind));
            } else if harness.is_some() {
                if let Some(p) = session.as_ref() {
                    evidence.heartbeat = p.output_heartbeat();
                    evidence.last_submit_age = p.last_submit().map(|s| now.duration_since(s));
                }
                if let Some(obs) = Self::observe(&mut transcript, id, bound.get(id)) {
                    evidence.transcript_turn = Some(obs.state);
                    evidence.transcript_age = obs.mtime.and_then(|m| m.elapsed().ok());
                    evidence.transcript_present = true;
                    evidence.transcript_error = obs.error.clone();
                    evidence.bg_active = obs.bg;
                    evidence.intent = obs.intent.clone();
                    let sid = obs.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    evidence.official = match official.get(&sid).map(String::as_str) {
                        Some("busy") => Some(Official::Busy),
                        Some("waiting") => Some(Official::Waiting { error: official_errors.contains(&sid) }),
                        Some("idle") => Some(Official::Idle),
                        _ => None,
                    };
                }
                evidence.hook_turn = self
                    .turn_hooks
                    .lock()
                    .unwrap()
                    .get(id)
                    .map(|(turn, at)| (*turn, now.duration_since(*at)));
                evidence.hook_beat = self.hook_beat.lock().unwrap().get(id).map(|at| now.duration_since(*at));
                evidence.compact_age = self.compact.lock().unwrap().get(id).map(|at| now.duration_since(*at));
                evidence.attention = self.attention.try_lock().ok().and_then(|a| {
                    a.get(id).and_then(|f| {
                        WaitKind::parse(&f.kind)
                            .map(|kind| (kind, f.reason.clone(), f.at.map(|at| now.duration_since(at))))
                    })
                });
                evidence.bg_active |= self
                    .hook_activity
                    .try_lock()
                    .ok()
                    .is_some_and(|h| h.get(id).is_some_and(|a| !a.is_empty()));
                evidence.screen = self.screen.lock().unwrap().get(id).cloned();
            }
            let (state, reason) = resolve(&evidence);
            let since = previous
                .get(id)
                .filter(|prev| prev.state.same_kind(&state))
                .map(|prev| prev.since)
                .unwrap_or(now);
            let has_error = matches!(state, AgentState::Error { .. })
                || evidence.transcript_error.is_some()
                || matches!(evidence.official, Some(Official::Waiting { error: true }));
            next.insert(
                id.clone(),
                Resolved { state, reason, bg_active: evidence.bg_active, has_error, intent: evidence.intent, since },
            );
        }
        drop(transcript);
        let mut memo = self.resolved.lock().unwrap();
        *memo = (Some(now), next);
    }

    /// 기록 파일을 stat 하고, 모습(길이·mtime)이 바뀐 것만 다시 읽는다.
    fn observe<'a>(
        cache: &'a mut HashMap<String, TurnObservation>,
        id: &str,
        bound: Option<&PathBuf>,
    ) -> Option<&'a TurnObservation> {
        let path = bound.cloned().or_else(|| cache.get(id).map(|o| o.path.clone()))?;
        let Ok(meta) = std::fs::metadata(&path) else {
            cache.remove(id);
            return None;
        };
        let (len, mtime) = (meta.len(), meta.modified().ok());
        let unchanged = cache
            .get(id)
            .is_some_and(|o| o.path == path && o.len == len && o.mtime == mtime);
        if !unchanged {
            let (tail, idle) = crate::socket::read_tail(&path, TAIL_WINDOW);
            let state = crate::transcript::turn_state_from_tail(&tail)?;
            let sid = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let snap = crate::transcript::snapshot_from_tail(&sid, &tail, idle);
            cache.insert(
                id.to_string(),
                TurnObservation {
                    state,
                    path,
                    len,
                    mtime,
                    error: crate::transcript::harness_error(&tail),
                    intent: snap.intent,
                    bg: !snap.background.is_empty() || !snap.subagents.is_empty(),
                },
            );
        }
        cache.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_pty::AgentKind;

    fn claude() -> Evidence {
        Evidence { harness: Some(AgentKind::Claude), transcript_present: true, ..Default::default() }
    }
    /// 화면을 본다 — 비어 있어도 「봤는데 조용하다」는 뜻이다.
    fn sc(e: &mut Evidence) -> &mut ScreenSigns {
        e.screen.get_or_insert_with(Default::default)
    }
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_shell_or_a_mirror_is_decided_before_anything_else() {
        assert_eq!(resolve(&Evidence::default()).0, AgentState::Idle);
        let mut e = claude();
        e.remote = Some(("working".into(), None, None));
        e.transcript_turn = Some(TurnState::Idle);
        assert_eq!(resolve(&e), (AgentState::Working, "remote"));
        e.remote = Some(("nonsense".into(), None, None));
        assert_eq!(resolve(&e).0, AgentState::Unknown);
    }

    /// 훅이 연 턴은 도는 중이다 — 낡았어도 박동이 있으면(긴 빌드) 계속.
    #[test]
    fn an_open_hook_turn_is_working_until_it_goes_stale_and_silent() {
        let mut e = claude();
        e.hook_turn = Some((HookTurn::Open, secs(1)));
        assert_eq!(resolve(&e), (AgentState::Working, "hook turn open"));
        e.hook_turn = Some((HookTurn::Open, secs(300)));
        e.transcript_turn = Some(TurnState::Working);
        e.transcript_age = Some(secs(400));
        assert_eq!(resolve(&e), (AgentState::Idle, "turn closed"));
        e.heartbeat = true;
        assert_eq!(resolve(&e).0, AgentState::Working);
    }

    /// 훅과 기록이 다른 말을 하면 **더 새로운** 쪽이 이긴다 — Stop 은 기록보다 먼저 온다.
    #[test]
    fn the_newer_turn_signal_wins() {
        let mut e = claude();
        e.hook_turn = Some((HookTurn::Closed, secs(1)));
        e.transcript_turn = Some(TurnState::Working);
        e.transcript_age = Some(secs(3));
        assert_eq!(resolve(&e).0, AgentState::Idle);
        e.hook_turn = Some((HookTurn::Open, secs(9)));
        e.transcript_turn = Some(TurnState::Idle);
        e.transcript_age = Some(secs(2));
        assert_eq!(resolve(&e).0, AgentState::Idle);
        e.transcript_turn = Some(TurnState::Working);
        assert_eq!(resolve(&e), (AgentState::Working, "transcript turn open"));
    }

    /// 승인 표식은 그 뒤에 기록이 자라거나 훅이 오면 사람이 답한 것이다.
    #[test]
    fn attention_is_superseded_by_newer_activity() {
        let mut e = claude();
        e.transcript_turn = Some(TurnState::Working);
        e.transcript_age = Some(secs(30));
        e.attention = Some((WaitKind::Permission, "Bash".into(), Some(secs(10))));
        assert!(matches!(resolve(&e).0, AgentState::Waiting { kind: WaitKind::Permission, .. }));
        e.transcript_age = Some(secs(2));
        assert_eq!(resolve(&e).0, AgentState::Working);
        e.transcript_age = Some(secs(30));
        e.hook_turn = Some((HookTurn::Closed, secs(1)));
        assert_eq!(resolve(&e).0, AgentState::Idle);
        // 방치 표식은 새 턴이 열리면 낡은 것이다.
        e.hook_turn = Some((HookTurn::Open, secs(1)));
        e.attention = Some((WaitKind::Idle, "".into(), Some(secs(10))));
        assert_eq!(resolve(&e).0, AgentState::Working);
        e.hook_turn = Some((HookTurn::Closed, secs(20)));
        let (state, reason) = resolve(&e);
        assert_eq!(state, AgentState::Waiting { kind: WaitKind::Idle, reason: "다음 지시 기다림".into() });
        assert_eq!(reason, "idle prompt");
    }

    #[test]
    fn compacting_holds_inside_its_window_and_a_stop_ends_it() {
        let mut e = claude();
        e.compact_age = Some(secs(20));
        e.hook_turn = Some((HookTurn::Open, secs(60)));
        assert_eq!(resolve(&e), (AgentState::Compacting, "precompact hook"));
        e.hook_turn = Some((HookTurn::Closed, secs(5)));
        assert_eq!(resolve(&e).0, AgentState::Idle);
        e.compact_age = Some(secs(900));
        e.hook_turn = Some((HookTurn::Open, secs(60)));
        assert_eq!(resolve(&e).0, AgentState::Working);
    }

    /// API 오류가 마지막 활동이면 Error, 도구 하나 실패는 도는 중 그대로.
    #[test]
    fn a_hard_error_becomes_error_and_a_soft_one_does_not() {
        let mut e = claude();
        e.transcript_turn = Some(TurnState::Working);
        e.transcript_age = Some(secs(200));
        e.transcript_error = Some(HarnessError { label: "API 오류".into(), hard: true });
        assert_eq!(resolve(&e), (AgentState::Error { label: "API 오류".into() }, "harness error"));
        e.transcript_age = Some(secs(1));
        e.transcript_error = Some(HarnessError { label: "도구 실패".into(), hard: false });
        assert_eq!(resolve(&e).0, AgentState::Working);
        e.transcript_turn = Some(TurnState::Idle);
        e.transcript_error = None;
        e.official = Some(Official::Waiting { error: true });
        assert!(matches!(resolve(&e).0, AgentState::Error { .. }));
    }

    #[test]
    /// Enter 뒤에 훅이 닫았으면 브리지가 아니다 — Stop 이 정본. 반대로 훅이 닫힌 뒤 Enter 를
    /// 쳤으면 브리지가 잇는다.
    #[test]
    fn a_hook_after_the_enter_beats_the_bridge() {
        let mut e = Evidence { harness: Some(kasa_pty::AgentKind::Claude), ..Default::default() };
        e.last_submit_age = Some(Duration::from_secs(3));
        e.hook_turn = Some((HookTurn::Closed, Duration::from_millis(100)));
        assert_eq!(resolve(&e), (AgentState::Idle, "turn closed"));
        e.hook_turn = Some((HookTurn::Closed, Duration::from_secs(9)));
        assert_eq!(resolve(&e), (AgentState::Working, "enter bridge"));
        e.hook_turn = Some((HookTurn::Open, Duration::from_millis(100)));
        assert_eq!(resolve(&e), (AgentState::Working, "hook turn open"));
    }

    fn enter_bridge_and_official_busy_cover_the_hookless_gaps() {
        let mut e = claude();
        e.transcript_turn = Some(TurnState::Idle);
        e.transcript_age = Some(secs(50));
        e.last_submit_age = Some(Duration::from_millis(800));
        assert_eq!(resolve(&e), (AgentState::Working, "enter bridge"));
        let mut e = claude();
        e.transcript_present = false;
        e.official = Some(Official::Busy);
        assert_eq!(resolve(&e), (AgentState::Working, "official busy"));
        e.official = None;
        assert_eq!(resolve(&e), (AgentState::Unknown, "no evidence"));
    }

    #[test]
    fn codex_and_agy_follow_their_transcripts_and_keep_a_screen_wait() {
        let mut e = Evidence { harness: Some(AgentKind::Codex), transcript_present: true, ..Default::default() };
        e.transcript_turn = Some(TurnState::Working);
        e.transcript_age = Some(secs(3));
        sc(&mut e).spinner = true;
        assert_eq!(resolve(&e).0, AgentState::Working);
        sc(&mut e).spinner = false;
        sc(&mut e).approval = Some("Allow command?".into());
        assert_eq!(
            resolve(&e),
            (AgentState::Waiting { kind: WaitKind::Permission, reason: "Allow command?".into() }, "screen approval")
        );
        let mut c = claude();
        c.transcript_turn = Some(TurnState::Working);
        c.transcript_age = Some(secs(3));
        sc(&mut c).approval = Some("Allow?".into());
        assert_eq!(resolve(&c).0, AgentState::Waiting { kind: WaitKind::Permission, reason: "Allow?".into() }, "claude 도 화면 승인 위젯이 보이면 사람 차례다 — 훅이 안 걸린 세션의 유일한 눈");
        let mut a = Evidence { harness: Some(AgentKind::Agy), transcript_present: true, ..Default::default() };
        a.transcript_age = Some(secs(5));
        assert_eq!(resolve(&a), (AgentState::Working, "agy fresh"));
        a.transcript_age = Some(secs(50));
        assert_eq!(resolve(&a).0, AgentState::Idle);
    }

    /// 화면은 둘째 눈이다 — 정본이 없거나 정본과 어긋날 때만 판정을 바꾼다.
    #[test]
    fn the_screen_cross_checks_the_hooks() {
        // ① 열린 턴인데 화면·박동 모두 6초 조용 → 90초를 안 기다리고 닫는다.
        let mut e = claude();
        e.hook_turn = Some((HookTurn::Open, secs(8)));
        e.hook_beat = Some(secs(8));
        assert_eq!(resolve(&e).0, AgentState::Working, "화면을 못 본 pane(배경 탭·원격)은 화면 규칙이 쉰다");
        sc(&mut e);
        assert_eq!(resolve(&e), (AgentState::Idle, "turn closed"), "조용한 화면이 열린 턴을 일찍 닫는다");
        sc(&mut e).spinner = true;
        assert_eq!(resolve(&e), (AgentState::Working, "hook turn open"), "스피너가 살아 있으면 닫지 않는다");
        sc(&mut e).spinner = false;
        e.heartbeat = true;
        assert_eq!(resolve(&e).0, AgentState::Working, "출력 박동도 살아 있는 증거다");
        e.heartbeat = false;
        sc(&mut e).approval = Some("Do you want to proceed?".into());
        assert_eq!(resolve(&e).0, AgentState::Waiting { kind: WaitKind::Permission, reason: "Do you want to proceed?".into() }, "승인 위젯은 스피너 없는 게 정상 — 닫지 않고 대기");
        // ② 훅이 닫았다는데 화면은 살아 돈다(훅 죽음·기록 밀림) → 도는 쪽을 믿는다.
        let mut d = claude();
        d.hook_turn = Some((HookTurn::Closed, secs(2)));
        sc(&mut d).spinner = true;
        d.heartbeat = true;
        assert_eq!(resolve(&d), (AgentState::Working, "screen spinner"));
        d.heartbeat = false;
        assert_eq!(resolve(&d).0, AgentState::Idle, "스피너 글자만 남고 박동이 없으면 옛 화면이다");
        // ③ 훅·기록·명부가 아무것도 없는 하네스 → 스피너가 유일한 눈.
        let mut o = Evidence { harness: AgentKind::from_id("gemini"), ..Default::default() };
        assert!(matches!(o.harness, Some(AgentKind::Other(_))));
        assert_eq!(resolve(&o), (AgentState::Unknown, "no evidence"));
        sc(&mut o).spinner = true;
        assert_eq!(resolve(&o).0, AgentState::Unknown, "박동 없는 스피너 글자는 굳은 화면일 수 있다");
        o.heartbeat = true;
        assert_eq!(resolve(&o), (AgentState::Working, "screen spinner"));
        // ⑤ 훅 attention 이 남아 있어도 화면이 살아 돌면 사람이 답한 것이다.
        let mut w = claude();
        w.attention = Some((WaitKind::Permission, "Bash".into(), Some(secs(3))));
        w.hook_turn = Some((HookTurn::Open, secs(20)));
        w.hook_beat = Some(secs(20));
        assert_eq!(resolve(&w).0, AgentState::Waiting { kind: WaitKind::Permission, reason: "Bash".into() });
        sc(&mut w).spinner = true;
        w.heartbeat = true;
        assert_eq!(resolve(&w), (AgentState::Working, "hook turn open"));
        // ⑥ 스피너와 위젯 글자가 함께 보이면 위젯은 인용이다.
        let mut q = claude();
        q.hook_turn = Some((HookTurn::Open, secs(1)));
        sc(&mut q).spinner = true;
        sc(&mut q).approval = Some("Do you want to proceed?".into());
        assert_eq!(resolve(&q).0, AgentState::Working);
        // ④ 끊김 문구는 오류 — 기록엔 안 남는 종류라 화면만 안다. 열린 턴이 이긴다(재시도 중은 돌고 있는 것).
        let mut t = claude();
        t.transcript_present = true;
        sc(&mut t).trouble = Some("연결 끊김");
        assert_eq!(resolve(&t), (AgentState::Error { label: "연결 끊김".into() }, "screen trouble"));
        t.hook_turn = Some((HookTurn::Open, secs(1)));
        sc(&mut t).spinner = true;
        assert_eq!(resolve(&t).0, AgentState::Working);
    }

    #[test]
    fn an_idle_prompt_is_not_a_hand_needed() {
        let idle = AgentState::Waiting { kind: WaitKind::Idle, reason: "Claude is waiting for your input".into() };
        assert!(!idle.needs_you());
        assert!(!idle.is_busy());
        assert_eq!(idle.board_word(), "waiting", "보드 낱말은 종전 계약대로 — attention_kind 가 idle 로 가른다");
        assert!(AgentState::Waiting { kind: WaitKind::Question, reason: String::new() }.needs_you());
    }

    #[test]
    fn a_remote_idle_prompt_does_not_become_a_hand_needed() {
        let mut e = Evidence { harness: Some(kasa_pty::AgentKind::Claude), ..Default::default() };
        // 그 기계가 종류를 보내 주면 그대로 가른다 — 방치는 사람을 부르지 않는다.
        e.remote = Some(("waiting".into(), Some("다음 지시 기다림".into()), Some(WaitKind::Idle)));
        let (state, reason) = resolve(&e);
        assert_eq!(reason, "remote");
        assert!(!state.needs_you(), "방치 알림은 주황으로 부르지 않는다");
        e.remote = Some(("waiting".into(), Some("Bash".into()), Some(WaitKind::Permission)));
        assert!(resolve(&e).0.needs_you());
        // 옛 판 기계는 그 칸을 안 보낸다 — 그때는 종전대로 승인으로 친다.
        e.remote = Some(("waiting".into(), None, None));
        assert!(resolve(&e).0.needs_you());
    }

    #[test]
    fn board_words_stay_within_the_four_the_consumers_compare() {
        for state in [AgentState::Unknown, AgentState::Idle, AgentState::Working, AgentState::Compacting,
            AgentState::Waiting { kind: WaitKind::Question, reason: String::new() }, AgentState::Error { label: String::new() }]
        {
            assert!(["unknown", "idle", "working", "waiting"].contains(&state.board_word()));
        }
    }
}
