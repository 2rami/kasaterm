//! WGPU 네이티브 운영 보드의 상태와 worker 스냅샷.
//!
//! THESIS: 터미널을 가리는 대시보드가 아니라, 작업 방 하나로 오가는 운영실이다.
//! OWN-WORLD: 현재 터미널 팔레트, 얇은 경계, 상태색과 학생 스프라이트를 공유한다.
//! STORY: 기기와 방을 따라 요청·진행·완료를 읽고, 선택한 창의 근거를 펼친다.
//! FIRST VIEWPORT: 왼쪽 운영 탭, 오른쪽 전체 기기 필터와 방별 작업 행, 최근 변경.
//! FORM: desktop workspace; seed native-board-room.
//! FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, and DESIGN.md
//!
//! paint는 이 파일의 `Snapshot`만 읽는다. transcript, 파일, git, 프로세스, 원격
//! 캐시는 worker가 읽고 세대 번호가 붙은 `DataEnvelope`로만 GUI에 건넨다.

use super::*;
use kasa_socket::backend::{Backend, PaneActivity};
use crate::session_transfer::{SessionIdentity, SessionRow, TransferSnapshot, TransferRequest, RoomTarget, TransferResult, TransferStatus};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BoardTab {
    #[default]
    Overview,
    Agents,
    Schedule,
    Git,
    Machines,
}

impl BoardTab {
    pub(crate) const ALL: [Self; 5] = [
        Self::Overview,
        Self::Agents,
        Self::Schedule,
        Self::Git,
        Self::Machines,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Overview => "보드",
            Self::Agents => "에이전트",
            Self::Schedule => "스케줄",
            Self::Git => "소스 컨트롤",
            Self::Machines => "이사",
        }
    }

    /// 머리글 밑 한 줄 설명(목업 .sub).
    pub(crate) const fn desc(self) -> &'static str {
        match self {
            Self::Overview => "연결된 기기의 모든 방과 최근 변경",
            Self::Agents => "pane 밖에서도 계속 도는 대화",
            Self::Schedule => "지정한 때에 학생에게 지시를 보냅니다",
            Self::Git => "대상 pane의 저장소",
            Self::Machines => "세션을 다른 기기·방으로 옮깁니다",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardInput {
    ScheduleText,
    ScheduleMinutes,
    ScheduleAt,
    GitMessage,
    TransferRoomName,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TransferStep {
    #[default]
    Select,
    Destination,
    Confirm,
}

#[derive(Clone, Debug)]
enum TransferConfirmation {
    Move(TransferRequest),
    Close(Vec<SessionIdentity>),
}

#[derive(Clone, Debug, Default)]
struct TransferUi {
    step: TransferStep,
    room_filter: Option<(String, Option<String>)>,
    selected: HashSet<SessionIdentity>,
    shell_selected: HashSet<SessionIdentity>,
    destination: String,
    room: Option<String>,
    new_room: bool,
    room_name: String,
    show_shells: bool,
    detail: Option<String>,
    pending: Option<TransferConfirmation>,
    busy: bool,
    results: Vec<TransferResult>,
    result_names: std::collections::HashMap<String, String>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct TransferEnvelope {
    generation: u64,
    result: Option<TransferResult>,
    finished: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BackgroundRow {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) name: String,
    pub(crate) cwd: String,
    pub(crate) state: String,
    pub(crate) status: String,
    pub(crate) kind: String,
    pub(crate) pid: u32,
    pub(crate) started_at: u64,
    pub(crate) parent_surface: Option<String>,
    /// 원격 board에서 온 세션이면 기계 라벨. 로컬 PID와 같은 숫자여도 이 값이
    /// 있으면 이 프로세스에서 signal을 보내면 안 된다.
    pub(crate) machine: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalBackgroundProcess {
    pub(crate) pid: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GitRow {
    pub(crate) path: String,
    pub(crate) marker: char,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GitSnapshot {
    pub(crate) branch: String,
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
    pub(crate) insertions: u32,
    pub(crate) deletions: u32,
    pub(crate) no_repo: bool,
    pub(crate) error: String,
    pub(crate) rows: Vec<GitRow>,
}

#[derive(Clone, Debug)]
pub(crate) struct FaceAsset {
    pub(crate) name: String,
    pub(crate) key: String,
    pub(crate) rgba: Arc<Vec<u8>>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct BoardAddress {
    machine_id: String,
    surface_key: String,
    surface_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct OverviewPane {
    id: String,
    address: BoardAddress,
    machine_label: String,
    room_id: Option<String>,
    room_label: String,
    character: Option<String>,
    harness: Option<String>,
    title: String,
    request: String,
    progress: String,
    status: String,
    status_reason: Option<String>,
    done_outcome: Option<String>,
    done_summary: Option<String>,
    observed_at_ms: u64,
    freshness: String,
    detached: bool,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct OverviewSource {
    machine_id: String,
    label: String,
    state: String,
    observed_at_ms: u64,
    error: Option<String>,
    complete: bool,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct OverviewChange {
    cursor: String,
    at_ms: u64,
    kind: String,
    pane_id: Option<String>,
    machine_id: String,
    room_id: Option<String>,
    room_label: Option<String>,
    summary: String,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct OverviewData {
    schema_version: u32,
    cursor: String,
    observed_at_ms: u64,
    sources: Vec<OverviewSource>,
    panes: Vec<OverviewPane>,
    recent_changes: Vec<OverviewChange>,
    journal_error: Option<String>,
    #[serde(skip)]
    local_machine_id: Option<String>,
    #[serde(skip)]
    error: Option<String>,
    #[serde(skip)]
    gap: Option<String>,
    #[serde(skip)]
    detail: Option<OverviewDetail>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OverviewSelection {
    id: String,
    address: BoardAddress,
    generation: u64,
}

#[derive(Clone, Debug)]
struct OverviewDetail {
    selection: OverviewSelection,
    observed_at_ms: u64,
    lines: Vec<String>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OverviewSort {
    #[default]
    Name,
    Status,
    Recent,
}

#[derive(Clone, Debug, Default)]
struct OverviewUi {
    machine: Option<String>,
    room: Option<(String, Option<String>)>,
    sort: OverviewSort,
    order: Vec<String>,
    selection: Option<OverviewSelection>,
    selection_generation: u64,
    show_changes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoardProbeMode {
    Live,
    Fixture,
    Rejected,
}

fn board_probe_mode(debug_build: bool, requested: bool, isolated: bool) -> BoardProbeMode {
    if !requested { BoardProbeMode::Live }
    else if debug_build && isolated { BoardProbeMode::Fixture }
    else { BoardProbeMode::Rejected }
}

fn board_fixture_requested() -> bool {
    std::env::var_os("KASATERM_TEST_BOARD_FIXTURE").is_some()
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BoardData {
    pub(crate) agents: Arc<Vec<PaneActivity>>,
    pub(crate) background: Arc<Vec<BackgroundRow>>,
    pub(crate) schedules: Arc<Vec<kasa_mcp::ScheduleItem>>,
    pub(crate) transfer: Arc<TransferSnapshot>,
    pub(crate) git: Arc<GitSnapshot>,
    pub(crate) faces: Arc<Vec<FaceAsset>>,
    pub(crate) error: Option<String>,
    overview: Arc<OverviewData>,
}

#[derive(Clone, Debug)]
struct DataEnvelope {
    generation: u64,
    data: BoardData,
}

#[derive(Clone, Debug)]
struct ActionEnvelope {
    generation: u64,
    ok: bool,
    message: String,
}

#[derive(Default)]
struct Mailbox {
    data: Option<DataEnvelope>,
    actions: Vec<ActionEnvelope>,
    transfers: Vec<TransferEnvelope>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Tab(BoardTab),
    Return,
    Refresh,
    FocusPane(String),
    OverviewMachine(Option<String>),
    OverviewRoom(Option<(String, Option<String>)>),
    OverviewSort(OverviewSort),
    OverviewDetail(String),
    OverviewChanges,
    OverviewCopy(BoardAddress),
    OverviewFocus(BoardAddress),
    OverviewSave(BoardAddress),
    ResumeBackground(String, String),
    StopBackground(LocalBackgroundProcess),
    ConfirmStopBackground(LocalBackgroundProcess),
    CancelStopBackground,
    ScheduleKind(String),
    ScheduleSurface(String),
    Input(BoardInput),
    ScheduleAdd,
    ScheduleToggle(String),
    ScheduleDelete(String),
    GitFile(String),
    GitAll,
    GitClear,
    GitCommit,
    GitPush,
    TransferFilter(Option<(String, Option<String>)>),
    TransferSelect(SessionIdentity, bool),
    TransferSelectRoom,
    TransferClear,
    TransferDestination,
    TransferMachine(String),
    TransferRoom(String),
    TransferNewRoom,
    TransferReview,
    TransferCloseReview,
    TransferConfirm,
    TransferBack,
    TransferShells,
    TransferDetail(String),
    TransferView(SessionIdentity),
}

#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub(crate) target: Target,
    pub(crate) rect: Rect,
    pub(crate) text_cursor: bool,
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) area: Rect,
    pub(crate) tab: BoardTab,
    pub(crate) cursor: (f32, f32),
    pub(crate) scroll: f32,
    pub(crate) data: Arc<BoardData>,
    pub(crate) target_pane: String,
    pub(crate) target_cwd: String,
    pub(crate) refreshing: bool,
    pub(crate) schedule_kind: String,
    pub(crate) schedule_surface: String,
    pub(crate) schedule_text: String,
    pub(crate) schedule_minutes: String,
    pub(crate) schedule_at: String,
    pub(crate) git_message: String,
    pub(crate) git_selected: Arc<HashSet<String>>,
    pub(crate) input: Option<BoardInput>,
    pub(crate) caret: usize,
    pub(crate) preedit: String,
    pub(crate) caret_on: bool,
    pub(crate) toast: Option<(bool, String)>,
    overview: OverviewUi,
    pub(crate) pending_stop: Option<LocalBackgroundProcess>,
    transfer: TransferUi,
    fixture: bool,
}

pub(crate) struct PaintOutput {
    pub(crate) hits: Vec<Hit>,
    pub(crate) content_h: f32,
    pub(crate) view_h: f32,
    pub(crate) caret_rect: Option<Rect>,
}

pub(crate) struct Scene {
    tab: BoardTab,
    return_pane: Option<String>,
    target_pane: Option<String>,
    target_window: usize,
    target_cwd: String,
    data: Arc<BoardData>,
    mailbox: Arc<Mutex<Mailbox>>,
    generation: Arc<AtomicU64>,
    requested_generation: u64,
    applied_generation: u64,
    action_generation: u64,
    applied_action_generation: u64,
    refreshing: bool,
    last_refresh: Option<Instant>,
    scroll: f32,
    scroll_max: f32,
    hits: Vec<Hit>,
    caret_rect: Option<Rect>,
    input: Option<BoardInput>,
    caret: usize,
    schedule_kind: String,
    schedule_surface: String,
    schedule_text: String,
    schedule_minutes: String,
    schedule_at: String,
    git_message: String,
    git_selected: HashSet<String>,
    toast: Option<(bool, String, Instant)>,
    overview: OverviewUi,
    pending_stop: Option<LocalBackgroundProcess>,
    transfer: TransferUi,
    transfer_generation: u64,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            tab: BoardTab::Overview,
            return_pane: None,
            target_pane: None,
            target_window: 0,
            target_cwd: String::new(),
            data: Arc::new(BoardData::default()),
            mailbox: Arc::new(Mutex::new(Mailbox::default())),
            generation: Arc::new(AtomicU64::new(0)),
            requested_generation: 0,
            applied_generation: 0,
            action_generation: 0,
            applied_action_generation: 0,
            refreshing: false,
            last_refresh: None,
            scroll: 0.0,
            scroll_max: 0.0,
            hits: Vec::new(),
            caret_rect: None,
            input: None,
            caret: 0,
            schedule_kind: "loop".to_string(),
            schedule_surface: String::new(),
            schedule_text: String::new(),
            schedule_minutes: "10".to_string(),
            schedule_at: String::new(),
            git_message: String::new(),
            git_selected: HashSet::new(),
            toast: None,
            overview: OverviewUi::default(),
            pending_stop: None,
            transfer: TransferUi::default(),
            transfer_generation: 0,
        }
    }
}

impl Scene {
    pub(crate) fn enter(
        &mut self,
        return_pane: Option<String>,
        target_window: usize,
        target_cwd: String,
    ) {
        if return_pane
            .as_deref()
            .is_some_and(|pane| crate::internal_room::InternalRoomKind::from_pane(pane).is_none())
        {
            self.target_pane.clone_from(&return_pane);
            self.return_pane = return_pane;
            self.target_window = target_window;
            self.target_cwd = target_cwd;
        }
    }

    pub(crate) fn leave(&mut self) {
        self.return_pane = None;
        self.target_pane = None;
        self.hits.clear();
        self.input = None;
        self.caret_rect = None;
        self.scroll = 0.0;
        self.scroll_max = 0.0;
        self.pending_stop = None;
        self.overview.selection = None;
        self.overview.selection_generation += 1;
    }

    pub(crate) fn return_pane(&self) -> Option<&str> {
        self.return_pane.as_deref()
    }

    pub(crate) fn target_pane(&self) -> Option<&str> {
        self.target_pane.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn target_window(&self) -> usize {
        self.target_window
    }

    pub(crate) fn set_tab(&mut self, tab: BoardTab) {
        if self.tab != tab {
            self.tab = tab;
            self.scroll = 0.0;
            self.input = None;
            self.caret_rect = None;
            self.last_refresh = None;
        }
    }

    pub(crate) fn tab(&self) -> BoardTab {
        self.tab
    }

    pub(crate) fn scroll_by(&mut self, delta: f32) -> bool {
        let next = (self.scroll + delta).clamp(0.0, self.scroll_max);
        let changed = (next - self.scroll).abs() > f32::EPSILON;
        self.scroll = next;
        changed
    }

    pub(crate) fn hit_at(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits.iter().rev().find(|hit| contains(hit.rect, (x, y)))
    }

    pub(crate) fn input(&self) -> Option<BoardInput> {
        self.input
    }

    pub(crate) fn set_input(&mut self, input: Option<BoardInput>, value_len: usize) {
        self.input = input;
        self.caret = value_len;
    }

    pub(crate) fn finish_paint(&mut self, output: PaintOutput) {
        self.scroll_max = (output.content_h - output.view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.scroll_max);
        self.hits = output.hits;
        self.caret_rect = output.caret_rect;
    }

    pub(crate) fn caret_rect(&self) -> Option<Rect> {
        self.caret_rect
    }

    pub(crate) fn snapshot(
        &self,
        area: Rect,
        cursor: (f32, f32),
        caret_on: bool,
        preedit: String,
    ) -> Snapshot {
        Snapshot {
            area,
            tab: self.tab,
            cursor,
            scroll: self.scroll,
            data: self.data.clone(),
            target_pane: self.target_pane.clone().unwrap_or_default(),
            target_cwd: self.target_cwd.clone(),
            refreshing: self.refreshing,
            schedule_kind: self.schedule_kind.clone(),
            schedule_surface: self.schedule_surface.clone(),
            schedule_text: self.schedule_text.clone(),
            schedule_minutes: self.schedule_minutes.clone(),
            schedule_at: self.schedule_at.clone(),
            git_message: self.git_message.clone(),
            git_selected: Arc::new(self.git_selected.clone()),
            input: self.input,
            caret: self.caret,
            preedit,
            caret_on,
            toast: self.toast.as_ref().map(|(ok, text, _)| (*ok, text.clone())),
            overview: self.overview.clone(),
            pending_stop: self.pending_stop.clone(),
            transfer: self.transfer.clone(),
            fixture: transfer_fixture_active() || board_fixture_active(),
        }
    }

    pub(crate) fn selected_git(&self) -> &HashSet<String> {
        &self.git_selected
    }

    fn toggle_overview_detail(&mut self, id: String) {
        self.overview.selection_generation += 1;
        self.overview.selection = if self.overview.selection.as_ref().is_some_and(|row| row.id == id) {
            None
        } else {
            self.data.overview.panes.iter().find(|row| row.id == id).map(|row| OverviewSelection {
                id, address: row.address.clone(), generation: self.overview.selection_generation,
            })
        };
        self.last_refresh = None;
    }

    fn revalidate_overview(&mut self) {
        if self.overview.selection.as_ref().is_some_and(|selected| {
            !self.data.overview.panes.iter().any(|row| row.id == selected.id && row.address == selected.address)
        }) {
            self.overview.selection = None;
            self.overview.selection_generation += 1;
        }
        let panes = &self.data.overview.panes;
        self.overview.order.retain(|id| panes.iter().any(|row| &row.id == id));
        let mut newcomers: Vec<_> = panes.iter().filter(|row| !self.overview.order.contains(&row.id)).collect();
        sort_overview_panes(&mut newcomers, self.overview.sort);
        self.overview.order.extend(newcomers.into_iter().map(|row| row.id.clone()));
    }

    fn sort_overview(&mut self, sort: OverviewSort) {
        self.overview.sort = sort;
        let mut panes: Vec<_> = self.data.overview.panes.iter().collect();
        sort_overview_panes(&mut panes, sort);
        self.overview.order = panes.into_iter().map(|row| row.id.clone()).collect();
        self.scroll = 0.0;
    }

    #[cfg(debug_assertions)]
    pub(crate) fn apply_board_probe_view(&mut self) {
        if !board_fixture_active() { return; }
        if let Ok(scroll) = std::env::var("KASATERM_TEST_BOARD_SCROLL") {
            if let Ok(scroll) = scroll.parse::<f32>() {
                self.scroll = scroll.clamp(0.0, self.scroll_max);
            }
        }
    }

    pub(crate) fn arm_stop(&mut self, target: LocalBackgroundProcess) {
        self.pending_stop = Some(target);
    }

    pub(crate) fn clear_stop(&mut self) {
        self.pending_stop = None;
    }

    pub(crate) fn toggle_git_file(&mut self, path: String) {
        if !self.git_selected.remove(&path) {
            self.git_selected.insert(path);
        }
    }

    pub(crate) fn set_all_git(&mut self, all: bool) {
        self.git_selected.clear();
        if all {
            self.git_selected
                .extend(self.data.git.rows.iter().map(|row| row.path.clone()));
        }
    }

    pub(crate) fn field(&self, input: BoardInput) -> &str {
        match input {
            BoardInput::ScheduleText => &self.schedule_text,
            BoardInput::ScheduleMinutes => &self.schedule_minutes,
            BoardInput::ScheduleAt => &self.schedule_at,
            BoardInput::GitMessage => &self.git_message,
            BoardInput::TransferRoomName => &self.transfer.room_name,
        }
    }

    pub(crate) fn edit_field(&mut self, input: BoardInput, mut edit: impl FnMut(&mut String, &mut usize)) {
        let (value, caret) = match input {
            BoardInput::ScheduleText => (&mut self.schedule_text, &mut self.caret),
            BoardInput::ScheduleMinutes => (&mut self.schedule_minutes, &mut self.caret),
            BoardInput::ScheduleAt => (&mut self.schedule_at, &mut self.caret),
            BoardInput::GitMessage => (&mut self.git_message, &mut self.caret),
            BoardInput::TransferRoomName => (&mut self.transfer.room_name, &mut self.caret),
        };
        edit(value, caret);
    }

    pub(crate) fn schedule_kind(&self) -> &str {
        &self.schedule_kind
    }

    pub(crate) fn set_schedule_kind(&mut self, kind: String) {
        self.schedule_kind = kind;
    }

    pub(crate) fn schedule_surface(&self) -> &str {
        &self.schedule_surface
    }

    pub(crate) fn set_schedule_surface(&mut self, surface: String) {
        self.schedule_surface = surface;
    }

    pub(crate) fn git_message(&self) -> &str {
        &self.git_message
    }

    /// 판이 바뀌었으면(커서) 바로, 아니어도 2.2초마다 한 번. 커서 비교는 잠금 하나라 매 틱
    /// 해도 싸고, 실패가 이어져도 300ms 바닥이 있어 헛돌지 않는다.
    pub(crate) fn refresh_due(&self) -> bool {
        if self.refreshing {
            return false;
        }
        let since = self.last_refresh.map(|at| at.elapsed());
        let Some(since) = since else { return true };
        if since >= std::time::Duration::from_millis(2200) {
            return true;
        }
        since >= std::time::Duration::from_millis(300)
            && kasa_mcp::board_service::cursor()
                .is_some_and(|cursor| cursor != self.data.overview.cursor)
    }

    pub(crate) fn request_refresh(
        &mut self,
        backend: Arc<dyn Backend>,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        let probe = board_probe_mode(cfg!(debug_assertions), board_fixture_requested(), board_fixture_active());
        if probe == BoardProbeMode::Rejected {
            self.data = Arc::new(BoardData {
                overview: Arc::new(OverviewData { error: Some("보드 검증 설정 오류예요. 디버그 빌드와 분리된 임시 상태 경로가 필요합니다.".into()), ..Default::default() }),
                ..Default::default()
            });
            self.last_refresh = Some(Instant::now());
            self.refreshing = false;
            return;
        }
        #[cfg(debug_assertions)]
        if probe == BoardProbeMode::Fixture {
            let first = self.applied_generation == 0;
            let mut overview = board_fixture();
            if first {
                self.tab = BoardTab::Overview;
                self.overview.show_changes = std::env::var("KASATERM_TEST_BOARD_CHANGES").as_deref() == Ok("1");
            }
            if let Some(selected) = self.overview.selection.clone() {
                overview.detail = overview.panes.iter().find(|row| row.id == selected.id && row.address == selected.address).map(|row| OverviewDetail {
                    selection: selected,
                    observed_at_ms: overview.observed_at_ms,
                    lines: vec![format!("요청 · {}", row.request), format!("응답 · {}", row.progress)],
                    error: (row.freshness != "fresh").then(|| "상세 연결을 확인하지 못했어요. 마지막 확인 내용입니다.".into()),
                });
            }
            let names = overview.panes.iter().filter_map(|row| row.character.as_deref()).collect::<HashSet<_>>();
            let faces = collect_overview_faces(names);
            self.data = Arc::new(BoardData { overview: Arc::new(overview), faces: Arc::new(faces), ..Default::default() });
            self.revalidate_overview();
            if first {
                self.applied_generation = 1;
                if let Ok(id) = std::env::var("KASATERM_TEST_BOARD_SELECT") {
                    self.toggle_overview_detail(id);
                }
            }
            self.last_refresh = if first && self.overview.selection.is_some() { None } else { Some(Instant::now()) };
            self.refreshing = false;
            return;
        }
        #[cfg(debug_assertions)]
        if let Some(data) = transfer_fixture() {
            self.data = Arc::new(BoardData { transfer: Arc::new(data), ..Default::default() });
            self.last_refresh = Some(Instant::now());
            self.refreshing = false;
            if self.applied_generation == 0 {
                self.applied_generation = 1;
                self.tab = BoardTab::Machines;
                self.transfer.show_shells = true;
                for row in self.data.transfer.sessions.iter().filter(|row| row.harness.is_some()).take(2) {
                    self.transfer.selected.insert(row.identity.clone());
                }
                if let Some(machine) = self.data.transfer.machines.iter().find(|machine| !machine.local) {
                    self.transfer.destination = machine.id.clone();
                    self.transfer.room = machine.rooms.first().map(|room| room.id.clone());
                }
                if let Ok(name) = std::env::var("KASATERM_TEST_TRANSFER_ROOM_NAME") {
                    self.transfer.new_room = true;
                    self.transfer.room = None;
                    self.transfer.room_name = name;
                }
                match std::env::var("KASATERM_TEST_TRANSFER_STEP").as_deref() {
                    Ok("destination") => self.transfer.step = TransferStep::Destination,
                    Ok("confirm") => self.review_transfer(false),
                    Ok("results") => {
                        self.transfer.step = TransferStep::Confirm;
                        self.transfer.results = self.transfer.selected.iter().enumerate().map(|(index, source)| TransferResult {
                            source: source.clone(), status: if index == 0 { TransferStatus::Succeeded } else { TransferStatus::Failed },
                            message: if index == 0 { "선택한 기기의 작업 방에 도착했어요" } else { "도착 기기의 연결이 끊겼어요. 연결 후 목록을 확인해 주세요." }.into(), destination: None,
                        }).collect();
                    }
                    _ => {}
                }
            }
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.requested_generation = generation;
        self.refreshing = true;
        self.last_refresh = Some(Instant::now());
        let mailbox = self.mailbox.clone();
        let target_window = self.target_window;
        let target_cwd = self.target_cwd.clone();
        let previous = self.data.clone();
        let tab = self.tab;
        let selection = (self.tab == BoardTab::Overview).then(|| self.overview.selection.clone()).flatten();
        std::thread::spawn(move || {
            let data = collect_data(&backend, target_window, &target_cwd, tab, &previous, selection);
            let mut mailbox = mailbox.lock().unwrap();
            if mailbox
                .data
                .as_ref()
                .is_none_or(|current| generation >= current.generation)
            {
                mailbox.data = Some(DataEnvelope { generation, data });
            }
            drop(mailbox);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    pub(crate) fn pump(&mut self) -> bool {
        let (data, actions, transfers) = {
            let mut mailbox = self.mailbox.lock().unwrap();
            (mailbox.data.take(), std::mem::take(&mut mailbox.actions), std::mem::take(&mut mailbox.transfers))
        };
        let mut changed = false;
        if let Some(envelope) = data {
            if envelope.generation >= self.requested_generation
                && envelope.generation >= self.applied_generation
            {
                self.applied_generation = envelope.generation;
                self.data = Arc::new(envelope.data);
                self.refreshing = false;
                if self.schedule_surface.is_empty() {
                    self.schedule_surface = self
                        .data
                        .agents
                        .first()
                        .map(|row| row.surface_id.clone())
                        .unwrap_or_default();
                }
                self.git_selected
                    .retain(|path| self.data.git.rows.iter().any(|row| &row.path == path));
                self.revalidate_transfer_selection();
                self.revalidate_overview();
                changed = true;
            }
        }
        for action in actions {
            if action.generation >= self.applied_action_generation {
                self.applied_action_generation = action.generation;
                self.toast = Some((action.ok, action.message, Instant::now()));
                self.last_refresh = None;
                changed = true;
            }
        }
        for event in transfers {
            if event.generation != self.transfer_generation { continue; }
            if let Some(result) = event.result {
                if let Some(previous) = self.transfer.results.iter_mut().find(|row| row.source == result.source) {
                    *previous = result;
                } else {
                    self.transfer.results.push(result);
                }
            }
            if event.finished {
                self.transfer.busy = false;
                self.transfer.pending = None;
                self.transfer.selected.clear();
                self.transfer.shell_selected.clear();
                self.last_refresh = None;
            }
            changed = true;
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() >= std::time::Duration::from_secs(4))
        {
            self.toast = None;
            changed = true;
        }
        changed
    }

    pub(crate) fn run_action(
        &mut self,
        backend: Arc<dyn Backend>,
        action: WorkerAction,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if transfer_fixture_active() { self.report_error("검증 화면에서는 실제 작업을 실행하지 않아요"); return; }
        self.action_generation += 1;
        let generation = self.action_generation;
        let mailbox = self.mailbox.clone();
        std::thread::spawn(move || {
            let result = execute_action(&backend, action);
            let (ok, message) = match result {
                Ok(message) => (true, message),
                Err(error) => (false, error.to_string()),
            };
            mailbox.lock().unwrap().actions.push(ActionEnvelope {
                generation,
                ok,
                message,
            });
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    pub(crate) fn wait_for_gui_result(
        &mut self,
        receiver: std::sync::mpsc::Receiver<std::result::Result<String, String>>,
        success: &'static str,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        self.action_generation += 1;
        let generation = self.action_generation;
        let mailbox = self.mailbox.clone();
        std::thread::spawn(move || {
            let result = receiver
                .recv_timeout(std::time::Duration::from_secs(30))
                .map_err(|_| "pane 생성 응답이 없어요".to_string())
                .and_then(|result| result);
            let (ok, message) = match result {
                Ok(pane) => (true, format!("{success} · {pane}")),
                Err(error) => (false, error),
            };
            mailbox.lock().unwrap().actions.push(ActionEnvelope {
                generation,
                ok,
                message,
            });
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    pub(crate) fn report_error(&mut self, message: impl Into<String>) {
        self.toast = Some((false, message.into(), Instant::now()));
    }

    fn revalidate_transfer_selection(&mut self) {
        if self.transfer.busy { return; }
        let before = self.transfer.selected.len() + self.transfer.shell_selected.len();
        let rows = &self.data.transfer.sessions;
        self.transfer.selected.retain(|id| rows.iter().any(|row| &row.identity == id && row.harness.is_some() && row.unavailable_reason.is_none()));
        self.transfer.shell_selected.retain(|id| rows.iter().any(|row| &row.identity == id && row.harness.is_none() && row.shell_closeable));
        if before != self.transfer.selected.len() + self.transfer.shell_selected.len() {
            self.transfer.pending = None;
            self.transfer.step = TransferStep::Select;
            self.transfer.error = Some("상태가 바뀐 세션을 선택에서 뺐어요. 목록을 확인하고 다시 골라 주세요.".into());
        }
    }

    fn review_transfer(&mut self, close: bool) {
        self.revalidate_transfer_selection();
        let pending = if close {
            if self.transfer.shell_selected.is_empty() {
                self.transfer.error = Some("닫을 셸을 먼저 골라 주세요.".into());
                return;
            }
            TransferConfirmation::Close(self.transfer.shell_selected.iter().cloned().collect())
        } else {
            match transfer_request(&self.transfer, &self.data.transfer) {
                Ok(request) => TransferConfirmation::Move(request),
                Err(error) => { self.transfer.error = Some(error); return; }
            }
        };
        self.transfer.pending = Some(pending);
        self.transfer.results.clear();
        self.transfer.error = None;
        self.transfer.step = TransferStep::Confirm;
        self.scroll = 0.0;
        self.input = None;
    }

    fn run_transfer(&mut self, backend: Arc<dyn Backend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
        if transfer_fixture_active() { self.transfer.error = Some("검증 화면에서는 실제 이사·닫기를 실행하지 않아요".into()); return; }
        if self.transfer.busy { return; }
        self.revalidate_transfer_selection();
        let Some(mut pending) = self.transfer.pending.take() else { return };
        if let TransferConfirmation::Move(ref request) = pending {
            match transfer_request(&self.transfer, &self.data.transfer) {
                Ok(current) if current.sessions == request.sessions
                    && current.destination_machine == request.destination_machine
                    && current.destination_room == request.destination_room => {}
                _ => {
                    self.transfer.error = Some("선택이나 도착 방이 바뀌었어요. 다시 확인해 주세요.".into());
                    self.transfer.step = TransferStep::Destination;
                    return;
                }
            }
        }
        let ids = match &mut pending {
            TransferConfirmation::Move(request) => {
                request.confirmed = true;
                request.sessions.clone()
            }
            TransferConfirmation::Close(ids) => ids.clone(),
        };
        self.transfer.result_names = ids.iter().filter_map(|id| {
            self.data.transfer.sessions.iter().find(|row| &row.identity == id).map(|row| (id.canonical_key(), transfer_session_name(row)))
        }).collect();
        self.transfer.results = ids.into_iter().map(|source| TransferResult {
            source, status: TransferStatus::Waiting, message: "대기 중".into(), destination: None,
        }).collect();
        self.transfer.busy = true;
        self.transfer.error = None;
        self.transfer_generation += 1;
        let generation = self.transfer_generation;
        let mailbox = self.mailbox.clone();
        std::thread::spawn(move || {
            let mut report = |result| {
                mailbox.lock().unwrap().transfers.push(TransferEnvelope { generation, result: Some(result), finished: false });
                let _ = proxy.send_event(UserEvent::Redraw);
            };
            let results = match pending {
                TransferConfirmation::Move(request) => crate::session_transfer::execute_with_progress(&backend, request, &mut report),
                TransferConfirmation::Close(ids) => crate::session_transfer::close_shells_with_progress(&backend, ids, true, &mut report),
            };
            for result in results { report(result); }
            mailbox.lock().unwrap().transfers.push(TransferEnvelope { generation, result: None, finished: true });
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
}

fn transfer_request(ui: &TransferUi, data: &TransferSnapshot) -> std::result::Result<TransferRequest, String> {
    if ui.selected.is_empty() { return Err("이사할 세션을 먼저 골라 주세요.".into()); }
    for id in &ui.selected {
        if !data.sessions.iter().any(|row| &row.identity == id && row.harness.is_some() && row.unavailable_reason.is_none()) {
            return Err("상태가 바뀐 세션이 있어요. 목록을 새로고침해 주세요.".into());
        }
    }
    let machine = data.machines.iter().find(|machine| machine.id == ui.destination)
        .ok_or_else(|| "도착할 기기를 골라 주세요.".to_string())?;
    if ui.selected.iter().any(|id| id.machine_id == machine.id) {
        return Err("현재 실행 중인 기기와 다른 기기를 골라 주세요.".into());
    }
    if !machine.online || !machine.room_transfer_supported {
        return Err(machine.unavailable_reason.clone().unwrap_or_else(|| "이 기기는 아직 방을 선택해 이사할 수 없어요.".into()));
    }
    let destination_room = if ui.new_room {
        let name = ui.room_name.trim();
        if name.is_empty() { return Err("새 방 이름을 적어 주세요.".into()); }
        RoomTarget::New(name.to_string())
    } else {
        let room = ui.room.as_ref().filter(|id| machine.rooms.iter().any(|room| &room.id == *id))
            .ok_or_else(|| "도착할 방을 골라 주세요.".to_string())?;
        RoomTarget::Existing(room.clone())
    };
    let mut sessions: Vec<_> = ui.selected.iter().cloned().collect();
    sessions.sort_by_key(SessionIdentity::canonical_key);
    Ok(TransferRequest { sessions, destination_machine: machine.id.clone(), destination_room, confirmed: false })
}

pub(crate) fn confirmed_resume_pane(
    created: Option<String>,
) -> std::result::Result<String, String> {
    created.ok_or_else(|| "이어받을 pane을 만들지 못했어요".to_string())
}

#[derive(Clone, Debug)]
pub(crate) enum WorkerAction {
    StopBackground(LocalBackgroundProcess),
    ScheduleAdd {
        kind: String,
        surface: String,
        text: String,
        minutes: u64,
        at_ts: f64,
    },
    ScheduleToggle(String),
    ScheduleDelete(String),
    GitCommit {
        cwd: String,
        files: Vec<String>,
        message: String,
    },
    GitPush { cwd: String },
    ViewSession(SessionIdentity),
}

fn execute_action(backend: &Arc<dyn Backend>, action: WorkerAction) -> anyhow::Result<String> {
    if transfer_fixture_active() { anyhow::bail!("검증 화면에서는 실제 작업을 실행하지 않아요"); }
    match action {
        WorkerAction::StopBackground(target) => {
            if target.pid == 0 {
                anyhow::bail!("종료할 세션 pid가 없어요");
            }
            let output = crate::proc::command("kill")
                .arg("-TERM")
                .arg(target.pid.to_string())
                .output()?;
            if !output.status.success() {
                anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
            }
            Ok("백그라운드 세션을 종료했어요".to_string())
        }
        WorkerAction::ScheduleAdd {
            kind,
            surface,
            text,
            minutes,
            at_ts,
        } => {
            let interval = minutes.max(1) * 60;
            kasa_mcp::schedule_add(&kind, &surface, &text, interval, at_ts, "")?;
            Ok("스케줄을 등록했어요".to_string())
        }
        WorkerAction::ScheduleToggle(id) => {
            if !kasa_mcp::schedule_toggle(&id) {
                anyhow::bail!("스케줄을 찾지 못했어요");
            }
            Ok("스케줄 상태를 바꿨어요".to_string())
        }
        WorkerAction::ScheduleDelete(id) => {
            if !kasa_mcp::schedule_delete(&id) {
                anyhow::bail!("스케줄을 찾지 못했어요");
            }
            Ok("스케줄을 지웠어요".to_string())
        }
        WorkerAction::GitCommit {
            cwd,
            files,
            message,
        } => {
            let result = kasa_mcp::git::git_commit(std::path::Path::new(&cwd), &files, &message);
            if result.get("ok").and_then(|value| value.as_bool()) != Some(true) {
                anyhow::bail!(
                    "{}",
                    result
                        .get("output")
                        .and_then(|value| value.as_str())
                        .unwrap_or("커밋하지 못했어요")
                );
            }
            Ok("커밋했어요".to_string())
        }
        WorkerAction::GitPush { cwd } => {
            let result = kasa_mcp::git::git_push(std::path::Path::new(&cwd));
            if result.get("ok").and_then(|value| value.as_bool()) != Some(true) {
                anyhow::bail!(
                    "{}",
                    result
                        .get("output")
                        .and_then(|value| value.as_str())
                        .unwrap_or("푸시하지 못했어요")
                );
            }
            Ok("푸시했어요".to_string())
        }
        WorkerAction::ViewSession(identity) => {
            crate::session_transfer::focus_session(backend, &identity)?;
            Ok("선택한 세션을 열었어요".into())
        }
    }
}

fn transfer_fixture_active() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("KASATERM_TEST_TRANSFER_FIXTURE").is_some()
        && std::env::var_os("KASATERM_SETTINGS_FILE").is_some()
        && std::env::var_os("KASATERM_SESSION_FILE").is_some()
        && std::env::var_os("KASATERM_SOCKET_PATH").is_some()
        && std::env::var("KASATERM_AUTORESTORE").is_ok_and(|value| value == "fresh")
}

pub(crate) fn board_fixture_active() -> bool {
    cfg!(debug_assertions)
        && crate::verification_run()
        && std::env::var_os("KASATERM_TEST_BOARD_FIXTURE").is_some()
        && std::env::var("KASATERM_AUTORESTORE").as_deref() == Ok("fresh")
        && std::env::var_os("TMPDIR").is_some_and(|root| {
            let root = std::path::PathBuf::from(root);
            let temporary = ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders"].iter().any(|base| root.starts_with(base))
                && root.file_name().is_some_and(|name| name.to_string_lossy().starts_with("kasaterm-board-"));
            temporary && ["KASATERM_SETTINGS_FILE", "KASATERM_SESSION_FILE", "KASATERM_SOCKET_PATH", "KASATERM_WINDOW_FILE", "KASATERM_VIEWER_STATE_FILE"].iter().all(|name| {
                std::env::var_os(name).is_some_and(|path| board_fixture_path(&root, std::path::Path::new(&path)))
            })
        })
}

fn board_fixture_path(root: &std::path::Path, path: &std::path::Path) -> bool {
    path.is_absolute() && path != root && path.starts_with(root)
        && !path.components().any(|part| matches!(part, std::path::Component::ParentDir))
}

#[cfg(debug_assertions)]
fn board_fixture() -> OverviewData {
    let failed = |message: &str| OverviewData { error: Some(message.into()), ..Default::default() };
    if !board_fixture_active() { return failed("검증 환경이 분리되지 않아 가상 보드를 열지 않았어요"); }
    let name = std::env::var("KASATERM_TEST_BOARD_FIXTURE").unwrap_or_default();
    let value = if name == "synthetic" {
        board_probe_value()
    } else {
        let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap());
        if !board_fixture_path(&root, std::path::Path::new(&name)) { return failed("검증 파일이 임시 폴더 밖에 있어 열지 않았어요"); }
        let Ok(body) = std::fs::read_to_string(&name) else { return failed("검증 파일을 읽지 못했어요"); };
        let Ok(value) = serde_json::from_str(&body) else { return failed("검증 파일의 형식을 읽지 못했어요"); };
        value
    };
    let local = value.get("local_machine_id").and_then(|v| v.as_str()).map(str::to_string);
    let gap = value.get("reset_required").and_then(|v| v.as_bool()) == Some(true);
    let mut data = overview_from_value(value).unwrap_or_else(|error| failed(&error));
    data.local_machine_id = local;
    data.gap = gap.then(|| "변경 기록 일부를 이어받지 못했어요. 현재 전체 목록을 다시 확인했습니다.".into());
    data
}

#[cfg(any(test, debug_assertions))]
fn board_probe_value() -> serde_json::Value {
    let at = 1_000_000_u64;
    let mut panes = Vec::new();
    for (index, (machine, machine_label, room, character, status, freshness)) in [
        ("device-a", "작업 컴퓨터", "제품", "아로나", "working", "fresh"),
        ("device-a", "작업 컴퓨터", "제품", "모모이", "waiting", "fresh"),
        ("device-a", "작업 컴퓨터", "문서", "미도리", "idle", "fresh"),
        ("device-b", "연결 컴퓨터", "제품", "아로나", "idle", "fresh"),
        ("device-b", "연결 컴퓨터", "자료", "", "unknown", "fresh"),
        ("device-c", "응답 없는 컴퓨터", "제품", "유즈", "working", "stale"),
    ].into_iter().enumerate() {
        panes.push(serde_json::json!({"id":format!("{machine}/surface-{index}"),
            "address":{"machine_id":machine,"surface_key":format!("surface-{index}"),"surface_id":format!("%{}", index % 3 + 1),"session_id":format!("session-{index}"),"instance_id":"fixture-instance"},
            "machine_label":machine_label,"room_id":room,"room_label":room,"character":character,"harness":if character.is_empty() {"shell"} else {"codex"},
            "title":"주문 내역 화면 점검","request":"좁은 화면에서도 주문 상태와 다음 행동을 읽을 수 있게 정리해 주세요. 긴 한글 문장과 https://example.test/a/very-long-unbroken-path-for-layout-checking 도 잘리지 않아야 합니다.",
            "progress":if status == "waiting" {"연결할 자료를 선택해 주세요"} else {"요청과 진행을 나누고, 이전 기록이 다른 창에 표시되지 않는지 확인하고 있어요"},
            "status":status,"status_reason":if status == "unknown" {"아직 지원되는 활동 신호가 없어요"} else {""},
            "done_outcome":if index == 3 {Some("succeeded")} else {None},"done_summary":if index == 3 {Some("확인을 마쳤고 변경을 남겼어요")} else {None},
            "observed_at_ms":if freshness == "fresh" {at-4_000} else {at-180_000},"freshness":freshness}));
    }
    serde_json::json!({"schema_version":1,"scope":"all","cursor":"fixture:9","observed_at_ms":at,"local_machine_id":"device-a","reset_required":true,
        "sources":[{"machine_id":"device-a","label":"작업 컴퓨터","state":"online","observed_at_ms":at-4_000,"complete":true},
            {"machine_id":"device-b","label":"연결 컴퓨터","state":"online","observed_at_ms":at-4_000,"complete":true},
            {"machine_id":"device-c","label":"응답 없는 컴퓨터","state":"offline","observed_at_ms":at-180_000,"complete":false,"error":"기기에서 응답하지 않아요"}],
        "panes":panes,"recent_changes":[{"cursor":"fixture:9","at_ms":at-3_000,"kind":"pane_changed","pane_id":"device-a/surface-1","machine_id":"device-a","summary":"자료 선택을 기다리고 있어요"},
            {"cursor":"fixture:8","at_ms":at-6_000,"kind":"pane_changed","pane_id":"device-b/surface-3","machine_id":"device-b","summary":"화면 확인을 마쳤다는 보고가 도착했어요"}]})
}

#[cfg(debug_assertions)]
fn transfer_fixture() -> Option<TransferSnapshot> {
    if !transfer_fixture_active() { return None; }
    let path = std::env::var("KASATERM_TEST_TRANSFER_FIXTURE").ok()?;
    Some(std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| TransferSnapshot { errors: vec!["검증 데이터를 읽지 못했어요".into()], ..Default::default() }))
}

fn collect_data(
    backend: &Arc<dyn Backend>,
    target_window: usize,
    target_cwd: &str,
    tab: BoardTab,
    previous: &BoardData,
    selection: Option<OverviewSelection>,
) -> BoardData {
    if tab == BoardTab::Overview {
        let overview = collect_overview(backend, &previous.overview, selection);
        let names = overview.panes.iter().filter_map(|row| row.character.as_deref()).collect::<HashSet<_>>();
        let faces = collect_overview_faces(names);
        return BoardData { overview: Arc::new(overview), faces: Arc::new(faces), error: None, ..previous.clone() };
    }
    let mut errors = Vec::new();
    let mut agents = match backend.collab_board() {
        Ok(rows) => rows,
        Err(error) => {
            errors.push(error.to_string());
            Vec::new()
        }
    };
    agents.extend(
        kasa_mcp::remoteboard::board_rows()
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok()),
    );
    agents.retain(|row| row.machine.is_some() || row.window_idx == target_window);
    let transfer = crate::session_transfer::collect(backend);
    let faces = agents
        .iter()
        .filter_map(|row| row.character.as_deref())
        .chain(transfer.sessions.iter().map(|row| row.name.as_str()).filter(|name| !name.is_empty()))
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let slug = theme::character_slug_any(name)?;
            let (rgba, width, height) = sprites::student_profile_rgba(slug)?;
            Some(FaceAsset {
                name: name.to_string(),
                key: format!("board:{slug}:profile"),
                rgba: Arc::new(rgba),
                width,
                height,
            })
        })
        .collect();
    let background = collect_background(backend).unwrap_or_else(|error| {
        errors.push(error.to_string());
        Vec::new()
    });
    let schedules = kasa_mcp::schedule_snapshot();
    let git = collect_git(target_cwd);
    BoardData {
        agents: Arc::new(agents),
        background: Arc::new(background),
        schedules: Arc::new(schedules),
        transfer: Arc::new(transfer),
        git: Arc::new(git),
        faces: Arc::new(faces),
        error: (!errors.is_empty()).then(|| errors.join(" · ")),
        overview: previous.overview.clone(),
    }
}

fn collect_overview(
    backend: &Arc<dyn Backend>,
    previous: &OverviewData,
    selection: Option<OverviewSelection>,
) -> OverviewData {
    let result = backend.collab_snapshot(&serde_json::json!({"scope": "all"}))
        .and_then(|value| overview_from_value(value).map_err(anyhow::Error::msg));
    let mut data = match result {
        Ok(data) => data,
        Err(error) => {
            return overview_failed(previous, format!("보드를 갱신하지 못했어요. 마지막 확인 내용을 보여줍니다. {error}"));
        }
    };
    data.gap = previous.gap.clone();
    data.local_machine_id = previous.local_machine_id.clone().or_else(|| {
        backend.collab_snapshot(&serde_json::json!({"scope": "local"})).ok()
            .and_then(|local| local.get("sources")?.as_array()?.first()?.get("machine_id")?.as_str().map(str::to_string))
    });
    if !previous.cursor.is_empty() {
        match backend.collab_changes(&serde_json::json!({"scope": "all", "since": previous.cursor, "limit": 100})) {
            Ok(value) => {
                if value.get("reset_required").and_then(|v| v.as_bool()) == Some(true) {
                    data.gap = Some("변경 기록 일부를 이어받지 못했어요. 현재 전체 목록을 다시 확인했습니다.".into());
                }
                let changes = value.get("changes").and_then(|v| serde_json::from_value::<Vec<OverviewChange>>(v.clone()).ok()).unwrap_or_default();
                merge_overview_changes(&mut data.recent_changes, &previous.recent_changes, changes);
                if value.get("has_more").and_then(|v| v.as_bool()) == Some(true) {
                    data.gap = Some("그사이 변경이 많아 최근 기록만 보여줍니다. 현재 전체 목록은 갱신했어요.".into());
                }
            }
            Err(_) => {
                data.gap = Some("변경 기록을 이어받지 못했어요. 현재 목록을 기준으로 확인해 주세요.".into());
                merge_overview_changes(&mut data.recent_changes, &previous.recent_changes, Vec::new());
            }
        }
    }
    if let Some(selection) = selection {
        if data.panes.iter().any(|row| row.id == selection.id && row.address == selection.address) {
            let mut detail = OverviewDetail { selection: selection.clone(), observed_at_ms: data.observed_at_ms, lines: Vec::new(), error: None };
            match backend.collab_inspect(&serde_json::json!({"address": selection.address, "limit": 20})) {
                Ok(value) => {
                    if value.get("address").and_then(|address| serde_json::from_value::<BoardAddress>(address.clone()).ok()).as_ref() != Some(&selection.address) {
                        detail.error = Some("창이 바뀌어 상세 내용을 표시하지 않았어요. 목록에서 다시 선택해 주세요.".into());
                    } else {
                        detail.observed_at_ms = value.get("observed_at_ms").and_then(|v| v.as_u64()).unwrap_or(data.observed_at_ms);
                        detail.lines = overview_detail_lines(&value);
                    }
                }
                Err(_) => {
                    detail.error = Some("상세 내용을 확인하지 못했어요. 연결을 확인한 뒤 새로고침해 주세요.".into());
                    if let Some(old) = previous.detail.as_ref().filter(|old| old.selection == selection) {
                        detail.lines = old.lines.clone();
                        detail.observed_at_ms = old.observed_at_ms;
                    }
                }
            }
            data.detail = Some(detail);
        }
    }
    data
}

fn overview_failed(previous: &OverviewData, error: String) -> OverviewData {
    let mut cached = previous.clone();
    cached.error = Some(error);
    cached.observed_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.as_millis() as u64).unwrap_or(previous.observed_at_ms);
    for row in &mut cached.panes { row.freshness = "stale".into(); }
    for source in &mut cached.sources { source.complete = false; }
    if let Some(detail) = &mut cached.detail {
        detail.error = Some("상세 내용을 다시 확인하지 못했어요. 마지막 확인 내용을 보여줍니다.".into());
    }
    cached
}

fn collect_overview_faces(names: HashSet<&str>) -> Vec<FaceAsset> {
    names.into_iter().filter_map(|name| {
        let slug = theme::character_slug_any(name)?;
        let (rgba, width, height) = sprites::student_profile_rgba(slug)?;
        Some(FaceAsset { name: name.into(), key: format!("board:{slug}:profile"), rgba: Arc::new(rgba), width, height })
    }).collect()
}

fn overview_from_value(value: serde_json::Value) -> Result<OverviewData, String> {
    let mut data: OverviewData = serde_json::from_value(value).map_err(|_| "보드 응답을 읽지 못했어요".to_string())?;
    if data.schema_version != 1 { return Err("보드 형식이 달라 갱신하지 못했어요".into()); }
    data.panes.retain(|row| !row.id.is_empty() && !row.address.machine_id.is_empty() && !row.address.surface_key.is_empty() && !row.address.surface_id.is_empty());
    data.panes.truncate(2000);
    data.sources.truncate(100);
    data.recent_changes.truncate(30);
    Ok(data)
}

fn merge_overview_changes(current: &mut Vec<OverviewChange>, previous: &[OverviewChange], changes: Vec<OverviewChange>) {
    current.extend(previous.iter().cloned());
    current.extend(changes);
    current.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.cursor.cmp(&a.cursor)));
    let mut seen = HashSet::new();
    current.retain(|row| seen.insert(row.cursor.clone()));
    current.truncate(30);
}

fn overview_detail_lines(value: &serde_json::Value) -> Vec<String> {
    let rows = ["activity", "events", "items"].iter().find_map(|key| value.get(key).and_then(|v| v.as_array()));
    let Some(rows) = rows else { return Vec::new(); };
    rows.iter().take(20).filter_map(|row| {
        if let Some(text) = row.as_str() { return Some(board_plain(text, 1600)); }
        let kind = ["role", "kind", "type"].iter().find_map(|key| row.get(key).and_then(|v| v.as_str())).unwrap_or("");
        let body = ["summary", "text", "content", "message", "label"].iter()
            .find_map(|key| row.get(key).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()))?;
        let label = if row.get("is_error").and_then(|v| v.as_bool()) == Some(true) { "오류" } else {
            match kind { "user" | "request" | "prompt" => "요청", "assistant" | "response" | "say" => "응답", "tool" | "tool_call" => "도구", "result" => "결과", "error" => "오류", _ => "활동" }
        };
        let name = row.get("name").and_then(|v| v.as_str()).filter(|name| !name.is_empty()).map(|name| format!(" · {name}")).unwrap_or_default();
        Some(board_plain(&format!("{label}{name} · {body}"), 1600))
    }).collect()
}

fn board_plain(value: &str, max_chars: usize) -> String {
    value.chars().filter(|ch| !ch.is_control() || ch.is_whitespace()).take(max_chars)
        .collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn overview_name(row: &OverviewPane) -> &str {
    row.character.as_deref().filter(|name| !name.is_empty())
        .unwrap_or_else(|| if row.title.is_empty() { &row.address.surface_id } else { &row.title })
}

fn overview_status(row: &OverviewPane) -> (&'static str, u8) {
    if row.freshness == "offline" { return ("연결 끊김", 1); }
    if row.freshness != "fresh" { return ("오래된 정보", 2); }
    if matches!(row.status.as_str(), "waiting" | "attention" | "blocked") { return ("확인 필요", 0); }
    match row.done_outcome.as_deref() {
        Some("succeeded") => return ("완료 보고", 5),
        Some("failed") => return ("실패 보고", 0),
        _ => {}
    }
    match row.status.as_str() {
        "working" | "running" | "building" | "thinking" | "compacting" => ("작업 중", 3),
        "idle" => ("대기 중", 4),
        _ => ("미확인", 2),
    }
}

fn sort_overview_panes(rows: &mut Vec<&OverviewPane>, sort: OverviewSort) {
    rows.sort_by(|a, b| {
        let selected = match sort {
            OverviewSort::Name => std::cmp::Ordering::Equal,
            OverviewSort::Status => overview_status(a).1.cmp(&overview_status(b).1),
            OverviewSort::Recent => b.observed_at_ms.cmp(&a.observed_at_ms),
        };
        selected.then_with(|| overview_name(a).cmp(overview_name(b))).then_with(|| a.id.cmp(&b.id))
    });
}

fn overview_is_local(data: &OverviewData, address: &BoardAddress) -> bool {
    data.local_machine_id.as_deref() == Some(&address.machine_id)
        && data.panes.iter().any(|row| row.address == *address && row.freshness == "fresh" && !row.detached)
}

fn overview_current_detail<'a>(data: &'a OverviewData, ui: &OverviewUi) -> Option<&'a OverviewDetail> {
    data.detail.as_ref().filter(|detail| ui.selection.as_ref() == Some(&detail.selection)
        && data.panes.iter().any(|row| row.id == detail.selection.id && row.address == detail.selection.address))
}

fn collect_background(backend: &Arc<dyn Backend>) -> anyhow::Result<Vec<BackgroundRow>> {
    let output = crate::proc::command(kasa_mcp::claude_bin())
        .args(["agents", "--json", "--all"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let local = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)?;
    let values = local
        .into_iter()
        .map(|value| (value, None))
        .chain(
            kasa_mcp::remoteboard::background_agents()
                .into_iter()
                .map(|value| {
                    let machine = value
                        .get("machine")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    (value, machine)
                }),
        );
    let pane_sids = backend.pane_session_ids().unwrap_or_default();
    Ok(values
        .into_iter()
        .map(|(value, machine)| {
            let session_id = text_value(&value, "sessionId");
            let parent_surface = value
                .get("parentSurface")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    pane_sids
                        .iter()
                        .find(|(_, sid)| sid == &session_id)
                        .map(|(pane, _)| pane.clone())
                });
            BackgroundRow {
                id: text_value(&value, "id"),
                session_id,
                name: text_value(&value, "name"),
                cwd: text_value(&value, "cwd"),
                state: text_value(&value, "state"),
                status: text_value(&value, "status"),
                kind: text_value(&value, "kind"),
                pid: value.get("pid").and_then(|value| value.as_u64()).unwrap_or(0) as u32,
                started_at: value
                    .get("startedAt")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                parent_surface,
                machine,
            }
        })
        .collect())
}

fn collect_git(cwd: &str) -> GitSnapshot {
    if cwd.is_empty() {
        return GitSnapshot {
            no_repo: true,
            ..Default::default()
        };
    }
    let value = kasa_mcp::git::git_status(std::path::Path::new(cwd));
    let mut rows = Vec::new();
    for (key, marker) in [
        ("staged", 'S'),
        ("modified", 'M'),
        ("untracked", 'U'),
    ] {
        for path in value
            .get(key)
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
        {
            if rows.iter().any(|row: &GitRow| row.path == path) {
                continue;
            }
            rows.push(GitRow {
                path: path.to_string(),
                marker,
            });
        }
    }
    GitSnapshot {
        branch: text_value(&value, "branch"),
        ahead: value.get("ahead").and_then(|value| value.as_u64()).unwrap_or(0) as u32,
        behind: value
            .get("behind")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32,
        insertions: value
            .get("insertions")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32,
        deletions: value
            .get("deletions")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32,
        no_repo: value
            .get("no_repo")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        error: text_value(&value, "error"),
        rows,
    }
}

fn text_value(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn contains(rect: Rect, point: (f32, f32)) -> bool {
    point.0 >= rect.0
        && point.0 <= rect.0 + rect.2
        && point.1 >= rect.1
        && point.1 <= rect.1 + rect.3
}

pub(crate) fn paint(g: &mut gpu::GpuRenderer, snapshot: &Snapshot) -> PaintOutput {
    let (ax, ay, aw, ah) = snapshot.area;
    let nav_w = if aw < 460.0 { 112.0 } else if aw < 760.0 { 154.0 } else { 190.0 };
    let mut hits = Vec::new();
    let mut caret_rect = None;
    g.rect(ax, ay, aw, ah, theme::bg());
    // 목업(플랫): 옆 목록은 구분선 없이 배경만 다르고, 머리글은 작은 흐림 글자 —
    // 설정창과 같은 틀. 아이콘·강조 막대는 걷었다.
    g.rect(ax, ay, nav_w, ah, theme::panel_bg());
    text(g, ax + 20.0, ay + 24.0, "운영 보드", 13.0, theme::text_dim(), false);

    let mut ny = ay + 56.0;
    for tab in BoardTab::ALL {
        let rect = (ax + 12.0, ny, nav_w - 24.0, 32.0);
        let selected = snapshot.tab == tab;
        let hover = contains(rect, snapshot.cursor);
        if selected || hover {
            round_rect(
                g,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                theme::radius_md().min(5.0),
                if selected { theme::surface_active() } else { theme::surface_hover() },
            );
        }
        let label = fit(g, tab.label(), rect.2 - 24.0, 12.0, selected);
        text(
            g,
            rect.0 + 12.0,
            rect.1 + 9.0,
            &label,
            12.0,
            if selected { theme::text() } else { theme::text_dim() },
            selected,
        );
        hit(g, &mut hits, Target::Tab(tab), rect, false);
        g.hover_pointer |= hover;
        ny += 36.0;
    }
    let back = (ax + 12.0, ay + ah - 46.0, nav_w - 24.0, 32.0);
    if contains(back, snapshot.cursor) {
        round_rect(g, back.0, back.1, back.2, back.3, theme::radius_md().min(5.0), theme::surface_hover());
        g.hover_pointer = true;
    }
    g.queue_icon("chevron-left", back.0 + 10.0, back.1 + 9.0, 15.0, theme::text_dim());
    let back_label = fit(g, "작업 방으로", back.2 - 39.0, 12.0, false);
    text(g, back.0 + 33.0, back.1 + 9.0, &back_label, 12.0, theme::text_dim(), false);
    hit(g, &mut hits, Target::Return, back, false);

    let content_x = ax + nav_w + if aw < 760.0 { 20.0 } else { 28.0 };
    let content_w = (aw - nav_w - if aw < 760.0 { 40.0 } else { 56.0 })
        .max(1.0)
        .min(800.0);
    text(g, content_x, ay + 22.0, snapshot.tab.label(), 20.0, theme::text(), true);
    // 기준 pane 알약: 채움 없는 테두리, pane 이름만 강조색(목업 .head .pill).
    let refresh = (content_x + content_w - 30.0, ay + 18.0, 30.0, 30.0);
    let pane = if snapshot.tab == BoardTab::Overview {
        "모든 방".into()
    } else if snapshot.target_cwd.is_empty() {
        snapshot.target_pane.clone()
    } else {
        format!("{} {}", snapshot.target_pane, short_path(&snapshot.target_cwd))
    };
    let prefix = if snapshot.tab == BoardTab::Overview { "" } else { "기준 pane · " };
    let prefix_w = g.measure_chrome_text(prefix, 11.0, false);
    let pane = fit(g, &pane, content_w * 0.5 - prefix_w - 40.0, 11.0, false);
    let pane_w = g.measure_chrome_text(&pane, 11.0, false);
    let pill = (refresh.0 - 10.0 - (prefix_w + pane_w + 20.0), ay + 21.0, prefix_w + pane_w + 20.0, 24.0);
    if content_w >= 300.0 {
        g.round_rect_stroke(pill.0, pill.1, pill.2, pill.3, theme::radius_md().min(5.0), 1.0, theme::border());
        text(g, pill.0 + 10.0, pill.1 + 6.0, prefix, 11.0, theme::text_dim(), false);
        text(g, pill.0 + 10.0 + prefix_w, pill.1 + 6.0, &pane, 11.0, theme::accent(), false);
    }
    icon_button(g, snapshot, &mut hits, refresh, "rotate-cw", Target::Refresh);
    if snapshot.refreshing && content_w >= 430.0 {
        text(g, pill.0 - 52.0, pill.1 + 6.0, "갱신 중", 10.5, theme::text_mute(), false);
    }
    let description = fit(g, snapshot.tab.desc(), content_w, 11.5, false);
    text(g, content_x, ay + 52.0, &description, 11.5, theme::text_dim(), false);
    divider(g, content_x, ay + 82.0, content_w);

    let body_top = ay + 97.0;
    let body_bottom = ay + ah - 14.0;
    let view_h = (body_bottom - body_top).max(0.0);
    g.push_clip(content_x, body_top, content_w, view_h);
    let mut y = body_top - snapshot.scroll;
    if let Some((ok, message)) = &snapshot.toast {
        notice(g, content_x, &mut y, content_w, message, *ok);
    }
    if let Some(error) = &snapshot.data.error {
        notice(g, content_x, &mut y, content_w, error, false);
    }
    match snapshot.tab {
        BoardTab::Overview => paint_overview(g, snapshot, &mut hits, content_x, &mut y, content_w),
        BoardTab::Agents => paint_agents(g, snapshot, &mut hits, content_x, &mut y, content_w),
        BoardTab::Schedule => paint_schedule(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        BoardTab::Git => paint_git(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        BoardTab::Machines => paint_machines(g, snapshot, &mut hits, &mut caret_rect, content_x, &mut y, content_w),
    }
    g.pop_clip();
    let content_h = (y + snapshot.scroll - body_top + 18.0).max(view_h);
    // 설정 화면과 같은 자리의 같은 실수 — 글자가 앉는 `content_*` 를 주면 막대가
    // 그 좌우 여백만큼 안으로 들어와 패널 가장자리에서 떨어져 뜬다(2026-09-05).
    // 스크롤되는 영역은 좌측 nav 오른쪽부터 패널 끝까지다.
    let scroll_x = ax + nav_w;
    let scroll_w = (ax + aw - scroll_x).max(0.0);
    crate::native_settings::paint_scroll_affordance(
        g,
        scroll_x,
        body_top,
        scroll_w,
        view_h,
        content_h,
        snapshot.scroll,
    );
    PaintOutput { hits, content_h, view_h, caret_rect }
}

fn paint_overview(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let data = &s.data.overview;
    if s.fixture {
        overview_note(g, x, y, w, "검증용 가상 보드", theme::text_dim());
    }
    if let Some(error) = &data.error {
        overview_note(g, x, y, w, error, theme::danger());
    }
    if data.schema_version == 0 && data.error.is_none() {
        overview_note(g, x, y, w, "연결된 기기와 방을 확인하고 있어요", theme::text_dim());
        return;
    }
    let mut machine_choices = vec![("전체 기기".into(), Target::OverviewMachine(None), s.overview.machine.is_none(), true)];
    let mut sources: Vec<_> = data.sources.iter().collect();
    sources.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.machine_id.cmp(&b.machine_id)));
    for source in &sources {
        machine_choices.push((source.label.clone(), Target::OverviewMachine(Some(source.machine_id.clone())),
            s.overview.machine.as_ref() == Some(&source.machine_id), true));
    }
    overview_choices(g, s, hits, x, y, w, machine_choices);
    let room_choices = overview_room_choices(data, &s.overview);
    if !room_choices.is_empty() {
        overview_choices(g, s, hits, x, y, w, room_choices);
    }
    let choices = [
        ("이름순", OverviewSort::Name), ("상태순", OverviewSort::Status), ("최근 확인순", OverviewSort::Recent),
    ].into_iter().map(|(label, sort)| (label.into(), Target::OverviewSort(sort), s.overview.sort == sort, true)).collect();
    overview_choices(g, s, hits, x, y, w, choices);
    let rows = overview_visible_rows(data, &s.overview);
    let attention = rows.iter().filter(|row| overview_status(row).1 == 0).count();
    let working = rows.iter().filter(|row| overview_status(row).1 == 3).count();
    let uncertain = rows.iter().filter(|row| matches!(overview_status(row).1, 1 | 2)).count();
    let summary = format!("작업 중 {working} · 확인 필요 {attention} · 상태 미확인 {uncertain}");
    overview_note(g, x, y, w, &summary, theme::text_dim());
    if let Some(gap) = &data.gap {
        overview_note(g, x, y, w, gap, theme::danger());
    }
    if data.journal_error.is_some() {
        overview_note(g, x, y, w, "변경 기록을 보관하지 못하고 있어요. 현재 목록은 계속 확인할 수 있습니다.", theme::danger());
    }
    let change_label = if s.overview.show_changes { "최근 변경 접기" } else { "최근 변경 보기" };
    text_button(g, s, hits, (x, *y, 116.0_f32.min(w), 28.0), change_label, Target::OverviewChanges, false);
    *y += 31.0;
    if s.overview.show_changes {
        paint_overview_changes(g, s, hits, x, y, w);
    } else if let Some(change) = data.recent_changes.iter().find(|row| overview_change_visible(row, data, &s.overview)) {
        let line = fit(g, &format!("{} · {}", board_relative_time(data.observed_at_ms, change.at_ms), board_plain(&change.summary, 240)), w, 11.0, false);
        text(g, x, *y, &line, 11.0, theme::text_dim(), false);
        *y += 24.0;
    }
    *y += 8.0;
    let mut group_count = 0;
    for source in sources {
        if s.overview.machine.as_ref().is_some_and(|machine| machine != &source.machine_id) { continue; }
        let source_rows: Vec<_> = rows.iter().copied().filter(|row| row.address.machine_id == source.machine_id).collect();
        let source_state = match source.state.as_str() {
            "online" if source.complete => "연결됨",
            "offline" | "disconnected" => "연결 끊김",
            "stale" | "error" => "오래된 정보",
            _ => "미확인",
        };
        let machine_line = format!("{} · {} · {}", source.label, source_state, board_relative_time(data.observed_at_ms, source.observed_at_ms));
        let machine_line = fit(g, &board_plain(&machine_line, 240), w, 12.5, true);
        text(g, x, *y, &machine_line, 12.5, theme::text(), true);
        *y += 25.0;
        if let Some(error) = &source.error {
            overview_note(g, x, y, w, &format!("{} · 마지막으로 확인한 내용을 유지합니다", board_reason(error)), theme::danger());
        }
        if source_rows.is_empty() {
            let message = if source.state == "online" && source.complete { "표시할 창이 없어요" } else { "연결을 확인하면 이 기기의 창을 불러올 수 있어요" };
            overview_note(g, x, y, w, message, theme::text_dim());
            *y += 14.0;
            continue;
        }
        let mut rooms: Vec<_> = source_rows.iter().map(|row| (row.room_id.clone(), row.room_label.clone())).collect();
        rooms.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        rooms.dedup_by(|a, b| a.0 == b.0);
        rooms.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        for (room_id, room_label) in rooms {
            let room_rows: Vec<_> = source_rows.iter().copied().filter(|row| row.room_id == room_id).collect();
            let name = if room_label.is_empty() { "방 미확인" } else { &room_label };
            let label = fit(g, &format!("{name} · {}개 창", room_rows.len()), w, 11.5, true);
            text(g, x, *y, &label, 11.5, theme::text_dim(), true);
            *y += 24.0;
            divider(g, x, *y, w);
            for row in room_rows { paint_overview_pane(g, s, hits, x, y, w, row); }
            *y += 20.0;
            group_count += 1;
        }
    }
    if group_count == 0 && data.sources.is_empty() {
        overview_note(g, x, y, w, "아직 확인한 기기가 없어요. 새로고침하면 연결된 기기를 다시 확인합니다.", theme::text_dim());
    }
}

fn overview_visible_rows<'a>(data: &'a OverviewData, ui: &OverviewUi) -> Vec<&'a OverviewPane> {
    let mut rows: Vec<_> = data.panes.iter().filter(|row| {
        ui.machine.as_ref().is_none_or(|machine| &row.address.machine_id == machine)
            && ui.room.as_ref().is_none_or(|(machine, id)| &row.address.machine_id == machine && &row.room_id == id)
    }).collect();
    let order: std::collections::HashMap<_, _> = ui.order.iter().enumerate().map(|(index, id)| (id.as_str(), index)).collect();
    rows.sort_by_key(|row| order.get(row.id.as_str()).copied().unwrap_or(usize::MAX));
    rows
}

fn overview_room_choices(data: &OverviewData, ui: &OverviewUi) -> Vec<(String, Target, bool, bool)> {
    let Some(machine) = ui.machine.as_ref().or_else(|| ui.room.as_ref().map(|room| &room.0)) else { return Vec::new(); };
    let mut rooms: Vec<_> = data.panes.iter().filter(|row| &row.address.machine_id == machine)
        .map(|row| (row.room_id.clone(), row.room_label.clone())).collect();
    rooms.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    rooms.dedup_by(|a, b| a.0 == b.0);
    rooms.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let active_missing = ui.room.as_ref().is_some_and(|(selected_machine, id)| selected_machine != machine || !rooms.iter().any(|room| &room.0 == id));
    let mut choices = vec![("모든 방".into(), Target::OverviewRoom(None), ui.room.is_none(), true)];
    choices.extend(rooms.into_iter().map(|(id, label)| {
        let room = (machine.clone(), id);
        let label = if label.is_empty() { "방 미확인".into() } else { label };
        let selected = ui.room.as_ref() == Some(&room);
        (label, Target::OverviewRoom(Some(room)), selected, true)
    }));
    if active_missing {
        choices.push(("선택한 방 · 현재 목록에 없음".into(), Target::OverviewRoom(ui.room.clone()), true, false));
    }
    choices
}

fn overview_change_visible(change: &OverviewChange, data: &OverviewData, ui: &OverviewUi) -> bool {
    ui.machine.as_ref().is_none_or(|machine| machine == &change.machine_id)
        && ui.room.as_ref().is_none_or(|(machine, room)| {
            if machine != &change.machine_id { return false; }
            if change.pane_id.is_none() { return true; }
            if let Some(event_room) = &change.room_id { return room.as_ref() == Some(event_room); }
            data.panes.iter().find(|row| change.pane_id.as_ref() == Some(&row.id))
                .is_none_or(|row| &row.address.machine_id == machine && &row.room_id == room)
        })
}

fn paint_overview_changes(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    let data = &s.data.overview;
    let changes: Vec<_> = data.recent_changes.iter().filter(|row| overview_change_visible(row, data, &s.overview)).take(12).collect();
    if changes.is_empty() {
        overview_note(g, x, y, w, "아직 확인한 변경이 없어요", theme::text_dim());
    }
    for change in changes {
        let start = *y;
        let pane = data.panes.iter().find(|row| change.pane_id.as_ref() == Some(&row.id));
        let source = data.sources.iter().find(|row| row.machine_id == change.machine_id).map(|row| row.label.as_str()).unwrap_or("기기 미확인");
        let room = change.room_label.as_deref().filter(|label| !label.is_empty())
            .or_else(|| pane.map(|row| row.room_label.as_str()))
            .unwrap_or(if change.pane_id.is_none() { "기기 상태" } else { "방 정보 없음" });
        let heading = format!("{} · {} · {}{}", board_relative_time(data.observed_at_ms, change.at_ms), source, room, pane.map(|row| format!(" · {}", overview_name(row))).unwrap_or_default());
        let heading = fit(g, &heading, w, 10.5, false);
        text(g, x, *y, &heading, 10.5, theme::text_dim(), false);
        *y += 18.0;
        let summary = if change.summary.is_empty() {
            match change.kind.as_str() { "removed" | "pane_removed" => "창이 목록에서 사라졌어요", "added" | "pane_added" => "새 창을 확인했어요", _ => "상태가 바뀌었어요" }
        } else { &change.summary };
        overview_note(g, x, y, w, summary, theme::text());
        if let Some(row) = pane {
            hit(g, hits, Target::OverviewDetail(row.id.clone()), (x, start, w, *y - start), false);
        }
        divider(g, x, *y, w);
        *y += 10.0;
    }
}

fn paint_overview_pane(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, row: &OverviewPane) {
    let expanded = s.overview.selection.as_ref().is_some_and(|selected| selected.id == row.id && selected.address == row.address);
    let top = *y;
    let status = overview_status(row);
    let color = overview_status_color(status.1);
    let tx = if w >= 260.0 { x + 44.0 } else { x };
    let tw = w - (tx - x);
    if w >= 260.0 { draw_overview_face(g, s, row, x, top + 12.0, 32.0); }
    let status_w = g.measure_chrome_text(status.0, 11.0, false);
    let name_w = if tw >= 280.0 { tw - status_w - 26.0 } else { tw };
    let name = fit(g, &board_plain(overview_name(row), 160), name_w, 13.0, true);
    text(g, tx, top + 12.0, &name, 13.0, theme::text(), true);
    let status_y = if tw >= 280.0 { top + 14.0 } else { top + 34.0 };
    let status_x = if tw >= 280.0 { x + w - status_w } else { tx + 13.0 };
    circle_rect(g, status_x - 12.0, status_y + 2.0, 6.0, color);
    text(g, status_x, status_y, status.0, 11.0, color, false);
    *y = if tw >= 280.0 { top + 35.0 } else { top + 56.0 };
    let meta = format!("{} · {}", row.harness.as_deref().filter(|value| !value.is_empty()).unwrap_or("하네스 미확인"), board_relative_time(s.data.overview.observed_at_ms, row.observed_at_ms));
    let meta = fit(g, &meta, tw, 10.5, false);
    text(g, tx, *y, &meta, 10.5, theme::text_dim(), false);
    *y += 21.0;
    let request = if row.request.is_empty() { &row.title } else { &row.request };
    if !request.is_empty() { paint_overview_summary(g, tx, y, tw, "요청", request, 2); }
    if !row.progress.is_empty() { paint_overview_summary(g, tx, y, tw, "진행", &row.progress, 2); }
    if let Some(summary) = row.done_summary.as_deref().filter(|summary| !summary.is_empty()) {
        if row.done_outcome.is_some() { paint_overview_summary(g, tx, y, tw, "보고", summary, 2); }
    }
    if row.detached {
        overview_note(g, tx, y, tw, "닫아 둔 창 · 완료 여부는 보고를 확인해 주세요", theme::text_dim());
    }
    if request.is_empty() && row.progress.is_empty() && row.done_summary.is_none() {
        overview_note(g, tx, y, tw, "아직 확인한 요청이나 진행 내용이 없어요", theme::text_dim());
    }
    if status.1 <= 2 {
        if let Some(reason) = row.status_reason.as_deref().filter(|reason| !reason.is_empty()) {
            let reason = fit(g, &board_reason(reason), tw, 10.5, false);
            text(g, tx, *y, &reason, 10.5, theme::text_dim(), false);
            *y += 20.0;
        }
    }
    hit(g, hits, Target::OverviewDetail(row.id.clone()), (x, top, w, *y - top), false);
    let details_label = if expanded { "상세 접기" } else { "상세 보기" };
    text_button(g, s, hits, (tx, *y, 76.0_f32.min(tw), 28.0), details_label, Target::OverviewDetail(row.id.clone()), false);
    *y += 34.0;
    if expanded { paint_overview_detail(g, s, hits, tx, y, tw, row); }
    divider(g, x, *y, w);
    *y += 10.0;
}

fn paint_overview_detail(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, row: &OverviewPane) {
    let data = &s.data.overview;
    let address_label = format!("{}/{}", row.address.machine_id, row.address.surface_key);
    paint_overview_summary(g, x, y, w, "주소", &address_label, 3);
    let local = overview_is_local(data, &row.address);
    let mut choices = vec![("주소 복사".into(), Target::OverviewCopy(row.address.clone()), false, true)];
    if local {
        choices.push(("창으로 이동".into(), Target::OverviewFocus(row.address.clone()), false, true));
        choices.push(overview_save_choice(&row.address));
    }
    overview_choices(g, s, hits, x, y, w, choices);
    if local {
        overview_note(g, x, y, w, "저장은 해당 창에서 실행해 주세요", theme::text_dim());
    }
    let detail = overview_current_detail(data, &s.overview);
    let Some(detail) = detail else {
        overview_note(g, x, y, w, "선택한 창의 최근 내용을 확인하고 있어요", theme::text_dim());
        return;
    };
    if let Some(error) = &detail.error { overview_note(g, x, y, w, error, theme::danger()); }
    let checked = format!("최근 내용 · {} 확인", board_relative_time(data.observed_at_ms, detail.observed_at_ms));
    overview_note(g, x, y, w, &checked, theme::text_dim());
    if detail.lines.is_empty() && detail.error.is_none() {
        overview_note(g, x, y, w, "이 창에서 확인할 수 있는 최근 활동이 없어요", theme::text_dim());
    }
    for line in &detail.lines {
        let lines = board_wrap(&board_plain(line, 1600), w, 5, |line| g.measure_chrome_text(line, 11.0, false));
        for line in lines {
            text(g, x, *y, &line, 11.0, theme::text(), false);
            *y += 18.0;
        }
        *y += 8.0;
    }
}

fn overview_save_choice(address: &BoardAddress) -> (String, Target, bool, bool) {
    // SaveSession queues pane-number-only input; it cannot retain the displayed
    // conversation's identity through the later terminal writes.
    ("저장".into(), Target::OverviewSave(address.clone()), false, false)
}

fn paint_overview_summary(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str, value: &str, max_lines: usize) {
    let label_w = 32.0;
    let lines = board_wrap(&board_plain(value, 1600), (w - label_w).max(0.0), max_lines, |line| g.measure_chrome_text(line, 11.5, false));
    text(g, x, *y, label, 10.5, theme::text_dim(), false);
    for line in lines {
        text(g, x + label_w, *y, &line, 11.5, theme::text(), false);
        *y += 19.0;
    }
    *y += 6.0;
}

fn overview_note(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, value: &str, color: [u8; 4]) {
    let lines = board_wrap(&board_plain(value, 1600), w, 3, |line| g.measure_chrome_text(line, 11.0, false));
    for line in lines {
        text(g, x, *y, &line, 11.0, color, false);
        *y += 18.0;
    }
    *y += 8.0;
}

fn overview_choices(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, choices: Vec<(String, Target, bool, bool)>) {
    let mut cx = x;
    for (label, target, selected, enabled) in choices {
        let label = fit(g, &board_plain(&label, 160), (w - 20.0).max(0.0), 11.0, selected);
        let width = (g.measure_chrome_text(&label, 11.0, selected) + 20.0).min(w);
        if cx > x && cx + width > x + w { cx = x; *y += 34.0; }
        let rect = (cx, *y, width, 28.0);
        let hover = enabled && contains(rect, s.cursor);
        if selected || hover {
            round_rect(g, rect.0, rect.1, rect.2, rect.3, theme::radius_md().min(5.0), if selected { theme::surface_active() } else { theme::surface_hover() });
        }
        text(g, cx + 10.0, *y + 8.0, &label, 11.0, if enabled { theme::text() } else { theme::text_mute() }, selected);
        if enabled { hit(g, hits, target, rect, false); }
        g.hover_pointer |= hover;
        cx += width + 6.0;
    }
    *y += 36.0;
}

fn overview_status_color(rank: u8) -> [u8; 4] {
    match rank { 0 => theme::danger(), 3 => theme::accent(), 5 => theme::success(), _ => theme::text_dim() }
}

fn board_reason(reason: &str) -> String {
    let message = match reason {
        "initial observation pending" => "첫 상태를 확인하고 있어요",
        "remote observation unavailable" | "source absent from current discovery" => "기기에서 최근 상태를 받지 못했어요",
        "local observation unavailable" => "이 기기의 최근 상태를 확인하지 못했어요",
        "live place; supported agent activity unavailable" => "지원되는 에이전트의 활동을 아직 확인하지 못했어요",
        "supported agent observed; transcript activity unavailable" => "실행 중인 에이전트의 최근 내용을 아직 확인하지 못했어요",
        "remote mirror; observe agent on its source machine" => "원격 화면을 보여주는 창이에요. 원래 기기에서 작업 상태를 확인해 주세요",
        "pane attention signal observed" => "응답이나 선택을 기다리고 있어요",
        _ => reason,
    };
    board_plain(message, 240)
}

fn board_relative_time(observed: u64, at: u64) -> String {
    if at == 0 { return "확인 시각 없음".into(); }
    let elapsed = observed.saturating_sub(at) / 1000;
    match elapsed { 0..=4 => "방금".into(), 5..=59 => format!("{elapsed}초 전"), 60..=3599 => format!("{}분 전", elapsed / 60), 3600..=86399 => format!("{}시간 전", elapsed / 3600), _ => format!("{}일 전", elapsed / 86400) }
}

fn board_wrap(value: &str, width: f32, max_lines: usize, mut measure: impl FnMut(&str) -> f32) -> Vec<String> {
    if width <= 0.0 || max_lines == 0 { return Vec::new(); }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        let candidate = format!("{line}{ch}");
        if !line.is_empty() && measure(&candidate) > width {
            lines.push(std::mem::take(&mut line));
            if lines.len() == max_lines {
                let last = lines.last_mut().unwrap();
                while !last.is_empty() && measure(&format!("{last}…")) > width { last.pop(); }
                if measure("…") <= width { last.push('…'); }
                return lines;
            }
        }
        if measure(&ch.to_string()) <= width { line.push(ch); }
    }
    if !line.is_empty() { lines.push(line); }
    lines
}

fn draw_overview_face(g: &mut gpu::GpuRenderer, s: &Snapshot, row: &OverviewPane, x: f32, y: f32, size: f32) {
    if let Some(face) = row.character.as_deref().and_then(|name| s.data.faces.iter().find(|face| face.name == name)) {
        if !g.has_image(&face.key) { g.upload_image(&face.key, &face.rgba, face.width, face.height); }
        g.queue_image_above(&face.key, x, y, size, size);
    } else {
        round_rect(g, x, y, size, size, theme::radius_md(), theme::surface_hover());
        g.queue_icon("terminal", x + 7.0, y + 7.0, size - 14.0, theme::text_dim());
    }
}

fn paint_agents(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    if s.data.background.is_empty() {
        empty(g, x, y, w, "백그라운드 세션이 없어요");
        return;
    }
    // 목업(플랫): 「이 기기」·「원격 기기」 묶음, 카드 대신 구분선 행.
    for (title, remote) in [("이 기기", false), ("원격 기기", true)] {
        let rows: Vec<_> = s.data.background.iter().filter(|row| row.machine.is_some() == remote).collect();
        if rows.is_empty() { continue; }
        section(g, x, y, title, "");
        for row in rows {
            let rect = (x, *y, w, 58.0);
            let state = background_state(row);
            let state_color = match row.state.as_str() {
                "blocked" => theme::danger(),
                "running" | "working" => theme::success(),
                "done" => theme::text_mute(),
                _ => theme::attention(),
            };
            let mut right = rect.0 + rect.2;
            if row.kind == "background" && row.machine.is_none() {
                // 멈춤은 되돌릴 수 없다. 확인을 기다리는 행은 「이어받기」를 접고 그
                // 자리를 확인 버튼에 내준다 — 이 순간 고를 것은 멈출지 말지뿐이다.
                let pending = background_stop_target(row)
                    .filter(|target| s.pending_stop.as_ref() == Some(target));
                if let Some(target) = pending {
                    let cancel = (right - 44.0, rect.1 + 15.0, 44.0, 28.0);
                    text_button(g, s, hits, cancel, "취소", Target::CancelStopBackground, false);
                    let confirm = (cancel.0 - 8.0 - 96.0, rect.1 + 15.0, 96.0, 28.0);
                    button(g, s, hits, confirm, "정말 멈추기", Target::ConfirmStopBackground(target), true);
                    right = confirm.0;
                } else {
                    let stop_rect = (right - 52.0, rect.1 + 15.0, 52.0, 28.0);
                    if let Some(target) = background_stop_target(row) {
                        text_button(g, s, hits, stop_rect, "멈추기", Target::StopBackground(target), true);
                    }
                    let resume = (stop_rect.0 - 8.0 - 84.0, rect.1 + 15.0, 84.0, 28.0);
                    button(
                        g,
                        s,
                        hits,
                        resume,
                        "이어받기",
                        Target::ResumeBackground(
                            if row.id.is_empty() { row.session_id.clone() } else { row.id.clone() },
                            row.cwd.clone(),
                        ),
                        true,
                    );
                    right = resume.0;
                }
            } else if row.kind == "background" {
                let note = "원격 기기에서만 제어 가능";
                let nw = g.measure_chrome_text(note, 11.0, false);
                text(g, right - nw, rect.1 + 23.0, note, 11.0, theme::text_mute(), false);
                right -= nw;
            }
            let sw = g.measure_chrome_text(state, 11.0, false);
            let sx = right - 16.0 - sw;
            circle_rect(g, sx - 14.0, rect.1 + 26.0, 8.0, state_color);
            text(g, sx, rect.1 + 23.0, state, 11.0, theme::text_dim(), false);
            let text_w = (sx - 14.0 - 16.0 - rect.0).max(60.0);
            let label = if row.name.is_empty() { &row.id } else { &row.name };
            let label = fit(g, label, text_w, 12.5, true);
            text(g, rect.0, rect.1 + 11.0, &label, 12.5, theme::text(), true);
            let origin = row
                .parent_surface
                .as_deref()
                .map(|pane| format!("연결 {pane}"))
                .unwrap_or_else(|| format_age(row.started_at));
            let location = row.machine.as_deref().unwrap_or("이 기기");
            let sub = format!("{} · {} · {}", short_path(&row.cwd), origin, location);
            let sub = fit(g, &sub, text_w, 10.5, false);
            text(g, rect.0, rect.1 + 32.0, &sub, 10.5, theme::text_dim(), false);
            divider(g, x, rect.1 + 57.0, w);
            *y += 58.0;
        }
        *y += 16.0;
    }
    notice(g, x, y, w, "「멈추기」를 두 번 누르면 정말 멈춰요 — 첫 번째는 확인, 두 번째가 실행", true);
}

fn paint_schedule(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    // 목업(플랫): 이름표 왼쪽 · 조작 오른쪽의 40px 행, 구분선으로 가른다.
    section(g, x, y, "새 스케줄", "");
    text(g, x, *y + 13.0, "방식", 12.0, theme::text(), false);
    segmented(
        g,
        s,
        hits,
        x,
        *y + 7.0,
        w,
        &[
            ("반복 루프", s.schedule_kind == "loop", Target::ScheduleKind("loop".into())),
            ("예약", s.schedule_kind == "cron", Target::ScheduleKind("cron".into())),
            ("타이머", s.schedule_kind == "timer", Target::ScheduleKind("timer".into())),
        ],
    );
    divider(g, x, *y + 39.0, w);
    *y += 40.0;
    let detail = if s.schedule_kind == "cron" {
        (&s.schedule_at, BoardInput::ScheduleAt, "Unix 시각(초)")
    } else {
        (&s.schedule_minutes, BoardInput::ScheduleMinutes, if s.schedule_kind == "loop" { "간격(분)" } else { "몇 분 뒤" })
    };
    text(g, x, *y + 13.0, detail.2, 12.0, theme::text(), false);
    field(g, s, hits, caret, (x + w - 120.0, *y + 5.0, 120.0, 30.0), "", detail.0, detail.1);
    divider(g, x, *y + 39.0, w);
    *y += 40.0;
    text(g, x, *y + 13.0, "대상", 12.0, theme::text(), false);
    *y += 40.0;
    let mut sx = x;
    let mut used = false;
    for row in s.data.agents.iter() {
        let label = agent_name(row);
        let bw = (g.measure_chrome_text(&label, 11.5, false) + 22.0).clamp(64.0, 150.0);
        if sx + bw > x + w {
            sx = x;
            *y += 34.0;
        }
        button(
            g,
            s,
            hits,
            (sx, *y - 6.0, bw, 26.0),
            &label,
            Target::ScheduleSurface(row.surface_id.clone()),
            s.schedule_surface == row.surface_id,
        );
        sx += bw + 6.0;
        used = true;
    }
    if !used {
        text(g, x, *y - 2.0, "이 방에 학생이 없어요", 11.0, theme::text_mute(), false);
    }
    *y += 28.0;
    divider(g, x, *y, w);
    *y += 12.0;
    text(g, x, *y + 6.0, "보낼 지시", 12.0, theme::text(), false);
    *y += 30.0;
    field(g, s, hits, caret, (x, *y, w, 36.0), "예: 테스트 돌리고 실패만 보고", &s.schedule_text, BoardInput::ScheduleText);
    *y += 48.0;
    divider(g, x, *y, w);
    button(g, s, hits, (x + w - 72.0, *y + 12.0, 72.0, 30.0), "등록", Target::ScheduleAdd, true);
    *y += 54.0;
    divider(g, x, *y, w);
    *y += 24.0;
    section(g, x, y, "등록된 스케줄", "");
    if s.data.schedules.is_empty() {
        empty(g, x, y, w, "예약된 작업이 없어요");
        return;
    }
    for item in s.data.schedules.iter() {
        let rect = (x, *y, w, 58.0);
        let kind = match item.kind.as_str() { "loop" => "반복", "cron" => "예약", _ => "타이머" };
        let title = format!("{kind} · {}", schedule_when(item));
        let title = fit(g, &title, w - 200.0, 12.5, true);
        text(g, rect.0, rect.1 + 11.0, &title, 12.5, if item.enabled { theme::text() } else { theme::text_dim() }, true);
        let sub = fit(g, &format!("{} · 「{}」", item.surface, item.text), w - 200.0, 10.5, false);
        text(g, rect.0, rect.1 + 32.0, &sub, 10.5, theme::text_dim(), false);
        text_button(g, s, hits, (rect.0 + rect.2 - 52.0, rect.1 + 15.0, 52.0, 28.0), "지우기", Target::ScheduleDelete(item.id.clone()), true);
        text_button(
            g,
            s,
            hits,
            (rect.0 + rect.2 - 52.0 - 8.0 - 52.0, rect.1 + 15.0, 52.0, 28.0),
            if item.enabled { "멈춤" } else { "켜기" },
            Target::ScheduleToggle(item.id.clone()),
            false,
        );
        divider(g, x, rect.1 + 57.0, w);
        *y += 58.0;
    }
}

fn paint_git(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let git = &s.data.git;
    if git.no_repo {
        empty(g, x, y, w, "이 pane은 Git 저장소에 있지 않아요");
        return;
    }
    if !git.error.is_empty() {
        notice(g, x, y, w, &git.error, false);
        return;
    }
    // 목업(플랫): 브랜치·원격·상태를 이름표/값 행으로.
    kv_row(g, x, y, w, "브랜치", if git.branch.is_empty() { "—" } else { &git.branch });
    kv_row(g, x, y, w, "원격", &format!("앞섬 {} · 뒤처짐 {}", git.ahead, git.behind));
    kv_row(g, x, y, w, "상태", &format!("변경 {}개 · +{} −{}", git.rows.len(), git.insertions, git.deletions));
    *y += 20.0;
    let head_y = *y;
    section(g, x, y, "변경", &format!("{}개 선택", s.git_selected.len()));
    if git.rows.is_empty() {
        empty(g, x, y, w, "변경된 파일이 없어요");
    } else {
        text_button(g, s, hits, (x + w - 44.0, head_y, 44.0, 26.0), "해제", Target::GitClear, false);
        text_button(g, s, hits, (x + w - 44.0 - 44.0, head_y, 44.0, 26.0), "전체", Target::GitAll, false);
        for row in git.rows.iter() {
            let rect = (x, *y, w, 40.0);
            let selected = s.git_selected.contains(&row.path);
            if contains(rect, s.cursor) {
                round_rect(g, rect.0 - 8.0, rect.1, rect.2 + 16.0, rect.3 - 1.0, theme::radius_sm(), theme::surface_hover());
            }
            let path = fit(g, &row.path, w - 60.0, 11.5, false);
            text(g, rect.0, rect.1 + 7.0, &path, 11.5, theme::text(), false);
            let marker = match row.marker { 'M' => "수정", 'A' | '?' => "추가", 'D' => "삭제", 'R' => "이름 바꿈", 'U' => "충돌", _ => "변경" };
            text(g, rect.0, rect.1 + 24.0, marker, 10.0, status_marker_color(row.marker), false);
            checkbox(g, rect.0 + rect.2 - 16.0, rect.1 + 12.0, selected);
            hit(g, hits, Target::GitFile(row.path.clone()), rect, false);
            divider(g, x, rect.1 + 39.0, w);
            *y += 40.0;
        }
    }
    *y += 24.0;
    section(g, x, y, "커밋", "");
    field(g, s, hits, caret, (x, *y, w, 36.0), "커밋 메시지", &s.git_message, BoardInput::GitMessage);
    *y += 48.0;
    divider(g, x, *y, w);
    let push_label = format!("푸시 ↑{}", git.ahead);
    let push_w = g.measure_chrome_text(&push_label, 11.5, false) + 24.0;
    button(g, s, hits, (x + w - push_w, *y + 12.0, push_w, 30.0), &push_label, Target::GitPush, false);
    button(g, s, hits, (x + w - push_w - 8.0 - 64.0, *y + 12.0, 64.0, 30.0), "커밋", Target::GitCommit, true);
    *y += 54.0;
    divider(g, x, *y, w);
    *y += 12.0;
}

fn paint_machines(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, caret: &mut Option<Rect>, x: f32, y: &mut f32, w: f32) {
    let ui = &s.transfer;
    let data = &s.data.transfer;
    if s.fixture { notice(g, x, y, w, "검증용 가상 세션 · 실제 이사·닫기는 실행되지 않아요", true); }
    if let Some(error) = &ui.error { notice(g, x, y, w, error, false); }
    for error in &data.errors { notice(g, x, y, w, error, false); }
    match ui.step {
        TransferStep::Select => {
            step_title(g, x, y, "1", "세션 고르기", true);
            let mut rooms = vec![("모든 방".to_string(), Target::TransferFilter(None), ui.room_filter.is_none(), true)];
            for machine in &data.machines {
                for room in &machine.rooms {
                    let key = (machine.id.clone(), Some(room.id.clone()));
                    rooms.push((format!("{} · {}", machine.label, room.title), Target::TransferFilter(Some(key.clone())), ui.room_filter.as_ref() == Some(&key), true));
                }
                if data.sessions.iter().any(|row| row.identity.machine_id == machine.id && row.room_id.is_none()) {
                    let key = (machine.id.clone(), None);
                    rooms.push((format!("{} · 방 밖", machine.label), Target::TransferFilter(Some(key.clone())), ui.room_filter.as_ref() == Some(&key), true));
                }
            }
            transfer_choices(g, s, hits, x, y, w, rooms);
            let selected = ui.selected.len();
            text_button(g, s, hits, (x - 4.0, *y, 120.0, 28.0), "보이는 세션 선택", Target::TransferSelectRoom, false);
            text_button(g, s, hits, (x + 124.0, *y, 72.0, 28.0), "선택 해제", Target::TransferClear, false);
            let cta = format!("선택한 {selected}개 이사할 곳 고르기");
            let cw = (g.measure_chrome_text(&cta, 11.5, false) + 28.0).min(w - 210.0).max(80.0);
            transfer_button(g, s, hits, (x + w - cw, *y - 1.0, cw, 30.0), &cta, Target::TransferDestination, selected > 0, true);
            *y += 40.0;
            divider(g, x, *y, w);
            *y += 8.0;
            let mut sessions: Vec<_> = data.sessions.iter().filter(|row| row.harness.is_some() && transfer_row_visible(ui, row)).collect();
            sessions.sort_by(|a, b| (&a.identity.machine_id, &a.room_id, &a.name).cmp(&(&b.identity.machine_id, &b.room_id, &b.name)));
            if sessions.is_empty() { empty(g, x, y, w, "이 방에서 실행 중인 세션이 없어요"); }
            let mut previous = None;
            for row in sessions {
                let group = transfer_room_label(data, row);
                if previous.as_deref() != Some(group.as_str()) {
                    let label = fit(g, &group, w, 11.0, true);
                    text(g, x, *y, &label, 11.0, theme::text_dim(), true);
                    *y += 24.0;
                    previous = Some(group);
                }
                paint_transfer_session(g, s, hits, x, y, w, row, false);
            }
            *y += 12.0;
            let shells: Vec<_> = data.sessions.iter().filter(|row| row.harness.is_none() && transfer_row_visible(ui, row)).collect();
            text_button(g, s, hits, (x - 4.0, *y, 180.0, 28.0), &format!("셸 따로 보기 · {}개 {}", shells.len(), if ui.show_shells { "접기" } else { "펼치기" }), Target::TransferShells, false);
            *y += 40.0;
            if ui.show_shells {
                text(g, x, *y, "셸만 닫으며, 실행 중인 학생은 닫지 않아요.", 10.5, theme::text_dim(), false);
                *y += 28.0;
                for row in shells { paint_transfer_session(g, s, hits, x, y, w, row, true); }
                let label = format!("선택한 셸 {}개 닫기 확인", ui.shell_selected.len());
                let cw = g.measure_chrome_text(&label, 11.5, false) + 28.0;
                transfer_button(g, s, hits, (x + w - cw, *y, cw, 30.0), &label, Target::TransferCloseReview, !ui.shell_selected.is_empty(), false);
                *y += 44.0;
            }
        }
        TransferStep::Destination => {
            text_button(g, s, hits, (x - 4.0, *y, 120.0, 28.0), "‹ 세션 다시 고르기", Target::TransferBack, false);
            *y += 44.0;
            step_title(g, x, y, "2", &format!("도착할 기기와 방 · {}개 세션을 함께 보냅니다", ui.selected.len()), true);
            let machines = data.machines.iter().map(|machine| {
                let same = ui.selected.iter().any(|id| id.machine_id == machine.id);
                let suffix = if same { " · 현재 기기" } else if !machine.online { " · 연결 안 됨" } else if !machine.room_transfer_supported { " · 업데이트 필요" } else { "" };
                (format!("{}{suffix}", machine.label), Target::TransferMachine(machine.id.clone()), ui.destination == machine.id, machine.online && machine.room_transfer_supported && !same)
            }).collect();
            transfer_choices(g, s, hits, x, y, w, machines);
            if let Some(machine) = data.machines.iter().find(|machine| machine.id == ui.destination) {
                *y += 12.0;
                section(g, x, y, "도착할 방", "기존 방을 고르거나 새 방 이름을 적어 주세요");
                let mut rooms: Vec<_> = machine.rooms.iter().map(|room| (room.title.clone(), Target::TransferRoom(room.id.clone()), !ui.new_room && ui.room.as_deref() == Some(&room.id), true)).collect();
                rooms.push(("새 방 만들기".into(), Target::TransferNewRoom, ui.new_room, true));
                transfer_choices(g, s, hits, x, y, w, rooms);
                if ui.new_room {
                    field(g, s, hits, caret, (x, *y, w, 40.0), "새 방 이름", &ui.room_name, BoardInput::TransferRoomName);
                    *y += 50.0;
                }
            }
            *y += 12.0;
            let valid = transfer_request(ui, data).is_ok();
            divider(g, x, *y, w);
            transfer_button(g, s, hits, (x + w - 120.0, *y + 12.0, 120.0, 30.0), "선택 내용 확인", Target::TransferReview, valid, true);
            *y += 54.0;
            text(g, x, *y, "다음 화면에서 확인해야 이사가 시작돼요.", 10.5, theme::text_dim(), false);
            *y += 30.0;
        }
        TransferStep::Confirm => {
            if ui.results.is_empty() {
                let Some(pending) = &ui.pending else { return };
                let (title, summary, ids, confirm) = match pending {
                    TransferConfirmation::Move(request) => {
                        let machine = data.machines.iter().find(|machine| machine.id == request.destination_machine);
                        let machine_name = machine.map(|m| m.label.as_str()).unwrap_or("연결 확인 필요");
                        let room = match &request.destination_room {
                            RoomTarget::New(name) => format!("새 방 · {name}"),
                            RoomTarget::Existing(id) => machine.and_then(|m| m.rooms.iter().find(|room| &room.id == id)).map(|room| room.title.clone()).unwrap_or_else(|| "방 확인 필요".into()),
                        };
                        ("이사할 내용 확인", format!("도착 · {machine_name} / {room}"), &request.sessions, "확인하고 이사")
                    }
                    TransferConfirmation::Close(ids) => ("셸 닫기 확인", "선택한 셸만 닫습니다. 실행 중인 학생은 유지돼요.".into(), ids, "확인하고 셸 닫기"),
                };
                step_title(g, x, y, "3", title, true);
                transfer_message(g, x, y, w, &summary);
                *y += 16.0;
                for id in ids {
                    if let Some(row) = data.sessions.iter().find(|row| &row.identity == id) {
                        let label = fit(g, &format!("{} · {}", transfer_session_name(row), transfer_room_label(data, row)), w - 24.0, 11.5, false);
                        text(g, x + 12.0, *y + 8.0, &label, 11.5, theme::text(), false);
                        *y += 32.0;
                    }
                }
                *y += 16.0;
                divider(g, x, *y, w);
                let cw = g.measure_chrome_text(confirm, 11.5, false) + 28.0;
                button(g, s, hits, (x + w - cw, *y + 12.0, cw, 30.0), confirm, Target::TransferConfirm, true);
                text_button(g, s, hits, (x + w - cw - 8.0 - 120.0, *y + 12.0, 120.0, 30.0), "취소하고 다시 고르기", Target::TransferBack, false);
                *y += 54.0;
            } else {
                let done = ui.results.iter().filter(|row| row.status == TransferStatus::Succeeded).count();
                let failed = ui.results.iter().filter(|row| matches!(row.status, TransferStatus::Failed | TransferStatus::Unknown)).count();
                section(g, x, y, if ui.busy { "이사·정리 진행 중" } else { "처리 결과" }, &format!("전체 {}개 · 완료 {done} · 확인 필요 {failed}", ui.results.len()));
                for result in &ui.results {
                    let name = ui.result_names.get(&result.source.canonical_key()).cloned().or_else(|| data.sessions.iter().find(|row| row.identity.canonical_key() == result.source.canonical_key()).map(transfer_session_name)).unwrap_or_else(|| "선택한 세션".into());
                    let state = match result.status { TransferStatus::Waiting => "대기", TransferStatus::Running => "진행", TransferStatus::Succeeded => "완료", TransferStatus::Failed => "실패", TransferStatus::Unknown => "결과 확인 필요" };
                    let title = fit(g, &format!("{name} · {state}"), w - 20.0, 12.0, true);
                    text(g, x + 10.0, *y + 8.0, &title, 12.0, if matches!(result.status, TransferStatus::Failed | TransferStatus::Unknown) { theme::danger() } else { theme::text() }, true);
                    *y += 32.0;
                    transfer_message(g, x + 10.0, y, w - 20.0, &result.message);
                    *y += 16.0;
                }
                if !ui.busy {
                    divider(g, x, *y, w);
                    button(g, s, hits, (x + w - 120.0, *y + 12.0, 120.0, 30.0), "목록으로 돌아가기", Target::TransferBack, false);
                    *y += 54.0;
                }
            }
        }
    }
}

fn transfer_row_visible(ui: &TransferUi, row: &SessionRow) -> bool {
    ui.room_filter.as_ref().is_none_or(|(machine, room)| &row.identity.machine_id == machine && &row.room_id == room)
}

fn transfer_message(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str) {
    let mut line = String::new();
    for ch in message.chars() {
        if ch == '\n' || (!line.is_empty() && g.measure_chrome_text(&format!("{line}{ch}"), 10.5, false) > w) {
            text(g, x, *y, &line, 10.5, theme::text_dim(), false);
            *y += 19.0;
            line.clear();
        }
        if ch != '\n' { line.push(ch); }
    }
    if !line.is_empty() { text(g, x, *y, &line, 10.5, theme::text_dim(), false); *y += 19.0; }
}

fn transfer_session_name(row: &SessionRow) -> String {
    if !row.name.is_empty() { row.name.clone() } else if row.harness.is_none() { "셸".into() } else { row.harness.clone().unwrap_or_else(|| "세션".into()) }
}

fn transfer_room_label(data: &TransferSnapshot, row: &SessionRow) -> String {
    let machine = data.machines.iter().find(|machine| machine.id == row.identity.machine_id);
    let name = machine.map(|machine| machine.label.as_str()).unwrap_or("기기 확인 필요");
    let room = machine.and_then(|m| row.room_id.as_ref().and_then(|id| m.rooms.iter().find(|room| &room.id == id))).map(|room| room.title.as_str()).unwrap_or("방 밖");
    format!("{name} · {room}")
}

fn transfer_button(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, rect: Rect, label: &str, target: Target, enabled: bool, primary: bool) {
    if enabled { button(g, s, hits, rect, label, target, primary); }
    else {
        g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, theme::radius_md().min(5.0), 1.0, theme::with_alpha(theme::border(), 140));
        let label = fit(g, label, rect.2 - 20.0, 11.5, false);
        let tx = rect.0 + (rect.2 - g.measure_chrome_text(&label, 11.5, false)) / 2.0;
        text(g, tx, rect.1 + (rect.3 - 12.0) / 2.0 - 1.0, &label, 11.5, theme::text_mute(), false);
    }
}

fn transfer_choices(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, choices: Vec<(String, Target, bool, bool)>) {
    let mut left = x;
    for (label, target, selected, enabled) in choices {
        let width = (g.measure_chrome_text(&label, 10.5, selected) + 28.0).clamp(90.0, w.min(260.0));
        if left > x && left + width > x + w { left = x; *y += 38.0; }
        transfer_button(g, s, hits, (left, *y, width, 30.0), &label, target, enabled, selected);
        left += width + 8.0;
    }
    *y += 42.0;
}

fn paint_transfer_session(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, row: &SessionRow, shell: bool) {
    let selected = if shell { s.transfer.shell_selected.contains(&row.identity) } else { s.transfer.selected.contains(&row.identity) };
    let enabled = row.unavailable_reason.is_none() && (!shell || row.shell_closeable);
    let key = row.identity.canonical_key();
    let expanded = s.transfer.detail.as_deref() == Some(&key);
    let compact = w < 440.0;
    let rect = (x, *y, w, if compact { 120.0 } else { 88.0 });
    // 목업(플랫): 채움 없이 구분선만. 고른 행은 체크박스가 말한다.
    checkbox(g, x + 8.0, *y + 12.0, selected);
    let activity = s.data.agents.iter().find(|agent| row.local_panes.contains(&agent.surface_id));
    if let Some(agent) = activity { draw_face(g, s, agent, x + 34.0, *y + 10.0, 28.0); }
    else if let Some(face) = s.data.faces.iter().find(|face| face.name == row.name) {
        if !g.has_image(&face.key) { g.upload_image(&face.key, &face.rgba, face.width, face.height); }
        g.queue_image_above(&face.key, x + 34.0, *y + 10.0, 28.0, 28.0);
    } else { g.queue_icon(if shell { "terminal" } else { "users" }, x + 39.0, *y + 14.0, 16.0, theme::text_dim()); }
    let text_w = (w - if compact { 88.0 } else { 152.0 }).max(38.0);
    let status = match row.status.as_str() { "working" | "thinking" | "compacting" => "작업 중", "waiting" | "blocked" => "확인 필요", _ => if shell { "셸" } else { "대기" } };
    let name = fit(g, &format!("{} · {status}", transfer_session_name(row)), text_w, 12.0, true);
    text(g, x + 72.0, *y + 10.0, &name, 12.0, theme::text(), true);
    let local = s.data.transfer.machines.iter().find(|machine| machine.id == row.identity.machine_id).is_some_and(|machine| machine.local);
    let viewing = if local { "여기서 실행" } else if row.local_panes.is_empty() { "원격만" } else { "여기서 보는 중" };
    let machine = s.data.transfer.machines.iter().find(|machine| machine.id == row.identity.machine_id).map(|machine| machine.label.as_str()).unwrap_or("기기 확인 필요");
    let status = fit(g, &format!("{machine} · {viewing}"), text_w, 10.0, false);
    text(g, x + 72.0, *y + 30.0, &status, 10.0, theme::text_dim(), false);
    let summary = row.unavailable_reason.as_deref().unwrap_or(if row.title.is_empty() { "최근 작업 제목 없음" } else { &row.title });
    let summary = fit(g, summary, text_w, 10.5, false);
    text(g, x + 72.0, *y + 51.0, &summary, 10.5, if enabled { theme::text_dim() } else { theme::attention() }, false);
    if enabled { hit(g, hits, Target::TransferSelect(row.identity.clone(), shell), (x, *y, if compact { w } else { w - 72.0 }, 76.0), false); }
    let view = if compact { (x + 16.0, *y + 82.0, 58.0, 28.0) } else { (x + w - 66.0, *y + 8.0, 58.0, 28.0) };
    let detail = if compact { (x + 82.0, *y + 82.0, 58.0, 28.0) } else { (x + w - 66.0, *y + 42.0, 58.0, 28.0) };
    text_button(g, s, hits, view, "보기", Target::TransferView(row.identity.clone()), false);
    text_button(g, s, hits, detail, if expanded { "접기" } else { "상세" }, Target::TransferDetail(key), false);
    *y += rect.3;
    if expanded {
        let rows = activity.map(|agent| vec![agent.last_prompt.as_str(), agent.last_reply.as_str(), agent.intent.as_str()]).unwrap_or_default();
        for line in rows.into_iter().filter(|line| !line.is_empty()).take(3) {
            let shown = fit(g, line, w - 32.0, 10.5, false);
            text(g, x + 16.0, *y + 6.0, &shown, 10.5, theme::text_dim(), false);
            *y += 24.0;
        }
        let last = activity.and_then(|agent| agent.idle_secs).map(|secs| format!("마지막 활동 · {}분 전", secs / 60)).unwrap_or_else(|| "마지막 활동 시각 정보 없음".into());
        text(g, x + 16.0, *y + 6.0, &last, 10.0, theme::text_dim(), false);
        *y += 32.0;
    }
    divider(g, x, *y - 1.0, w);
    *y += 8.0;
}

fn draw_face(g: &mut gpu::GpuRenderer, s: &Snapshot, row: &PaneActivity, x: f32, y: f32, size: f32) {
    if let Some(name) = row.character.as_deref() {
        if let Some(face) = s.data.faces.iter().find(|face| face.name == name) {
            if !g.has_image(&face.key) {
                g.upload_image(&face.key, &face.rgba, face.width, face.height);
            }
            g.queue_image_above(&face.key, x, y, size, size);
            return;
        }
    }
    round_rect(g, x, y, size, size, theme::radius_md(), theme::surface_hover());
    g.queue_icon("terminal", x + 8.0, y + 8.0, size - 16.0, theme::text_dim());
}

fn field(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    rect: Rect,
    placeholder: &str,
    value: &str,
    input: BoardInput,
) {
    let active = s.input == Some(input);
    outlined(g, rect, theme::surface());
    if active {
        stroke(g, rect, theme::accent());
    }
    let mut shown = value.to_string();
    if active && !s.preedit.is_empty() {
        let byte = char_to_byte(&shown, s.caret.min(shown.chars().count()));
        shown.insert_str(byte, &s.preedit);
    }
    if shown.is_empty() {
        text(g, rect.0 + 12.0, rect.1 + 12.0, placeholder, 11.5, theme::text_mute(), false);
    } else {
        let shown = fit(g, &shown, rect.2 - 24.0, 11.5, false);
        text(g, rect.0 + 12.0, rect.1 + 12.0, &shown, 11.5, theme::text(), false);
    }
    if active && s.caret_on {
        let before: String = value.chars().take(s.caret).collect();
        let cx = rect.0 + 12.0 + g.measure_chrome_text(&before, 11.5, false);
        let cr = (cx.min(rect.0 + rect.2 - 10.0), rect.1 + 10.0, 1.5, 18.0);
        g.rect(cr.0, cr.1, cr.2, cr.3, theme::accent());
        *caret = Some(cr);
    }
    hit(g, hits, Target::Input(input), rect, true);
}

fn section(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, title: &str, desc: &str) {
    // 목업(플랫): 묶음 이름은 작은 흐림 글자 한 줄. 설명이 있으면 「·」로 이어 붙인다.
    let line = if desc.is_empty() { title.to_string() } else { format!("{title} · {desc}") };
    text(g, x, *y + 6.0, &line, 11.0, theme::text_dim(), false);
    *y += 32.0;
}

/// 목업의 stepn — 번호 원(테두리) + 제목. 현재 단계만 강조색.
fn step_title(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, n: &str, title: &str, on: bool) {
    let color = if on { theme::accent() } else { theme::text_dim() };
    g.round_rect_stroke(x, *y + 2.0, 18.0, 18.0, 9.0, 1.0, if on { theme::accent() } else { theme::border() });
    let nw = g.measure_chrome_text(n, 10.0, false);
    text(g, x + (18.0 - nw) / 2.0, *y + 5.0, n, 10.0, color, false);
    text(g, x + 26.0, *y + 4.0, title, 12.0, color, on);
    *y += 32.0;
}

/// 행 아래 얇은 구분선. 카드 채움 대신 이것으로 행을 가른다.
fn divider(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32) {
    g.rect(x, y, w, 1.0, theme::with_alpha(theme::border(), 140));
}

/// 이름표 왼쪽 · 값 오른쪽의 정보 행(목업 .kv).
fn kv_row(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str, value: &str) {
    text(g, x, *y + 10.0, label, 11.5, theme::text_dim(), false);
    let shown = fit(g, value, w - 120.0, 11.5, false);
    let vw = g.measure_chrome_text(&shown, 11.5, false);
    text(g, x + w - vw, *y + 10.0, &shown, 11.5, theme::text(), false);
    divider(g, x, *y + 33.0, w);
    *y += 34.0;
}

fn notice(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str, ok: bool) {
    // 목업(플랫): 채움·아이콘 없이 왼쪽 2px 색선만.
    let color = if ok { theme::success() } else { theme::danger() };
    g.rect(x, *y, 2.0, 36.0, color);
    let message = fit(g, message, w - 24.0, 11.5, false);
    text(g, x + 14.0, *y + 11.0, &message, 11.5, theme::text(), false);
    *y += 48.0;
}

fn empty(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str) {
    text(g, x, *y + 14.0, message, 11.5, theme::text_mute(), false);
    divider(g, x, *y + 43.0, w);
    *y += 52.0;
}

fn outlined(g: &mut gpu::GpuRenderer, rect: Rect, fill: [u8; 4]) {
    round_rect(g, rect.0, rect.1, rect.2, rect.3, theme::radius_md(), theme::edge_on(fill));
    round_rect(g, rect.0 + 1.0, rect.1 + 1.0, rect.2 - 2.0, rect.3 - 2.0, (theme::radius_md() - 1.0).max(0.0), fill);
}

fn stroke(g: &mut gpu::GpuRenderer, rect: Rect, color: [u8; 4]) {
    g.rect(rect.0, rect.1, rect.2, 1.0, color);
    g.rect(rect.0, rect.1 + rect.3 - 1.0, rect.2, 1.0, color);
    g.rect(rect.0, rect.1, 1.0, rect.3, color);
    g.rect(rect.0 + rect.2 - 1.0, rect.1, 1.0, rect.3, color);
}

fn button(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, rect: Rect, label: &str, target: Target, primary: bool) {
    // 목업(플랫): 채움 없는 아웃라인. 주 단추는 강조색 테두리+글자, 나머지는 회색 테두리.
    let hover = contains(rect, s.cursor);
    if hover {
        round_rect(g, rect.0, rect.1, rect.2, rect.3, theme::radius_md().min(5.0), theme::surface_hover());
    }
    let line = if primary { theme::accent() } else if hover { theme::text_dim() } else { theme::border() };
    g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, theme::radius_md().min(5.0), 1.0, line);
    let shown = fit(g, label, rect.2 - 14.0, 11.5, false);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, false)) / 2.0;
    text(g, tx, rect.1 + (rect.3 - 12.0) / 2.0 - 1.0, &shown, 11.5, if primary { theme::accent() } else { theme::text() }, false);
    hit(g, hits, target, rect, false);
    g.hover_pointer |= hover;
}

/// 테두리도 없는 글자 단추(목업 .btn.txt). 보조 동작 — 상세·해제·멈추기.
fn text_button(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, rect: Rect, label: &str, target: Target, danger: bool) {
    let hover = contains(rect, s.cursor);
    let shown = fit(g, label, rect.2 - 4.0, 11.5, false);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, false)) / 2.0;
    let color = if danger && hover { theme::danger() } else if hover { theme::text() } else { theme::text_dim() };
    text(g, tx, rect.1 + (rect.3 - 12.0) / 2.0 - 1.0, &shown, 11.5, color, false);
    hit(g, hits, target, rect, false);
    g.hover_pointer |= hover;
}

/// 목업의 구분 선택 — 테두리 하나, 고른 칸만 강조색 테두리+글자. 오른쪽 끝에 붙는다.
fn segmented(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: f32, w: f32, cells: &[(&str, bool, Target)]) {
    if cells.is_empty() || w <= 4.0 { return; }
    let h = 26.0;
    let inset = 2.0;
    let pad = 10.0;
    let widths: Vec<f32> = cells.iter().map(|(label, _, _)| g.measure_chrome_text(label, 11.5, false) + pad * 2.0).collect();
    let total = widths.iter().sum::<f32>() + inset * 2.0;
    let outer = (x + w - total.min(w), y, total.min(w), h);
    let scale = ((outer.2 - inset * 2.0) / (total - inset * 2.0)).min(1.0);
    g.round_rect_stroke(outer.0, outer.1, outer.2, outer.3, 4.0, 1.0, theme::border());
    let mut cx = outer.0 + inset;
    for (i, (label, selected, target)) in cells.iter().enumerate() {
        let rect = (cx, y + inset, widths[i] * scale, h - inset * 2.0);
        cx += rect.2;
        let hover = contains(rect, s.cursor);
        if *selected {
            g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, 3.0, 1.0, theme::accent());
        }
        let shown = fit(g, label, rect.2 - 8.0, 11.5, false);
        let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, false)) / 2.0;
        text(g, tx, rect.1 + 4.0, &shown, 11.5, if *selected { theme::accent() } else if hover { theme::text() } else { theme::text_dim() }, false);
        hit(g, hits, target.clone(), rect, false);
        g.hover_pointer |= hover;
    }
}

fn icon_button(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, rect: Rect, icon: &str, target: Target) {
    let hover = contains(rect, s.cursor);
    if hover { round_rect(g, rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), theme::surface_active()); }
    let size = rect.2.min(rect.3).min(16.0);
    g.queue_icon(icon, rect.0 + (rect.2 - size) / 2.0, rect.1 + (rect.3 - size) / 2.0, size, if hover { theme::text() } else { theme::text_dim() });
    hit(g, hits, target, rect, false);
    g.hover_pointer |= hover;
}

fn hit(g: &gpu::GpuRenderer, hits: &mut Vec<Hit>, target: Target, rect: Rect, text_cursor: bool) {
    if let Some(rect) = g.clip_hit(rect) {
        hits.push(Hit { target, rect, text_cursor });
    }
}

fn text(g: &mut gpu::GpuRenderer, x: f32, y: f32, value: &str, size: f32, color: [u8; 4], bold: bool) {
    g.draw_text(x, y, value, gpu::DrawOpts { font_size: size, color, bold, italic: false });
}

fn fit(g: &mut gpu::GpuRenderer, value: &str, width: f32, size: f32, bold: bool) -> String {
    if g.measure_chrome_text(value, size, bold) <= width { return value.to_string(); }
    if width <= 0.0 || g.measure_chrome_text("…", size, bold) > width { return String::new(); }
    let mut out = String::new();
    for ch in value.chars() {
        let next = format!("{out}{ch}…");
        if g.measure_chrome_text(&next, size, bold) > width { break; }
        out.push(ch);
    }
    if out.chars().count() < value.chars().count() { out.push('…'); }
    out
}

pub(crate) fn status_label(row: &PaneActivity) -> String {
    if agent_needs_attention(row) {
        "확인 필요".to_string()
    } else if let Some(outcome) = &row.done_outcome {
        if outcome == "succeeded" { "완료 보고".to_string() } else { "실패 보고".to_string() }
    } else if row.status == "thinking" {
        "생각 중".to_string()
    } else if row.status == "compacting" {
        "대화 정리 중".to_string()
    } else if agent_is_working(row) {
        if row.intent.is_empty() { "작업 중".to_string() } else { row.intent.clone() }
    } else {
        "대기 중".to_string()
    }
}

pub(crate) fn agent_needs_attention(row: &PaneActivity) -> bool {
    row.waiting_for.is_some() || row.status == "blocked"
}

pub(crate) fn agent_is_working(row: &PaneActivity) -> bool {
    matches!(
        row.status.as_str(),
        "working" | "building" | "waiting" | "compacting" | "thinking"
    )
}

fn agent_name(row: &PaneActivity) -> String {
    row.character
        .as_deref()
        .filter(|value| !value.is_empty())
        .or(row.peer_name.as_deref().filter(|value| !value.is_empty()))
        .unwrap_or(&row.surface_id)
        .to_string()
}

fn background_state(row: &BackgroundRow) -> &str {
    match row.state.as_str() {
        "done" => "완료",
        "blocked" => "막힘",
        "running" | "working" => "작업 중",
        _ => if row.status.is_empty() { "대기" } else { &row.status },
    }
}

fn background_stop_target(row: &BackgroundRow) -> Option<LocalBackgroundProcess> {
    (row.machine.is_none() && row.pid > 0).then_some(LocalBackgroundProcess { pid: row.pid })
}

fn schedule_when(item: &kasa_mcp::ScheduleItem) -> String {
    if item.kind == "loop" { format!("{}분마다", item.interval_sec / 60) } else if item.enabled { "예약 대기".to_string() } else { "멈춤".to_string() }
}

fn short_path(path: &str) -> String {
    let parts: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() > 2 { format!("…/{}", parts[parts.len() - 2..].join("/")) } else if parts.is_empty() { path.to_string() } else { parts.join("/") }
}

fn format_age(started_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let secs = now.saturating_sub(started_at) / 1000;
    if secs < 60 {
        format!("{secs}초 전")
    } else if secs < 3600 {
        format!("{}분 전", secs / 60)
    } else {
        format!("{}시간 전", secs / 3600)
    }
}

fn status_marker_color(marker: char) -> [u8; 4] {
    match marker { 'U' => theme::success(), 'S' => theme::accent(), _ => theme::text_dim() }
}

fn checkbox(g: &mut gpu::GpuRenderer, x: f32, y: f32, checked: bool) {
    // 목업(플랫): 채움 없이 테두리. 켜짐은 강조색 테두리+강조색 체크.
    let color = if checked { theme::accent() } else { theme::border() };
    g.round_rect_stroke(x, y, 16.0, 16.0, 3.0, 1.0, color);
    if checked { g.queue_icon("check", x + 2.0, y + 2.0, 12.0, theme::accent()); }
}

fn char_to_byte(value: &str, at: usize) -> usize {
    value.char_indices().nth(at).map(|(byte, _)| byte).unwrap_or(value.len())
}

impl App {
    fn native_board_backend(&self) -> Option<Arc<dyn Backend>> {
        self.socket_backend
            .clone()
            .map(|backend| backend as Arc<dyn Backend>)
    }

    pub(crate) fn request_native_board_refresh(&mut self) {
        let Some(backend) = self.native_board_backend() else {
            return;
        };
        if board_fixture_requested() {
            self.board_scene.request_refresh(backend, self.proxy.clone());
            return;
        }
        let target = self
            .board_scene
            .target_pane()
            .map(str::to_string)
            .filter(|pane| self.window_of_pane(pane).is_some())
            .or_else(|| {
                (0..self.windows.len())
                    .find(|idx| self.internal_room_kind_at(*idx).is_none())
                    .and_then(|idx| {
                        let layout = if idx == self.active_window {
                            self.pty_layout.as_ref()
                        } else {
                            self.windows.get(idx).and_then(Option::as_ref)
                        };
                        layout
                            .and_then(|layout| layout.leaves().first().copied())
                            .map(str::to_string)
                    })
            });
        if let Some(pane) = target {
            let window = self.window_of_pane(&pane).unwrap_or(0);
            let cwd = self
                .pane_current_cwd(&pane)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.board_scene.enter(Some(pane), window, cwd);
        }
        self.board_scene.request_refresh(backend, self.proxy.clone());
    }

    pub(crate) fn native_board_tick(&mut self) {
        let changed = self.board_scene.pump();
        if self.board_room_active() && self.board_scene.refresh_due() {
            self.request_native_board_refresh();
        }
        if changed {
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    pub(crate) fn native_board_snapshot(&self, area: Rect) -> Option<Snapshot> {
        self.board_room_active().then(|| {
            self.board_scene.snapshot(
                area,
                self.cursor_px,
                self.last_blink_on,
                self.preedit.clone(),
            )
        })
    }

    pub(crate) fn finish_native_board_paint(&mut self, output: PaintOutput) {
        self.board_scene.finish_paint(output);
        if let (Some(window), Some((x, y, w, h))) =
            (self.window.as_ref(), self.board_scene.caret_rect())
        {
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(x as f64, y as f64),
                winit::dpi::LogicalSize::new(w.max(1.0) as f64, h.max(1.0) as f64),
            );
        }
    }

    pub(crate) fn native_board_contains(&self, x: f32, y: f32) -> bool {
        self.board_room_active()
            && self.window.as_ref().is_some_and(|window| {
                let scale = self.effective_scale();
                let size = window.inner_size();
                x >= self.effective_sidebar_w()
                    && x <= size.width as f32 / scale
                    && y >= TITLE_HEIGHT
                    && y <= size.height as f32 / scale
            })
    }

    pub(crate) fn native_board_cursor(&self, x: f32, y: f32) -> winit::window::CursorIcon {
        self.board_scene
            .hit_at(x, y)
            .map(|hit| {
                if hit.text_cursor {
                    winit::window::CursorIcon::Text
                } else {
                    winit::window::CursorIcon::Pointer
                }
            })
            .unwrap_or(winit::window::CursorIcon::Default)
    }

    pub(crate) fn native_board_wheel(&mut self, delta: winit::event::MouseScrollDelta) {
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => y * 42.0,
            winit::event::MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        if self.board_scene.scroll_by(-dy) {
            self.chrome_dirty = true;
        }
    }

    pub(crate) fn native_board_click(&mut self, x: f32, y: f32) -> bool {
        let Some(target) = self.board_scene.hit_at(x, y).map(|hit| hit.target.clone()) else {
            self.native_board_blur();
            self.board_scene.clear_stop();
            return false;
        };
        if board_fixture_requested()
            && !matches!(target, Target::Tab(_) | Target::Return | Target::Refresh
                | Target::OverviewMachine(_) | Target::OverviewRoom(_) | Target::OverviewSort(_)
                | Target::OverviewDetail(_) | Target::OverviewChanges | Target::OverviewCopy(_))
        {
            self.board_scene.report_error("검증용 보드에서는 실제 창을 조작하지 않아요");
            self.chrome_dirty = true;
            return true;
        }
        // 멈춤 확인은 그 행에서 답할 때만 살아 있다. 다른 곳을 누르면 접어, 화면에
        // 남은 확인이 나중 클릭에 엉뚱하게 걸리지 않게 한다.
        if !matches!(
            target,
            Target::StopBackground(_) | Target::ConfirmStopBackground(_)
        ) {
            self.board_scene.clear_stop();
        }
        match target {
            Target::Tab(tab) => {
                self.native_board_blur();
                self.board_scene.set_tab(tab);
            }
            Target::Return => {
                self.native_board_blur();
                self.return_from_board_room();
            }
            Target::Refresh => self.request_native_board_refresh(),
            Target::FocusPane(pane) => {
                self.native_board_blur();
                self.return_from_board_room();
                self.focus_surface(&pane);
            }
            Target::OverviewMachine(machine) => {
                self.board_scene.overview.machine = machine;
                self.board_scene.overview.room = None;
                self.board_scene.scroll = 0.0;
            }
            Target::OverviewRoom(room) => {
                self.board_scene.overview.room = room;
                self.board_scene.scroll = 0.0;
            }
            Target::OverviewSort(sort) => self.board_scene.sort_overview(sort),
            Target::OverviewDetail(id) => self.board_scene.toggle_overview_detail(id),
            Target::OverviewChanges => {
                self.board_scene.overview.show_changes = !self.board_scene.overview.show_changes;
            }
            Target::OverviewCopy(address) => {
                if self.board_scene.data.overview.panes.iter().any(|row| row.address == address) {
                    let value = serde_json::to_string(&address).unwrap_or_default();
                    let copied = if board_fixture_active() {
                        *crate::clipboard::probe_copied_text().lock().unwrap() = value;
                        true
                    } else {
                        arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(value)).is_ok()
                    };
                    self.board_scene.toast = Some((copied, if copied { "보드 주소를 복사했어요" } else { "주소를 복사하지 못했어요" }.into(), Instant::now()));
                }
            }
            Target::OverviewFocus(address) => {
                if overview_is_local(&self.board_scene.data.overview, &address)
                    && !board_fixture_active()
                    && self.window_of_pane(&address.surface_id).is_some()
                    && kasa_mcp::surface_keys::get(&address.surface_id).as_deref() == Some(&address.surface_key)
                {
                    self.native_board_blur();
                    self.return_from_board_room();
                    self.focus_surface(&address.surface_id);
                } else {
                    self.board_scene.report_error("이 창으로 이동할 수 없어요. 목록을 새로고침해 주세요");
                }
            }
            Target::OverviewSave(_) => {
                self.board_scene.report_error("저장은 해당 창에서 실행해 주세요. 보드에서는 저장 순간의 대화를 확인할 수 없어요");
            }
            Target::ResumeBackground(id, cwd) => {
                self.resume_background_in_target_room(id, cwd);
            }
            Target::StopBackground(target) => {
                // 첫 클릭은 확인을 세우기만 한다 — 여기서 kill 을 보내면 잘못 누른
                // 한 번으로 배경 세션이 사라지고 되돌릴 방법이 없다.
                self.board_scene.arm_stop(target);
                self.board_scene
                    .report_error("멈추면 되돌릴 수 없어요 · 「정말 멈추기」를 눌러 주세요".to_string());
            }
            Target::ConfirmStopBackground(target) => {
                self.board_scene.clear_stop();
                self.run_native_board_action(WorkerAction::StopBackground(target));
            }
            Target::CancelStopBackground => {
                self.board_scene.clear_stop();
            }
            Target::ScheduleKind(kind) => self.board_scene.set_schedule_kind(kind),
            Target::ScheduleSurface(surface) => self.board_scene.set_schedule_surface(surface),
            Target::Input(input) => {
                let len = self.board_scene.field(input).chars().count();
                self.board_scene.set_input(Some(input), len);
                self.ime_retarget(crate::ImeFocus::Board(input));
            }
            Target::ScheduleAdd => {
                let kind = self.board_scene.schedule_kind().to_string();
                let surface = self.board_scene.schedule_surface().to_string();
                let text = self.board_scene.field(BoardInput::ScheduleText).to_string();
                let minutes = self
                    .board_scene
                    .field(BoardInput::ScheduleMinutes)
                    .parse::<u64>()
                    .unwrap_or(10);
                let at_ts = self
                    .board_scene
                    .field(BoardInput::ScheduleAt)
                    .parse::<f64>()
                    .unwrap_or(0.0);
                self.run_native_board_action(WorkerAction::ScheduleAdd {
                    kind,
                    surface,
                    text,
                    minutes,
                    at_ts,
                });
            }
            Target::ScheduleToggle(id) => {
                self.run_native_board_action(WorkerAction::ScheduleToggle(id));
            }
            Target::ScheduleDelete(id) => {
                self.run_native_board_action(WorkerAction::ScheduleDelete(id));
            }
            Target::GitFile(path) => self.board_scene.toggle_git_file(path),
            Target::GitAll => self.board_scene.set_all_git(true),
            Target::GitClear => self.board_scene.set_all_git(false),
            Target::GitCommit => {
                let files = self.board_scene.selected_git().iter().cloned().collect();
                let message = self.board_scene.git_message().to_string();
                let cwd = self
                    .board_scene
                    .target_pane()
                    .and_then(|pane| self.pane_current_cwd(pane))
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.run_native_board_action(WorkerAction::GitCommit {
                    cwd,
                    files,
                    message,
                });
            }
            Target::GitPush => {
                let cwd = self
                    .board_scene
                    .target_pane()
                    .and_then(|pane| self.pane_current_cwd(pane))
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.run_native_board_action(WorkerAction::GitPush { cwd });
            }
            Target::TransferFilter(filter) => {
                self.board_scene.transfer.room_filter = filter;
                self.board_scene.scroll = 0.0;
            }
            Target::TransferSelect(identity, shell) => {
                let ui = &mut self.board_scene.transfer;
                let selected = if shell { &mut ui.shell_selected } else { &mut ui.selected };
                if !selected.remove(&identity) { selected.insert(identity); }
                ui.pending = None;
                ui.error = None;
            }
            Target::TransferSelectRoom => {
                let ui = &mut self.board_scene.transfer;
                for row in &self.board_scene.data.transfer.sessions {
                    if row.harness.is_some() && row.unavailable_reason.is_none() && transfer_row_visible(ui, row) {
                        ui.selected.insert(row.identity.clone());
                    }
                }
                ui.error = None;
            }
            Target::TransferClear => {
                self.board_scene.transfer.selected.clear();
                self.board_scene.transfer.shell_selected.clear();
                self.board_scene.transfer.pending = None;
            }
            Target::TransferDestination => {
                self.native_board_blur();
                self.board_scene.revalidate_transfer_selection();
                if !self.board_scene.transfer.selected.is_empty() {
                    self.board_scene.transfer.step = TransferStep::Destination;
                    self.board_scene.transfer.error = None;
                    self.board_scene.scroll = 0.0;
                }
            }
            Target::TransferMachine(machine) => {
                self.native_board_blur();
                self.board_scene.transfer.destination = machine;
                self.board_scene.transfer.room = None;
                self.board_scene.transfer.new_room = false;
                self.board_scene.transfer.pending = None;
                self.board_scene.transfer.error = None;
            }
            Target::TransferRoom(room) => {
                self.native_board_blur();
                self.board_scene.transfer.room = Some(room);
                self.board_scene.transfer.new_room = false;
                self.board_scene.transfer.pending = None;
                self.board_scene.transfer.error = None;
            }
            Target::TransferNewRoom => {
                self.board_scene.transfer.new_room = true;
                self.board_scene.transfer.room = None;
                let len = self.board_scene.transfer.room_name.chars().count();
                self.board_scene.set_input(Some(BoardInput::TransferRoomName), len);
                self.ime_retarget(crate::ImeFocus::Board(BoardInput::TransferRoomName));
            }
            Target::TransferReview => {
                self.native_board_blur();
                self.board_scene.review_transfer(false);
            }
            Target::TransferCloseReview => {
                self.native_board_blur();
                self.board_scene.review_transfer(true);
            }
            Target::TransferConfirm => {
                self.native_board_blur();
                if let Some(backend) = self.native_board_backend() {
                    self.board_scene.run_transfer(backend, self.proxy.clone());
                }
            }
            Target::TransferBack => {
                self.native_board_blur();
                if !self.board_scene.transfer.busy {
                    self.board_scene.transfer.step = TransferStep::Select;
                    self.board_scene.transfer.pending = None;
                    self.board_scene.transfer.results.clear();
                    self.board_scene.transfer.error = None;
                    self.board_scene.scroll = 0.0;
                }
            }
            Target::TransferShells => self.board_scene.transfer.show_shells = !self.board_scene.transfer.show_shells,
            Target::TransferDetail(key) => {
                let ui = &mut self.board_scene.transfer;
                ui.detail = if ui.detail.as_ref() == Some(&key) { None } else { Some(key) };
            }
            Target::TransferView(identity) => {
                self.native_board_blur();
                self.run_native_board_action(WorkerAction::ViewSession(identity));
            }
        }
        self.chrome_dirty = true;
        true
    }

    fn run_native_board_action(&mut self, action: WorkerAction) {
        let Some(backend) = self.native_board_backend() else {
            return;
        };
        self.board_scene
            .run_action(backend, action, self.proxy.clone());
    }

    fn resume_background_in_target_room(&mut self, id: String, cwd: String) {
        let Some(target) = self.board_scene.target_pane().map(str::to_string) else {
            self.board_scene.report_error("돌아갈 작업 pane이 없어요");
            return;
        };
        self.native_board_blur();
        if !self.return_from_board_room() || !self.focus_surface(&target) {
            self.board_scene
                .report_error("대상 작업 방으로 돌아가지 못했어요");
            self.set_toast("대상 작업 방으로 돌아가지 못했어요".to_string());
            return;
        }
        let (reply, receiver) = std::sync::mpsc::channel();
        let event = UserEvent::ResumeSession {
            id,
            cwd: (!cwd.is_empty()).then_some(cwd),
            newroom: false,
            attach: true,
            harness: "claude".to_string(),
            reply: Some(reply),
        };
        if self.proxy.send_event(event).is_err() {
            self.board_scene.report_error("이어받기 요청을 보내지 못했어요");
            self.set_toast("이어받기 요청을 보내지 못했어요".to_string());
            return;
        }
        self.board_scene
            .wait_for_gui_result(receiver, "세션을 이어받았어요", self.proxy.clone());
    }

    pub(crate) fn native_board_insert_into(&mut self, field: BoardInput, text: &str) {
        self.board_scene.edit_field(field, |value, caret| {
            let byte = char_to_byte(value, (*caret).min(value.chars().count()));
            value.insert_str(byte, text);
            *caret += text.chars().count();
        });
        self.chrome_dirty = true;
    }

    pub(crate) fn native_board_blur(&mut self) {
        if let Some(text) = self.hangul.flush() {
            if let Some(field) = self.board_scene.input() {
                self.native_board_insert_into(field, &text);
            }
        }
        self.board_scene.set_input(None, 0);
        if matches!(self.ime_focus, Some(crate::ImeFocus::Board(_))) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    pub(crate) fn native_board_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};
        if !self.board_room_active() {
            return false;
        }
        if event.state != ElementState::Pressed {
            return true;
        }
        let Some(field) = self.board_scene.input() else {
            let at = BoardTab::ALL
                .iter()
                .position(|tab| *tab == self.board_scene.tab())
                .unwrap_or(0);
            let next = match event.logical_key {
                Key::Named(NamedKey::ArrowUp) => at.saturating_sub(1),
                Key::Named(NamedKey::ArrowDown) => (at + 1).min(BoardTab::ALL.len() - 1),
                _ => return false,
            };
            self.board_scene.set_tab(BoardTab::ALL[next]);
            self.chrome_dirty = true;
            return true;
        };
        self.ime_retarget(crate::ImeFocus::Board(field));
        if self.host_mod() {
            return true;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.native_board_blur();
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.native_board_blur();
                return true;
            }
            Key::Named(NamedKey::Space) => {
                self.native_board_insert_into(field, " ");
                return true;
            }
            Key::Character(text) => {
                if !(self.ime_active || self.in_preedit) {
                    self.native_board_insert_into(field, text);
                }
                return true;
            }
            Key::Named(NamedKey::Backspace)
            | Key::Named(NamedKey::Delete)
            | Key::Named(NamedKey::ArrowLeft)
            | Key::Named(NamedKey::ArrowRight)
            | Key::Named(NamedKey::Home)
            | Key::Named(NamedKey::End) => {}
            _ => return true,
        }
        self.board_scene.edit_field(field, |value, caret| {
            let _ = crate::lineedit::key(value, caret, &event.logical_key);
        });
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn native_board_ime(&mut self, ime: winit::event::Ime) {
        if !self.board_room_active() {
            return;
        }
        match ime {
            winit::event::Ime::Enabled => self.ime_active = true,
            winit::event::Ime::Disabled => {
                self.ime_active = false;
                self.in_preedit = false;
                self.preedit.clear();
            }
            winit::event::Ime::Preedit(text, _) => {
                if let Some(field) = self.board_scene.input() {
                    self.ime_focus = Some(crate::ImeFocus::Board(field));
                    self.ime_active = true;
                    self.in_preedit = !text.is_empty();
                    self.preedit = text;
                }
            }
            winit::event::Ime::Commit(text) => {
                if let Some(field) = self.board_scene.input() {
                    self.native_board_insert_into(field, &text);
                }
                self.in_preedit = false;
                self.preedit.clear();
            }
        }
        self.chrome_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_worker_snapshot_cannot_replace_the_requested_generation() {
        let mut scene = Scene::default();
        scene.requested_generation = 4;
        scene.mailbox.lock().unwrap().data = Some(DataEnvelope {
            generation: 3,
            data: BoardData {
                error: Some("stale".to_string()),
                ..Default::default()
            },
        });
        assert!(!scene.pump());
        assert!(scene.data.error.is_none());
        scene.mailbox.lock().unwrap().data = Some(DataEnvelope {
            generation: 4,
            data: BoardData {
                error: Some("fresh".to_string()),
                ..Default::default()
            },
        });
        assert!(scene.pump());
        assert_eq!(scene.data.error.as_deref(), Some("fresh"));
    }

    #[test]
    fn target_pane_survives_board_navigation() {
        let mut scene = Scene::default();
        scene.enter(Some("%7".to_string()), 3, "/repo".to_string());
        scene.set_tab(BoardTab::Git);
        assert_eq!(scene.target_pane(), Some("%7"));
        assert_eq!(scene.target_window(), 3);
        assert_eq!(scene.target_cwd, "/repo");
    }

    fn overview_data() -> OverviewData {
        let mut data = overview_from_value(board_probe_value()).unwrap();
        data.local_machine_id = Some("device-a".into());
        data
    }

    #[test]
    fn overview_groups_same_pane_numbers_and_room_names_by_machine_identity() {
        let data = overview_data();
        let local = &data.panes[0];
        let remote = &data.panes[3];
        assert_eq!(local.address.surface_id, remote.address.surface_id);
        assert_eq!(local.room_label, remote.room_label);
        assert_ne!(local.id, remote.id);
        assert_eq!(overview_visible_rows(&data, &OverviewUi::default()).len(), 6);
        let filter = OverviewUi { machine: Some("device-b".into()), room: Some(("device-b".into(), Some("제품".into()))), ..Default::default() };
        let visible = overview_visible_rows(&data, &filter);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, remote.id);
        assert!(overview_is_local(&data, &local.address));
        assert!(!overview_is_local(&data, &remote.address));
        let mut changed = local.address.clone();
        changed.instance_id = Some("another-instance".into());
        assert!(!overview_is_local(&data, &changed));
        assert_eq!(remote.character.as_deref(), Some("아로나"));
        assert_ne!(remote.character.as_deref(), Some(remote.machine_label.as_str()));
    }

    #[test]
    fn overview_room_filter_survives_rename_and_always_offers_clear() {
        let mut data = overview_data();
        let ui = OverviewUi { machine: Some("device-b".into()), room: Some(("device-b".into(), Some("제품".into()))), ..Default::default() };
        let before: Vec<_> = overview_visible_rows(&data, &ui).iter().map(|row| row.id.clone()).collect();
        data.panes[3].room_label = "새로운 방 이름".into();
        let after: Vec<_> = overview_visible_rows(&data, &ui).iter().map(|row| row.id.clone()).collect();
        assert_eq!(after, before);
        let choices = overview_room_choices(&data, &ui);
        assert!(choices.iter().any(|choice| choice.0 == "새로운 방 이름" && choice.2 && choice.3));
        data.panes.retain(|row| row.id == before[0]);
        let one_room = overview_room_choices(&data, &ui);
        assert!(one_room.iter().any(|choice| matches!(choice.1, Target::OverviewRoom(None)) && choice.3));
        assert!(one_room.iter().any(|choice| choice.2 && choice.3));
        data.panes.clear();
        let missing_room = overview_room_choices(&data, &ui);
        assert!(missing_room.iter().any(|choice| matches!(choice.1, Target::OverviewRoom(None)) && choice.3));
        assert!(missing_room.iter().any(|choice| choice.2 && !choice.3));
        let cleared = OverviewUi { room: None, ..ui };
        assert!(overview_room_choices(&data, &cleared).iter().any(|choice| matches!(choice.1, Target::OverviewRoom(None)) && choice.2 && choice.3));
    }

    #[test]
    fn overview_room_changes_keep_deleted_panes_and_source_failures() {
        let mut data = overview_data();
        let ui = OverviewUi { machine: Some("device-b".into()), room: Some(("device-b".into(), Some("제품".into()))), ..Default::default() };
        let removed = OverviewChange { machine_id: "device-b".into(), pane_id: Some(data.panes[3].id.clone()),
            room_id: Some("제품".into()), room_label: Some("옛 방 이름".into()), kind: "pane_removed".into(), ..Default::default() };
        data.panes[3].room_id = Some("다른 방".into());
        data.panes[3].room_label = "다른 이름".into();
        assert!(overview_change_visible(&removed, &data, &ui));
        data.panes.retain(|row| Some(&row.id) != removed.pane_id.as_ref());
        assert!(overview_change_visible(&removed, &data, &ui));
        let other_room = OverviewUi { room: Some(("device-b".into(), Some("다른 방".into()))), ..ui.clone() };
        assert!(!overview_change_visible(&removed, &data, &other_room));
        let source_error = OverviewChange { machine_id: "device-b".into(), kind: "source_error".into(), ..Default::default() };
        assert!(overview_change_visible(&source_error, &data, &ui));
        assert!(overview_change_visible(&source_error, &data, &other_room));
        let other_machine = OverviewUi { machine: Some("device-a".into()), room: Some(("device-a".into(), Some("제품".into()))), ..Default::default() };
        assert!(!overview_change_visible(&source_error, &data, &other_machine));
        let legacy_removed = OverviewChange { room_id: None, room_label: None, ..removed };
        assert!(overview_change_visible(&legacy_removed, &data, &ui));
    }

    #[test]
    fn overview_save_keeps_the_address_but_cannot_queue_unvalidated_input() {
        let data = overview_data();
        let original = data.panes[0].address.clone();
        let mut replacement = original.clone();
        replacement.session_id = Some("replacement-conversation".into());
        replacement.instance_id = Some("replacement-process".into());
        for address in [&original, &replacement] {
            let choice = overview_save_choice(address);
            assert_eq!(choice.1, Target::OverviewSave(address.clone()));
            assert!(!choice.3);
        }
        let source = include_str!("native_board.rs").split_once("#[cfg(test)]\nmod tests {").unwrap().0;
        assert!(!source.contains("UserEvent::SaveSession"));
        let click = source.split_once("Target::OverviewSave(_) => {").unwrap().1.split_once("Target::ResumeBackground").unwrap().0;
        assert!(click.contains("report_error"));
        assert!(!click.contains("send_event"));
    }

    #[test]
    fn overview_fixture_requests_are_rejected_outside_an_isolated_debug_build() {
        assert_eq!(board_probe_mode(true, false, false), BoardProbeMode::Live);
        assert_eq!(board_probe_mode(true, true, true), BoardProbeMode::Fixture);
        for (debug, isolated) in [(true, false), (false, false), (false, true)] {
            assert_eq!(board_probe_mode(debug, true, isolated), BoardProbeMode::Rejected);
        }
        let source = include_str!("native_board.rs");
        let request = source.split_once("pub(crate) fn request_refresh(").unwrap().1.split_once("pub(crate) fn pump(").unwrap().0;
        let rejected = request.split_once("if probe == BoardProbeMode::Rejected {").unwrap().1.split_once("#[cfg(debug_assertions)]").unwrap().0;
        assert!(rejected.contains("return;"));
        assert!(!rejected.contains("collect_data"));
        assert!(!rejected.contains("std::thread::spawn"));
        assert!(request.contains("#[cfg(debug_assertions)]\n        if probe == BoardProbeMode::Fixture"));
        let app_request = source.split_once("pub(crate) fn request_native_board_refresh(").unwrap().1.split_once("let target =").unwrap().0;
        assert!(app_request.contains("if board_fixture_requested()"));
        assert!(app_request.contains("return;"));
    }

    #[test]
    fn idle_is_not_completion_and_unavailable_evidence_stays_uncertain() {
        let data = overview_data();
        assert_eq!(overview_status(&data.panes[2]).0, "대기 중");
        assert_eq!(overview_status(&data.panes[3]).0, "완료 보고");
        assert_eq!(overview_status(&data.panes[4]).0, "미확인");
        assert_eq!(overview_status(&data.panes[5]).0, "오래된 정보");
        assert_eq!(overview_status(&data.panes[1]).0, "확인 필요");
        let mut disconnected = data.panes[5].clone();
        disconnected.freshness = "offline".into();
        assert_eq!(overview_status(&disconnected).0, "연결 끊김");
    }

    #[test]
    fn overview_order_stays_anchored_until_the_user_sorts_again() {
        let mut scene = Scene::default();
        scene.data = Arc::new(BoardData { overview: Arc::new(overview_data()), ..Default::default() });
        scene.sort_overview(OverviewSort::Status);
        let before = scene.overview.order.clone();
        let mut changed = (*scene.data.overview).clone();
        changed.panes[0].status = "waiting".into();
        changed.panes[1].status = "idle".into();
        scene.data = Arc::new(BoardData { overview: Arc::new(changed), ..Default::default() });
        scene.revalidate_overview();
        assert_eq!(scene.overview.order, before);
        scene.sort_overview(OverviewSort::Status);
        assert_ne!(scene.overview.order, before);
    }

    #[test]
    fn overview_inspection_rejects_a_previous_selection_and_replaced_session() {
        let mut scene = Scene::default();
        let mut data = overview_data();
        scene.data = Arc::new(BoardData { overview: Arc::new(data.clone()), ..Default::default() });
        scene.toggle_overview_detail(data.panes[0].id.clone());
        data.detail = Some(OverviewDetail { selection: scene.overview.selection.clone().unwrap(), observed_at_ms: data.observed_at_ms, lines: vec!["첫 번째 창의 내용".into()], error: None });
        assert!(overview_current_detail(&data, &scene.overview).is_some());
        scene.toggle_overview_detail(data.panes[3].id.clone());
        assert!(overview_current_detail(&data, &scene.overview).is_none());
        scene.toggle_overview_detail(data.panes[0].id.clone());
        assert!(overview_current_detail(&data, &scene.overview).is_none());
        data.detail.as_mut().unwrap().selection = scene.overview.selection.clone().unwrap();
        assert!(overview_current_detail(&data, &scene.overview).is_some());
        data.panes[0].address.session_id = Some("new-conversation".into());
        assert!(overview_current_detail(&data, &scene.overview).is_none());
        scene.data = Arc::new(BoardData { overview: Arc::new(data), ..Default::default() });
        scene.revalidate_overview();
        assert!(scene.overview.selection.is_none());
    }

    #[test]
    fn failed_overview_refresh_preserves_last_information_but_disables_focus() {
        let previous = overview_data();
        let data = overview_failed(&previous, "offline".into());
        assert_eq!(data.panes.len(), previous.panes.len());
        assert_eq!(data.panes[0].request, previous.panes[0].request);
        assert_eq!(data.panes[0].progress, previous.panes[0].progress);
        assert_eq!(data.panes[0].observed_at_ms, previous.panes[0].observed_at_ms);
        assert!(data.panes.iter().all(|row| row.freshness == "stale"));
        assert!(!overview_is_local(&data, &data.panes[0].address));
        assert!(data.detail.is_none());
        assert!(data.error.is_some());
    }

    #[test]
    fn overview_long_korean_and_urls_wrap_inside_the_measured_width() {
        let measure = |line: &str| line.chars().map(|ch| if ch.is_ascii() { 6.0 } else { 12.0 }).sum::<f32>();
        for width in [36.0, 112.0, 248.0, 560.0] {
            let value = "공백없는아주긴한글내용으로확인합니다https://example.test/averylongunbrokentoken?with=parameters";
            let full = board_wrap(value, width, 100, measure);
            assert_eq!(full.concat(), value);
            assert!(full.iter().all(|line| measure(line) <= width));
            let short = board_wrap(value, width, 2, measure);
            assert!(short.len() <= 2);
            assert!(short.iter().all(|line| measure(line) <= width));
            if full.len() > 2 { assert!(short.last().unwrap().ends_with('…')); }
        }
    }

    #[test]
    fn overview_detail_and_recent_changes_are_bounded_and_keep_error_signals() {
        let events = serde_json::json!({"events":[{"kind":"prompt","text":"요청 내용"}, {"kind":"result","name":"Check","text":"실패 이유","is_error":true}]});
        let lines = overview_detail_lines(&events);
        assert_eq!(lines[0], "요청 · 요청 내용");
        assert_eq!(lines[1], "오류 · Check · 실패 이유");
        let many = serde_json::json!({"events":vec![serde_json::json!({"kind":"say","text":"가".repeat(2000)}); 100]});
        let lines = overview_detail_lines(&many);
        assert_eq!(lines.len(), 20);
        assert!(lines.iter().all(|line| line.chars().count() <= 1600));
        let data = overview_data();
        let mut changes = data.recent_changes.clone();
        merge_overview_changes(&mut changes, &data.recent_changes, data.recent_changes.clone());
        assert_eq!(changes.len(), 2);
        assert!(changes[0].at_ms >= changes[1].at_ms);
    }

    #[test]
    fn overview_probe_rejects_user_paths_and_parent_traversal() {
        let root = std::path::Path::new("/tmp/kasaterm-board-probe");
        assert!(board_fixture_path(root, &root.join("session.json")));
        assert!(!board_fixture_path(root, std::path::Path::new("/Users/example/session.json")));
        assert!(!board_fixture_path(root, &root.join("../session.json")));
        assert!(!board_fixture_path(root, root));
    }

    #[test]
    fn overview_collection_exits_before_legacy_agent_and_remote_loaders() {
        let source = include_str!("native_board.rs");
        let collection = source.split_once("fn collect_data(").unwrap().1;
        let overview = collection.split_once("if tab == BoardTab::Overview {").unwrap().1
            .split_once("let mut errors = Vec::new();").unwrap().0;
        assert!(overview.contains("collect_overview"));
        assert!(overview.contains("return BoardData"));
        for forbidden in ["collab_board()", "collect_background", "session_transfer::collect", "pane_tasks_snapshot", "remoteboard::board_rows"] {
            assert!(!overview.contains(forbidden), "전체 보드에서 구형 수집 호출: {forbidden}");
        }
    }

    fn transfer_data() -> TransferSnapshot {
        use crate::session_transfer::{TransferMachine, RoomInfo};
        TransferSnapshot {
            machines: vec![
                TransferMachine { id: "source".into(), label: "맥북".into(), local: true, online: true, room_transfer_supported: true, ..Default::default() },
                TransferMachine { id: "destination".into(), label: "미니".into(), online: true, room_transfer_supported: true, rooms: vec![RoomInfo { id: "work".into(), title: "작업 방".into() }], ..Default::default() },
            ],
            sessions: vec![SessionRow {
                identity: SessionIdentity { machine_id: "source".into(), pane_id: "%8".into(), session_id: Some("conversation".into()), instance: "instance".into(), token: "generation".into() },
                harness: Some("codex".into()), name: "학생".into(), status: "idle".into(), ..Default::default()
            }],
            errors: Vec::new(),
        }
    }

    #[test]
    fn transfer_review_is_non_mutating_and_requires_a_current_room() {
        let data = transfer_data();
        let mut scene = Scene::default();
        scene.data = Arc::new(BoardData { transfer: Arc::new(data.clone()), ..Default::default() });
        scene.transfer.selected.insert(data.sessions[0].identity.clone());
        scene.transfer.destination = "destination".into();
        scene.review_transfer(false);
        assert!(scene.transfer.pending.is_none());
        scene.transfer.room = Some("work".into());
        scene.review_transfer(false);
        assert!(matches!(&scene.transfer.pending, Some(TransferConfirmation::Move(request)) if !request.confirmed));
        assert!(!scene.transfer.busy);
        assert!(scene.mailbox.lock().unwrap().transfers.is_empty());
        assert!(scene.mailbox.lock().unwrap().actions.is_empty());
    }

    #[test]
    fn changed_session_identity_cancels_review_even_when_the_pane_number_matches() {
        let data = transfer_data();
        let mut scene = Scene::default();
        scene.transfer.selected.insert(data.sessions[0].identity.clone());
        scene.transfer.destination = "destination".into();
        scene.transfer.room = Some("work".into());
        scene.data = Arc::new(BoardData { transfer: Arc::new(data.clone()), ..Default::default() });
        scene.review_transfer(false);
        let mut changed = data;
        changed.sessions[0].identity.token = "replacement".into();
        scene.data = Arc::new(BoardData { transfer: Arc::new(changed), ..Default::default() });
        scene.revalidate_transfer_selection();
        assert!(scene.transfer.pending.is_none());
        assert!(scene.transfer.selected.is_empty());
        assert!(scene.transfer.error.is_some());
    }

    #[test]
    fn destination_validation_rejects_same_machine_and_empty_new_room() {
        let data = transfer_data();
        let mut ui = TransferUi::default();
        ui.selected.insert(data.sessions[0].identity.clone());
        ui.destination = "source".into();
        ui.new_room = true;
        ui.room_name = "새 작업".into();
        assert!(transfer_request(&ui, &data).is_err());
        ui.destination = "destination".into();
        ui.room_name = "   ".into();
        assert!(transfer_request(&ui, &data).is_err());
        ui.room_name = "  새 작업  ".into();
        let request = transfer_request(&ui, &data).unwrap();
        assert_eq!(request.destination_room, RoomTarget::New("새 작업".into()));
        assert!(!request.confirmed);
    }

    #[test]
    fn partial_transfer_results_survive_refresh_and_ignore_old_generations() {
        let data = transfer_data();
        let mut scene = Scene::default();
        scene.transfer_generation = 2;
        scene.transfer.busy = true;
        let result = TransferResult { source: data.sessions[0].identity.clone(), status: TransferStatus::Failed, message: "연결을 확인해 주세요".into(), destination: None };
        scene.mailbox.lock().unwrap().transfers.push(TransferEnvelope { generation: 1, result: Some(result.clone()), finished: true });
        scene.pump();
        assert!(scene.transfer.busy);
        assert!(scene.transfer.results.is_empty());
        scene.mailbox.lock().unwrap().transfers.push(TransferEnvelope { generation: 2, result: Some(result), finished: true });
        scene.pump();
        assert!(!scene.transfer.busy);
        assert_eq!(scene.transfer.results[0].status, TransferStatus::Failed);
        scene.pump();
        assert_eq!(scene.transfer.results.len(), 1);
    }

    #[test]
    fn paint_has_no_process_file_or_network_work() {
        let source = include_str!("native_board.rs");
        let paint = source
            .split_once("pub(crate) fn paint(")
            .unwrap()
            .1
            .split_once("impl App {")
            .unwrap()
            .0;
        for forbidden in [
            "std::thread::spawn",
            "std::process::Command",
            "read_to_string",
            "git_status(",
            "TcpStream",
            "reqwest",
            "curl",
            "collab_snapshot(",
            "collab_changes(",
            "collab_inspect(",
        ] {
            assert!(!paint.contains(forbidden), "paint에서 I/O 발견: {forbidden}");
        }
    }

    /// 「x」 한 번으로 배경 세션이 죽으면 안 된다 — 되돌릴 수 없다. 첫 클릭은 확인을
    /// 세우기만 하고, 실제 kill 은 확인 클릭에서만 나간다. 뼈대(enum·arm/clear·
    /// pending_stop)만 있고 배선이 없으면 화면은 종전처럼 한 번에 죽인다.
    #[test]
    fn stopping_a_background_session_needs_a_confirming_second_click() {
        let source = include_str!("native_board.rs");
        let click = source
            .split_once("pub(crate) fn native_board_click")
            .unwrap()
            .1
            .split_once("fn run_native_board_action")
            .unwrap()
            .0;
        let first = click
            .split_once("Target::StopBackground(target) => {")
            .expect("첫 클릭 갈래")
            .1
            .split_once("Target::ConfirmStopBackground")
            .expect("확인 갈래가 뒤따라야 한다")
            .0;
        assert!(first.contains("arm_stop"), "첫 클릭은 확인만 세운다");
        assert!(
            !first.contains("WorkerAction::StopBackground"),
            "첫 클릭에서 kill 이 나가면 확인 단계가 장식이 된다"
        );
        let confirmed = click
            .split_once("Target::ConfirmStopBackground(target) => {")
            .expect("확인 갈래")
            .1;
        assert!(
            confirmed.contains("WorkerAction::StopBackground"),
            "확인 클릭이 실제로 멈춘다"
        );
        assert!(click.contains("Target::CancelStopBackground"), "취소 갈래");
        assert!(
            click.contains("clear_stop"),
            "다른 곳을 누르면 확인이 접혀야 한다"
        );
        assert!(
            source.contains("s.pending_stop.as_ref() == Some(target)"),
            "paint 가 확인 대기 행을 실제로 갈라 그려야 한다"
        );
    }

    #[test]
    fn clicks_route_mutations_to_typed_worker_actions() {
        let source = include_str!("native_board.rs");
        let click = source
            .split_once("pub(crate) fn native_board_click")
            .unwrap()
            .1
            .split_once("fn run_native_board_action")
            .unwrap()
            .0;
        for action in [
            "WorkerAction::StopBackground",
            "WorkerAction::ScheduleAdd",
            "WorkerAction::ScheduleToggle",
            "WorkerAction::ScheduleDelete",
            "WorkerAction::GitCommit",
            "WorkerAction::GitPush",
            "WorkerAction::ViewSession",
        ] {
            assert!(click.contains(action), "worker action routing 누락: {action}");
        }
        // 이어받기는 pane 을 실제로 만드는 일이라 워커 스레드가 아니라 GUI
        // 스레드로 간다(`UserEvent` 의 reply 채널로 결과를 되받는다). 워커에 남으면
        // 만들어진 pane 을 확인할 길이 없어 성공 토스트가 거짓이 된다.
        for gui in ["resume_background_in_target_room"] {
            assert!(click.contains(gui), "GUI 스레드 경로 누락: {gui}");
        }
        for forbidden in ["git_status(", "std::process::Command", "read_to_string"] {
            assert!(!click.contains(forbidden), "click에서 직접 I/O 발견: {forbidden}");
        }
    }
}
