//! 오른쪽 열 「작업」 탭 — 정리와 조율 두 모드, 그리고 권한 표.
//!
//! - 정리: 사람이 터미널을 직접 볼 때. 지금 창의 현재 작업·변경·다음 일·막힘·검증을 한 칸에 모은다.
//!   보여 주기만 한다 — 창에 입력을 넣거나 학생을 멈추는 단추가 없다.
//! - 조율: 나쵸에게 맡길 때. 할 일 판(`work.rs`)과 같은 목록을 좁게 — 사람 차례가 맨 위, 누르면 그
//!   학생의 창이나 작업판의 그 일로 간다.
//!
//! 모드는 나쵸가 정본이고(`crate::work_mode`) 사람이 이 탭의 단추를 누를 때만 바꾼다. 무엇을 보고
//! 있는지·어느 창구로 말하는지로 짐작하지 않는다. 모드를 바꿔도 권한은 그대로다.

use super::work::{work_items, Lane, WorkItem, WorkKey};
use super::*;
use crate::nacho_tasks::TaskBook;
use crate::work_mode::{Capabilities, ModeState, Reach, WorkMode};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const REFRESH_MS: u64 = 3_000;
const CAPS_MS: u64 = 30_000;
const RESTART_FACTS_MS: u64 = 10_000;

/// 지금 창 — 정리 모드가 보는 대상. 창 번호는 이 기기 안에서만 뜻이 있다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Focus {
    pub(crate) surface: String,
    pub(crate) cwd: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SideData {
    overview: Arc<OverviewData>,
    tasks: Arc<TaskBook>,
    git: Option<GitSnapshot>,
    report: Option<serde_json::Value>,
    mode: Option<ModeState>,
    mode_reach: Option<Reach>,
    caps: Option<Result<Capabilities, Reach>>,
    caps_at: Option<Instant>,
    notice: Option<String>,
    /// 가상 보드 — 실제 작업이 아니다.
    fixture: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SideHit {
    Mode(WorkMode),
    Local(String),
    Board(WorkKey),
    OpenBoard,
}

pub(crate) struct WorkSide {
    data: Arc<SideData>,
    mailbox: Arc<Mutex<Option<SideData>>>,
    busy: Arc<AtomicBool>,
    last_refresh: Option<Instant>,
    focus: Focus,
    /// 누른 모드 — 나쵸가 답할 때까지 단추를 다시 받지 않는다.
    writing: Option<WorkMode>,
    queued_write: Option<(WorkMode, String, String)>,
    cache: Option<ModeState>,
    refusals: Option<(Vec<String>, Instant)>,
    hits: Vec<(Rect, SideHit)>,
    pub(crate) scroll: f32,
    scroll_max: f32,
}

impl Default for WorkSide {
    fn default() -> Self {
        Self {
            data: Arc::default(),
            mailbox: Arc::default(),
            busy: Arc::default(),
            last_refresh: None,
            focus: Focus::default(),
            writing: None,
            queued_write: None,
            cache: crate::work_mode::read_cache(),
            refusals: None,
            hits: Vec::new(),
            scroll: 0.0,
            scroll_max: 0.0,
        }
    }
}

/// 화면에 세울 모드와 그 출처. 나쵸 값이 먼저, 못 닿으면 마지막으로 확인한 값, 그것도 없으면 없음 —
/// 기본값을 지어내 고른 척하지 않는다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeOrigin {
    Nacho,
    Cache,
    Unknown,
}

pub(crate) fn shown_mode(nacho: Option<&ModeState>, cache: Option<&ModeState>) -> (Option<WorkMode>, ModeOrigin) {
    match (nacho, cache) {
        (Some(state), _) => (Some(state.mode), ModeOrigin::Nacho),
        (None, Some(state)) => (Some(state.mode), ModeOrigin::Cache),
        (None, None) => (None, ModeOrigin::Unknown),
    }
}

/// 단추를 눌러 쓸 수 있는가. 못 쓰면 그 이유(단추 옆에 적는다).
pub(crate) fn write_block(reach: Option<&Reach>, nacho: Option<&ModeState>, writing: bool) -> Option<String> {
    if writing {
        return Some("나쵸에 적는 중이에요".into());
    }
    match (reach, nacho) {
        (Some(reach), _) => Some(reach.sentence()),
        (None, None) => Some("나쵸의 지금 모드를 아직 확인하지 못했어요".into()),
        (None, Some(_)) => None,
    }
}

/// 정리 칸 하나 — 이름과 한두 줄, 그리고 색.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) label: &'static str,
    pub(crate) value: String,
    pub(crate) tone: Tone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Plain,
    Quiet,
    Alert,
    Good,
}

fn report_text(report: Option<&serde_json::Value>, key: &str) -> String {
    report.and_then(|r| r.get(key)).and_then(|v| v.as_str()).map(|s| board_plain(s, 600)).unwrap_or_default()
}

/// 정리 모드의 다섯 칸. 모르는 칸은 모른다고 적는다 — 추측으로 채우지 않는다.
pub(super) fn organize_lines(row: Option<&OverviewPane>, git: Option<&GitSnapshot>, report: Option<&serde_json::Value>) -> Vec<Line> {
    let line = |label, value: String, tone| Line { label, value, tone };
    let report_status = report_text(report, "status");
    let now = match row {
        Some(row) if !row.request.is_empty() => board_plain(&row.request, 400),
        Some(row) if !row.title.is_empty() => board_plain(&row.title, 400),
        Some(_) => "요청을 아직 확인하지 못했어요".into(),
        None => "보드에 이 창이 아직 없어요".into(),
    };
    let progress = row.map(|r| board_plain(&r.progress, 400)).filter(|p| !p.is_empty());
    let mut lines = vec![line("현재 작업", match progress {
        Some(progress) => format!("{now}\n{progress}"),
        None => now,
    }, Tone::Plain)];

    let mut changes = Vec::new();
    match git {
        Some(git) if git.no_repo => changes.push("git 저장소가 아니에요".to_string()),
        Some(git) if !git.error.is_empty() => changes.push(format!("git 을 읽지 못했어요 ({})", board_plain(&git.error, 80))),
        Some(git) if git.rows.is_empty() => changes.push(format!("{} · 바뀐 파일 없음", if git.branch.is_empty() { "브랜치 미확인" } else { &git.branch })),
        Some(git) => {
            changes.push(format!("{} · 파일 {}개 · +{} −{}", if git.branch.is_empty() { "브랜치 미확인" } else { &git.branch }, git.rows.len(), git.insertions, git.deletions));
            for row in git.rows.iter().take(4) {
                changes.push(format!("{} {}", row.marker, row.path));
            }
            if git.rows.len() > 4 {
                changes.push(format!("외 {}개", git.rows.len() - 4));
            }
        }
        None => changes.push("폴더를 아직 확인하지 못했어요".into()),
    }
    let reported = report_text(report, "changed");
    if !reported.is_empty() {
        changes.push(format!("보고: {reported}"));
    }
    lines.push(line("변경", changes.join("\n"), Tone::Plain));

    let next = report_text(report, "next");
    lines.push(if next.is_empty() {
        line("다음 일", "적힌 다음 일이 없어요".into(), Tone::Quiet)
    } else {
        line("다음 일", next.clone(), Tone::Plain)
    });

    let waiting = row.filter(|r| overview_status(r).1 == 0 && r.done_outcome.as_deref() != Some("failed"));
    let blocked = if let Some(row) = waiting {
        let why = [row.progress.as_str(), row.status_reason.as_deref().unwrap_or("")].into_iter()
            .find(|s| !s.is_empty()).map(|s| board_plain(s, 200)).unwrap_or_else(|| "무엇을 기다리는지 확인하지 못했어요".into());
        line("막힘", format!("사람 답을 기다려요 · {why}"), Tone::Alert)
    } else if row.is_some_and(|r| r.status == "blocked") || matches!(report_status.as_str(), "blocked" | "needs_restart" | "needs_approval") {
        let why = if next.is_empty() { "막힌 까닭이 적혀 있지 않아요".to_string() } else { next };
        line("막힘", format!("{} · {why}", match report_status.as_str() {
            "needs_restart" => "재시작이 있어야 이어져요",
            "needs_approval" => "승인이 있어야 이어져요",
            _ => "막혔어요",
        }), Tone::Alert)
    } else if row.is_some_and(|r| r.freshness != "fresh") {
        line("막힘", "창 관측이 오래됐어요 — 지금 막혔는지 모릅니다".into(), Tone::Quiet)
    } else {
        line("막힘", "없음".into(), Tone::Quiet)
    };
    lines.push(blocked);

    let tests = report_text(report, "tests");
    let verify = match (row.and_then(|r| r.done_outcome.as_deref()), tests.is_empty()) {
        (Some("failed"), _) => line("검증", if tests.is_empty() { "실패 보고".into() } else { format!("실패 보고 · {tests}") }, Tone::Alert),
        (_, false) => line("검증", format!("학생이 적은 검사 · {tests}"), Tone::Plain),
        (Some("succeeded"), true) => line("검증", "완료 보고만 있어요 · 검증 기록 없음".into(), Tone::Quiet),
        _ => line("검증", "검증 기록 없음".into(), Tone::Quiet),
    };
    lines.push(verify);
    lines
}

/// 이 창의 마지막 나쵸 보고. 창 열쇠가 같은 봉투만 — 창 번호는 앱을 껐다 켜면 다른 창에 다시 쓰인다.
pub(crate) fn latest_report(dir: &std::path::Path, surface_key: &str) -> Option<serde_json::Value> {
    if surface_key.is_empty() {
        return None;
    }
    let mut best: Option<(u64, serde_json::Value)> = None;
    for sub in ["new", "done"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else { continue };
        for entry in entries.flatten().take(2000) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(value) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else {
                continue;
            };
            if value.get("surface_key").and_then(|v| v.as_str()) != Some(surface_key) {
                continue;
            }
            let at = value.get("at_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
                best = Some((at, value));
            }
        }
    }
    best.map(|(_, value)| value)
}

/// 권한 표의 한 줄.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PermRow {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) tone: Tone,
}

/// 권한 표 — 나쵸가 준 규칙과 지금 실제로 되는 것. 나쵸 표가 없으면 규칙을 지어내지 않는다.
pub(crate) fn permission_rows(caps: Option<&Result<Capabilities, Reach>>, restart: Option<&[String]>) -> Vec<PermRow> {
    let row = |label: &str, value: String, tone| PermRow { label: label.into(), value, tone };
    let mut rows = Vec::new();
    match caps {
        Some(Ok(caps)) if !caps.tiers.is_empty() => {
            for tier in &caps.tiers {
                let name = if tier.label.is_empty() { tier.tier.clone() } else { tier.label.clone() };
                let mut lines = vec![tier.rule.clone()].into_iter().filter(|r| !r.is_empty()).collect::<Vec<_>>();
                for action in &tier.actions {
                    lines.push(if action.how.is_empty() { format!("· {}", action.label) } else { format!("· {} — {}", action.label, action.how_word()) });
                }
                rows.push(row(&name, lines.join("\n"), Tone::Plain));
            }
        }
        Some(Ok(_)) => rows.push(row("규칙", "이 나쵸는 권한 표를 아직 주지 않아요 — 규칙을 여기서 확인할 수 없어요".into(), Tone::Quiet)),
        Some(Err(reach)) => rows.push(row("규칙", reach.sentence(), Tone::Quiet)),
        None => rows.push(row("규칙", "나쵸 권한 표를 확인하고 있어요".into(), Tone::Quiet)),
    }
    rows.push(row("무제한 모드", match caps {
        Some(Ok(caps)) if caps.unlimited_mode == Some(true) => "나쵸가 있다고 말하지만 이 앱은 그런 모드를 세우지 않아요".into(),
        _ => "없어요 — 되돌리기 어려운 일은 늘 한 번 확인해요".into(),
    }, Tone::Quiet));
    if let Some(Ok(caps)) = caps.filter(|c| c.as_ref().is_ok_and(|c| !c.notes.is_empty())) {
        rows.push(row("나쵸 안내", caps.notes.iter().map(|n| format!("· {n}")).collect::<Vec<_>>().join("\n"), Tone::Quiet));
    }
    let approvals = match caps {
        Some(Ok(caps)) => caps.approvals.as_ref(),
        _ => None,
    };
    rows.push(match approvals {
        Some(a) if a.enabled => {
            let actions = if a.http_actions.is_empty() { String::new() } else { format!(" · {}", a.http_actions.join("·")) };
            row("나쵸 승인 창구", format!("켜짐{actions}"), Tone::Good)
        }
        Some(_) => row("나쵸 승인 창구", "꺼짐 — 승인이 필요한 일은 지금 실행되지 않아요".into(), Tone::Quiet),
        None => row("나쵸 승인 창구", "확인하지 못했어요".into(), Tone::Quiet),
    });
    let via = approvals.map(|a| a.decide_via.as_str()).filter(|v| !v.is_empty());
    rows.push(row("앱 안 승인", match via {
        Some("owner_dm_button") => "지원 안 함 — 주인 DM 의 확인 단추로만 결정해요".into(),
        Some(other) => format!("지원 안 함 — {other} 로만 결정해요"),
        None => "지원 안 함 — 앱에는 결정하는 창구가 없어요".into(),
    }, Tone::Quiet));
    rows.push(match restart {
        None => row("이 기기 앱 재시작", "확인하고 있어요".into(), Tone::Quiet),
        Some([]) => row("이 기기 앱 재시작", format!("창구 있음(판 {}) · 지금 막는 사유 없음 · 실행은 승인 뒤", kasa_socket::app_restart::CAPABILITY), Tone::Plain),
        Some(reasons) => row("이 기기 앱 재시작", format!("창구 있음(판 {}) · 지금은 거부: {}", kasa_socket::app_restart::CAPABILITY, reasons.join(" / ")), Tone::Quiet),
    });
    rows
}

fn collect(
    backend: Option<Arc<dyn Backend>>,
    focus: &Focus,
    mode: Option<WorkMode>,
    previous: &SideData,
    write: Option<(WorkMode, String, String)>,
) -> SideData {
    let mut data = SideData { caps: previous.caps.clone(), caps_at: previous.caps_at, ..Default::default() };
    let fetched = match write {
        Some((mode, rev, nonce)) => match crate::work_mode::post_mode(mode, &rev, &nonce) {
            Err(Reach::Failed(word)) if word == "stale_rev" => {
                data.notice = Some("다른 곳에서 먼저 바뀌어 나쵸 값을 다시 읽었어요".into());
                crate::work_mode::fetch_mode()
            }
            other => other,
        },
        None => crate::work_mode::fetch_mode(),
    };
    match fetched {
        Ok(state) => {
            crate::work_mode::write_cache(&state);
            data.mode = Some(state);
        }
        Err(reach) => data.mode_reach = Some(reach),
    }
    if data.caps_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(CAPS_MS)) {
        data.caps = Some(crate::work_mode::fetch_capabilities());
        data.caps_at = Some(Instant::now());
    }
    let shown = data.mode.as_ref().map(|s| s.mode).or(mode);
    let organize = shown == Some(WorkMode::Organize);
    let probe = board_probe_mode(cfg!(debug_assertions), board_fixture_requested(), board_fixture_active());
    if probe == BoardProbeMode::Rejected {
        data.overview = Arc::new(OverviewData { error: Some("보드 검증 설정 오류예요. 디버그 빌드와 분리된 임시 상태 경로가 필요합니다.".into()), ..Default::default() });
    } else if probe == BoardProbeMode::Fixture {
        #[cfg(debug_assertions)]
        {
            data.overview = Arc::new(board_fixture());
            data.fixture = true;
        }
    } else if let Some(backend) = backend {
        let scope = if organize { "local" } else { "all" };
        data.overview = Arc::new(
            backend.collab_snapshot(&serde_json::json!({"scope": scope}))
                .map_err(|e| e.to_string())
                .and_then(overview_from_value)
                .unwrap_or_else(|error| OverviewData { error: Some(format!("보드를 읽지 못했어요 · {error}")), ..Default::default() }),
        );
    }
    if organize {
        data.git = Some(collect_git(&focus.cwd));
        let key = focus_row(&data.overview, focus).map(|r| r.address.surface_key.clone()).unwrap_or_default();
        data.report = kasa_socket::nacho_inbox::inbox_root().ok().and_then(|dir| latest_report(&dir, &key));
        data.tasks = previous.tasks.clone();
    } else {
        data.tasks = Arc::new(crate::nacho_tasks::refresh(&previous.tasks));
    }
    data
}

fn focus_row<'a>(data: &'a OverviewData, focus: &Focus) -> Option<&'a OverviewPane> {
    if focus.surface.is_empty() {
        return None;
    }
    let local = data.local_machine_id.as_deref();
    data.panes.iter().find(|row| row.address.surface_id == focus.surface && local.is_none_or(|m| row.address.machine_id == m))
}

impl WorkSide {
    fn shown(&self) -> (Option<WorkMode>, ModeOrigin) {
        shown_mode(self.data.mode.as_ref(), self.cache.as_ref())
    }

    /// 검증 훅이 누를 자리 — 마지막 프레임에 그 단추가 눌릴 수 있게 그려졌을 때만.
    pub(crate) fn mode_button(&self, mode: WorkMode) -> Option<Rect> {
        self.hits.iter().find(|(_, hit)| *hit == SideHit::Mode(mode)).map(|(rect, _)| *rect)
    }

    pub(crate) fn nacho_mode(&self) -> Option<WorkMode> {
        self.data.mode.as_ref().map(|state| state.mode)
    }
}

impl App {
    /// 탭이 보일 때만 워커를 깨운다.
    pub(crate) fn pump_work_side(&mut self) {
        if self.info.tab != state::SideTab::Work || !self.git.col_visible {
            return;
        }
        if let Some(next) = self.work_side.mailbox.lock().ok().and_then(|mut g| g.take()) {
            if next.mode.is_some() {
                self.work_side.cache = next.mode.clone();
            }
            if let Some(notice) = next.notice.clone() {
                self.set_toast(notice);
            }
            self.work_side.data = Arc::new(next);
            self.work_side.writing = None;
            self.chrome_dirty = true;
        }
        if self.work_side.refusals.as_ref().is_none_or(|(_, at)| at.elapsed() >= Duration::from_millis(RESTART_FACTS_MS)) {
            let facts = self.restart_facts();
            let reasons = kasa_socket::app_restart::refusals(&facts.machine_id, &facts, facts.observed_at_ms)
                .iter().map(|r| r.message()).collect();
            self.work_side.refusals = Some((reasons, Instant::now()));
            self.chrome_dirty = true;
        }
        let focus = {
            let ws = self.ws.lock().unwrap();
            ws.active_pane.clone().map(|pane| ws.active_tab_pid(&pane))
        }.map(|surface| Focus {
            cwd: self.pane_current_cwd(&surface).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            surface,
        }).unwrap_or_default();
        let moved = focus != self.work_side.focus;
        self.work_side.focus = focus.clone();
        let due = self.work_side.last_refresh.is_none_or(|t| t.elapsed() >= Duration::from_millis(REFRESH_MS));
        let write = self.work_side.queued_write.take();
        if !(due || moved || write.is_some()) {
            return;
        }
        if self.work_side.busy.swap(true, Ordering::Relaxed) {
            if write.is_some() {
                self.work_side.queued_write = write;
            }
            return;
        }
        self.work_side.last_refresh = Some(Instant::now());
        let backend = self.socket_backend.clone().map(|b| b as Arc<dyn Backend>);
        let (mode, _) = self.work_side.shown();
        let previous = self.work_side.data.clone();
        let (mailbox, busy) = (self.work_side.mailbox.clone(), self.work_side.busy.clone());
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let data = collect(backend, &focus, mode, &previous, write);
            if let Ok(mut slot) = mailbox.lock() {
                *slot = Some(data);
            }
            busy.store(false, Ordering::Relaxed);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 작업 탭의 클릭. 처리했으면 `true`.
    pub(crate) fn work_side_click(&mut self, x: f32, y: f32) -> bool {
        let Some(target) = self.work_side.hits.iter().rev().find(|(r, _)| contains(*r, (x, y))).map(|(_, t)| t.clone()) else {
            return false;
        };
        match target {
            SideHit::Mode(mode) => {
                let blocked = write_block(self.work_side.data.mode_reach.as_ref(), self.work_side.data.mode.as_ref(), self.work_side.writing.is_some());
                let current = self.work_side.data.mode.as_ref();
                if blocked.is_some() || current.is_some_and(|s| s.mode == mode) {
                    return true;
                }
                let rev = current.map(|s| s.rev.clone()).unwrap_or_default();
                self.work_side.writing = Some(mode);
                self.work_side.queued_write = Some((mode, rev, crate::work_mode::new_nonce()));
                self.work_side.scroll = 0.0;
            }
            SideHit::Local(surface) => {
                self.focus_surface(&surface);
            }
            SideHit::Board(key) => {
                if self.open_board_room() {
                    self.board_scene.set_tab(BoardTab::Work);
                    self.board_scene.select_work(key);
                    self.request_native_board_refresh();
                }
            }
            SideHit::OpenBoard => {
                if self.open_board_room() {
                    self.board_scene.set_tab(BoardTab::Work);
                    self.request_native_board_refresh();
                }
            }
        }
        true
    }
}

const PAD_L: f32 = 14.0;
const PAD_R: f32 = 12.0;

fn tone_color(tone: Tone) -> [u8; 4] {
    match tone {
        Tone::Plain => theme::text(),
        Tone::Quiet => theme::text_dim(),
        Tone::Alert => theme::danger(),
        Tone::Good => theme::success(),
    }
}

/// 여러 줄 글 — 줄바꿈을 지키고 줄마다 폭에 맞춰 접는다.
fn wrapped(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, value: &str, size: f32, color: [u8; 4], max_lines: usize) {
    let mut left = max_lines;
    for part in value.split('\n') {
        if left == 0 {
            break;
        }
        let lines = board_wrap(part, w, left, |line| g.measure_chrome_text(line, size, false));
        left = left.saturating_sub(lines.len().max(1));
        for line in lines {
            text(g, x, *y, &line, size, color, false);
            *y += size + 7.0;
        }
    }
}

fn head(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str) {
    head_weight(g, x, y, w, label, true);
}

/// 이름·상태가 끼는 머리줄은 보통 굵기 — 굵은 한글은 글자마다 굵기가 갈려 보이는 자리가 있다(`docs/design.md`).
fn head_weight(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str, bold: bool) {
    let label = fit(g, label, w, 11.0, bold);
    text(g, x, *y, &label, 11.0, theme::text_dim(), bold);
    *y += 20.0;
    divider(g, x, *y, w);
    *y += 10.0;
}

/// 테두리만 있는 단추(`docs/design.md` 4장). 고른 것은 강조색 테두리·글자, 못 누르면 흐리게.
fn outline_button(g: &mut gpu::GpuRenderer, cursor: (f32, f32), rect: Rect, label: &str, selected: bool, enabled: bool) -> bool {
    let hover = enabled && contains(rect, cursor);
    let stroke = if selected { theme::accent() } else if hover { theme::text_dim() } else { theme::with_alpha(theme::border(), if enabled { 255 } else { 140 }) };
    g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, theme::radius_md().min(5.0), 1.0, stroke);
    let color = if selected { theme::accent() } else if enabled { theme::text() } else { theme::text_mute() };
    let shown = fit(g, label, rect.2 - 12.0, 11.5, selected);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, selected)) / 2.0;
    text(g, tx, rect.1 + (rect.3 - 12.0) / 2.0 - 1.0, &shown, 11.5, color, selected);
    g.hover_pointer |= hover;
    hover
}

pub(crate) fn paint_work_side(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    side: &mut WorkSide,
    col_x: f32,
    col_w: f32,
    top: f32,
    bottom: f32,
) {
    side.hits.clear();
    let x = col_x + PAD_L;
    let w = (col_w - PAD_L - PAD_R).max(40.0);
    let view_h = (bottom - top).max(0.0);
    side.scroll = side.scroll.clamp(0.0, side.scroll_max);
    g.push_clip(col_x, top, col_w, view_h);
    let start = top + 4.0;
    let mut y = start - side.scroll;
    let data = side.data.clone();
    let (mode, origin) = side.shown();

    // 모드 단추 둘. 조작 높이는 설정 화면과 같은 26.
    let caps = data.caps.as_ref().and_then(|c| c.as_ref().ok());
    let block = write_block(data.mode_reach.as_ref(), data.mode.as_ref(), side.writing.is_some());
    let bw = ((w - 6.0) / 2.0).min(120.0);
    for (index, choice) in WorkMode::ALL.into_iter().enumerate() {
        let rect = (x + index as f32 * (bw + 6.0), y, bw, 26.0);
        let label = caps.and_then(|c| c.modes.iter().find(|m| m.mode == choice)).map(|m| m.label.clone()).unwrap_or_else(|| choice.label().into());
        let selected = side.writing.map_or(mode == Some(choice), |w| w == choice);
        outline_button(g, cursor, rect, &label, selected, block.is_none());
        if block.is_none() {
            side.hits.push((rect, SideHit::Mode(choice)));
        }
    }
    y += 34.0;
    let origin_note = match origin {
        ModeOrigin::Nacho => None,
        ModeOrigin::Cache => Some("마지막으로 확인한 값이에요".to_string()),
        ModeOrigin::Unknown => Some("모드를 아직 모릅니다 — 고른 척하지 않아요".to_string()),
    };
    if let Some(chosen) = mode {
        let info = caps.and_then(|c| c.modes.iter().find(|m| m.mode == chosen));
        match info.filter(|m| !m.summary.is_empty() || !m.effects.is_empty()) {
            Some(info) => {
                if !info.summary.is_empty() {
                    wrapped(g, x, &mut y, w, &info.summary, 11.0, theme::text(), 3);
                }
                for effect in &info.effects {
                    wrapped(g, x, &mut y, w, &format!("· {effect}"), 10.5, theme::text_dim(), 2);
                }
            }
            None => wrapped(g, x, &mut y, w, chosen.blurb(), 10.5, theme::text_dim(), 4),
        }
    }
    let mut notes: Vec<String> = origin_note.into_iter().collect();
    notes.extend(block.filter(|_| side.writing.is_none()));
    notes.push("모드를 바꿔도 권한이 늘거나 도는 일이 멈추지 않아요".into());
    for note in notes {
        wrapped(g, x, &mut y, w, &note, 10.5, theme::text_mute(), 3);
    }
    y += 10.0;

    if data.fixture {
        wrapped(g, x, &mut y, w, "검증용 가상 보드 · 실제 작업이 아니에요", 10.5, theme::text_dim(), 2);
    }
    if let Some(error) = &data.overview.error {
        wrapped(g, x, &mut y, w, error, 10.5, theme::danger(), 3);
    }
    match mode {
        Some(WorkMode::Organize) => paint_organize(g, side, &data, x, &mut y, w),
        _ => paint_coordinate(g, cursor, side, &data, x, &mut y, w),
    }
    y += 6.0;
    head(g, x, &mut y, w, "권한");
    let restart = side.refusals.as_ref().map(|(r, _)| r.as_slice());
    for row in permission_rows(data.caps.as_ref(), restart) {
        let label = fit(g, &row.label, w, 11.0, false);
        text(g, x, y, &label, 11.0, theme::text_dim(), false);
        y += 18.0;
        // 권한 목록은 자르지 않는다 — 가려진 줄은 「없다」로 읽힌다.
        wrapped(g, x, &mut y, w, &row.value, 11.0, tone_color(row.tone), 40);
        y += 4.0;
    }
    let content_h = y + side.scroll - start + 12.0;
    side.scroll_max = (content_h - view_h).max(0.0);
    g.pop_clip();
    side.hits.retain(|(r, _)| r.1 + r.3 > top && r.1 < bottom);
}

fn paint_organize(g: &mut gpu::GpuRenderer, side: &WorkSide, data: &SideData, x: f32, y: &mut f32, w: f32) {
    let row = focus_row(&data.overview, &side.focus);
    let who = row.map(|r| {
        let name = r.character.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| r.address.surface_id.clone());
        let (state, _) = overview_status(r);
        format!("지금 창 · {name} · {state}")
    }).unwrap_or_else(|| format!("지금 창 · {}", if side.focus.surface.is_empty() { "없음" } else { &side.focus.surface }));
    head_weight(g, x, y, w, &who, false);
    for line in organize_lines(row, data.git.as_ref(), data.report.as_ref()) {
        text(g, x, *y, line.label, 11.0, theme::text_dim(), false);
        *y += 18.0;
        wrapped(g, x, y, w, &line.value, 11.5, tone_color(line.tone), 6);
        *y += 6.0;
    }
    let observed = data.overview.observed_at_ms;
    let mut source = vec![match row {
        Some(r) => format!("창 관측 {}", board_relative_time(observed, r.observed_at_ms)),
        None => "창 관측 없음".into(),
    }];
    if let Some(at) = data.report.as_ref().and_then(|r| r.get("at_ms")).and_then(|v| v.as_u64()) {
        source.push(format!("학생 보고 {}", board_relative_time(kasa_socket::board::now_ms(), at)));
    }
    wrapped(g, x, y, w, &source.join(" · "), 10.5, theme::text_mute(), 2);
    *y += 8.0;
}

fn paint_coordinate(g: &mut gpu::GpuRenderer, cursor: (f32, f32), side: &mut WorkSide, data: &SideData, x: f32, y: &mut f32, w: f32) {
    let items = work_items(&data.overview, &data.tasks);
    let count = |lane| items.iter().filter(|i| i.lane == lane).count();
    let summary = format!("사람 차례 {} · 진행 {} · 검증 {} · 완료 {}", count(Lane::Answer), count(Lane::Progress), count(Lane::Verify), count(Lane::Done));
    wrapped(g, x, y, w, &summary, 11.0, theme::text_dim(), 2);
    *y += 4.0;
    let local = data.overview.local_machine_id.clone();
    for (lane, title, limit) in [(Lane::Answer, "사람 차례", 6usize), (Lane::Progress, "진행", 4), (Lane::Verify, "검증", 3), (Lane::Done, "완료", 3)] {
        let lane_items: Vec<&WorkItem> = items.iter().filter(|i| i.lane == lane).collect();
        if lane_items.is_empty() && lane != Lane::Answer {
            continue;
        }
        head(g, x, y, w, &format!("{title} {}", lane_items.len()));
        if lane_items.is_empty() {
            wrapped(g, x, y, w, "지금 답할 것이 없어요", 11.0, theme::text_dim(), 1);
        }
        for item in lane_items.iter().take(limit) {
            let start = *y;
            let row_w = w;
            let hover = contains((x - 4.0, start - 3.0, row_w + 8.0, 40.0), cursor);
            if hover {
                round_rect(g, x - 4.0, start - 3.0, row_w + 8.0, 40.0, theme::radius_md().min(5.0), theme::surface_hover());
            }
            let state_color = if lane == Lane::Answer { theme::danger() } else if lane == Lane::Progress { theme::accent() } else { theme::text_dim() };
            let state_label = match item.finish {
                Some(finish) => finish.label().to_string(),
                None => item.state_label.clone(),
            };
            let sw = g.measure_chrome_text(&state_label, 10.5, false).min(w * 0.4);
            let title = fit(g, &board_plain(&item.title, 200), (w - sw - 10.0).max(30.0), 12.0, false);
            text(g, x, start, &title, 12.0, theme::text(), false);
            let state_label = fit(g, &state_label, sw + 1.0, 10.5, false);
            let state_x = x + w - g.measure_chrome_text(&state_label, 10.5, false);
            text(g, state_x, start + 1.0, &state_label, 10.5, state_color, false);
            let mut meta = Vec::new();
            meta.extend(item.student.clone());
            if !item.machine_id.is_empty() {
                meta.push(data.overview.sources.iter().find(|s| s.machine_id == item.machine_id).map(|s| s.label.clone()).filter(|l| !l.is_empty())
                    .unwrap_or_else(|| format!("기기 {}…", item.machine_id.chars().take(6).collect::<String>())));
            }
            meta.push(item.project.clone());
            meta.push(board_relative_time(data.overview.observed_at_ms.max(data.tasks.checked_at_ms()), item.observed_at_ms));
            if item.stale {
                meta.push("오래된 정보".into());
            }
            let meta = fit(g, &meta.join(" · "), w, 10.5, false);
            text(g, x, start + 18.0, &meta, 10.5, theme::text_dim(), false);
            g.hover_pointer |= hover;
            let pane = item.pane_id.as_ref().and_then(|id| data.overview.panes.iter().find(|r| &r.id == id));
            let target = match pane {
                Some(row) if local.as_deref() == Some(row.address.machine_id.as_str()) && row.freshness == "fresh" && !data.fixture => {
                    SideHit::Local(row.address.surface_id.clone())
                }
                _ => SideHit::Board(item.key.clone()),
            };
            side.hits.push(((x - 4.0, start - 3.0, row_w + 8.0, 40.0), target));
            *y = start + 44.0;
        }
        if lane_items.len() > limit {
            wrapped(g, x, y, w, &format!("외 {}개 · 작업판에서 보기", lane_items.len() - limit), 10.5, theme::text_mute(), 1);
        }
        *y += 6.0;
    }
    let rect = (x, *y, w.min(200.0), 26.0);
    outline_button(g, cursor, rect, "작업판 크게 보기", false, true);
    side.hits.push((rect, SideHit::OpenBoard));
    *y += 34.0;
    let book = &data.tasks;
    let note = match (book.source(), book.error()) {
        (crate::nacho_tasks::BookSource::Live, None) => format!("나쵸 장부 작업 {}개", book.tasks().len()),
        (crate::nacho_tasks::BookSource::Live, Some(error)) => format!("나쵸 장부 · {error} · 창 관측만 보여요"),
        (crate::nacho_tasks::BookSource::Fixture, _) => "검증용 가상 장부".into(),
        (crate::nacho_tasks::BookSource::Unasked, _) => "나쵸 장부를 확인하고 있어요".into(),
    };
    wrapped(g, x, y, w, &note, 10.5, theme::text_mute(), 2);
    *y += 6.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(mode: WorkMode, rev: &str) -> ModeState {
        ModeState { mode, rev: rev.into(), changed_at_ms: 1, changed_by: "app:desktop".into() }
    }

    #[test]
    fn nacho_decides_the_mode_and_nothing_is_guessed() {
        let nacho = state(WorkMode::Organize, "3");
        let cache = state(WorkMode::Coordinate, "2");
        assert_eq!(shown_mode(Some(&nacho), Some(&cache)), (Some(WorkMode::Organize), ModeOrigin::Nacho));
        assert_eq!(shown_mode(None, Some(&cache)), (Some(WorkMode::Coordinate), ModeOrigin::Cache));
        assert_eq!(shown_mode(None, None), (None, ModeOrigin::Unknown), "모르면 고른 척하지 않는다");
    }

    #[test]
    fn the_buttons_only_work_when_nacho_answered() {
        let nacho = state(WorkMode::Coordinate, "3");
        assert_eq!(write_block(None, Some(&nacho), false), None);
        assert!(write_block(Some(&Reach::NoKey), None, false).unwrap().contains("키가 없어요"), "키 없는 기기는 읽기만");
        assert!(write_block(Some(&Reach::OldNacho), None, false).unwrap().contains("모드를 몰라요"));
        assert!(write_block(None, None, false).is_some(), "나쵸 값을 모르면 쓰지 않는다");
        assert!(write_block(None, Some(&nacho), true).is_some(), "누른 뒤 답을 받기 전엔 다시 안 받는다");
    }

    fn pane(status: &str, attention: Option<&str>, outcome: Option<&str>) -> OverviewPane {
        serde_json::from_value(serde_json::json!({
            "id": "m/%3", "address": {"machine_id": "m", "surface_key": "k3", "surface_id": "%3"},
            "machine_label": "미니", "room_label": "kasaterm · ~/kasaterm", "character": "치나츠", "harness": "claude",
            "title": "작업 탭", "request": "작업 탭을 만든다", "progress": "검사를 돌리는 중", "status": status,
            "attention_kind": attention, "done_outcome": outcome, "observed_at_ms": 10, "freshness": "fresh"
        })).unwrap()
    }

    #[test]
    fn organize_lines_say_what_is_known_and_admit_what_is_not() {
        let git = GitSnapshot { branch: "main".into(), rows: vec![GitRow { path: "a.rs".into(), marker: 'M' }], insertions: 3, deletions: 1, ..Default::default() };
        let lines = organize_lines(Some(&pane("working", None, None)), Some(&git), None);
        let labels: Vec<&str> = lines.iter().map(|l| l.label).collect();
        assert_eq!(labels, vec!["현재 작업", "변경", "다음 일", "막힘", "검증"]);
        assert!(lines[0].value.contains("작업 탭을 만든다") && lines[0].value.contains("검사를 돌리는 중"));
        assert!(lines[1].value.contains("main · 파일 1개 · +3 −1") && lines[1].value.contains("M a.rs"));
        assert_eq!(lines[2].value, "적힌 다음 일이 없어요");
        assert_eq!(lines[3].value, "없음");
        assert_eq!(lines[4].value, "검증 기록 없음");

        let waiting = organize_lines(Some(&pane("waiting", Some("permission"), None)), None, None);
        assert_eq!(waiting[3].tone, Tone::Alert, "승인 대기는 막힘으로");
        let idle = organize_lines(Some(&pane("waiting", Some("idle"), None)), None, None);
        assert_eq!(idle[3].value, "없음", "방치는 사람을 부르지 않는다");
        let done = organize_lines(Some(&pane("idle", None, Some("succeeded"))), None, None);
        assert!(done[4].value.contains("검증 기록 없음"), "자기 완료 보고는 검증이 아니다");

        let report = serde_json::json!({"status": "blocked", "next": "키를 받아야 이어진다", "tests": "cargo test 12 통과", "changed": "a.rs"});
        let reported = organize_lines(Some(&pane("idle", None, None)), None, Some(&report));
        assert_eq!(reported[2].value, "키를 받아야 이어진다");
        assert!(reported[3].value.contains("막혔어요") && reported[3].tone == Tone::Alert);
        assert!(reported[4].value.contains("cargo test 12 통과"));
        assert!(reported[1].value.contains("보고: a.rs"));
        assert_eq!(organize_lines(None, None, None)[0].value, "보드에 이 창이 아직 없어요");
    }

    #[test]
    fn only_a_report_from_this_very_pane_is_used() {
        let dir = std::env::temp_dir().join(format!("work-side-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("done")).unwrap();
        std::fs::create_dir_all(dir.join("new")).unwrap();
        let write = |sub: &str, name: &str, value: serde_json::Value| std::fs::write(dir.join(sub).join(name), value.to_string()).unwrap();
        write("done", "a.json", serde_json::json!({"surface": "%3", "surface_key": "k3", "at_ms": 10, "next": "옛 보고"}));
        write("new", "b.json", serde_json::json!({"surface": "%3", "surface_key": "k3", "at_ms": 20, "next": "새 보고"}));
        write("done", "c.json", serde_json::json!({"surface": "%3", "at_ms": 30, "next": "열쇠 없는 같은 번호 창"}));
        write("done", "d.json", serde_json::json!({"surface": "%3", "surface_key": "other", "at_ms": 40, "next": "다른 창"}));
        assert_eq!(latest_report(&dir, "k3").unwrap()["next"], "새 보고");
        assert!(latest_report(&dir, "").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_permission_table_shows_real_switches_and_no_unlimited_mode() {
        let caps: Result<Capabilities, Reach> = Ok(crate::work_mode::parse_capabilities(&serde_json::json!({
            "approvals": {"enabled": false, "http_actions": ["kasaterm_restart"], "decide_in_app": false, "decide_via": "owner_dm_button"},
            "policy": {"tiers": [{"tier": "confirm_once", "label": "매번 확인", "actions": ["설치", "재시작"], "rule": "대상·범위·해시·만료 1회"}]}
        })));
        let rows = permission_rows(Some(&caps), Some(&["일하는 학생 2".to_string()]));
        let find = |label: &str| rows.iter().find(|r| r.label == label).unwrap().value.clone();
        assert_eq!(find("매번 확인"), "대상·범위·해시·만료 1회\n· 설치\n· 재시작");
        assert!(find("나쵸 승인 창구").starts_with("꺼짐"));
        assert!(find("앱 안 승인").contains("주인 DM"));
        assert!(find("이 기기 앱 재시작").contains("거부: 일하는 학생 2"));
        assert!(find("무제한 모드").starts_with("없어요"));
        let old = crate::work_mode::parse_probe(503, br#"{"error":"approvals_disabled"}"#);
        let rows = permission_rows(Some(&old), Some(&[]));
        assert!(rows[0].value.contains("권한 표를 아직 주지 않아요"), "표가 없으면 규칙을 지어내지 않는다");
        let offline = permission_rows(Some(&Err(Reach::NoKey)), None);
        assert!(offline[0].value.contains("키가 없어요"));
        assert!(offline.iter().any(|r| r.label == "나쵸 승인 창구" && r.value == "확인하지 못했어요"));
    }
}
