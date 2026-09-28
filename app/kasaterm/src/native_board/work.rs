//! 사람용 작업현황은 에이전트 관측을 작업 장부로 가장하지 않는다.
//!
//! 두 정본을 한 줄기로 모은다. 창·학생·기기는 보드 스냅샷(`OverviewData`)이고, 작업의
//! 단계·승인·증거는 나쵸 작업 장부(`crate::nacho_tasks`)다. 둘은 서로를 대신하지 않는다 —
//! 장부가 끊겨도 창 관측은 그대로 보이고, 창이 오래돼도 장부의 단계는 장부 것이다.
//!
//! 순서는 「내 답변 필요 → 진행 중 → 결과」이며 기기 명부는 관측 도구에만 남긴다.
//! 완료 줄기는 **끝남과 성공을 가른다** — 종료 신호나 자기 보고만 있는 일은 「검증 안 됨」
//! 이고, 같은 판의 검증 기록이 통과를 말할 때만 「성공 확인」이다.
//!
//! 승인 실행은 서버의 1회용·범위·만료 확인을 우회하지 않도록 대화와 구분한다.

use super::*;
use crate::nacho_tasks::{BookSource, TaskBook, TaskCard, TaskState, Verdict};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Lane {
    Answer,
    Progress,
    Verify,
    Done,
}

impl Lane {
    const fn label(self) -> &'static str {
        match self {
            Self::Answer => "내 답변 필요",
            Self::Progress => "진행 중",
            Self::Verify => "검증",
            Self::Done => "결과",
        }
    }
}

/// 끝난 일의 종류. 끝남(`Unverified`)과 성공(`Verified`)이 다른 칸인 것이 이 타입의 이유다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    Verified,
    Unverified,
    Failed,
    Cancelled,
}

impl Finish {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Verified => "성공 확인",
            Self::Unverified => "끝남 · 검증 안 됨",
            Self::Failed => "실패",
            Self::Cancelled => "접음",
        }
    }

    fn color(self) -> [u8; 4] {
        match self {
            Self::Verified => theme::success(),
            Self::Failed => theme::danger(),
            Self::Unverified => theme::text_dim(),
            Self::Cancelled => theme::text_mute(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WorkKey {
    Task(String),
    Pane(String),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkUi {
    pub(crate) project: Option<String>,
    pub(crate) selected: Option<WorkKey>,
    pub(crate) filters_open: bool,
    pub(crate) expanded: HashSet<Lane>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkItem {
    pub(crate) key: WorkKey,
    pub(crate) lane: Lane,
    pub(crate) finish: Option<Finish>,
    pub(crate) title: String,
    pub(crate) project: String,
    pub(crate) state_label: String,
    pub(crate) student: Option<String>,
    pub(crate) machine_id: String,
    pub(crate) pane_id: Option<String>,
    pub(crate) observed_at_ms: u64,
    /// 이 일을 보여 주는 관측이 오래됐거나 끊겼다 — 단계는 마지막 확인 값이다.
    pub(crate) stale: bool,
}

/// 창의 방 이름에서 프로젝트 이름을 뗀다. 방 이름은 `프로젝트 · 경로` 모양이다.
pub(crate) fn pane_project(room_label: &str) -> String {
    let head = room_label.split(" · ").next().unwrap_or("").trim();
    if head.is_empty() { "방 미확인".into() } else { head.to_string() }
}

fn pane_is_agent(row: &OverviewPane) -> bool {
    row.character.as_deref().is_some_and(|name| !name.is_empty())
        || row.harness.as_deref().is_some_and(|harness| !harness.is_empty() && harness != "shell")
}

/// 장부에 없는 창 하나가 어느 줄기에 서는가. 쉬는 창·상태를 모르는 창은 줄기에 안 서고
/// 기기·학생 목록에만 남는다 — 할 일 판은 「손댈 것」과 「도는 것」만 센다.
fn pane_lane(row: &OverviewPane) -> Option<(Lane, Option<Finish>, &'static str)> {
    if !pane_is_agent(row) || row.detached {
        return None;
    }
    let (label, rank) = overview_status(row);
    match (rank, row.done_outcome.as_deref()) {
        (_, Some("failed")) => Some((Lane::Done, Some(Finish::Failed), "실패 보고")),
        // 자기 보고는 검증이 아니다 — 끝났다는 말만 있다.
        (_, Some("succeeded")) => Some((Lane::Done, Some(Finish::Unverified), "완료 보고")),
        (0, _) => Some((Lane::Answer, None, label)),
        (3, _) => Some((Lane::Progress, None, label)),
        _ => None,
    }
}

fn task_lane(task: &TaskCard, verdict: Verdict) -> (Lane, Option<Finish>) {
    match task.state {
        TaskState::ApprovalNeeded => (Lane::Answer, None),
        TaskState::Verifying | TaskState::PostRestartVerifying => (Lane::Verify, None),
        TaskState::Done => (Lane::Done, Some(if verdict == Verdict::Passed { Finish::Verified } else { Finish::Unverified })),
        TaskState::Failed => (Lane::Done, Some(Finish::Failed)),
        TaskState::Cancelled => (Lane::Done, Some(Finish::Cancelled)),
        TaskState::Queued | TaskState::Working | TaskState::RestartPending | TaskState::Unknown => {
            (if task.needs_you() { Lane::Answer } else { Lane::Progress }, None)
        }
    }
}

/// 장부의 작업이 가리키는 창. 일↔창의 정본은 장부의 (기기 id, 창 열쇠)다 — 열쇠가 오면 그것만
/// 보고, 옛 나쵸처럼 창 번호(`%N`)뿐이면 기기 id 와 함께 맞을 때만 잇는다(번호는 재사용된다).
/// 장부가 그 창이 다른 일로 넘어갔다고 하면 잇지 않는다.
fn linked_pane<'a>(data: &'a OverviewData, task: &TaskCard) -> Option<&'a OverviewPane> {
    if task.superseded() {
        return None;
    }
    let machine = task.machine_id()?;
    if let Some(key) = task.surface_key() {
        return data.panes.iter().find(|row| row.address.machine_id == machine && row.address.surface_key == key);
    }
    let surface = task.surface()?;
    data.panes.iter().find(|row| {
        row.address.machine_id == machine && (row.address.surface_key == surface || row.address.surface_id == surface)
    })
}

/// 두 정본을 한 목록으로. 장부 작업에 이어진 창은 따로 세지 않는다(같은 일이 두 번 뜨지 않게).
pub(crate) fn work_items(data: &OverviewData, book: &TaskBook) -> Vec<WorkItem> {
    let mut items = Vec::new();
    let mut claimed = HashSet::new();
    for task in book.tasks() {
        let pane = linked_pane(data, task);
        if let Some(pane) = pane {
            claimed.insert(pane.id.clone());
        }
        let (lane, finish) = task_lane(task, book.verdict(task));
        let character = book.detail(&task.id).and_then(|d| d.report.as_ref()).map(|r| r.character.clone()).filter(|c| !c.is_empty());
        items.push(WorkItem {
            key: WorkKey::Task(task.id.clone()),
            lane,
            finish,
            title: if task.goal.is_empty() { task.id.clone() } else { task.goal.clone() },
            project: Some(task.project.clone()).filter(|p| !p.is_empty() && p != "기타")
                .or_else(|| pane.map(|row| pane_project(&row.room_label)))
                .unwrap_or_else(|| "프로젝트 미지정".into()),
            state_label: if task.state_label.is_empty() { task.state.label().into() } else { task.state_label.clone() },
            student: pane.and_then(|row| row.character.clone()).filter(|c| !c.is_empty()).or(character),
            machine_id: task.machine_id().unwrap_or_default().to_string(),
            pane_id: pane.map(|row| row.id.clone()),
            observed_at_ms: task.updated_ms,
            stale: book.stale() || pane.is_some_and(|row| row.freshness != "fresh"),
        });
    }
    for row in &data.panes {
        if claimed.contains(&row.id) {
            continue;
        }
        let Some((lane, finish, label)) = pane_lane(row) else { continue };
        items.push(WorkItem {
            key: WorkKey::Pane(row.id.clone()),
            lane,
            finish,
            title: if row.request.is_empty() { row.title.clone() } else { row.request.clone() },
            project: pane_project(&row.room_label),
            state_label: label.into(),
            student: row.character.clone().filter(|name| !name.is_empty()),
            machine_id: row.address.machine_id.clone(),
            pane_id: Some(row.id.clone()),
            observed_at_ms: row.observed_at_ms,
            stale: row.freshness != "fresh",
        });
    }
    // 답할 것은 오래 기다린 것부터, 나머지는 최근 것부터.
    items.sort_by(|a, b| {
        a.lane.cmp(&b.lane).then_with(|| match a.lane {
            Lane::Answer => a.observed_at_ms.cmp(&b.observed_at_ms),
            _ => b.observed_at_ms.cmp(&a.observed_at_ms),
        })
    });
    items
}

fn project_choices(items: &[WorkItem]) -> Vec<String> {
    let mut projects: Vec<String> = items.iter().map(|item| item.project.clone()).collect();
    projects.sort();
    projects.dedup();
    projects
}

fn human_group(lane: Lane) -> Lane {
    if lane == Lane::Verify { Lane::Progress } else { lane }
}

impl Scene {
    pub(crate) fn select_work(&mut self, key: WorkKey) {
        self.hits.clear();
        if self.work.selected.as_ref() == Some(&key) {
            self.work.selected = None;
            self.overview.selection = None;
            self.overview.selection_generation += 1;
            return;
        }
        let items = work_items(&self.data.overview, &self.data.tasks);
        let pane = items.iter().find(|item| item.key == key).and_then(|item| item.pane_id.clone());
        self.work.selected = Some(key);
        // 창이 이어진 일은 방별 보드와 같은 상세 조회(최근 활동)를 탄다.
        self.overview.selection_generation += 1;
        self.overview.selection = pane.and_then(|id| {
            self.data.overview.panes.iter().find(|row| row.id == id).map(|row| OverviewSelection {
                id, address: row.address.clone(), generation: self.overview.selection_generation,
            })
        });
        self.last_refresh = None;
    }
}

fn machine_label<'a>(data: &'a OverviewData, machine_id: &'a str) -> &'a str {
    data.sources.iter().find(|s| s.machine_id == machine_id).map(|s| s.label.as_str())
        .filter(|l| !l.is_empty())
        .unwrap_or(if machine_id.is_empty() { "기기 미확인" } else { machine_id })
}

/// 장부가 목록에서 뺀 것을 한 줄로 — 빠진 줄은 화면에서 안 보이므로 적지 않으면 「없다」로 읽힌다.
fn book_scope_note(book: &TaskBook) -> String {
    let mut note = String::new();
    if !book.desk_scope() {
        note.push_str(" · 나쵸가 아직 폰·펫 창구 일만 보내요(디스코드·슬랙 일은 빠짐)");
    }
    if book.hidden_count() > 0 {
        note.push_str(&format!(" · 사내 채널에서 맡긴 일 {}건은 싣지 않았어요", book.hidden_count()));
    }
    note
}

fn book_line(book: &TaskBook, now: u64) -> (String, [u8; 4]) {
    match (book.source(), book.error()) {
        (BookSource::Fixture, _) => (format!("검증용 가상 장부 · 실제 작업이 아니에요{}", book_scope_note(book)), theme::text_dim()),
        (BookSource::Unasked, _) => ("나쵸 장부를 확인하고 있어요".into(), theme::text_dim()),
        (BookSource::Live, None) => (
            format!("나쵸 장부 · 작업 {}개 · {} 확인{}", book.tasks().len(), board_relative_time(now, book.checked_at_ms()), book_scope_note(book)),
            theme::text_dim(),
        ),
        (BookSource::Live, Some(error)) if book.stale() => (
            format!("나쵸 장부 연결 끊김 · {} 받은 목록을 유지해요 · {error}", board_relative_time(now, book.last_ok_ms())),
            theme::danger(),
        ),
        (BookSource::Live, Some(error)) => (format!("{error} · 창 관측만 보여줘요"), theme::text_dim()),
    }
}

pub(super) fn paint_work(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let data = &s.data.overview;
    let book = &s.data.tasks;
    if s.fixture {
        overview_note(g, x, y, w, "검증용 가상 작업현황", theme::text_dim());
    }
    let all = work_items(data, book);
    let items: Vec<&WorkItem> = all.iter()
        .filter(|item| s.work.project.as_ref().is_none_or(|p| &item.project == p)).collect();
    let now = data.observed_at_ms.max(book.checked_at_ms());
    let filter_label = s.work.project.as_deref().unwrap_or("모든 프로젝트");
    let filter_width = (g.measure_chrome_text(filter_label, 12.0, false) + 20.0).min(w).min(260.0);
    button(g, s, hits, (x, *y, filter_width, 26.0), filter_label, Target::WorkFilters, s.work.filters_open);
    *y += 40.0;
    if s.work.filters_open {
        let mut choices = vec![("전체".to_string(), Target::WorkProject(None), s.work.project.is_none(), true)];
        for project in project_choices(&all) {
            let selected = s.work.project.as_deref() == Some(project.as_str());
            choices.push((project.clone(), Target::WorkProject(Some(project)), selected, true));
        }
        overview_choices(g, s, hits, x, y, w, choices);
    }

    if let Some(error) = &data.error {
        overview_note(g, x, y, w, error, theme::danger());
    }
    let (line, color) = book_line(book, now);
    if book.error().is_some() || book.stale() || book.hidden_count() > 0 || !book.desk_scope() {
        overview_note(g, x, y, w, &line, color);
    }
    if items.is_empty() && data.schema_version == 0 && data.error.is_none() {
        overview_note(g, x, y, w, "작업을 확인하고 있어요…", theme::text_dim());
        return;
    }
    if s.work.project.as_ref().is_some_and(|p| !all.iter().any(|item| &item.project == p)) {
        overview_note(g, x, y, w, "이 프로젝트의 현재 작업이 없어요. 모든 프로젝트에서 다시 확인할 수 있어요.", theme::text_dim());
    }
    for lane in [Lane::Answer, Lane::Progress, Lane::Done] {
        let rows: Vec<&WorkItem> = items.iter().copied().filter(|item| human_group(item.lane) == lane).collect();
        group_title(g, x, y, w, &format!("{}  {}", lane.label(), rows.len()));
        if rows.is_empty() {
            let note = match lane {
                Lane::Answer => "지금 답할 일이 없어요",
                Lane::Progress => "진행 중인 일이 없어요. 나쵸에게 새 일을 맡겨 보세요.",
                _ => "작업이 끝나면 검사 결과와 함께 여기에 남아요",
            };
            overview_note(g, x, y, w, note, theme::text_dim());
        }
        let visible = if s.work.expanded.contains(&lane) { rows.len() } else { 8 };
        for item in rows.iter().take(visible) {
            paint_task_row(g, s, hits, x, y, w, item);
            if s.work.selected.as_ref() == Some(&item.key) {
                paint_detail(g, s, hits, x + 10.0, y, (w - 20.0).max(1.0), item);
                *y += 12.0;
            }
        }
        if rows.len() > 8 {
            let label = if s.work.expanded.contains(&lane) { "접기".into() } else { format!("{}개 더 보기", rows.len() - 8) };
            text_button(g, s, hits, (x, *y, 128.0_f32.min(w), 26.0), &label, Target::WorkMore(lane), false);
            *y += 34.0;
        }
        *y += 24.0;
    }
    if s.work.selected.as_ref().is_some_and(|key| !all.iter().any(|item| &item.key == key)) {
        overview_note(g, x, y, w, "고른 일이 현재 목록에 없어요. 다른 일을 골라 주세요.", theme::text_dim());
    }
    if book.error().is_none() && book.desk_scope() && book.hidden_count() == 0 {
        let checked = format!("{} 확인 · 완료 보고와 검증 통과는 따로 표시해요", board_relative_time(now, book.checked_at_ms()));
        overview_note(g, x, y, w, &checked, theme::text_dim());
    }
}

fn group_title(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str) {
    let label = fit(g, label, w, 11.0, true);
    text(g, x, *y, &label, 11.0, theme::text_dim(), true);
    *y += 22.0;
    divider(g, x, *y, w);
    *y += 10.0;
}


/// 카드 한 줄 설명. 모르는 칸은 빼고 아는 것만 — 「미확인」 두 개가 좁은 카드의 폭을 다 먹는다.
/// 모른다는 사실은 상세의 「맡음」「연결」이 말한다.
fn meta_line(s: &Snapshot, item: &WorkItem, place: Option<&str>) -> String {
    let data = &s.data.overview;
    let mut parts = Vec::new();
    parts.extend(item.student.clone());
    if !item.machine_id.is_empty() {
        parts.push(machine_label(data, &item.machine_id).to_string());
    }
    parts.push(item.project.clone());
    if let Some(place) = place.filter(|p| !p.is_empty() && *p != "?") {
        parts.push(place.to_string());
    }
    parts.push(board_relative_time(data.observed_at_ms.max(s.data.tasks.checked_at_ms()), item.observed_at_ms));
    parts.join(" · ")
}

fn task_of<'a>(s: &'a Snapshot, item: &WorkItem) -> Option<&'a TaskCard> {
    match &item.key {
        WorkKey::Task(id) => s.data.tasks.tasks().iter().find(|t| &t.id == id),
        WorkKey::Pane(_) => None,
    }
}

fn pane_of<'a>(s: &'a Snapshot, item: &WorkItem) -> Option<&'a OverviewPane> {
    item.pane_id.as_ref().and_then(|id| s.data.overview.panes.iter().find(|row| &row.id == id))
}

fn task_status(item: &WorkItem) -> (String, [u8; 4]) {
    let (label, color) = match item.finish {
        Some(Finish::Unverified) if matches!(item.key, WorkKey::Pane(_)) =>
            ("완료 보고 · 검증 안 됨".to_string(), theme::text_dim()),
        Some(finish) => (finish.label().to_string(), finish.color()),
        None => (item.state_label.clone(), if item.lane == Lane::Answer { theme::danger() } else { theme::accent() }),
    };
    (if item.stale { format!("{label} · 오래된 정보") } else { label }, color)
}

fn paint_task_row(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, item: &WorkItem) {
    let start = *y;
    let inset = 10.0;
    let inner = (w - inset * 2.0).max(1.0);
    let title = board_wrap(&board_plain(&item.title, 400), inner, 2, |line| g.measure_chrome_text(line, 12.0, false));
    let task = task_of(s, item);
    let (status, color) = task_status(item);
    let detail = task.map(|t| t.step.as_str()).filter(|step| !step.is_empty()).unwrap_or("");
    let meta = meta_line(s, item, None);
    let status_line = if detail.is_empty() { status } else { format!("{status} · {detail}") };
    let mut cy = start + 10.0;
    let height = title.len().max(1) as f32 * 18.0 + 58.0;
    let rect = (x, start, w, height);
    let selected = s.work.selected.as_ref() == Some(&item.key);
    if selected || contains(rect, s.cursor) {
        round_rect(g, x, start, w, height, theme::radius_sm(),
            if selected { theme::surface_active() } else { theme::surface_hover() });
    }
    for line in title {
        text(g, x + inset, cy, &line, 12.0, theme::text(), false);
        cy += 18.0;
    }
    let status_line = fit(g, &status_line, inner, 10.5, false);
    text(g, x + inset, cy + 4.0, &status_line, 10.5, color, false);
    let meta = fit(g, &meta, inner, 10.5, false);
    text(g, x + inset, cy + 22.0, &meta, 10.5, theme::text_dim(), false);
    hit(g, hits, Target::WorkSelect(item.key.clone()), rect, false);
    g.hover_pointer |= contains(rect, s.cursor);
    divider(g, x, start + height, w);
    *y = start + height + 6.0;
}

fn paint_detail(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, item: &WorkItem) {
    let data = &s.data.overview;
    let book = &s.data.tasks;
    let task = task_of(s, item);
    let detail = task.and_then(|t| book.detail(&t.id).filter(|d| d.rev == t.rev));
    let pane = pane_of(s, item);
    button(g, s, hits, (x, *y, 160.0_f32.min(w), 26.0), "이 일 나쵸에게 물어보기", Target::WorkChat(item.key.clone()), true);
    *y += 40.0;
    if item.lane == Lane::Answer {
        let ask = task.map(|t| t.attention.as_str()).filter(|v| !v.is_empty())
            .or_else(|| pane.map(|row| row.progress.as_str()).filter(|v| !v.is_empty()))
            .unwrap_or("무엇을 기다리는지 아직 확인하지 못했어요");
        paint_overview_summary(g, x, y, w, "필요한 답", ask, 4);
    }
    let lines = board_wrap(&board_plain(&item.title, 400), w, 3, |line| g.measure_chrome_text(line, 13.0, false));
    for line in lines {
        text(g, x, *y, &line, 13.0, theme::text(), false);
        *y += 20.0;
    }
    *y += 6.0;
    let origin = match (task, pane) {
        (Some(t), _) => format!("나쵸 작업 {} · {}", t.id, if t.place.is_empty() || t.place == "?" { "들어온 창구 미확인" } else { &t.place }),
        (None, Some(row)) => format!("창 관측 · {} · {}", machine_label(data, &row.address.machine_id), if row.room_label.is_empty() { "방 미확인" } else { &row.room_label }),
        (None, None) => "출처 미확인".into(),
    };
    paint_overview_summary(g, x, y, w, "출처", &origin, 2);
    let who = format!(
        "{} · {}{}",
        item.student.as_deref().unwrap_or("학생 미확인"),
        machine_label(data, &item.machine_id),
        pane.map(|row| format!(" · {}", row.address.surface_id)).unwrap_or_default(),
    );
    paint_overview_summary(g, x, y, w, "맡음", &who, 2);
    if let Some(hint) = pane.and_then(|row| row.origin_task_env.as_deref()).filter(|h| !h.is_empty()) {
        paint_overview_summary(g, x, y, w, "표식", &origin_hint_line(hint, task.map(|t| t.id.as_str())), 2);
    }
    let source = data.sources.iter().find(|src| src.machine_id == item.machine_id);
    let link = match (source, pane) {
        (Some(src), Some(row)) => format!(
            "기기 {} · 창 {} · {} 확인",
            if src.state == "online" && src.complete { "연결됨" } else { "연결 끊김" },
            if row.freshness == "fresh" { "최신" } else { "오래됨" },
            board_relative_time(data.observed_at_ms, row.observed_at_ms),
        ),
        (Some(src), None) => format!(
            "기기 {} · 이어진 창 없음 · {} 확인",
            if src.state == "online" && src.complete { "연결됨" } else { "연결 끊김" },
            board_relative_time(data.observed_at_ms, src.observed_at_ms),
        ),
        _ => "이어진 기기·창 없음".into(),
    };
    paint_overview_summary(g, x, y, w, "연결", &link, 2);
    if let Some(t) = task {
        let mut stage = if t.state_label.is_empty() { t.state.label().to_string() } else { t.state_label.clone() };
        if !t.step.is_empty() { stage = format!("{stage} · {}", t.step); }
        paint_overview_summary(g, x, y, w, "단계", &stage, 2);
        if let Some(d) = detail {
            if !d.remaining.is_empty() {
                paint_overview_summary(g, x, y, w, "남음", &d.remaining.join(" · "), 3);
            }
        }
    }
    let evidence = match (task, detail) {
        (Some(_), Some(d)) => {
            let mut rows = Vec::new();
            match &d.verify {
                Some(v) => rows.push(format!("검증 {} · {}{}", if v.ok { "통과" } else { "실패" }, board_relative_time(data.observed_at_ms.max(book.checked_at_ms()), v.at_ms), if v.note.is_empty() { String::new() } else { format!(" · {}", v.note) })),
                None => rows.push("검증 기록 없음".into()),
            }
            if let Some(r) = &d.report {
                if !r.summary.is_empty() { rows.push(format!("보고 · {}", r.summary)); }
                if !r.tests.is_empty() { rows.push(format!("검사 · {}", r.tests)); }
                if !r.changed.is_empty() { rows.push(format!("바뀐 파일 {}개", r.changed.len())); }
            }
            rows
        }
        (Some(_), None) => vec!["상세를 아직 받지 못했어요".into()],
        (None, _) => {
            let mut rows = vec!["나쵸 장부에 없는 창이에요 · 검증 기록 없음".to_string()];
            if let Some(summary) = pane.and_then(|row| row.done_summary.clone()).filter(|v| !v.is_empty()) {
                rows.push(format!("자기 보고 · {summary}"));
            }
            rows
        }
    };
    for row in evidence {
        paint_overview_summary(g, x, y, w, "증거", &row, 3);
    }
    if let Some(approval) = task.and_then(|t| book.approval(t)) {
        let what = [approval.action.as_str(), approval.what.as_str()].into_iter().find(|v| !v.is_empty()).unwrap_or("승인할 내용 미확인");
        let mut line = what.to_string();
        if approval.one_use { line.push_str(" · 1회용"); }
        if approval.expires_at_ms > 0 {
            let now = data.observed_at_ms.max(book.checked_at_ms());
            line.push_str(&if approval.expires_at_ms > now {
                format!(" · {}분 뒤 만료", (approval.expires_at_ms - now).div_ceil(60_000))
            } else {
                " · 만료됨".to_string()
            });
        }
        paint_overview_summary(g, x, y, w, "승인", &line, 2);
        overview_note(g, x, y, w, "승인 실행은 기기 인증이 정해진 뒤 켭니다 · 지금은 보기만 해요", theme::text_mute());
    }
    if let Some(row) = pane {
        let mut choices = vec![("주소 복사".into(), Target::OverviewCopy(row.address.clone()), false, true)];
        if overview_is_local(data, &row.address) {
            choices.push(("창으로 이동".into(), Target::OverviewFocus(row.address.clone()), false, true));
        }
        overview_choices(g, s, hits, x, y, w, choices);
        if let Some(recent) = overview_current_detail(data, &s.overview) {
            overview_note(g, x, y, w, &format!("최근 활동 · {} 확인", board_relative_time(data.observed_at_ms, recent.observed_at_ms)), theme::text_dim());
            for line in recent.lines.iter().rev().take(4).rev() {
                for line in board_wrap(&board_plain(line, 800), w, 3, |line| g.measure_chrome_text(line, 11.0, false)) {
                    text(g, x, *y, &line, 11.0, theme::text(), false);
                    *y += 18.0;
                }
                *y += 6.0;
            }
        }
    }
    text_button(g, s, hits, (x, *y, 60.0, 28.0), "닫기", Target::WorkSelect(item.key.clone()), false);
    *y += 34.0;
}

/// 창의 env 표식 한 줄. 표식은 출처 힌트라, 장부가 이은 일과 다르면(떠 있는 창에 새 일을 넘긴
/// 경우) 다르다고 적는다 — 판정은 여전히 장부 몫이다.
fn origin_hint_line(hint: &str, task: Option<&str>) -> String {
    match task {
        Some(id) if id != hint => format!("창 env 는 나쵸 작업 {hint} · 이 일과 다름 · 표시용"),
        _ => format!("창 env 의 나쵸 작업 {hint} · 표시용, 판정 근거 아님"),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::nacho_tasks::{TaskPlace, fixture_book};

    fn pane(id: &str, status: &str, attention: Option<&str>, outcome: Option<&str>) -> OverviewPane {
        OverviewPane {
            id: id.into(),
            address: BoardAddress {
                machine_id: "m1".into(),
                surface_key: format!("key-{id}"),
                surface_id: format!("%{id}"),
                ..Default::default()
            },
            room_label: "kasaterm · ~/Desktop/momewomo/kasaterm".into(),
            character: Some("치나츠".into()),
            harness: Some("claude".into()),
            request: format!("요청 {id}"),
            status: status.into(),
            attention_kind: attention.map(str::to_string),
            done_outcome: outcome.map(str::to_string),
            observed_at_ms: 1_000,
            freshness: "fresh".into(),
            ..Default::default()
        }
    }

    fn overview(panes: Vec<OverviewPane>) -> OverviewData {
        OverviewData { schema_version: 1, panes, ..Default::default() }
    }

    #[test]
    fn pane_lanes_follow_attention_and_reports() {
        let data = overview(vec![
            pane("1", "waiting", Some("permission"), None),
            pane("2", "working", None, None),
            pane("3", "idle", None, Some("succeeded")),
            pane("4", "idle", None, None),
            pane("5", "waiting", Some("idle"), None),
        ]);
        let items = work_items(&data, &TaskBook::default());
        let lanes: Vec<_> = items.iter().map(|item| (item.pane_id.clone().unwrap(), item.lane)).collect();
        assert_eq!(lanes, vec![
            ("1".into(), Lane::Answer),
            ("2".into(), Lane::Progress),
            ("3".into(), Lane::Done),
        ], "쉬는 창·방치는 줄기에 서지 않는다");
        assert_eq!(items[2].finish, Some(Finish::Unverified), "자기 보고는 성공 확인이 아니다");
    }

    #[test]
    fn shells_and_detached_panes_stay_out_of_streams() {
        let mut shell = pane("1", "working", None, None);
        shell.character = None;
        shell.harness = Some("shell".into());
        let mut closed = pane("2", "working", None, None);
        closed.detached = true;
        assert!(work_items(&overview(vec![shell, closed]), &TaskBook::default()).is_empty());
    }

    #[test]
    fn project_comes_from_the_room_label_head() {
        assert_eq!(pane_project("nacho-neko · …sktop/momewomo/nacho-neko"), "nacho-neko");
        assert_eq!(pane_project(""), "방 미확인");
        assert_eq!(pane_project("개인맥북"), "개인맥북");
    }

    #[test]
    fn a_task_claims_its_pane_only_when_machine_and_surface_both_match() {
        let data = overview(vec![pane("1", "working", None, None), pane("2", "working", None, None)]);
        let tasks = book_of(vec![
            ("w1", Some(TaskPlace { surface: Some("%1".into()), machine_id: "m1".into(), ..Default::default() })),
            ("w2", Some(TaskPlace { surface: Some("%2".into()), machine_id: "other".into(), ..Default::default() })),
        ]);
        let items = work_items(&data, &tasks);
        let panes: Vec<_> = items.iter().filter(|i| matches!(i.key, WorkKey::Pane(_))).map(|i| i.pane_id.clone().unwrap()).collect();
        assert_eq!(panes, vec!["2".to_string()], "%1 은 w1 이 가져가고, 다른 기기의 %2 는 못 가져간다");
    }

    fn book_of(rows: Vec<(&str, Option<TaskPlace>)>) -> TaskBook {
        let cards = rows.into_iter().map(|(id, student)| TaskCard {
            id: id.into(), state: TaskState::Working, student, rev: "1".into(), ..Default::default()
        }).collect();
        crate::nacho_tasks::refresh_with(&TaskBook::default(), 1, &StaticLedger(cards))
    }

    struct StaticLedger(Vec<TaskCard>);

    impl crate::nacho_tasks::Ledger for StaticLedger {
        fn list(&self) -> Result<crate::nacho_tasks::TaskList, String> {
            Ok(crate::nacho_tasks::TaskList { tasks: self.0.clone(), ..Default::default() })
        }
        fn detail(&self, _: &str) -> Result<crate::nacho_tasks::TaskDetail, String> { Err("없음".into()) }
    }

    #[test]
    fn fixture_tasks_fill_every_lane_and_keep_ended_apart_from_verified() {
        let items = work_items(&overview(Vec::new()), &fixture_book(10_000_000));
        for lane in [Lane::Answer, Lane::Progress, Lane::Verify, Lane::Done] {
            assert!(items.iter().any(|i| i.lane == lane), "{lane:?} 줄기가 비었다");
        }
        let finishes: Vec<_> = items.iter().filter_map(|i| i.finish).collect();
        assert!(finishes.contains(&Finish::Verified));
        assert!(finishes.contains(&Finish::Unverified), "검증 기록 없는 끝남은 따로 보여야 한다");
        assert!(finishes.contains(&Finish::Failed));
    }

    #[test]
    fn surface_key_decides_the_link_and_superseded_tasks_let_go() {
        let data = overview(vec![pane("1", "working", None, None)]);
        let place = |key: Option<&str>, superseded: Option<&str>| Some(TaskPlace {
            surface: Some("%1".into()), surface_key: key.map(str::to_string), superseded_by: superseded.map(str::to_string),
            host: None, machine_id: "m1".into(),
        });
        let linked = |p| work_items(&data, &book_of(vec![("w1", p)])).iter().any(|i| i.key == WorkKey::Task("w1".into()) && i.pane_id.is_some());
        assert!(linked(place(Some("key-1"), None)), "열쇠가 맞으면 잇는다");
        assert!(!linked(place(Some("other-key"), None)), "열쇠가 오면 번호가 맞아도 열쇠로만 판단한다");
        assert!(!linked(place(Some("key-1"), Some("w2"))), "다른 일로 넘어간 창은 잇지 않는다");
        assert!(linked(place(None, None)), "옛 나쵸(열쇠 없음)는 기기+번호로");
    }

    #[test]
    fn origin_hint_is_labeled_display_only_and_flags_a_different_task() {
        assert!(origin_hint_line("we409f946", None).contains("판정 근거 아님"));
        assert!(origin_hint_line("we409f946", Some("we409f946")).contains("표시용"));
        assert!(origin_hint_line("wf66d6f5a", Some("we409f946")).contains("이 일과 다름"));
    }

    #[test]
    fn human_groups_keep_verification_in_progress_and_results_distinct() {
        assert_eq!(human_group(Lane::Answer), Lane::Answer);
        assert_eq!(human_group(Lane::Progress), Lane::Progress);
        assert_eq!(human_group(Lane::Verify), Lane::Progress);
        assert_eq!(human_group(Lane::Done), Lane::Done);
    }
}
