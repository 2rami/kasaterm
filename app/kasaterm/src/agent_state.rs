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
    pub(crate) fn needs_you(&self) -> bool {
        matches!(self, Self::Waiting { .. })
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
    pub remote: Option<(String, Option<String>)>,
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
    /// codex·agy 전용 화면 승인 폴백 — 롤아웃에 승인 요청 이벤트가 없어 남긴 유일한
    /// 화면 판독. claude 에는 안 쓴다.
    pub screen_wait: Option<String>,
    /// 훅·기록이 본 뒤 작업(서브에이전트·백그라운드).
    pub bg_active: bool,
    pub intent: String,
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
/// agy 는 턴 경계를 안 남긴다 — 기록이 이만큼 안에 자랐으면 도는 중.
const AGY_ACTIVE: Duration = Duration::from_secs(15);

fn remote_state(word: &str, reason: Option<&str>) -> AgentState {
    match word {
        "working" => AgentState::Working,
        "idle" => AgentState::Idle,
        "waiting" => AgentState::Waiting {
            kind: WaitKind::Permission,
            reason: reason.unwrap_or("").to_string(),
        },
        _ => AgentState::Unknown,
    }
}

/// 턴이 열려 있나 — 훅과 기록 중 **더 새로운** 경계가 정하고, 그 위에 staleness 를 건다.
/// 둘 다 없으면 명부·브리지·agy 폴백.
fn turn_open(e: &Evidence) -> Option<&'static str> {
    if e.last_submit_age.is_some_and(|age| age < SUBMIT_BRIDGE) {
        return Some("enter bridge");
    }
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
            if stale { None } else { Some(reason) }
        }
        Some((false, _, _)) => None,
        None => match e.harness {
            Some(kasa_pty::AgentKind::Agy) => {
                let fresh = e.transcript_age.is_some_and(|age| age < AGY_ACTIVE);
                (fresh || e.heartbeat).then_some("agy fresh")
            }
            _ => matches!(e.official, Some(Official::Busy)).then_some("official busy"),
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
    if let Some((word, reason)) = &e.remote {
        return (remote_state(word, reason.as_deref()), "remote");
    }
    if e.harness.is_none() {
        return (AgentState::Idle, "shell");
    }
    if let Some((kind, reason, age)) = &e.attention {
        if matches!(kind, WaitKind::Permission | WaitKind::Question) && attention_live(e, *kind, *age) {
            let reason = if reason.trim().is_empty() { kind.default_reason().to_string() } else { reason.clone() };
            return (AgentState::Waiting { kind: *kind, reason }, "hook attention");
        }
    }
    if let (Some(reason), Some(kind)) = (&e.screen_wait, &e.harness) {
        if !matches!(kind, kasa_pty::AgentKind::Claude) {
            return (
                AgentState::Waiting { kind: WaitKind::Permission, reason: reason.clone() },
                "screen approval (no hooks)",
            );
        }
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
    /// codex·agy pane 의 화면 승인 폴백 — GUI 스캔이 채운다.
    pub screen_wait: Mutex<HashMap<String, String>>,
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
        for map in [&self.perm_mode, &self.screen_wait] {
            map.lock().unwrap().retain(|id, _| live.contains(id));
        }
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
                evidence.remote = Some((word, why));
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
                evidence.screen_wait = self.screen_wait.lock().unwrap().get(id).cloned();
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
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_shell_or_a_mirror_is_decided_before_anything_else() {
        assert_eq!(resolve(&Evidence::default()).0, AgentState::Idle);
        let mut e = claude();
        e.remote = Some(("working".into(), None));
        e.transcript_turn = Some(TurnState::Idle);
        assert_eq!(resolve(&e), (AgentState::Working, "remote"));
        e.remote = Some(("nonsense".into(), None));
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
        assert_eq!(resolve(&e).0, AgentState::Working);
        e.screen_wait = Some("Allow command?".into());
        assert_eq!(
            resolve(&e),
            (AgentState::Waiting { kind: WaitKind::Permission, reason: "Allow command?".into() }, "screen approval (no hooks)")
        );
        let mut c = claude();
        c.transcript_turn = Some(TurnState::Working);
        c.transcript_age = Some(secs(3));
        c.screen_wait = Some("Allow?".into());
        assert_eq!(resolve(&c).0, AgentState::Working, "claude 는 화면 승인을 안 믿는다");
        let mut a = Evidence { harness: Some(AgentKind::Agy), transcript_present: true, ..Default::default() };
        a.transcript_age = Some(secs(5));
        assert_eq!(resolve(&a), (AgentState::Working, "agy fresh"));
        a.transcript_age = Some(secs(50));
        assert_eq!(resolve(&a).0, AgentState::Idle);
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
