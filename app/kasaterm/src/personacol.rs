//! 우측 칼럼의 네이티브 「대화」 탭 상태와 비동기 작업 경계.
//!
//! 화면 그리기는 이 모듈의 메모리만 읽는다. 로스터·선택 저장·원화 디코딩·모델
//! 호출은 짧은 워커가 수행하고 mailbox 로 완성본을 돌려준다. 캐릭터 교체 때
//! `generation`을 올려, 먼저 출발한 옛 캐릭터의 답이 늦게 와도 새 말풍선이나
//! 대화 기록에 섞이지 않게 한다.

use image::ImageReader;
use kasa_socket::backend::PaneActivity;
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const DRAFT_MIN_H: f32 = 36.0;
pub(crate) const DRAFT_MAX_H: f32 = 96.0;
pub(crate) const DRAFT_VERTICAL_PAD: f32 = 18.0;
pub(crate) const PORTRAIT_TEXTURE_PREFIX: &str = "persona-portrait:";

const REQUEST_HISTORY_LIMIT: usize = 12;
const MEMORY_HISTORY_LIMIT: usize = 24;
const BOARD_SPEAK_COOLDOWN: Duration = Duration::from_secs(45);
const IDLE_SPEAK_COOLDOWN: Duration = Duration::from_secs(8 * 60);
const MAX_DRAFT_CHARS: usize = 4_000;
const MAX_QUERY_CHARS: usize = 128;
const MAX_DECODE_EDGE: u32 = 2_048;
const MAX_DECODE_ALLOC: u64 = 32 << 20;

type Wake = Arc<dyn Fn() + Send + Sync>;
type FallbackBytes = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// 전역 IME 소유권에 실을 값. `ImeFocus::Persona(PersonaInput)`처럼 필드까지
/// 함께 싣지 않으면 검색칸으로 옮긴 순간 조합 중이던 글자가 새 칸에 확정된다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonaInput {
    Draft,
    RosterQuery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UiRect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

impl UiRect {
    pub(crate) fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

/// 렌더가 프레임마다 채우고 입력 처리가 읽는 클릭 대상.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PersonaAction {
    OpenRoster,
    CloseRoster,
    Focus(PersonaInput),
    Send,
    PickCharacter(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RosterEntry {
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) school: String,
    pub(crate) accent: [u8; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PersonaProfile {
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) accent: [u8; 4],
}

impl Default for PersonaProfile {
    fn default() -> Self {
        Self {
            name: "아로나".to_string(),
            slug: String::new(),
            accent: [0x4a, 0x90, 0xe2, 0xff],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PortraitFallback {
    Missing,
    Rejected,
}

#[derive(Clone)]
pub(crate) struct PortraitImage {
    pub(crate) rgba: Arc<[u8]>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) texture_key: String,
}

#[derive(Clone)]
pub(crate) enum PortraitState {
    Loading,
    Ready(PortraitImage),
    /// 원화가 없거나 안전 제한을 넘으면 기존 도트 스프라이트를 그린다.
    Fallback {
        slug: String,
        reason: PortraitFallback,
        /// 번들 idle frame-0도 워커에서 미리 디코딩한다. None은 번들에도 그
        /// 슬러그가 없다는 뜻이며, 렌더는 파일이나 PNG를 다시 열지 않는다.
        image: Option<PortraitImage>,
    },
}

impl Default for PortraitState {
    fn default() -> Self {
        Self::Loading
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum BubbleState {
    #[default]
    Hidden,
    Thinking,
    Reply(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConversationTurn {
    pub(crate) role: &'static str,
    pub(crate) content: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ActivityTone {
    Waiting,
    Working,
    #[default]
    Idle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ActivitySummary {
    pub(crate) tone: ActivityTone,
    pub(crate) active_count: usize,
}

impl ActivitySummary {
    pub(crate) fn label(&self) -> String {
        match self.tone {
            ActivityTone::Waiting => "확인 필요".to_string(),
            ActivityTone::Working => format!("작업 중 {}", self.active_count),
            ActivityTone::Idle => "idle".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProactiveReason {
    Opened,
    CharacterChanged,
    BoardChanged,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoardObservation {
    pub(crate) changed: bool,
    pub(crate) proactive: Option<ProactiveReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftEnter {
    Ignored,
    Newline,
    Submit,
}

/// 렌더가 실제 글자 폭으로 줄바꿈한 뒤 코어에 돌려주는 입력창 측정값.
/// `caret_row`는 전체 visual row 기준이라 긴 글에서도 커서 행을 정확히 보존한다.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DraftLayoutMetrics {
    pub(crate) total_rows: usize,
    pub(crate) caret_row: usize,
    pub(crate) line_height: f32,
}

/// 36~96px 안에서 현재 보일 visual row 범위. 렌더는 `first_row` 앞을 건너뛰고
/// `visible_rows`만 그리면 되며, 별도의 글자 자르기 추측을 만들지 않는다.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DraftViewport {
    pub(crate) first_row: usize,
    pub(crate) visible_rows: usize,
    pub(crate) total_rows: usize,
    pub(crate) height: f32,
}

impl Default for DraftViewport {
    fn default() -> Self {
        Self {
            first_row: 0,
            visible_rows: 1,
            total_rows: 1,
            height: DRAFT_MIN_H,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonaEffect {
    Speak(ProactiveReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileLoadKind {
    Initial,
    Switch,
}

struct LoadedProfile {
    generation: u64,
    profile: PersonaProfile,
    roster: Vec<RosterEntry>,
    portrait: PortraitState,
}

enum WorkerResult {
    Profile {
        generation: u64,
        kind: ProfileLoadKind,
        result: Result<LoadedProfile, String>,
    },
    Chat {
        generation: u64,
        message: String,
        result: Result<String, String>,
    },
}

/// 네이티브 Persona 탭의 완전한 UI 상태. GPU 리소스는 담지 않고 디코딩된 픽셀만
/// 담아, 렌더가 필요할 때 기존 이미지 캐시에 한 번 올릴 수 있게 한다.
pub(crate) struct PersonaColState {
    pub(crate) profile: PersonaProfile,
    pub(crate) portrait: PortraitState,
    pub(crate) roster: Vec<RosterEntry>,
    pub(crate) roster_open: bool,
    pub(crate) roster_query: String,
    pub(crate) roster_query_caret: usize,
    pub(crate) draft: String,
    pub(crate) draft_caret: usize,
    pub(crate) bubble: BubbleState,
    pub(crate) activity: ActivitySummary,
    pub(crate) busy: bool,
    pub(crate) switching_to: Option<String>,
    /// 캐릭터 저장 실패는 기존 말풍선을 덮지 않고 로스터 자리에서 보여준다.
    pub(crate) switch_error: Option<String>,
    pub(crate) draft_viewport: DraftViewport,
    pub(crate) bubble_scroll: f32,
    pub(crate) roster_scroll: f32,
    pub(crate) body_rect: UiRect,
    pub(crate) bubble_extent: (f32, f32),
    pub(crate) roster_extent: (f32, f32),
    pub(crate) hits: Vec<(PersonaAction, UiRect)>,
    generation: u64,
    history: Vec<ConversationTurn>,
    board_signature: Option<String>,
    last_spoke_at: Option<Duration>,
    initial_requested: bool,
    texture_reset_pending: bool,
    tx: Sender<WorkerResult>,
    rx: Receiver<WorkerResult>,
    wake: Wake,
    fallback_bytes: FallbackBytes,
}

impl Default for PersonaColState {
    fn default() -> Self {
        Self::with_waker(|| {})
    }
}

impl PersonaColState {
    pub(crate) fn with_waker(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::with_sources(wake, |_| None)
    }

    /// 앱 통합 입구. `fallback_bytes`에는 번들 idle frame-0 바이트만 돌려주는
    /// 함수를 건넨다. 이 함수 자체는 GUI 스레드에서 불리지 않고 워커로 복제된다.
    pub(crate) fn with_sources(
        wake: impl Fn() + Send + Sync + 'static,
        fallback_bytes: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            profile: PersonaProfile::default(),
            portrait: PortraitState::default(),
            roster: Vec::new(),
            roster_open: false,
            roster_query: String::new(),
            roster_query_caret: 0,
            draft: String::new(),
            draft_caret: 0,
            bubble: BubbleState::Hidden,
            activity: ActivitySummary::default(),
            busy: false,
            switching_to: None,
            switch_error: None,
            draft_viewport: DraftViewport::default(),
            bubble_scroll: 0.0,
            roster_scroll: 0.0,
            body_rect: UiRect::default(),
            bubble_extent: (0.0, 0.0),
            roster_extent: (0.0, 0.0),
            hits: Vec::new(),
            generation: 1,
            history: Vec::new(),
            board_signature: None,
            last_spoke_at: None,
            initial_requested: false,
            texture_reset_pending: false,
            tx,
            rx,
            wake: Arc::new(wake),
            fallback_bytes: Arc::new(fallback_bytes),
        }
    }

    /// 첫 표시 때 한 번만 로스터·선택·그림을 준비한다. 호출 자체는 파일을 읽지 않는다.
    pub(crate) fn request_initial_load(&mut self) -> bool {
        if self.initial_requested {
            return false;
        }
        self.initial_requested = true;
        let generation = self.generation;
        let fallback_bytes = self.fallback_bytes.clone();
        self.spawn(move || WorkerResult::Profile {
            generation,
            kind: ProfileLoadKind::Initial,
            result: Ok(load_profile(generation, None, &fallback_bytes)),
        });
        true
    }

    /// 고른 캐릭터 저장과 새 그림 준비를 화면 밖에서 시작한다. 전환 중 두 번째
    /// 전환은 막아 저장 순서가 뒤집히는 경합을 만들지 않는다.
    pub(crate) fn switch_character(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || name == self.profile.name || self.switching_to.is_some() {
            return false;
        }
        self.switching_to = Some(name.to_string());
        self.switch_error = None;
        self.roster_open = false;
        self.roster_query.clear();
        self.roster_query_caret = 0;
        let generation = self.generation;
        let next_generation = generation.wrapping_add(1).max(1);
        let name = name.to_string();
        let fallback_bytes = self.fallback_bytes.clone();
        self.spawn(move || {
            let result = kasa_mcp::persona::set_character(&name)
                .map(|_| load_profile(next_generation, Some(name), &fallback_bytes));
            WorkerResult::Profile {
                generation,
                kind: ProfileLoadKind::Switch,
                result,
            }
        });
        true
    }

    /// mailbox 완성본만 UI 상태로 옮긴다. `PersonaEffect::Speak`는 호출부가 이미
    /// 가진 board 스냅샷과 묶어 [`begin_proactive`]에 넘기면 된다.
    pub(crate) fn pump(&mut self, now: Duration) -> (bool, Vec<PersonaEffect>) {
        let pending: Vec<WorkerResult> = self.rx.try_iter().collect();
        let mut changed = false;
        let mut effects = Vec::new();
        for result in pending {
            if self.apply_result(result, now, &mut effects) {
                changed = true;
            }
        }
        (changed, effects)
    }

    fn apply_result(
        &mut self,
        result: WorkerResult,
        now: Duration,
        effects: &mut Vec<PersonaEffect>,
    ) -> bool {
        match result {
            WorkerResult::Profile {
                generation,
                kind,
                result,
            } => {
                if generation != self.generation {
                    return false;
                }
                match result {
                    Ok(loaded) => {
                        if kind == ProfileLoadKind::Switch {
                            self.history.clear();
                            self.last_spoke_at = None;
                            self.busy = false;
                            self.bubble = BubbleState::Hidden;
                        }
                        self.generation = loaded.generation;
                        self.profile = loaded.profile;
                        self.roster = loaded.roster;
                        self.portrait = loaded.portrait;
                        self.texture_reset_pending = true;
                        self.roster_scroll = 0.0;
                        self.bubble_scroll = 0.0;
                        self.switching_to = None;
                        self.switch_error = None;
                        effects.push(PersonaEffect::Speak(match kind {
                            ProfileLoadKind::Initial => ProactiveReason::Opened,
                            ProfileLoadKind::Switch => ProactiveReason::CharacterChanged,
                        }));
                    }
                    Err(error) => {
                        self.switching_to = None;
                        if kind == ProfileLoadKind::Switch {
                            self.switch_error = Some(error);
                        } else {
                            self.bubble = BubbleState::Error(error);
                        }
                    }
                }
                true
            }
            WorkerResult::Chat {
                generation,
                message,
                result,
            } => {
                if generation != self.generation {
                    return false;
                }
                self.busy = false;
                self.last_spoke_at = Some(now);
                self.bubble_scroll = 0.0;
                match result {
                    Ok(reply) => {
                        if !message.is_empty() {
                            self.history.push(ConversationTurn {
                                role: "user",
                                content: message,
                            });
                        }
                        self.history.push(ConversationTurn {
                            role: "assistant",
                            content: reply.clone(),
                        });
                        trim_front(&mut self.history, MEMORY_HISTORY_LIMIT);
                        self.bubble = BubbleState::Reply(reply);
                    }
                    Err(error) => self.bubble = BubbleState::Error(error),
                }
                true
            }
        }
    }

    /// 사용자 전송. 바쁜 동안에는 입력을 지우지 않는다.
    pub(crate) fn send_draft(&mut self, board: &[PaneActivity]) -> bool {
        if self.busy || self.switching_to.is_some() {
            return false;
        }
        let message = self.draft.trim().to_string();
        if message.is_empty() {
            return false;
        }
        if !self.begin_chat(message, false, board) {
            return false;
        }
        self.draft.clear();
        self.draft_caret = 0;
        true
    }

    pub(crate) fn begin_proactive(
        &mut self,
        _reason: ProactiveReason,
        board: &[PaneActivity],
    ) -> bool {
        self.begin_chat(String::new(), true, board)
    }

    fn begin_chat(&mut self, message: String, unprompted: bool, board: &[PaneActivity]) -> bool {
        if self.busy || self.switching_to.is_some() {
            return false;
        }
        self.busy = true;
        self.bubble = BubbleState::Thinking;
        let history = self
            .history
            .iter()
            .rev()
            .take(REQUEST_HISTORY_LIMIT)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|turn| kasa_mcp::persona::Turn {
                role: turn.role.to_string(),
                content: turn.content,
            })
            .collect();
        let request = kasa_mcp::persona::ChatReq {
            message: message.clone(),
            history,
            character: self.profile.name.clone(),
            unprompted,
        };
        let board = board.to_vec();
        let generation = self.generation;
        self.spawn(move || WorkerResult::Chat {
            generation,
            message,
            result: kasa_mcp::persona::chat_blocking(&request, &board),
        });
        true
    }

    /// board 폴링 결과를 메모리 상태로 반영하고, 변화가 말할 만한 시점이면 이유를
    /// 돌려준다. 첫 스냅샷은 기준만 세우며 자동 발화를 만들지 않는다.
    pub(crate) fn observe_board(
        &mut self,
        board: &[PaneActivity],
        now: Duration,
    ) -> BoardObservation {
        let summary = activity_summary(board);
        let changed = summary != self.activity;
        self.activity = summary;
        let signature = board_signature(board);
        let Some(previous) = self.board_signature.replace(signature.clone()) else {
            return BoardObservation {
                changed: true,
                proactive: None,
            };
        };
        let signature_changed = previous != signature;
        BoardObservation {
            changed: changed || signature_changed,
            proactive: (signature_changed
                && !self.busy
                && self.switching_to.is_none()
                && elapsed_over(now, self.last_spoke_at, BOARD_SPEAK_COOLDOWN))
            .then_some(ProactiveReason::BoardChanged),
        }
    }

    /// 8분 잡담 게이트. 보이지 않는 탭은 호출부가 `visible=false`로 넘긴다.
    pub(crate) fn idle_proactive(&self, visible: bool, now: Duration) -> Option<ProactiveReason> {
        (visible
            && !self.busy
            && self.switching_to.is_none()
            && elapsed_over(now, self.last_spoke_at, IDLE_SPEAK_COOLDOWN))
        .then_some(ProactiveReason::Idle)
    }

    pub(crate) fn open_roster(&mut self) {
        self.roster_open = true;
        self.roster_query.clear();
        self.roster_query_caret = 0;
        self.roster_scroll = 0.0;
    }

    pub(crate) fn close_roster(&mut self) {
        self.roster_open = false;
    }

    pub(crate) fn filtered_roster_indices(&self) -> Vec<usize> {
        let needle = self.roster_query.trim().to_lowercase();
        self.roster
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                needle.is_empty()
                    || entry.name.to_lowercase().contains(&needle)
                    || entry.slug.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub(crate) fn text(&self, input: PersonaInput) -> (&str, usize) {
        match input {
            PersonaInput::Draft => (&self.draft, self.draft_caret),
            PersonaInput::RosterQuery => (&self.roster_query, self.roster_query_caret),
        }
    }

    pub(crate) fn insert_text(&mut self, input: PersonaInput, text: &str) -> bool {
        let limit = match input {
            PersonaInput::Draft => MAX_DRAFT_CHARS,
            PersonaInput::RosterQuery => MAX_QUERY_CHARS,
        };
        let (value, caret) = self.text_mut(input);
        let remaining = limit.saturating_sub(value.chars().count());
        if remaining == 0 {
            return false;
        }
        let mut insert = text.replace('\r', "");
        if input == PersonaInput::RosterQuery {
            insert = insert.replace('\n', " ");
        }
        let insert = insert.chars().take(remaining).collect::<String>();
        if insert.is_empty() {
            return false;
        }
        let byte = char_to_byte(value, *caret);
        value.insert_str(byte, &insert);
        *caret += insert.chars().count();
        true
    }

    pub(crate) fn backspace(&mut self, input: PersonaInput) -> bool {
        let (value, caret) = self.text_mut(input);
        if *caret == 0 {
            return false;
        }
        let start = char_to_byte(value, *caret - 1);
        let end = char_to_byte(value, *caret);
        value.replace_range(start..end, "");
        *caret -= 1;
        true
    }

    pub(crate) fn delete_forward(&mut self, input: PersonaInput) -> bool {
        let (value, caret) = self.text_mut(input);
        if *caret >= value.chars().count() {
            return false;
        }
        let start = char_to_byte(value, *caret);
        let end = char_to_byte(value, *caret + 1);
        value.replace_range(start..end, "");
        true
    }

    pub(crate) fn move_caret(&mut self, input: PersonaInput, delta: isize) {
        let (value, caret) = self.text_mut(input);
        let len = value.chars().count();
        *caret = if delta < 0 {
            caret.saturating_sub(delta.unsigned_abs())
        } else {
            caret.saturating_add(delta as usize).min(len)
        };
    }

    pub(crate) fn draft_enter(&mut self, shift: bool, composing: bool) -> DraftEnter {
        if composing || self.busy || self.switching_to.is_some() {
            return DraftEnter::Ignored;
        }
        if shift {
            return if self.insert_text(PersonaInput::Draft, "\n") {
                DraftEnter::Newline
            } else {
                DraftEnter::Ignored
            };
        }
        if self.draft.trim().is_empty() {
            DraftEnter::Ignored
        } else {
            DraftEnter::Submit
        }
    }

    /// 긴 draft의 커서가 96px 입력창 밖으로 밀리지 않도록 visual row 창을 맞춘다.
    /// 줄바꿈 자체는 폰트 폭을 아는 렌더가 계산하고, 이 함수는 순수한 viewport만 맡는다.
    pub(crate) fn update_draft_layout(&mut self, metrics: DraftLayoutMetrics) -> DraftViewport {
        let total_rows = metrics.total_rows.max(1);
        let caret_row = metrics.caret_row.min(total_rows - 1);
        let line_height = if metrics.line_height.is_finite() && metrics.line_height > 0.0 {
            metrics.line_height
        } else {
            DRAFT_MIN_H - DRAFT_VERTICAL_PAD
        };
        let height = draft_height(total_rows as f32 * line_height);
        let visible_rows =
            (((height - DRAFT_VERTICAL_PAD) / line_height).floor() as usize).clamp(1, total_rows);
        let max_first = total_rows.saturating_sub(visible_rows);
        let mut first_row = self.draft_viewport.first_row.min(max_first);
        if caret_row < first_row {
            first_row = caret_row;
        } else if caret_row >= first_row + visible_rows {
            first_row = (caret_row + 1).saturating_sub(visible_rows);
        }
        self.draft_viewport = DraftViewport {
            first_row: first_row.min(max_first),
            visible_rows,
            total_rows,
            height,
        };
        self.draft_viewport
    }

    pub(crate) fn set_scroll_extent(&mut self, roster: bool, viewport: f32, content: f32) {
        if roster {
            self.roster_extent = (viewport.max(0.0), content.max(0.0));
            self.roster_scroll = clamp_scroll(self.roster_scroll, self.roster_extent);
        } else {
            self.bubble_extent = (viewport.max(0.0), content.max(0.0));
            self.bubble_scroll = clamp_scroll(self.bubble_scroll, self.bubble_extent);
        }
    }

    pub(crate) fn scroll_by(&mut self, roster: bool, delta: f32) -> bool {
        let (scroll, extent) = if roster {
            (&mut self.roster_scroll, self.roster_extent)
        } else {
            (&mut self.bubble_scroll, self.bubble_extent)
        };
        let next = clamp_scroll(*scroll + delta, extent);
        let changed = (next - *scroll).abs() > f32::EPSILON;
        *scroll = next;
        changed
    }

    pub(crate) fn begin_layout(&mut self, body_rect: UiRect) {
        self.body_rect = body_rect;
        self.hits.clear();
    }

    pub(crate) fn push_hit(&mut self, action: PersonaAction, rect: UiRect) {
        self.hits.push((action, rect));
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<PersonaAction> {
        self.hits
            .iter()
            .rev()
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(action, _)| action.clone())
    }

    /// 새 원화 세대가 도착한 첫 프레임에만 기존 Persona 텍스처를 비우게 한다.
    pub(crate) fn take_texture_reset(&mut self) -> bool {
        std::mem::take(&mut self.texture_reset_pending)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    fn text_mut(&mut self, input: PersonaInput) -> (&mut String, &mut usize) {
        match input {
            PersonaInput::Draft => (&mut self.draft, &mut self.draft_caret),
            PersonaInput::RosterQuery => (&mut self.roster_query, &mut self.roster_query_caret),
        }
    }

    fn spawn(&self, work: impl FnOnce() -> WorkerResult + Send + 'static) {
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            if tx.send(work()).is_ok() {
                wake();
            }
        });
    }
}

pub(crate) fn draft_height(text_height: f32) -> f32 {
    (text_height.max(0.0) + DRAFT_VERTICAL_PAD).clamp(DRAFT_MIN_H, DRAFT_MAX_H)
}

fn elapsed_over(now: Duration, since: Option<Duration>, threshold: Duration) -> bool {
    since.is_some_and(|then| now.saturating_sub(then) > threshold)
}

fn activity_summary(board: &[PaneActivity]) -> ActivitySummary {
    let is_idle = |status: &str| status.trim().is_empty() || status == "idle";
    let is_waiting = |status: &str| matches!(status, "waiting" | "blocked" | "needs-you");
    let tone = if board.iter().any(|row| is_waiting(&row.status)) {
        ActivityTone::Waiting
    } else if board.iter().any(|row| !is_idle(&row.status)) {
        ActivityTone::Working
    } else {
        ActivityTone::Idle
    };
    ActivitySummary {
        tone,
        active_count: board.iter().filter(|row| !is_idle(&row.status)).count(),
    }
}

fn board_signature(board: &[PaneActivity]) -> String {
    board
        .iter()
        .map(|row| {
            format!(
                "{}:{}:{}",
                row.surface_id,
                row.status,
                row.title.chars().take(20).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn trim_front<T>(items: &mut Vec<T>, limit: usize) {
    if items.len() > limit {
        items.drain(..items.len() - limit);
    }
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

fn clamp_scroll(value: f32, extent: (f32, f32)) -> f32 {
    value.clamp(0.0, (extent.1 - extent.0).max(0.0))
}

fn load_profile(
    generation: u64,
    chosen: Option<String>,
    fallback_bytes: &FallbackBytes,
) -> LoadedProfile {
    let roster = load_roster();
    let name = chosen.unwrap_or_else(|| kasa_mcp::persona::character_name(""));
    let profile = profile_for(&name, &roster);
    let portrait = load_portrait(generation, &profile, fallback_bytes);
    LoadedProfile {
        generation,
        profile,
        roster,
        portrait,
    }
}

fn load_roster() -> Vec<RosterEntry> {
    let Some(chars) = kasa_mcp::character::characters_json() else {
        return Vec::new();
    };
    roster_from_json(&chars)
}

fn roster_from_json(chars: &serde_json::Value) -> Vec<RosterEntry> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for key in ["leaders", "members"] {
        for member in chars
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = member
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            if name.is_empty() || !seen.insert(name.to_string()) {
                continue;
            }
            out.push(RosterEntry {
                name: name.to_string(),
                slug: member
                    .get("slug")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                school: member
                    .get("school")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                accent: member
                    .get("header_color")
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_hex)
                    .unwrap_or([0x8a, 0xa6, 0xc8, 0xff]),
            });
        }
    }
    out
}

fn profile_for(name: &str, roster: &[RosterEntry]) -> PersonaProfile {
    if let Some(entry) = roster.iter().find(|entry| entry.name == name) {
        return PersonaProfile {
            name: entry.name.clone(),
            slug: entry.slug.clone(),
            accent: entry.accent,
        };
    }
    PersonaProfile {
        name: name.to_string(),
        slug: kasa_mcp::persona::slug_for(name).unwrap_or_default(),
        ..PersonaProfile::default()
    }
}

fn parse_hex(value: &str) -> Option<[u8; 4]> {
    let value = value.trim().strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
        0xff,
    ])
}

fn load_portrait(
    generation: u64,
    profile: &PersonaProfile,
    fallback_bytes: &FallbackBytes,
) -> PortraitState {
    let primary = kasa_mcp::persona::portrait(&profile.slug);
    if let Some((bytes, _)) = primary.as_ref() {
        if let Some(image) = decoded_image(
            bytes,
            format!("{PORTRAIT_TEXTURE_PREFIX}{generation}:{}", profile.slug),
        ) {
            return PortraitState::Ready(image);
        }
    }
    let image = fallback_bytes(&profile.slug).and_then(|bytes| {
        decoded_image(
            &bytes,
            format!(
                "{PORTRAIT_TEXTURE_PREFIX}{generation}:{}:fallback",
                profile.slug
            ),
        )
    });
    PortraitState::Fallback {
        slug: profile.slug.clone(),
        reason: if primary.is_some() {
            PortraitFallback::Rejected
        } else {
            PortraitFallback::Missing
        },
        image,
    }
}

fn decoded_image(bytes: &[u8], texture_key: String) -> Option<PortraitImage> {
    let (rgba, width, height) = decode_portrait_bytes(bytes)?;
    Some(PortraitImage {
        rgba: Arc::from(rgba),
        width,
        height,
        texture_key,
    })
}

/// `settings_media`와 같은 삼중 제한: 압축 파일 32MB, 변 2048px, 디코더 할당
/// 32MB. 원화는 contain-fit이므로 제한 안쪽 이미지를 다시 키우거나 자르지 않는다.
fn decode_portrait_bytes(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if bytes.is_empty() || bytes.len() as u64 > kasa_mcp::persona::MAX_PORTRAIT_FILE_BYTES {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    let decoded = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    if decoded > MAX_DECODE_ALLOC {
        return None;
    }
    Some((rgba.into_raw(), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, status: &str, title: &str) -> PaneActivity {
        PaneActivity {
            surface_id: id.to_string(),
            status: status.to_string(),
            title: title.to_string(),
            ..PaneActivity::default()
        }
    }

    #[test]
    fn board_change_and_idle_gates_match_web_contract() {
        let mut state = PersonaColState::default();
        let initial = vec![row("%1", "idle", "처음")];
        let first = state.observe_board(&initial, Duration::from_secs(1));
        assert_eq!(first.proactive, None);

        state.last_spoke_at = Some(Duration::ZERO);
        let changed = vec![row("%1", "working", "한글 제목이 바뀜")];
        let early = state.observe_board(&changed, Duration::from_secs(45));
        assert_eq!(early.proactive, None, "45초와 같은 순간에는 아직 이르다");

        let changed_again = vec![row("%1", "waiting", "다시 바뀜")];
        let due = state.observe_board(&changed_again, Duration::from_secs(46));
        assert_eq!(due.proactive, Some(ProactiveReason::BoardChanged));
        assert_eq!(state.activity.label(), "확인 필요");

        state.last_spoke_at = Some(Duration::from_secs(46));
        assert_eq!(
            state.idle_proactive(true, Duration::from_secs(46 + 8 * 60)),
            None
        );
        assert_eq!(
            state.idle_proactive(true, Duration::from_secs(47 + 8 * 60)),
            Some(ProactiveReason::Idle)
        );
        assert_eq!(state.idle_proactive(false, Duration::from_secs(1000)), None);
    }

    #[test]
    fn board_status_aliases_share_one_activity_vocabulary() {
        let needs_you = activity_summary(&[
            row("%1", "needs-you", "승인"),
            row("%2", "blocked", "입력 대기"),
        ]);
        assert_eq!(needs_you.tone, ActivityTone::Waiting);
        assert_eq!(needs_you.active_count, 2);

        let working = activity_summary(&[
            row("%1", "compacting", "정리"),
            row("%2", "reviewing", "검수"),
            row("%3", "idle", "쉼"),
            row("%4", "", "아직 없음"),
        ]);
        assert_eq!(working.tone, ActivityTone::Working);
        assert_eq!(working.active_count, 2);
        assert_eq!(working.label(), "작업 중 2");
    }

    #[test]
    fn stale_chat_result_cannot_cross_character_generation() {
        let mut state = PersonaColState::default();
        state.generation = 8;
        state.bubble = BubbleState::Thinking;
        let mut effects = Vec::new();
        let changed = state.apply_result(
            WorkerResult::Chat {
                generation: 7,
                message: "옛 질문".to_string(),
                result: Ok("옛 캐릭터 답".to_string()),
            },
            Duration::from_secs(2),
            &mut effects,
        );
        assert!(!changed);
        assert_eq!(state.bubble, BubbleState::Thinking);
        assert!(state.history.is_empty());
    }

    #[test]
    fn failed_character_switch_preserves_existing_conversation() {
        let mut state = PersonaColState::default();
        state.switching_to = Some("미도리".to_string());
        state.bubble = BubbleState::Reply("기존 답".to_string());
        state.history.push(ConversationTurn {
            role: "assistant",
            content: "기존 답".to_string(),
        });
        state.last_spoke_at = Some(Duration::from_secs(11));
        let mut effects = Vec::new();
        assert!(state.apply_result(
            WorkerResult::Profile {
                generation: state.generation,
                kind: ProfileLoadKind::Switch,
                result: Err("저장 실패".to_string()),
            },
            Duration::from_secs(12),
            &mut effects,
        ));
        assert_eq!(state.bubble, BubbleState::Reply("기존 답".to_string()));
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.last_spoke_at, Some(Duration::from_secs(11)));
        assert_eq!(state.switch_error.as_deref(), Some("저장 실패"));
        assert!(effects.is_empty());
    }

    #[test]
    fn successful_character_switch_resets_conversation_after_save() {
        let mut state = PersonaColState::default();
        state.generation = 4;
        state.switching_to = Some("미도리".to_string());
        state.busy = true;
        state.bubble = BubbleState::Reply("기존 답".to_string());
        state.history.push(ConversationTurn {
            role: "assistant",
            content: "기존 답".to_string(),
        });
        state.last_spoke_at = Some(Duration::from_secs(11));
        let mut effects = Vec::new();
        assert!(state.apply_result(
            WorkerResult::Profile {
                generation: 4,
                kind: ProfileLoadKind::Switch,
                result: Ok(LoadedProfile {
                    generation: 5,
                    profile: PersonaProfile {
                        name: "미도리".to_string(),
                        slug: "midori".to_string(),
                        accent: [1, 2, 3, 255],
                    },
                    roster: Vec::new(),
                    portrait: PortraitState::Loading,
                }),
            },
            Duration::from_secs(12),
            &mut effects,
        ));
        assert_eq!(state.generation(), 5);
        assert_eq!(state.profile.name, "미도리");
        assert_eq!(state.bubble, BubbleState::Hidden);
        assert!(state.history.is_empty());
        assert_eq!(state.last_spoke_at, None);
        assert!(!state.busy);
        assert_eq!(
            effects,
            vec![PersonaEffect::Speak(ProactiveReason::CharacterChanged)]
        );
    }

    #[test]
    fn history_keeps_24_turns_and_requests_only_latest_12() {
        let mut state = PersonaColState::default();
        let mut effects = Vec::new();
        for index in 0..13 {
            state.busy = true;
            state.apply_result(
                WorkerResult::Chat {
                    generation: state.generation,
                    message: format!("질문 {index}"),
                    result: Ok(format!("답 {index}")),
                },
                Duration::from_secs(index),
                &mut effects,
            );
        }
        assert_eq!(state.history.len(), MEMORY_HISTORY_LIMIT);
        let tail = state
            .history
            .iter()
            .rev()
            .take(REQUEST_HISTORY_LIMIT)
            .collect::<Vec<_>>();
        assert_eq!(tail.len(), REQUEST_HISTORY_LIMIT);
        assert_eq!(tail[0].content, "답 12");
        assert_eq!(tail[11].content, "질문 7");
    }

    #[test]
    fn draft_enter_respects_shift_and_ime_composition() {
        let mut state = PersonaColState::default();
        state.insert_text(PersonaInput::Draft, "안녕");
        assert_eq!(state.draft_enter(false, true), DraftEnter::Ignored);
        assert_eq!(state.draft, "안녕");
        assert_eq!(state.draft_enter(true, false), DraftEnter::Newline);
        assert_eq!(state.draft, "안녕\n");
        state.insert_text(PersonaInput::Draft, "하세요");
        assert_eq!(state.draft_enter(false, false), DraftEnter::Submit);
        assert_eq!(state.draft, "안녕\n하세요");
    }

    #[test]
    fn text_editing_is_character_safe_and_query_is_bounded() {
        let mut state = PersonaColState::default();
        state.insert_text(PersonaInput::Draft, "가나다");
        state.move_caret(PersonaInput::Draft, -1);
        assert!(state.backspace(PersonaInput::Draft));
        assert_eq!(state.text(PersonaInput::Draft), ("가다", 1));
        assert!(state.delete_forward(PersonaInput::Draft));
        assert_eq!(state.text(PersonaInput::Draft), ("가", 1));

        let long = "a".repeat(MAX_QUERY_CHARS + 20);
        state.insert_text(PersonaInput::RosterQuery, &long);
        assert_eq!(state.roster_query.len(), MAX_QUERY_CHARS);
    }

    #[test]
    fn roster_search_matches_name_and_slug() {
        let mut state = PersonaColState::default();
        state.roster = vec![
            RosterEntry {
                name: "미도리".to_string(),
                slug: "midori".to_string(),
                school: "밀레니엄".to_string(),
                accent: [1, 2, 3, 255],
            },
            RosterEntry {
                name: "아로나".to_string(),
                slug: "arona".to_string(),
                school: "싯딤".to_string(),
                accent: [4, 5, 6, 255],
            },
        ];
        state.roster_query = "MIDO".to_string();
        assert_eq!(state.filtered_roster_indices(), vec![0]);
        state.roster_query = "아로".to_string();
        assert_eq!(state.filtered_roster_indices(), vec![1]);
    }

    #[test]
    fn draft_height_and_scroll_are_clamped() {
        assert_eq!(draft_height(0.0), DRAFT_MIN_H);
        assert_eq!(draft_height(50.0), 68.0);
        assert_eq!(draft_height(500.0), DRAFT_MAX_H);

        let mut state = PersonaColState::default();
        state.set_scroll_extent(false, 100.0, 250.0);
        assert!(state.scroll_by(false, 999.0));
        assert_eq!(state.bubble_scroll, 150.0);
        assert!(state.scroll_by(false, -999.0));
        assert_eq!(state.bubble_scroll, 0.0);
    }

    #[test]
    fn draft_viewport_keeps_caret_row_visible() {
        let mut state = PersonaColState::default();
        let bottom = state.update_draft_layout(DraftLayoutMetrics {
            total_rows: 20,
            caret_row: 19,
            line_height: 18.0,
        });
        assert_eq!(bottom.height, DRAFT_MAX_H);
        assert_eq!(bottom.visible_rows, 4);
        assert_eq!(bottom.first_row, 16);

        let top = state.update_draft_layout(DraftLayoutMetrics {
            total_rows: 20,
            caret_row: 2,
            line_height: 18.0,
        });
        assert_eq!(top.first_row, 2);
        assert!(top.first_row <= 2 && 2 < top.first_row + top.visible_rows);
    }

    #[test]
    fn portrait_decoder_rejects_dimensions_over_2048() {
        let image =
            image::RgbaImage::from_pixel(MAX_DECODE_EDGE + 1, 1, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(decode_portrait_bytes(bytes.get_ref()).is_none());
    }

    #[test]
    fn fallback_sprite_is_decoded_before_render() {
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let fallback: FallbackBytes = Arc::new(move |_| Some(bytes.get_ref().clone()));
        let profile = PersonaProfile {
            name: "테스트".to_string(),
            slug: "no-such-persona-fallback".to_string(),
            accent: [1, 2, 3, 255],
        };
        let portrait = load_portrait(9, &profile, &fallback);
        let PortraitState::Fallback {
            reason: PortraitFallback::Missing,
            image: Some(image),
            ..
        } = portrait
        else {
            panic!("번들 폴백 픽셀이 준비돼야 한다");
        };
        assert_eq!((image.width, image.height), (2, 3));
        assert!(image.texture_key.ends_with(":fallback"));
    }

    #[test]
    fn roster_parser_deduplicates_and_falls_back_on_bad_color() {
        let roster = roster_from_json(&serde_json::json!({
            "leaders": [{"name":"아로나","slug":"arona","header_color":"#112233"}],
            "members": [
                {"name":"아로나","slug":"duplicate"},
                {"name":"미도리","slug":"midori","header_color":"bad"}
            ]
        }));
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].accent, [0x11, 0x22, 0x33, 0xff]);
        assert_eq!(roster[1].accent, [0x8a, 0xa6, 0xc8, 0xff]);
    }
}
