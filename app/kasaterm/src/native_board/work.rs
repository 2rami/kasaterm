//! 할 일 판 — 통합 작업판 B안(할 일 먼저)의 네이티브 화면.
//!
//! 두 정본을 한 줄기로 모은다. 창·학생·기기는 보드 스냅샷(`OverviewData`)이고, 작업의
//! 단계·승인·증거는 나쵸 작업 장부(`crate::nacho_tasks`)다. 둘은 서로를 대신하지 않는다 —
//! 장부가 끊겨도 창 관측은 그대로 보이고, 창이 오래돼도 장부의 단계는 장부 것이다.
//!
//! 순서는 「답이 필요한 것 → 진행 · 검증 · 완료」, 옆(좁으면 아래)에 기기·학생이다.
//! 완료 줄기는 **끝남과 성공을 가른다** — 종료 신호나 자기 보고만 있는 일은 「검증 안 됨」
//! 이고, 같은 판의 검증 기록이 통과를 말할 때만 「성공 확인」이다.
//!
//! 승인은 보여 주기만 한다. 서버에 1회용·범위·만료를 담은 승인 계약이 없어서, 단추 자리는
//! 비우지 않고 꺼 둔 채 이유를 적는다(`docs/design.md` 「없는 동작의 자리」).

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
    const STREAMS: [Self; 3] = [Self::Progress, Self::Verify, Self::Done];

    const fn label(self) -> &'static str {
        match self {
            Self::Answer => "답이 필요한 것",
            Self::Progress => "진행",
            Self::Verify => "검증",
            Self::Done => "완료",
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
    const fn label(self) -> &'static str {
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

/// 장부의 작업이 가리키는 창. 기기와 창 번호가 둘 다 맞아야 잇는다 — 창 번호만으로
/// 이으면 다른 기기의 같은 `%3` 에 붙고, 창 번호는 재사용된다.
fn linked_pane<'a>(data: &'a OverviewData, task: &TaskCard) -> Option<&'a OverviewPane> {
    let surface = task.surface()?;
    let machine = task.machine_id()?;
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

/// 줄기 셋을 나란히 세울 폭. 이보다 좁으면 한 열로 쌓는다 — 한 줄기가 200px 밑이면
/// 제목 두 줄에 글자 여섯 자도 안 들어간다.
fn stream_columns(w: f32) -> usize {
    if w >= 640.0 { 3 } else { 1 }
}

impl Scene {
    pub(crate) fn select_work(&mut self, key: WorkKey) {
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

fn book_line(book: &TaskBook, now: u64) -> (String, [u8; 4]) {
    match (book.source(), book.error()) {
        (BookSource::Fixture, _) => ("검증용 가상 장부 · 실제 작업이 아니에요".into(), theme::text_dim()),
        (BookSource::Unasked, _) => ("나쵸 장부를 확인하고 있어요".into(), theme::text_dim()),
        (BookSource::Live, None) => (
            format!("나쵸 장부 · 작업 {}개 · {} 확인", book.tasks().len(), board_relative_time(now, book.checked_at_ms())),
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
    side: Option<(f32, f32)>,
) {
    let data = &s.data.overview;
    let book = &s.data.tasks;
    let top = *y;
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
    let now = data.observed_at_ms.max(book.checked_at_ms());
    let (line, color) = book_line(book, now);
    overview_note(g, x, y, w, &line, color);

    let all = work_items(data, book);
    let mut choices = vec![("전체".to_string(), Target::WorkProject(None), s.work.project.is_none(), true)];
    for project in project_choices(&all) {
        let selected = s.work.project.as_deref() == Some(project.as_str());
        choices.push((project.clone(), Target::WorkProject(Some(project)), selected, true));
    }
    if let Some(missing) = s.work.project.as_ref().filter(|p| !all.iter().any(|item| &item.project == *p)) {
        choices.push((format!("{missing} · 현재 목록에 없음"), Target::WorkProject(Some(missing.clone())), true, false));
    }
    overview_choices(g, s, hits, x, y, w, choices);
    let items: Vec<&WorkItem> = all.iter().filter(|item| s.work.project.as_ref().is_none_or(|p| &item.project == p)).collect();
    let count = |lane| items.iter().filter(|item| item.lane == lane).count();
    let summary = format!(
        "답 {} · 진행 {} · 검증 {} · 완료 {}",
        count(Lane::Answer), count(Lane::Progress), count(Lane::Verify), count(Lane::Done)
    );
    overview_note(g, x, y, w, &summary, theme::text_dim());

    let answers: Vec<&WorkItem> = items.iter().copied().filter(|item| item.lane == Lane::Answer).collect();
    group_title(g, x, y, w, &format!("{} {}", Lane::Answer.label(), answers.len()));
    if answers.is_empty() {
        overview_note(g, x, y, w, "지금 답할 것이 없어요", theme::text_dim());
    }
    for item in answers {
        paint_answer(g, s, hits, x, y, w, item);
    }
    *y += 12.0;

    let columns = stream_columns(w);
    let gap = 16.0;
    let col_w = (w - gap * (columns as f32 - 1.0)) / columns as f32;
    let stream_top = *y;
    let mut bottom = *y;
    for (index, lane) in Lane::STREAMS.into_iter().enumerate() {
        let (cx, mut cy) = if columns == 3 { (x + index as f32 * (col_w + gap), stream_top) } else { (x, bottom) };
        let lane_items: Vec<&WorkItem> = items.iter().copied().filter(|item| item.lane == lane).collect();
        group_title(g, cx, &mut cy, col_w, &format!("{} {}", lane.label(), lane_items.len()));
        if lane_items.is_empty() {
            overview_note(g, cx, &mut cy, col_w, "없음", theme::text_mute());
        }
        for item in lane_items.iter().take(8) {
            paint_stream_item(g, s, hits, cx, &mut cy, col_w, item);
        }
        if lane_items.len() > 8 {
            overview_note(g, cx, &mut cy, col_w, &format!("외 {}개 · 프로젝트를 골라 좁혀 보세요", lane_items.len() - 8), theme::text_dim());
        }
        bottom = bottom.max(cy + if columns == 1 { 12.0 } else { 0.0 });
    }
    *y = bottom + 12.0;

    let selected = s.work.selected.as_ref().and_then(|key| all.iter().find(|item| &item.key == key));
    match side {
        Some((sx, sw)) => {
            let mut sy = top;
            if let Some(item) = selected {
                paint_detail(g, s, hits, sx, &mut sy, sw, item);
                sy += 12.0;
            }
            paint_machines(g, s, sx, &mut sy, sw);
            *y = y.max(sy);
        }
        None => {
            if let Some(item) = selected {
                paint_detail(g, s, hits, x, y, w, item);
                *y += 12.0;
            }
            paint_machines(g, s, x, y, w);
        }
    }
    if s.work.selected.is_some() && selected.is_none() {
        overview_note(g, x, y, w, "고른 일이 현재 목록에 없어요. 다시 골라 주세요", theme::text_dim());
    }
}

fn group_title(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str) {
    let label = fit(g, label, w, 11.0, true);
    text(g, x, *y, &label, 11.0, theme::text_dim(), true);
    *y += 22.0;
    divider(g, x, *y, w);
    *y += 10.0;
}

fn face(g: &mut gpu::GpuRenderer, s: &Snapshot, name: Option<&str>, x: f32, y: f32, size: f32) {
    if let Some(face) = name.and_then(|name| s.data.faces.iter().find(|face| face.name == name)) {
        if !g.has_image(&face.key) { g.upload_image(&face.key, &face.rgba, face.width, face.height); }
        g.queue_image_above(&face.key, x, y, size, size);
    } else {
        round_rect(g, x, y, size, size, theme::radius_md(), theme::surface_hover());
        g.queue_icon("terminal", x + size * 0.22, y + size * 0.22, size * 0.56, theme::text_dim());
    }
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

fn paint_answer(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, item: &WorkItem) {
    let start = *y;
    let task = task_of(s, item);
    let pane = pane_of(s, item);
    let selected = s.work.selected.as_ref() == Some(&item.key);
    let tx = if w >= 260.0 { x + 44.0 } else { x };
    let tw = w - (tx - x);
    if w >= 260.0 { face(g, s, item.student.as_deref(), x, start + 2.0, 32.0); }
    let status_w = g.measure_chrome_text(&item.state_label, 11.0, false);
    // 제목은 사람이 쓴 긴 문장이라 보통 굵기다 — 굵은 한글은 글자마다 굵기가 갈려 보이는 자리가 있다.
    let title = fit(g, &board_plain(&item.title, 200), (tw - status_w - 26.0).max(40.0), 13.0, false);
    text(g, tx, start + 2.0, &title, 13.0, theme::text(), false);
    circle_rect(g, x + w - status_w - 12.0, start + 6.0, 6.0, theme::danger());
    text(g, x + w - status_w, start + 4.0, &item.state_label, 11.0, theme::danger(), false);
    *y = start + 24.0;
    let meta = fit(g, &meta_line(s, item, task.map(|t| t.place.as_str())), tw, 10.5, false);
    text(g, tx, *y, &meta, 10.5, theme::text_dim(), false);
    *y += 20.0;
    let ask = task.map(|t| t.attention.clone()).filter(|a| !a.is_empty())
        .or_else(|| pane.map(|row| row.progress.clone()).filter(|p| !p.is_empty()))
        .unwrap_or_else(|| "무엇을 기다리는지 아직 확인하지 못했어요".into());
    paint_overview_summary(g, tx, y, tw, "필요", &ask, 2);
    if item.stale {
        overview_note(g, tx, y, tw, "오래된 정보 · 마지막으로 확인한 상태예요", theme::text_dim());
    }
    hit(g, hits, Target::WorkSelect(item.key.clone()), (x, start, w, *y - start), false);
    let mut bx = tx;
    let detail_label = if selected { "상세 접기" } else { "상세 보기" };
    text_button(g, s, hits, (bx, *y, 76.0, 28.0), detail_label, Target::WorkSelect(item.key.clone()), false);
    bx += 84.0;
    if let Some(row) = pane.filter(|row| overview_is_local(&s.data.overview, &row.address)) {
        button(g, s, hits, (bx, *y, 96.0, 26.0), "창으로 이동", Target::OverviewFocus(row.address.clone()), true);
        bx += 104.0;
    }
    let mut below = None;
    if task.is_some_and(|t| t.state == TaskState::ApprovalNeeded) {
        disabled_button(g, (bx, *y, 64.0, 26.0), "승인");
        bx += 72.0;
        // 꺼 둔 이유가 잘리면 꺼 둔 단추만 남는다 — 옆에 안 들어가면 다음 줄로 내린다.
        let note = "서버의 1회용·범위·만료 확인이 생기면 켭니다";
        if g.measure_chrome_text(note, 10.5, false) <= x + w - bx {
            text(g, bx, *y + 7.0, note, 10.5, theme::text_mute(), false);
        } else {
            below = Some(note);
        }
    }
    *y += 36.0;
    if let Some(note) = below {
        overview_note(g, tx, y, tw, note, theme::text_mute());
    }
    divider(g, x, *y, w);
    *y += 12.0;
}

/// 누를 수 없는 단추 — 자리는 지키고 흐리게. 클릭 영역을 만들지 않는다.
fn disabled_button(g: &mut gpu::GpuRenderer, rect: Rect, label: &str) {
    g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, theme::radius_md().min(5.0), 1.0, theme::with_alpha(theme::border(), 140));
    let shown = fit(g, label, rect.2 - 14.0, 11.5, false);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, false)) / 2.0;
    text(g, tx, rect.1 + (rect.3 - 12.0) / 2.0 - 1.0, &shown, 11.5, theme::text_mute(), false);
}

fn paint_stream_item(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, item: &WorkItem) {
    let start = *y;
    let selected = s.work.selected.as_ref() == Some(&item.key);
    let rect_h_guess = 86.0;
    let hover = contains((x, start, w, rect_h_guess), s.cursor);
    let pad = 10.0;
    let inner = w - pad * 2.0;
    let mut cy = start + pad;
    let lines = board_wrap(&board_plain(&item.title, 400), inner, 2, |line| g.measure_chrome_text(line, 12.0, false));
    let title_h = lines.len().max(1) as f32 * 18.0;
    let meta = fit(g, &meta_line(s, item, task_of(s, item).map(|t| t.place.as_str())), inner, 10.5, false);
    let height = pad + title_h + 4.0 + 18.0 + 20.0 + pad;
    if selected || hover {
        round_rect(g, x, start, w, height, theme::radius_md().min(5.0), if selected { theme::surface_active() } else { theme::surface_hover() });
    }
    for line in &lines {
        text(g, x + pad, cy, line, 12.0, theme::text(), false);
        cy += 18.0;
    }
    if lines.is_empty() { cy += 18.0; }
    cy += 4.0;
    text(g, x + pad, cy, &meta, 10.5, theme::text_dim(), false);
    cy += 18.0;
    let (label, color) = match item.finish {
        // 창의 완료 보고는 학생 자신의 말이다 — 장부의 「끝남」과 같은 낱말로 부르지 않는다.
        Some(Finish::Unverified) if matches!(item.key, WorkKey::Pane(_)) => ("완료 보고 · 검증 안 됨".to_string(), Finish::Unverified.color()),
        Some(finish) => (finish.label().to_string(), finish.color()),
        None => (item.state_label.clone(), if item.lane == Lane::Progress { theme::accent() } else { theme::text_dim() }),
    };
    let label = if item.stale { format!("{label} · 오래된 정보") } else { label };
    let label = fit(g, &label, inner - 14.0, 10.5, false);
    circle_rect(g, x + pad, cy + 4.0, 6.0, color);
    text(g, x + pad + 12.0, cy + 1.0, &label, 10.5, color, false);
    hit(g, hits, Target::WorkSelect(item.key.clone()), (x, start, w, height), false);
    g.hover_pointer |= hover;
    *y = start + height + 6.0;
}

fn paint_detail(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, item: &WorkItem) {
    let data = &s.data.overview;
    let book = &s.data.tasks;
    let task = task_of(s, item);
    let detail = task.and_then(|t| book.detail(&t.id).filter(|d| d.rev == t.rev));
    let pane = pane_of(s, item);
    group_title(g, x, y, w, "고른 일");
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
    if let Some(approval) = detail.and_then(|d| d.approval.as_ref()).filter(|a| a.needed) {
        let what = if approval.what.is_empty() { "승인할 내용 미확인" } else { &approval.what };
        paint_overview_summary(g, x, y, w, "승인", what, 2);
        overview_note(g, x, y, w, "승인 단추는 서버가 1회용·범위·만료를 확인할 수 있게 된 뒤 켭니다", theme::text_mute());
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

fn paint_machines(g: &mut gpu::GpuRenderer, s: &Snapshot, x: f32, y: &mut f32, w: f32) {
    let data = &s.data.overview;
    group_title(g, x, y, w, "기기 · 학생");
    let mut sources: Vec<_> = data.sources.iter().collect();
    sources.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.machine_id.cmp(&b.machine_id)));
    if sources.is_empty() {
        overview_note(g, x, y, w, "아직 확인한 기기가 없어요", theme::text_dim());
    }
    for source in sources {
        let state = match source.state.as_str() {
            "online" if source.complete => "연결됨",
            "offline" | "disconnected" => "연결 끊김",
            _ => "미확인",
        };
        let students: Vec<&OverviewPane> = data.panes.iter()
            .filter(|row| row.address.machine_id == source.machine_id && pane_is_agent(row) && !row.detached)
            .collect();
        let head = format!("{} · {} · {} · 학생 {}", source.label, state, board_relative_time(data.observed_at_ms, source.observed_at_ms), students.len());
        let head = fit(g, &board_plain(&head, 200), w, 12.0, true);
        text(g, x, *y, &head, 12.0, theme::text(), true);
        *y += 22.0;
        for row in students.iter().take(8) {
            let (label, rank) = overview_status(row);
            let color = overview_status_color(rank);
            face(g, s, row.character.as_deref(), x, *y, 20.0);
            let name = row.character.as_deref().filter(|n| !n.is_empty()).unwrap_or(&row.address.surface_id);
            let label_w = g.measure_chrome_text(label, 10.5, false);
            let line = fit(g, &format!("{name} · {}", pane_project(&row.room_label)), (w - 28.0 - label_w - 22.0).max(0.0), 11.0, false);
            text(g, x + 28.0, *y + 3.0, &line, 11.0, theme::text(), false);
            circle_rect(g, x + w - label_w - 12.0, *y + 7.0, 6.0, color);
            text(g, x + w - label_w, *y + 4.0, label, 10.5, color, false);
            *y += 26.0;
        }
        if students.len() > 8 {
            overview_note(g, x, y, w, &format!("외 {}명", students.len() - 8), theme::text_dim());
        }
        *y += 10.0;
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
            ("w1", Some(TaskPlace { surface: Some("%1".into()), host: None, machine_id: "m1".into() })),
            ("w2", Some(TaskPlace { surface: Some("%2".into()), host: None, machine_id: "other".into() })),
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
        fn list(&self) -> Result<Vec<TaskCard>, String> { Ok(self.0.clone()) }
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
    fn narrow_boards_stack_the_streams() {
        assert_eq!(stream_columns(639.0), 1);
        assert_eq!(stream_columns(640.0), 3);
    }
}
