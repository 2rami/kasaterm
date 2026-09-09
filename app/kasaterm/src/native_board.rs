//! WGPU 네이티브 운영 보드의 상태와 worker 스냅샷.
//!
//! THESIS: 터미널을 가리는 대시보드가 아니라, 작업 방 하나로 오가는 운영실이다.
//! OWN-WORLD: 현재 터미널 팔레트, 얇은 경계, 상태색과 학생 스프라이트를 공유한다.
//! STORY: 확인할 것부터 보고 학생·예약·Git·기계를 한 자리에서 조작한다.
//! FIRST VIEWPORT: 왼쪽 운영 탭, 오른쪽에는 대상 pane과 현재 현황이 바로 보인다.
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

    pub(crate) const fn icon(self) -> &'static str {
        match self {
            Self::Overview => "rows-2",
            Self::Agents => "users",
            Self::Schedule => "rotate-cw",
            Self::Git => "git-branch",
            Self::Machines => "server",
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

#[derive(Clone, Debug, Default)]
pub(crate) struct BoardData {
    pub(crate) agents: Arc<Vec<PaneActivity>>,
    pub(crate) tasks: Arc<Vec<kasa_mcp::PaneTaskView>>,
    pub(crate) background: Arc<Vec<BackgroundRow>>,
    pub(crate) schedules: Arc<Vec<kasa_mcp::ScheduleItem>>,
    pub(crate) transfer: Arc<TransferSnapshot>,
    pub(crate) git: Arc<GitSnapshot>,
    pub(crate) faces: Arc<Vec<FaceAsset>>,
    pub(crate) error: Option<String>,
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
    ToggleAgentDetail(String),
    SavePane(String),
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
    pub(crate) expanded_agent: Option<String>,
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
    expanded_agent: Option<String>,
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
            expanded_agent: None,
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
            expanded_agent: self.expanded_agent.clone(),
            pending_stop: self.pending_stop.clone(),
            transfer: self.transfer.clone(),
            fixture: transfer_fixture_active(),
        }
    }

    pub(crate) fn selected_git(&self) -> &HashSet<String> {
        &self.git_selected
    }

    pub(crate) fn toggle_agent_detail(&mut self, pane: String) {
        if self.expanded_agent.as_deref() == Some(pane.as_str()) {
            self.expanded_agent = None;
        } else {
            self.expanded_agent = Some(pane);
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

    pub(crate) fn refresh_due(&self) -> bool {
        !self.refreshing
            && self
                .last_refresh
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(2200))
    }

    pub(crate) fn request_refresh(
        &mut self,
        backend: Arc<dyn Backend>,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
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
        std::thread::spawn(move || {
            let data = collect_data(&backend, target_window, &target_cwd);
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
) -> BoardData {
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
    let visible_agents: HashSet<_> = agents.iter().map(|row| row.surface_id.as_str()).collect();
    let tasks = kasa_mcp::pane_tasks_snapshot(backend, None)
        .into_iter()
        .filter(|task| visible_agents.contains(task.pane.as_str()))
        .collect();
    let background = collect_background(backend).unwrap_or_else(|error| {
        errors.push(error.to_string());
        Vec::new()
    });
    let schedules = kasa_mcp::schedule_snapshot();
    let git = collect_git(target_cwd);
    BoardData {
        agents: Arc::new(agents),
        tasks: Arc::new(tasks),
        background: Arc::new(background),
        schedules: Arc::new(schedules),
        transfer: Arc::new(transfer),
        git: Arc::new(git),
        faces: Arc::new(faces),
        error: (!errors.is_empty()).then(|| errors.join(" · ")),
    }
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
    let nav_w = if aw < 760.0 { 154.0 } else { 190.0 };
    let mut hits = Vec::new();
    let mut caret_rect = None;
    g.rect(ax, ay, aw, ah, theme::bg());
    g.rect(ax, ay, nav_w, ah, theme::panel_bg());
    g.rect(ax + nav_w - 1.0, ay, 1.0, ah, theme::border());
    text(g, ax + 20.0, ay + 20.0, "운영 보드", 18.0, theme::text(), true);
    let busy = snapshot
        .data
        .agents
        .iter()
        .filter(|row| agent_is_working(row))
        .count();
    let waiting = snapshot
        .data
        .agents
        .iter()
        .filter(|row| agent_needs_attention(row))
        .count();
    text(
        g,
        ax + 20.0,
        ay + 47.0,
        &format!("작업 중 {busy} · 확인 필요 {waiting}"),
        11.0,
        if waiting > 0 { theme::danger() } else { theme::text_dim() },
        false,
    );

    let mut ny = ay + 82.0;
    for tab in BoardTab::ALL {
        let rect = (ax + 10.0, ny, nav_w - 20.0, 36.0);
        let selected = snapshot.tab == tab;
        let hover = contains(rect, snapshot.cursor);
        if selected || hover {
            round_rect(
                g,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                theme::radius_md(),
                if selected { theme::surface_active() } else { theme::surface_hover() },
            );
        }
        if selected {
            g.rect(rect.0, rect.1 + 8.0, 2.0, rect.3 - 16.0, theme::accent());
        }
        g.queue_icon(
            tab.icon(),
            rect.0 + 12.0,
            rect.1 + 10.0,
            15.0,
            if selected { theme::text() } else { theme::text_mute() },
        );
        text(
            g,
            rect.0 + 36.0,
            rect.1 + 10.0,
            tab.label(),
            13.0,
            if selected { theme::text() } else { theme::text_dim() },
            selected,
        );
        hit(g, &mut hits, Target::Tab(tab), rect, false);
        g.hover_pointer |= hover;
        ny += 39.0;
    }
    let back = (ax + 12.0, ay + ah - 48.0, nav_w - 24.0, 34.0);
    if contains(back, snapshot.cursor) {
        round_rect(g, back.0, back.1, back.2, back.3, theme::radius_md(), theme::surface_hover());
        g.hover_pointer = true;
    }
    g.queue_icon("chevron-left", back.0 + 10.0, back.1 + 9.0, 15.0, theme::text_dim());
    text(g, back.0 + 33.0, back.1 + 9.0, "작업 방으로", 12.0, theme::text_dim(), false);
    hit(g, &mut hits, Target::Return, back, false);

    let content_x = ax + nav_w + if aw < 760.0 { 22.0 } else { 38.0 };
    let content_w = (aw - nav_w - if aw < 760.0 { 44.0 } else { 76.0 })
        .max(180.0)
        .min(1120.0);
    text(g, content_x, ay + 20.0, snapshot.tab.label(), 24.0, theme::text(), true);
    let target = if snapshot.target_cwd.is_empty() {
        snapshot.target_pane.clone()
    } else {
        format!("{} · {}", snapshot.target_pane, short_path(&snapshot.target_cwd))
    };
    let target = format!("기준 pane · {target}");
    let target = fit(g, &target, content_w - 92.0, 11.5, true);
    let target_w = (g.measure_chrome_text(&target, 11.5, true) + 20.0).min(content_w - 72.0);
    round_rect(
        g,
        content_x,
        ay + 48.0,
        target_w,
        25.0,
        theme::radius_sm(),
        theme::with_alpha(theme::accent(), 36),
    );
    text(g, content_x + 10.0, ay + 54.0, &target, 11.5, theme::text(), true);
    let refresh = (content_x + content_w - 34.0, ay + 18.0, 32.0, 32.0);
    icon_button(g, snapshot, &mut hits, refresh, "rotate-cw", Target::Refresh);
    if snapshot.refreshing {
        text(g, refresh.0 - 62.0, refresh.1 + 9.0, "갱신 중", 10.5, theme::text_mute(), false);
    }
    g.rect(content_x, ay + 82.0, content_w, 1.0, theme::border());

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
    let awaiting: Vec<_> = s
        .data
        .agents
        .iter()
        .filter(|row| agent_needs_attention(row))
        .collect();
    if !awaiting.is_empty() {
        section(g, x, y, "확인 필요", &format!("선생님을 기다리는 학생 {}명", awaiting.len()));
        for row in awaiting {
            let rect = (x, *y, w, 42.0);
            outlined(g, rect, theme::with_alpha(theme::danger(), 24));
            status_dot(g, rect.0 + 14.0, rect.1 + 17.0, row);
            text(g, rect.0 + 32.0, rect.1 + 7.0, &agent_name(row), 12.5, theme::text(), true);
            text(
                g,
                rect.0 + 32.0,
                rect.1 + 23.0,
                row.waiting_for.as_deref().unwrap_or("응답이 필요해요"),
                10.5,
                theme::danger(),
                false,
            );
            if row.machine.is_none() {
                hit(g, hits, Target::FocusPane(row.surface_id.clone()), rect, false);
            }
            *y += 48.0;
        }
        *y += 8.0;
    }
    section(g, x, y, "현황", "현재 대상 방의 학생과 진행 흐름");
    if s.data.agents.is_empty() {
        empty(g, x, y, w, "이 방에서 일하는 학생이 아직 없어요");
        return;
    }
    for row in s.data.agents.iter() {
        let room_tasks: Vec<_> = s
            .data
            .tasks
            .iter()
            .filter(|task| task.pane == row.surface_id)
            .collect();
        let tasks: Vec<_> = room_tasks.iter().copied().filter(|task| task.mine).collect();
        let unassigned = room_tasks.iter().filter(|task| task.owner.is_empty()).count();
        let others = room_tasks
            .iter()
            .filter(|task| !task.mine && !task.owner.is_empty())
            .count();
        let expanded = s.expanded_agent.as_deref() == Some(row.surface_id.as_str());
        let task_fold_lines = usize::from(unassigned > 0) + usize::from(others > 0);
        let summary_lines = usize::from(!tasks.is_empty())
            + task_fold_lines
            + usize::from(!row.subagents.is_empty() || !row.background.is_empty())
            + usize::from(!row.recent_tools.is_empty());
        let detail_lines = if expanded {
            tasks.len().min(5)
                + task_fold_lines
                + row.subagents.len().min(3)
                + row.background.len().min(3)
                + row.recent_tools.len().min(8)
        } else {
            summary_lines
        };
        let h = 72.0 + detail_lines as f32 * 22.0;
        let rect = (x, *y, w, h);
        outlined(g, rect, theme::surface_hover());
        draw_face(g, s, row, rect.0 + 12.0, rect.1 + 12.0, 34.0);
        status_dot(g, rect.0 + 51.0, rect.1 + 18.0, row);
        text(g, rect.0 + 66.0, rect.1 + 10.0, &agent_name(row), 13.0, theme::text(), true);
        let project = if row.title.is_empty() { &row.intent } else { &row.title };
        let project = fit(g, project, w - 190.0, 11.0, false);
        text(g, rect.0 + 66.0, rect.1 + 29.0, &project, 11.0, theme::text_dim(), false);
        text(
            g,
            rect.0 + 66.0,
            rect.1 + 47.0,
            &status_label(row),
            10.5,
            theme::enforce_contrast_at(status_color(row), theme::surface_hover(), 4.5),
            true,
        );
        let detail = (rect.0 + rect.2 - 140.0, rect.1 + 12.0, 60.0, 28.0);
        button(
            g,
            s,
            hits,
            detail,
            if expanded { "접기" } else { "상세" },
            Target::ToggleAgentDetail(row.surface_id.clone()),
            false,
        );
        if row.machine.is_none() {
            let save = (rect.0 + rect.2 - 72.0, rect.1 + 12.0, 60.0, 28.0);
            button(g, s, hits, save, "저장", Target::SavePane(row.surface_id.clone()), false);
        } else {
            text(
                g,
                rect.0 + rect.2 - 72.0,
                rect.1 + 20.0,
                "원격",
                10.0,
                theme::text_mute(),
                true,
            );
        }
        let mut ey = rect.1 + 70.0;
        if !tasks.is_empty() {
            if expanded {
                for task in tasks.iter().take(5) {
                    g.queue_icon("square-check", rect.0 + 16.0, ey, 13.0, theme::text_mute());
                    let task_text = format!("{} · {}", task.status, task.subject);
                    let task_text = fit(g, &task_text, w - 52.0, 10.5, false);
                    text(g, rect.0 + 36.0, ey + 1.0, &task_text, 10.5, theme::text_dim(), false);
                    ey += 22.0;
                }
            } else {
                let doing = tasks.iter().filter(|task| task.status == "in_progress").count();
                let done = tasks.iter().filter(|task| task.status == "completed").count();
                g.queue_icon("square-check", rect.0 + 16.0, ey, 13.0, theme::text_mute());
                text(g, rect.0 + 36.0, ey + 1.0, &format!("태스크 · 진행 {doing} · 완료 {done}"), 10.5, theme::text_dim(), false);
                ey += 22.0;
            }
        }
        if unassigned > 0 {
            g.queue_icon("square", rect.0 + 16.0, ey, 13.0, theme::text_mute());
            text(
                g,
                rect.0 + 36.0,
                ey + 1.0,
                &format!("미배정 태스크 {unassigned}개"),
                10.5,
                theme::text_mute(),
                false,
            );
            ey += 22.0;
        }
        if others > 0 {
            g.queue_icon("users", rect.0 + 16.0, ey, 13.0, theme::text_mute());
            text(
                g,
                rect.0 + 36.0,
                ey + 1.0,
                &format!("같은 방 다른 캐릭터 태스크 {others}개"),
                10.5,
                theme::text_mute(),
                false,
            );
            ey += 22.0;
        }
        if !row.subagents.is_empty() || !row.background.is_empty() {
            if expanded {
                for label in row.subagents.iter().take(3) {
                    g.queue_icon("users", rect.0 + 16.0, ey, 13.0, theme::accent());
                    let label = fit(g, &format!("서브에이전트 · {label}"), w - 52.0, 10.5, false);
                    text(g, rect.0 + 36.0, ey + 1.0, &label, 10.5, theme::text_dim(), false);
                    ey += 22.0;
                }
                for label in row.background.iter().take(3) {
                    g.queue_icon("terminal", rect.0 + 16.0, ey, 13.0, theme::accent());
                    let label = fit(g, &format!("백그라운드 · {label}"), w - 52.0, 10.5, false);
                    text(g, rect.0 + 36.0, ey + 1.0, &label, 10.5, theme::text_dim(), false);
                    ey += 22.0;
                }
            } else {
                g.queue_icon("users", rect.0 + 16.0, ey, 13.0, theme::accent());
                text(
                    g,
                    rect.0 + 36.0,
                    ey + 1.0,
                    &format!("서브 {} · 백그라운드 {}", row.subagents.len(), row.background.len()),
                    10.5,
                    theme::text_dim(),
                    false,
                );
                ey += 22.0;
            }
        }
        if !row.recent_tools.is_empty() {
            if expanded {
                for (index, tool) in row.recent_tools.iter().rev().take(8).enumerate() {
                    g.queue_icon("braces", rect.0 + 16.0, ey, 13.0, theme::text_mute());
                    let tool = fit(g, &format!("{}  {tool}", index + 1), w - 52.0, 10.0, false);
                    text(g, rect.0 + 36.0, ey + 1.0, &tool, 10.0, theme::text_dim(), false);
                    ey += 22.0;
                }
            } else {
                g.queue_icon("braces", rect.0 + 16.0, ey, 13.0, theme::text_mute());
                let tools = row.recent_tools.iter().rev().take(3).cloned().collect::<Vec<_>>().join("  →  ");
                let tools = fit(g, &tools, w - 52.0, 10.0, false);
                text(g, rect.0 + 36.0, ey + 1.0, &tools, 10.0, theme::text_dim(), false);
            }
        }
        *y += h + 8.0;
    }
}

fn paint_agents(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    section(g, x, y, "백그라운드 에이전트", "pane 밖에서도 계속 도는 대화");
    if s.data.background.is_empty() {
        empty(g, x, y, w, "백그라운드 세션이 없어요");
        return;
    }
    for row in s.data.background.iter() {
        let rect = (x, *y, w, 58.0);
        outlined(g, rect, theme::surface_hover());
        let label = if row.name.is_empty() { &row.id } else { &row.name };
        let label = fit(g, label, w - 210.0, 12.5, true);
        text(g, rect.0 + 14.0, rect.1 + 10.0, &label, 12.5, theme::text(), true);
        let origin = row
            .parent_surface
            .as_deref()
            .map(|pane| format!("연결 {pane}"))
            .unwrap_or_else(|| format_age(row.started_at));
        let location = row.machine.as_deref().unwrap_or("이 기기");
        let sub = format!(
            "{} · {} · {} · {}",
            background_state(row),
            short_path(&row.cwd),
            origin,
            location,
        );
        let sub = fit(g, &sub, w - 210.0, 10.5, false);
        text(g, rect.0 + 14.0, rect.1 + 32.0, &sub, 10.5, theme::text_dim(), false);
        if row.kind == "background" && row.machine.is_none() {
            // 멈춤은 되돌릴 수 없다. 확인을 기다리는 행은 「이어받기」를 접고 그
            // 자리를 확인 버튼에 내준다 — 둘을 함께 두면 폭이 겹치고, 이 순간
            // 고를 것은 멈출지 말지뿐이다.
            let pending = background_stop_target(row)
                .filter(|target| s.pending_stop.as_ref() == Some(target));
            if let Some(target) = pending {
                let confirm = (rect.0 + rect.2 - 164.0, rect.1 + 14.0, 96.0, 30.0);
                button(
                    g,
                    s,
                    hits,
                    confirm,
                    "정말 멈추기",
                    Target::ConfirmStopBackground(target),
                    true,
                );
                let cancel = (rect.0 + rect.2 - 56.0, rect.1 + 14.0, 44.0, 30.0);
                icon_button(g, s, hits, cancel, "x", Target::CancelStopBackground);
            } else {
                let resume = (rect.0 + rect.2 - 164.0, rect.1 + 14.0, 96.0, 30.0);
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
                let stop = (rect.0 + rect.2 - 56.0, rect.1 + 14.0, 44.0, 30.0);
                if let Some(target) = background_stop_target(row) {
                    icon_button(g, s, hits, stop, "x", Target::StopBackground(target));
                }
            }
        } else if row.kind == "background" {
            text(
                g,
                rect.0 + rect.2 - 176.0,
                rect.1 + 21.0,
                "원격 기기에서만 제어 가능",
                10.0,
                theme::text_mute(),
                false,
            );
        }
        *y += 66.0;
    }
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
    section(g, x, y, "새 스케줄", "반복 지시, 예약, 타이머를 학생에게 보냅니다");
    let gap = 6.0;
    let kw = ((w - gap * 2.0) / 3.0).max(70.0);
    for (i, (kind, label)) in [("loop", "반복 루프"), ("cron", "예약"), ("timer", "타이머")]
        .into_iter()
        .enumerate()
    {
        button(
            g,
            s,
            hits,
            (x + i as f32 * (kw + gap), *y, kw, 32.0),
            label,
            Target::ScheduleKind(kind.to_string()),
            s.schedule_kind == kind,
        );
    }
    *y += 42.0;
    text(g, x, *y, "대상", 11.0, theme::text_dim(), true);
    *y += 20.0;
    let mut sx = x;
    for row in s.data.agents.iter() {
        let label = agent_name(row);
        let bw = (g.measure_chrome_text(&label, 10.5, false) + 22.0).clamp(70.0, 150.0);
        if sx + bw > x + w {
            sx = x;
            *y += 36.0;
        }
        button(
            g,
            s,
            hits,
            (sx, *y, bw, 30.0),
            &label,
            Target::ScheduleSurface(row.surface_id.clone()),
            s.schedule_surface == row.surface_id,
        );
        sx += bw + 6.0;
    }
    *y += 42.0;
    field(g, s, hits, caret, (x, *y, w, 40.0), "보낼 지시", &s.schedule_text, BoardInput::ScheduleText);
    *y += 50.0;
    let detail = if s.schedule_kind == "cron" {
        (&s.schedule_at, BoardInput::ScheduleAt, "Unix 시각(초)")
    } else {
        (&s.schedule_minutes, BoardInput::ScheduleMinutes, if s.schedule_kind == "loop" { "간격(분)" } else { "몇 분 뒤" })
    };
    field(g, s, hits, caret, (x, *y, 190.0, 38.0), detail.2, detail.0, detail.1);
    button(g, s, hits, (x + 202.0, *y, 88.0, 38.0), "등록", Target::ScheduleAdd, true);
    *y += 58.0;
    section(g, x, y, "등록됨", "멈추거나 다시 켜고, 필요 없는 항목은 지울 수 있어요");
    if s.data.schedules.is_empty() {
        empty(g, x, y, w, "예약된 작업이 없어요");
        return;
    }
    for item in s.data.schedules.iter() {
        let rect = (x, *y, w, 58.0);
        outlined(
            g,
            rect,
            if item.enabled {
                theme::surface_hover()
            } else {
                theme::surface()
            },
        );
        let kind = match item.kind.as_str() { "loop" => "반복", "cron" => "예약", _ => "타이머" };
        text(g, rect.0 + 14.0, rect.1 + 9.0, kind, 10.0, theme::accent(), true);
        let item_text = fit(g, &item.text, w - 180.0, 12.0, false);
        text(g, rect.0 + 62.0, rect.1 + 8.0, &item_text, 12.0, theme::text(), false);
        text(g, rect.0 + 14.0, rect.1 + 34.0, &format!("{} · {}", item.surface, schedule_when(item)), 10.5, theme::text_dim(), false);
        icon_button(g, s, hits, (rect.0 + rect.2 - 72.0, rect.1 + 14.0, 28.0, 28.0), if item.enabled { "minus" } else { "arrow-up" }, Target::ScheduleToggle(item.id.clone()));
        icon_button(g, s, hits, (rect.0 + rect.2 - 36.0, rect.1 + 14.0, 28.0, 28.0), "x", Target::ScheduleDelete(item.id.clone()));
        *y += 66.0;
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
    section(g, x, y, "대상 pane의 저장소", &short_path(&s.target_cwd));
    if git.no_repo {
        empty(g, x, y, w, "이 pane은 Git 저장소에 있지 않아요");
        return;
    }
    if !git.error.is_empty() {
        notice(g, x, y, w, &git.error, false);
        return;
    }
    let summary = (x, *y, w, 54.0);
    outlined(g, summary, theme::surface_hover());
    g.queue_icon("git-branch", summary.0 + 14.0, summary.1 + 18.0, 15.0, theme::accent());
    text(g, summary.0 + 38.0, summary.1 + 9.0, if git.branch.is_empty() { "—" } else { &git.branch }, 13.0, theme::text(), true);
    text(g, summary.0 + 38.0, summary.1 + 31.0, &format!("앞섬 {} · 뒤처짐 {} · +{} −{}", git.ahead, git.behind, git.insertions, git.deletions), 10.5, theme::text_dim(), false);
    *y += 66.0;
    if git.rows.is_empty() {
        empty(g, x, y, w, "변경된 파일이 없어요");
    } else {
        for row in git.rows.iter() {
            let rect = (x, *y, w, 34.0);
            let selected = s.git_selected.contains(&row.path);
            if selected || contains(rect, s.cursor) {
                round_rect(g, rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), theme::surface_hover());
            }
            checkbox(g, rect.0 + 8.0, rect.1 + 8.0, selected);
            text(g, rect.0 + 36.0, rect.1 + 9.0, &row.marker.to_string(), 10.5, status_marker_color(row.marker), true);
            let path = fit(g, &row.path, w - 70.0, 11.0, false);
            text(g, rect.0 + 58.0, rect.1 + 8.0, &path, 11.0, theme::text(), false);
            hit(g, hits, Target::GitFile(row.path.clone()), rect, false);
            *y += 36.0;
        }
        *y += 8.0;
        button(g, s, hits, (x, *y, 72.0, 30.0), "전체", Target::GitAll, false);
        button(g, s, hits, (x + 80.0, *y, 72.0, 30.0), "해제", Target::GitClear, false);
        text(g, x + 166.0, *y + 8.0, &format!("{}개 선택", s.git_selected.len()), 10.5, theme::text_dim(), false);
        *y += 42.0;
    }
    field(g, s, hits, caret, (x, *y, w, 40.0), "커밋 메시지", &s.git_message, BoardInput::GitMessage);
    *y += 50.0;
    button(g, s, hits, (x, *y, 112.0, 38.0), "커밋", Target::GitCommit, true);
    button(g, s, hits, (x + 122.0, *y, 100.0, 38.0), &format!("푸시 ↑{}", git.ahead), Target::GitPush, false);
    *y += 52.0;
}

fn paint_machines(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, caret: &mut Option<Rect>, x: f32, y: &mut f32, w: f32) {
    let ui = &s.transfer;
    let data = &s.data.transfer;
    if s.fixture { notice(g, x, y, w, "검증용 가상 세션 · 실제 이사·닫기는 실행되지 않아요", true); }
    if let Some(error) = &ui.error { notice(g, x, y, w, error, false); }
    for error in &data.errors { notice(g, x, y, w, error, false); }
    match ui.step {
        TransferStep::Select => {
            section(g, x, y, "1  세션 고르기", "선택하지 않은 세션은 현재 자리에 남아요");
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
            transfer_button(g, s, hits, (x, *y, w, 36.0), &format!("선택한 {selected}개 이사할 곳 고르기"), Target::TransferDestination, selected > 0, true);
            *y += 46.0;
            button(g, s, hits, (x, *y, (w / 2.0 - 4.0).min(136.0), 30.0), "보이는 세션 선택", Target::TransferSelectRoom, false);
            button(g, s, hits, (x + (w / 2.0).min(144.0), *y, (w / 2.0 - 4.0).min(110.0), 30.0), "선택 해제", Target::TransferClear, false);
            *y += 42.0;
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
            button(g, s, hits, (x, *y, w, 34.0), &format!("{} 셸 따로 보기 · {}개", if ui.show_shells { "접기" } else { "펼치기" }, shells.len()), Target::TransferShells, false);
            *y += 44.0;
            if ui.show_shells {
                text(g, x, *y, "셸만 닫으며, 실행 중인 학생은 닫지 않아요.", 10.5, theme::text_dim(), false);
                *y += 28.0;
                for row in shells { paint_transfer_session(g, s, hits, x, y, w, row, true); }
                transfer_button(g, s, hits, (x, *y, w, 36.0), &format!("선택한 셸 {}개 닫기 확인", ui.shell_selected.len()), Target::TransferCloseReview, !ui.shell_selected.is_empty(), false);
                *y += 48.0;
            }
        }
        TransferStep::Destination => {
            button(g, s, hits, (x, *y, 100.0, 30.0), "세션 다시 고르기", Target::TransferBack, false);
            *y += 44.0;
            section(g, x, y, "2  도착할 기기와 방", &format!("{}개 세션을 함께 보냅니다", ui.selected.len()));
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
            transfer_button(g, s, hits, (x, *y, w, 38.0), "선택 내용 확인", Target::TransferReview, valid, true);
            *y += 50.0;
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
                        ("3  이사할 내용 확인", format!("도착 · {machine_name} / {room}"), &request.sessions, "확인하고 이사")
                    }
                    TransferConfirmation::Close(ids) => ("셸 닫기 확인", "선택한 셸만 닫습니다. 실행 중인 학생은 유지돼요.".into(), ids, "확인하고 셸 닫기"),
                };
                section(g, x, y, title, "선택한 항목만 처리하며, 나머지는 그대로 남아요");
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
                button(g, s, hits, (x, *y, w, 38.0), confirm, Target::TransferConfirm, true);
                *y += 48.0;
                button(g, s, hits, (x, *y, w, 32.0), "취소하고 다시 고르기", Target::TransferBack, false);
                *y += 44.0;
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
                    button(g, s, hits, (x, *y, w, 36.0), "목록으로 돌아가기", Target::TransferBack, false);
                    *y += 48.0;
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
        outlined(g, rect, theme::surface());
        let label = fit(g, label, rect.2 - 20.0, 10.5, false);
        text(g, rect.0 + 10.0, rect.1 + (rect.3 - 12.0) / 2.0, &label, 10.5, theme::text_dim(), false);
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
    if selected || contains(rect, s.cursor) { round_rect(g, x, *y, w, rect.3, theme::radius_sm(), theme::surface_hover()); }
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
    button(g, s, hits, view, "보기", Target::TransferView(row.identity.clone()), false);
    button(g, s, hits, detail, if expanded { "접기" } else { "상세" }, Target::TransferDetail(key), false);
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
    g.rect(x, *y - 1.0, w, 1.0, theme::border());
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
    text(g, x, *y, title, 13.0, theme::text(), true);
    text(g, x, *y + 20.0, desc, 10.5, theme::text_dim(), false);
    *y += 44.0;
}

fn notice(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str, ok: bool) {
    let rect = (x, *y, w, 38.0);
    outlined(
        g,
        rect,
        theme::with_alpha(if ok { theme::success() } else { theme::danger() }, 24),
    );
    g.queue_icon(if ok { "square-check" } else { "triangle-alert" }, x + 12.0, *y + 11.0, 14.0, if ok { theme::success() } else { theme::danger() });
    let message = fit(g, message, w - 46.0, 10.5, false);
    text(g, x + 34.0, *y + 11.0, &message, 10.5, theme::text(), false);
    *y += 48.0;
}

fn empty(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str) {
    let rect = (x, *y, w, 72.0);
    outlined(g, rect, theme::surface());
    text(g, x + 16.0, *y + 26.0, message, 11.5, theme::text_dim(), false);
    *y += 82.0;
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
    let hover = contains(rect, s.cursor);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        if primary { if hover { theme::surface_active() } else { theme::accent() } } else if hover { theme::surface_active() } else { theme::surface_hover() },
    );
    let shown = fit(g, label, rect.2 - 14.0, 10.5, primary);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 10.5, primary)) / 2.0;
    text(g, tx, rect.1 + (rect.3 - 11.0) / 2.0 - 1.0, &shown, 10.5, if primary { [255, 255, 255, 255] } else { theme::text() }, primary);
    hit(g, hits, target, rect, false);
    g.hover_pointer |= hover;
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
    let mut out = String::new();
    for ch in value.chars() {
        let next = format!("{out}{ch}…");
        if g.measure_chrome_text(&next, size, bold) > width { break; }
        out.push(ch);
    }
    if out.chars().count() < value.chars().count() { out.push('…'); }
    out
}

fn status_dot(g: &mut gpu::GpuRenderer, x: f32, y: f32, row: &PaneActivity) {
    circle_rect(g, x, y, 8.0, status_color(row));
}

fn status_dot_raw(g: &mut gpu::GpuRenderer, x: f32, y: f32, status: &str) {
    let color = if status == "blocked" {
        theme::danger()
    } else if matches!(
        status,
        "working" | "building" | "waiting" | "thinking" | "compacting"
    ) {
        theme::accent()
    } else {
        theme::success()
    };
    circle_rect(g, x, y, 8.0, color);
}

fn status_color(row: &PaneActivity) -> [u8; 4] {
    if agent_needs_attention(row) {
        theme::danger()
    } else if row.done_outcome.as_deref() == Some("failed") {
        theme::danger()
    } else if agent_is_working(row) {
        theme::accent()
    } else {
        theme::success()
    }
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

fn agent_is_working(row: &PaneActivity) -> bool {
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
    let color = if checked { theme::accent() } else { theme::border() };
    round_rect(g, x, y, 16.0, 16.0, theme::radius_sm(), color);
    if checked { g.queue_icon("square-check", x + 1.0, y + 1.0, 14.0, [255, 255, 255, 255]); }
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
            Target::ToggleAgentDetail(pane) => self.board_scene.toggle_agent_detail(pane),
            Target::SavePane(pane) => {
                self.save_pane_confirmed(pane);
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

    fn save_pane_confirmed(&mut self, pane: String) {
        let (reply, receiver) = std::sync::mpsc::channel();
        let event = UserEvent::SaveSession {
            surface: Some(pane),
            reply: Some(reply),
        };
        if self.proxy.send_event(event).is_err() {
            self.board_scene.report_error("저장 요청을 보내지 못했어요");
            return;
        }
        self.board_scene
            .wait_for_gui_result(receiver, "백그라운드 저장을 시작했어요", self.proxy.clone());
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
        // 저장·이어받기는 pane 을 실제로 만드는 일이라 워커 스레드가 아니라 GUI
        // 스레드로 간다(`UserEvent` 의 reply 채널로 결과를 되받는다). 워커에 남으면
        // 만들어진 pane 을 확인할 길이 없어 성공 토스트가 거짓이 된다.
        for gui in ["save_pane_confirmed", "resume_background_in_target_room"] {
            assert!(click.contains(gui), "GUI 스레드 경로 누락: {gui}");
        }
        for forbidden in ["git_status(", "std::process::Command", "read_to_string"] {
            assert!(!click.contains(forbidden), "click에서 직접 I/O 발견: {forbidden}");
        }
    }
}
