use super::*;

type Rect = (f32, f32, f32, f32);
pub(crate) const HEADER_H: f32 = 48.0;
const ROOM_H: f32 = 30.0;
const PANE_H: f32 = 44.0;
const DEVICE_H: f32 = 42.0;
/// 아래에 끌어다 둔 기기 절의 머리줄 높이.
const SECTION_H: f32 = 36.0;
/// 끌고 있는 동안 바닥에 그리는 착지 안내 띠.
const DROP_H: f32 = 40.0;

/// 아래에 끌어다 둔 기기 하나. 절마다 따로 굴러야 위 목록과 스크롤이 안 얽힌다.
pub(crate) struct Pinned {
    pub(crate) label: String,
    scroll: f32,
    content_h: f32,
    view: Option<Rect>,
}

/// 피커 항목이나 머리줄에서 잡은 끌기. 문턱을 넘어야 `active` — 그 전에 놓으면 클릭이다.
pub(crate) struct NavDrag {
    label: String,
    start: (f32, f32),
    from_picker: bool,
    pub(crate) active: bool,
}

#[derive(Default)]
pub(crate) struct NavigationState {
    pub(crate) machine: Option<String>,
    pub(crate) picker: bool,
    pub(crate) scroll: f32,
    pub(crate) pinned: Vec<Pinned>,
    pub(crate) drag: Option<NavDrag>,
    picker_scroll: f32,
    picker_index: usize,
    content_h: f32,
    viewport: Option<Rect>,
    /// 위 목록과 아래 절을 합친 세로 구간 — 끌어다 놓을 자리 판정에 쓴다.
    full_view: Option<Rect>,
    picker_view: Option<Rect>,
    picker_content_h: f32,
    collapsed_rooms: std::collections::HashSet<String>,
    hits: Vec<(Action, Rect)>,
    picker_hits: Vec<(Option<String>, Rect)>,
    last_click: Option<(std::time::Instant, String)>,
}

#[derive(Clone)]
enum Action {
    Picker,
    Menu(String),
    Room(String),
    Pane(String, state::MachinesColRow),
    Close(state::MachinesColBtn),
    /// 아래 절의 머리줄 — 누르는 일은 없고 우클릭 메뉴의 대상만 된다.
    Section(String),
    Unpin(String),
}

fn hit(p: (f32, f32), r: Rect) -> bool {
    p.0 >= r.0 && p.0 < r.0 + r.2 && p.1 >= r.1 && p.1 < r.1 + r.3
}

fn clipped(r: Rect, viewport: Rect) -> Option<Rect> {
    let left = r.0.max(viewport.0);
    let top = r.1.max(viewport.1);
    let right = (r.0 + r.2).min(viewport.0 + viewport.2);
    let bottom = (r.1 + r.3).min(viewport.1 + viewport.3);
    (right > left && bottom > top).then_some((left, top, right - left, bottom - top))
}

fn room_key(machine: &str, room: &str) -> String {
    format!("{}:{machine}{room}", machine.len())
}

fn mirror_double_click(last: &mut Option<(Instant, String)>, key: String, now: Instant) -> bool {
    let double = last.as_ref().is_some_and(|(at, previous)| *previous == key && now.duration_since(*at).as_millis() < 400);
    *last = if double { None } else { Some((now, key)) };
    double
}

fn rows(machine: &state::MachinesColMachine) -> Vec<&state::MachinesColRow> {
    if !machine.online { return Vec::new(); }
    let mut rows: Vec<_> = machine.remote.iter().chain(machine.mirrored.iter()).collect();
    rows.sort_by(|a, b| a.room.cmp(&b.room).then_with(|| {
        let number = |r: &state::MachinesColRow| r.remote_id.trim_start_matches('%')
            .parse::<u64>().unwrap_or(u64::MAX);
        number(a).cmp(&number(b))
    }));
    rows
}

fn content_height(machine: &state::MachinesColMachine, collapsed: &std::collections::HashSet<String>) -> f32 {
    let mut last_room = None;
    let mut height = 0.0;
    for row in rows(machine) {
        if last_room != Some(row.room.as_str()) {
            height += ROOM_H;
            last_room = Some(row.room.as_str());
        }
        if !collapsed.contains(&room_key(&machine.label, &row.room)) { height += PANE_H; }
    }
    if height == 0.0 { height = PANE_H; }
    if machine.closed > 0 { height += PANE_H; }
    height + 8.0
}

/// 아래 절들의 높이. 절 수로 고르게 나눈 몫을 상한으로 두되 내용이 적으면 그만큼만
/// 차지한다 — 남는 자리는 위 목록이 쓴다.
fn section_heights(
    machines: &[state::MachinesColMachine],
    pinned: &[Pinned],
    collapsed: &std::collections::HashSet<String>,
    avail: f32,
) -> Vec<f32> {
    if pinned.is_empty() { return Vec::new(); }
    let share = (avail / (pinned.len() + 1) as f32).max(SECTION_H + PANE_H);
    pinned.iter().map(|pin| {
        let body = machines.iter().find(|m| m.label == pin.label)
            .map_or(PANE_H, |m| content_height(m, collapsed));
        (SECTION_H + body).min(share)
    }).collect()
}

/// 아래 절이 통째로 먹는 높이 — 위 방 목록은 그만큼 짧아진다.
pub(crate) fn pinned_total_h(info: &state::InfoState, avail: f32) -> f32 {
    section_heights(&info.machines_col.machines, &info.navigation.pinned, &info.navigation.collapsed_rooms, avail)
        .iter().sum()
}

/// 끌던 기기를 놓았을 때 절로 받는 자리 — 머리줄 아래 사이드바 전부.
fn drop_target(cursor: (f32, f32), width: f32, full: Rect) -> bool {
    cursor.0 >= 0.0 && cursor.0 < width
        && cursor.1 >= TITLE_HEIGHT + HEADER_H && cursor.1 < full.1 + full.3 + 24.0
}

fn text(g: &mut gpu::GpuRenderer, value: &str, x: f32, y: f32, width: f32, size: f32, color: [u8; 4], bold: bool) {
    let value = crate::info::fit_text(g, value, width.max(0.0), size, bold);
    g.draw_text(x, y, &value, gpu::DrawOpts { font_size: size, color, bold, italic: false });
}

fn status(machine: &state::MachinesColMachine) -> String {
    if machine.online {
        if machine.outdated { "연결됨 · 업데이트 필요".into() } else { "연결됨".into() }
    } else {
        crate::machinescol::ago_label(machine.ago_secs)
    }
}

fn scrollbar(g: &mut gpu::GpuRenderer, view: Rect, content_h: f32, scroll: f32, width: f32) {
    if content_h <= view.3 { return; }
    let h = (view.3 * view.3 / content_h).max(20.0).min(view.3);
    let y = view.1 + (view.3 - h) * scroll / (content_h - view.3);
    g.rect(width - 4.0, y, 2.0, h, theme::text_mute());
}

/// 한 기기의 방·pane 줄을 `view` 안에 그린다. 위 목록과 아래 절이 같은 함수를 쓴다 —
/// 두 자리의 줄이 달리 보이면 같은 pane 인지 헷갈린다.
fn draw_rows(
    g: &mut gpu::GpuRenderer,
    hits: &mut Vec<(Action, Rect)>,
    collapsed_rooms: &std::collections::HashSet<String>,
    machine: &state::MachinesColMachine,
    cursor: (f32, f32),
    width: f32,
    view: Rect,
    scroll: f32,
) {
    g.push_clip(view.0, view.1, view.2, view.3);
    let mut y = view.1 - scroll;
    let mut last_room = None;
    let machine_rows = rows(machine);
    if machine_rows.is_empty() {
        let message = if machine.online { "열린 pane 없음" } else { "기기에 연결할 수 없음" };
        text(g, message, 16.0, y + 12.0, width - 32.0, 11.0, theme::text_dim(), false);
        y += PANE_H;
    }
    for row in machine_rows {
        let key = room_key(&machine.label, &row.room);
        let collapsed = collapsed_rooms.contains(&key);
        if last_room != Some(row.room.as_str()) {
            let r = (8.0, y, width - 16.0, ROOM_H);
            if let Some(r) = clipped(r, view) {
                if hit(cursor, r) { g.rect(r.0, r.1, r.2, r.3, theme::surface_hover()); }
                g.hover_pointer |= hit(cursor, r);
                g.queue_icon(if collapsed { "chevron-right" } else { "chevron-down" }, 14.0, y + 9.0, 12.0, theme::text_dim());
                text(g, if row.room.is_empty() { "방 이름 없음" } else { &row.room }, 34.0, y + 8.0, width - 50.0, 11.0, theme::text_dim(), true);
                hits.push((Action::Room(key.clone()), r));
            }
            last_room = Some(row.room.as_str());
            y += ROOM_H;
        }
        if collapsed { continue; }
        let full = (14.0, y, width - 24.0, PANE_H);
        if let Some(r) = clipped(full, view) {
            let hover = hit(cursor, r);
            if hover { g.rect(full.0, y, full.2, full.3, theme::surface_hover()); }
            g.hover_pointer |= hover && !row.closed;
            let mirrored = !row.pane.is_empty();
            let close = (width - 34.0, y + 8.0, 24.0, 28.0);
            let right = if hover && !row.closed { width - 40.0 } else { width - 16.0 };
            if mirrored {
                crate::render::dashed_rect(g, full.0, y + 2.0, full.2, PANE_H - 4.0, theme::border());
            }
            g.queue_icon(if mirrored { "external-link" } else { "terminal" }, 22.0, y + 7.0, 13.0, theme::text_dim());
            let title = if row.title.is_empty() { row.name.as_str() } else { row.title.as_str() };
            let title = if title.is_empty() { row.remote_id.as_str() } else { title };
            text(g, title, 43.0, y + 5.0, right - 43.0, 11.5, theme::text(), false);
            let details = if row.closed {
                "닫힘 · 원본 기기에서 되살리기".to_string()
            } else if mirrored {
                "이 기기에서 보는 중".to_string()
            } else if row.status.contains("wait") || row.status.contains("attention") {
                format!("{} · 기다림", row.remote_id)
            } else {
                format!("{} · 두 번 눌러 열기", row.remote_id)
            };
            text(g, &details, 43.0, y + 24.0, right - 43.0, 10.0, theme::text_dim(), false);
            if !row.closed {
                if hover {
                    g.queue_icon("x", close.0 + 6.0, close.1 + 8.0, 12.0, if hit(cursor, close) { theme::attention() } else { theme::text_dim() });
                    if let Some(r) = clipped(close, view) {
                        hits.push((Action::Close(state::MachinesColBtn::Close {
                            label: machine.label.clone(), remote_id: row.remote_id.clone(),
                            name: row.name.clone(), pane: row.pane.clone(),
                        }), r));
                    }
                }
                hits.push((Action::Pane(machine.label.clone(), row.clone()), r));
            }
        }
        y += PANE_H;
    }
    if machine.closed > 0 {
        text(g, &format!("닫힌 pane {} · 원본에서 되살리기", machine.closed), 16.0, y + 12.0, width - 32.0, 10.0, theme::text_dim(), false);
    }
    g.pop_clip();
}

pub(crate) fn draw(g: &mut gpu::GpuRenderer, info: &mut state::InfoState, cursor: (f32, f32), width: f32, viewport: Rect) {
    let nav = &mut info.navigation;
    nav.hits.clear();
    nav.viewport = None;
    nav.full_view = (width > 0.0).then_some(viewport);
    for pin in &mut nav.pinned { pin.view = None; }
    if width <= 0.0 {
        nav.picker = false;
        nav.picker_hits.clear();
        nav.drag = None;
        return;
    }
    let machines = &info.machines_col.machines;
    let selected = nav.machine.as_deref().and_then(|name| machines.iter().find(|m| m.label == name));
    let label = nav.machine.as_deref().unwrap_or_else(|| crate::info::local_machine_name());
    let label = if label.is_empty() { "이 기기" } else { label };
    let head = (8.0, TITLE_HEIGHT + 4.0, width - 16.0, HEADER_H - 8.0);
    let menu_w = if selected.is_some() { 28.0 } else { 0.0 };
    let choose = (head.0, head.1, head.2 - menu_w, head.3);
    if hit(cursor, choose) || nav.picker {
        g.rect(head.0, head.1, choose.2, head.3, theme::surface_hover());
    }
    g.hover_pointer |= hit(cursor, choose);
    let tint = if selected.is_some() { crate::render::machine_tint(label) } else { theme::text_dim() };
    g.queue_icon("monitor", 14.0, head.1 + 12.0, 16.0, tint);
    text(g, label, 38.0, head.1 + 4.0, choose.2 - 54.0, 12.0, theme::text(), true);
    let sub = match selected {
        Some(m) => status(m),
        None if nav.machine.is_some() => "등록되지 않은 기기".into(),
        None => "이 기기 · 방과 pane".into(),
    };
    text(g, &sub, 38.0, head.1 + 22.0, choose.2 - 54.0, 10.0, theme::text_dim(), false);
    g.queue_icon("chevron-down", choose.0 + choose.2 - 18.0, head.1 + 13.0, 12.0, theme::text_dim());
    nav.hits.push((Action::Picker, choose));
    if let Some(machine) = selected {
        let r = (head.0 + head.2 - menu_w, head.1, menu_w, head.3);
        if hit(cursor, r) { g.rect(r.0, r.1, r.2, r.3, theme::surface_hover()); }
        g.hover_pointer |= hit(cursor, r);
        g.queue_icon("ellipsis-horizontal", r.0 + 7.0, r.1 + 13.0, 14.0, theme::text_dim());
        nav.hits.push((Action::Menu(machine.label.clone()), r));
    }
    g.rect(12.0, TITLE_HEIGHT + HEADER_H, (width - 24.0).max(0.0), 1.0, theme::border());

    let NavigationState { pinned, collapsed_rooms, hits, machine: main, scroll, content_h, viewport: main_view, .. } = nav;
    let heights = section_heights(machines, pinned, collapsed_rooms, viewport.3);
    let pinned_total: f32 = heights.iter().sum();
    let view = (viewport.0, viewport.1, viewport.2, (viewport.3 - pinned_total).max(0.0));
    *main_view = Some(view);
    if main.is_some() {
        match selected {
            None => text(g, "기기를 다시 선택해 주세요", 16.0, view.1 + 12.0, width - 32.0, 11.0, theme::text_dim(), false),
            Some(machine) => {
                *content_h = content_height(machine, collapsed_rooms);
                *scroll = scroll.clamp(0.0, (*content_h - view.3).max(0.0));
                draw_rows(g, hits, collapsed_rooms, machine, cursor, width, view, *scroll);
                scrollbar(g, view, *content_h, *scroll, width);
            }
        }
    }
    // 아래 절 — 위 목록이 남긴 바닥에 순서대로 쌓인다.
    let mut y = viewport.1 + viewport.3 - pinned_total;
    for (pin, h) in pinned.iter_mut().zip(heights) {
        let machine = machines.iter().find(|m| m.label == pin.label);
        g.rect(12.0, y, (width - 24.0).max(0.0), 1.0, theme::border());
        let head = (8.0, y + 2.0, width - 16.0, SECTION_H - 4.0);
        let unpin = (width - 32.0, y + 7.0, 22.0, 22.0);
        let menu = (width - 54.0, y + 7.0, 22.0, 22.0);
        if hit(cursor, head) { g.rect(head.0, head.1, head.2, head.3, theme::surface_hover()); }
        let tint = if machine.is_some() { crate::render::machine_tint(&pin.label) } else { theme::text_dim() };
        g.queue_icon("monitor", 14.0, y + 11.0, 14.0, tint);
        text(g, &pin.label, 36.0, y + 4.0, width - 96.0, 11.5, theme::text(), true);
        let sub = machine.map_or_else(|| "등록되지 않은 기기".to_string(), status);
        text(g, &sub, 36.0, y + 20.0, width - 96.0, 9.5, theme::text_dim(), false);
        g.hover_pointer |= hit(cursor, menu) || hit(cursor, unpin);
        g.queue_icon("ellipsis-horizontal", menu.0 + 4.0, menu.1 + 4.0, 14.0, theme::text_dim());
        g.queue_icon("x", unpin.0 + 5.0, unpin.1 + 5.0, 12.0, if hit(cursor, unpin) { theme::attention() } else { theme::text_dim() });
        // 작은 단추가 머리줄보다 앞이어야 한다 — 맞춤은 앞선 것이 이긴다.
        hits.push((Action::Unpin(pin.label.clone()), unpin));
        hits.push((Action::Menu(pin.label.clone()), menu));
        hits.push((Action::Section(pin.label.clone()), head));
        let body = (viewport.0, y + SECTION_H, viewport.2, (h - SECTION_H).max(0.0));
        pin.view = Some(body);
        match machine {
            None => text(g, "기기 목록에서 사라졌어요", 16.0, body.1 + 12.0, width - 32.0, 11.0, theme::text_dim(), false),
            Some(machine) => {
                pin.content_h = content_height(machine, collapsed_rooms);
                pin.scroll = pin.scroll.clamp(0.0, (pin.content_h - body.3).max(0.0));
                draw_rows(g, hits, collapsed_rooms, machine, cursor, width, body, pin.scroll);
                scrollbar(g, body, pin.content_h, pin.scroll, width);
            }
        }
        y += h;
    }
}

/// 끌고 있는 기기 — 커서를 따르는 칩과 바닥의 착지 띠. 다른 오버레이 뒤에 그려야
/// 피커·메뉴에 가리지 않는다.
pub(crate) fn draw_drag(g: &mut gpu::GpuRenderer, info: &state::InfoState, cursor: (f32, f32), width: f32) {
    let nav = &info.navigation;
    let (Some(drag), Some(full)) = (nav.drag.as_ref().filter(|d| d.active), nav.full_view) else { return; };
    if width <= 0.0 { return; }
    let over = drop_target(cursor, width, full);
    let tone = if over { theme::accent() } else { theme::text_mute() };
    let band = (8.0, full.1 + full.3 - DROP_H, (width - 16.0).max(0.0), DROP_H - 4.0);
    g.rect(band.0, band.1, band.2, band.3, theme::with_alpha(tone, if over { 60 } else { 28 }));
    crate::render::dashed_rect(g, band.0, band.1, band.2, band.3, tone);
    text(g, "놓으면 아래에 같이 보여요", band.0 + 10.0, band.1 + 12.0, band.2 - 20.0, 10.5,
        if over { theme::text() } else { theme::text_dim() }, false);
    // 칩은 커서 위에 띄운다 — 옆에 두면 좁은 사이드바에선 착지 띠의 안내 문구를 덮는다.
    let chip_w = 160.0_f32.min(width - 24.0).max(60.0);
    let chip = ((cursor.0 - 24.0).clamp(8.0, (width - chip_w - 8.0).max(8.0)), cursor.1 - 44.0, chip_w, 28.0);
    g.rect(chip.0, chip.1, chip.2, chip.3, theme::surface_active());
    crate::render::dashed_rect(g, chip.0, chip.1, chip.2, chip.3, theme::border());
    g.queue_icon("monitor", chip.0 + 8.0, chip.1 + 7.0, 14.0, crate::render::machine_tint(&drag.label));
    text(g, &drag.label, chip.0 + 28.0, chip.1 + 7.0, chip.2 - 36.0, 11.5, theme::text(), true);
}

pub(crate) fn draw_picker(g: &mut gpu::GpuRenderer, info: &mut state::InfoState, cursor: (f32, f32), width: f32, bottom: f32) {
    let nav = &mut info.navigation;
    nav.picker_hits.clear();
    nav.picker_view = None;
    if !nav.picker || width <= 0.0 { return; }
    let top = TITLE_HEIGHT + HEADER_H;
    nav.picker_content_h = (info.machines_col.machines.len() + 1) as f32 * DEVICE_H;
    let height = nav.picker_content_h.min((bottom - top - 8.0).max(0.0));
    let viewport = (8.0, top, (width - 16.0).max(0.0), height);
    nav.picker_view = Some(viewport);
    nav.picker_scroll = nav.picker_scroll.clamp(0.0, (nav.picker_content_h - height).max(0.0));
    g.rect(viewport.0, top, viewport.2, height, theme::panel_bg());
    g.push_clip(viewport.0, top, viewport.2, height);
    for (index, machine) in std::iter::once(None).chain(info.machines_col.machines.iter().map(Some)).enumerate() {
        let selection = machine.map(|m| m.label.clone());
        let y = top + index as f32 * DEVICE_H - nav.picker_scroll;
        let full = (viewport.0, y, viewport.2, DEVICE_H);
        let Some(r) = clipped(full, viewport) else { continue; };
        let selected = nav.machine == selection;
        let pinned = selection.as_ref().is_some_and(|l| nav.pinned.iter().any(|p| &p.label == l));
        if selected || index == nav.picker_index || hit(cursor, r) { g.rect(full.0, full.1, full.2, full.3, if selected { theme::surface_active() } else { theme::surface_hover() }); }
        g.hover_pointer |= hit(cursor, r);
        let label = machine.map(|m| m.label.as_str()).unwrap_or_else(|| crate::info::local_machine_name());
        text(g, if label.is_empty() { "이 기기" } else { label }, 20.0, y + 5.0, width - 52.0, 12.0, theme::text(), true);
        let detail = match machine {
            None => "이 기기".to_string(),
            Some(m) if pinned => format!("{} · 아래에 보는 중", status(m)),
            Some(m) => format!("{} · 아래로 끌어 함께 보기", status(m)),
        };
        text(g, &detail, 20.0, y + 24.0, width - 44.0, 10.0, theme::text_dim(), false);
        if selected { g.queue_icon("check", width - 32.0, y + 13.0, 14.0, theme::accent()); }
        nav.picker_hits.push((selection, r));
    }
    g.pop_clip();
    g.rect(viewport.0, top + height, viewport.2, 1.0, theme::border());
}

impl App {
    /// 격리 앱에서 사이드바 탐색을 단계별로 눌러 보는 프로브. 켜려면 네 가지가 함께
    /// 필요하다 — `KASATERM_WINDOW_SIZE`(검증 실행 표시) · `KASATERM_AUTODECKTIP=1`
    /// (사이드바가 기본으로 열림) · `KASATERM_AUTOINFO=rooms` + `KASATERM_AUTOINFO_MS=2000`
    /// (「맥북」 픽스처는 Info 타이머가 돌아야 심긴다) · `KASATERM_MACHINES='[]'`.
    /// 결과는 `KASATERM_AUTONAV_DIR/navigation.log` 와 단계별 png.
    pub(crate) fn run_pending_sidebar_navigation_probe(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::{Mutex, OnceLock};
        if !crate::verification_run()
            || !std::env::var("KASATERM_AUTOINFO").is_ok_and(|v| v == "rooms")
            || !std::env::var("KASATERM_MACHINES").is_ok_and(|v| v == "[]")
        { return; }
        let Ok(folder) = std::env::var("KASATERM_AUTONAV_DIR") else { return; };
        static STEP: OnceLock<Mutex<(Instant, usize)>> = OnceLock::new();
        let mut step = STEP.get_or_init(|| Mutex::new((Instant::now(), 0))).lock().unwrap();
        if step.1 >= 12 || step.0.elapsed().as_millis() < if step.1 == 0 { 10000 } else { 700 } { return; }
        let Some(window) = self.window.as_ref().map(|w| w.id()) else { return; };
        let click = |app: &mut Self, r: Rect| {
            app.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
            for state in [ElementState::Pressed, ElementState::Released] {
                app.window_event(event_loop, window, WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(), state, button: MouseButton::Left,
                });
            }
        };
        let mut report = String::new();
        let stage = step.1;
        match stage {
            0 => {
                let head = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::Picker).then_some(*r));
                if let Some(r) = head { click(self, r); }
                report = format!("picker_open={}", self.info.navigation.picker);
            }
            1 => {
                let item = self.info.navigation.picker_hits.iter().find_map(|(s, r)| (s.as_deref() == Some("맥북")).then_some(*r));
                if let Some(r) = item { click(self, r); }
                report = format!("remote_selected={}", self.info.navigation.machine.as_deref() == Some("맥북"));
            }
            2 => {
                let menu = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::Menu(_)).then_some(*r));
                if let Some(r) = menu { click(self, r); }
                report = format!("machine_menu={} local_hits_cleared={}", self.info.machine_menu.is_some(), self.window_tab_rects.is_empty());
            }
            3 => {
                click(self, (self.tab_strip_w() + 360.0, 150.0, 1.0, 1.0));
                report = format!("menu_closed={}", self.info.machine_menu.is_none());
                let room = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::Room(_)).then_some(*r));
                if let Some(r) = room { click(self, r); }
                report.push_str(&format!(" room_collapsed={}", !self.info.navigation.collapsed_rooms.is_empty()));
            }
            4 => {
                let head = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::Picker).then_some(*r));
                if let Some(r) = head { click(self, r); }
                report = format!("picker_reopened={}", self.info.navigation.picker);
            }
            5 => {
                let item = self.info.navigation.picker_hits.iter().find_map(|(s, r)| s.is_none().then_some(*r));
                if let Some(r) = item { click(self, r); }
                report = format!("local_selected={}", self.info.navigation.machine.is_none());
            }
            6 => {
                self.cursor_px = (30.0, self.sidebar_content_top() + 100.0);
                self.handle_wheel(MouseScrollDelta::LineDelta(0.0, -100.0));
                report = format!("local_scroll={} local_hits={}", self.sidebar_scroll_px > 0.0, !self.window_tab_rects.is_empty());
            }
            7 => {
                report = format!("last_room_reachable={} no_remote_sessions={}", self.window_tab_rects.iter().any(|(i, _)| i + 1 == self.windows.len()), self.pty.keys().all(|id| !kasa_mcp::remote::is_remote_pane(id)));
            }
            8 => {
                let head = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::Picker).then_some(*r));
                if let Some(r) = head { click(self, r); }
                report = format!("picker_for_drag={}", self.info.navigation.picker);
            }
            9 => {
                // 피커 항목을 잡아 바닥으로 끌어 놓는다 — 누르기·이동·놓기를 따로 보내야
                // 문턱 판정이 실제 경로를 탄다.
                let item = self.info.navigation.picker_hits.iter().find_map(|(s, r)| (s.as_deref() == Some("맥북")).then_some(*r));
                if let Some(r) = item {
                    let scale = self.effective_scale();
                    self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    self.window_event(event_loop, window, WindowEvent::MouseInput {
                        device_id: winit::event::DeviceId::dummy(), state: ElementState::Pressed, button: MouseButton::Left,
                    });
                    let full = self.info.navigation.full_view.unwrap_or((0.0, 200.0, 200.0, 400.0));
                    let to = (30.0, full.1 + full.3 - 20.0);
                    self.window_event(event_loop, window, WindowEvent::CursorMoved {
                        device_id: winit::event::DeviceId::dummy(),
                        position: winit::dpi::PhysicalPosition::new((to.0 * scale) as f64, (to.1 * scale) as f64),
                    });
                    let nav = &self.info.navigation;
                    report = format!("drag_active={} picker_closed={}", nav.drag.as_ref().is_some_and(|d| d.active), !nav.picker);
                }
            }
            10 => {
                // 끌던 것을 놓는다 — 직전 단계의 캡처가 끌기 중 화면(칩·착지 띠)을 담는다.
                self.window_event(event_loop, window, WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(), state: ElementState::Released, button: MouseButton::Left,
                });
                let nav = &self.info.navigation;
                report = format!("pinned={} main_local={} drag_cleared={}", nav.pinned.len(), nav.machine.is_none(), nav.drag.is_none());
            }
            11 => {
                let win_h = self.window.as_ref().map_or(0.0, |w| w.inner_size().height as f32 / self.effective_scale());
                let nav = &self.info.navigation;
                report = format!("pinned_view={} local_hits={} local_shrunk={} pinned_hits={}",
                    nav.pinned.first().is_some_and(|p| p.view.is_some()), !self.window_tab_rects.is_empty(),
                    self.sidebar_avail_h(win_h) < self.sidebar_full_avail_h(win_h),
                    nav.hits.iter().any(|(a, _)| matches!(a, Action::Pane(label, _) if label == "맥북")));
            }
            _ => {}
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window { window.request_redraw(); }
        if let Some(g) = &mut self.gpu { g.capture_next = Some(format!("{folder}/stage-{stage}.png")); }
        let path = std::path::Path::new(&folder).join("navigation.log");
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(file, "stage={stage} {report}");
        }
        step.0 = Instant::now();
        step.1 += 1;
    }

    pub(crate) fn sidebar_content_top(&self) -> f32 { TITLE_HEIGHT + HEADER_H + 10.0 }

    fn open_navigation_picker(&mut self) {
        self.info.navigation.picker = true;
        self.info.navigation.picker_index = self.info.navigation.machine.as_ref()
            .and_then(|name| self.info.machines_col.machines.iter().position(|m| &m.label == name))
            .map_or(0, |index| index + 1);
    }

    /// 위 목록의 기기를 바꾼다. 같은 기기가 아래 절에도 있으면 거기서 뺀다 — 한
    /// 기기가 두 자리에 서면 어느 쪽 스크롤이 맞는지 알 수 없다.
    fn select_navigation_machine(&mut self, label: Option<String>) {
        let nav = &mut self.info.navigation;
        if let Some(label) = &label { nav.pinned.retain(|p| &p.label != label); }
        nav.machine = label;
        nav.scroll = 0.0;
        nav.last_click = None;
    }

    /// 기기를 아래 절로 붙인다. 위 목록에 서 있던 기기면 위는 이 기기로 돌아간다 —
    /// 「밑으로 끌어 내린」 것이지 복사한 것이 아니다.
    fn pin_navigation_machine(&mut self, label: String) {
        let nav = &mut self.info.navigation;
        if nav.machine.as_deref() == Some(label.as_str()) {
            nav.machine = None;
            nav.scroll = 0.0;
        }
        if !nav.pinned.iter().any(|p| p.label == label) {
            nav.pinned.push(Pinned { label, scroll: 0.0, content_h: 0.0, view: None });
        }
        // 위 목록이 짧아졌으니 굴림도 새 한계 안으로.
        if let Some(win_h) = self.window.as_ref().map(|w| w.inner_size().height as f32 / self.effective_scale()) {
            self.sidebar_scroll_px = self.sidebar_scroll_px.min(self.sidebar_max_scroll(win_h));
        }
    }

    pub(crate) fn sidebar_navigation_click(&mut self, cursor: (f32, f32)) -> bool {
        if self.tabs_on_top || self.tab_strip_w() <= 0.0 { return false; }
        if self.info.navigation.picker {
            let selected = self.info.navigation.picker_hits.iter().find(|(_, r)| hit(cursor, *r)).map(|(s, _)| s.clone());
            match selected {
                // 다른 기기는 놓을 때 정한다 — 그대로 놓으면 고르기, 끌면 아래에 붙이기.
                Some(Some(label)) => {
                    self.info.navigation.drag = Some(NavDrag { label, start: cursor, from_picker: true, active: false });
                }
                Some(None) => {
                    self.info.navigation.picker = false;
                    self.select_navigation_machine(None);
                }
                None => self.info.navigation.picker = false,
            }
            self.chrome_dirty = true;
            return true;
        }
        let action = self.info.navigation.hits.iter().find(|(_, r)| hit(cursor, *r)).map(|(a, _)| a.clone());
        let Some(action) = action else { return false; };
        match action {
            Action::Picker => match self.info.navigation.machine.clone() {
                // 머리줄도 잡아 내릴 수 있다. 피커는 놓을 때 연다.
                Some(label) => self.info.navigation.drag = Some(NavDrag { label, start: cursor, from_picker: false, active: false }),
                None => self.open_navigation_picker(),
            },
            Action::Menu(label) => {
                self.info.machine_menu = Some((cursor.0, cursor.1, label));
                self.info.machines_col.last_refresh = None;
            }
            Action::Section(_) => {}
            Action::Unpin(label) => self.info.navigation.pinned.retain(|p| p.label != label),
            Action::Room(key) => {
                if !self.info.navigation.collapsed_rooms.remove(&key) { self.info.navigation.collapsed_rooms.insert(key); }
            }
            Action::Close(action) => { self.machines_col_act(action); }
            Action::Pane(label, row) => {
                let existing = (!row.pane.is_empty()).then_some(row.pane.clone()).or_else(|| {
                    kasa_mcp::machines::find(&label).and_then(|machine| {
                        kasa_pty::live_sessions().into_iter().find(|id| {
                            kasa_mcp::remote::remote_info(id).is_some_and(|info| info.base == machine.base && info.remote_id == row.remote_id)
                        })
                    })
                });
                if let Some(pane) = existing {
                    self.info.navigation.last_click = None;
                    if !self.reveal_pane_tab(&pane) {
                        let hidden = self.closed_pane_index(&pane).filter(|&i| {
                            self.closed_panes[i].alive && self.closed_panes[i].stashed
                        });
                        if let Some(i) = hidden { self.reopen_closed_pane_at(i); }
                    }
                } else if !row.remote_id.is_empty() && !row.closed {
                    let now = std::time::Instant::now();
                    let key = room_key(&label, &row.remote_id);
                    let double = mirror_double_click(&mut self.info.navigation.last_click, key, now);
                    if double {
                        self.machines_col_act(state::MachinesColBtn::Mirror { label, remote_id: row.remote_id, name: row.name, cwd: row.remote_cwd });
                    }
                }
            }
        }
        self.chrome_dirty = true;
        true
    }

    /// 커서가 움직일 때 — 잡은 기기가 문턱을 넘으면 끌기가 되고 피커는 닫힌다.
    /// 끌기 중이면 true 를 돌려 다른 hover 처리가 끼어들지 않게 한다.
    pub(crate) fn sidebar_navigation_drag_move(&mut self) -> bool {
        let cursor = self.cursor_px;
        let Some(drag) = self.info.navigation.drag.as_mut() else { return false; };
        let (dx, dy) = (cursor.0 - drag.start.0, cursor.1 - drag.start.1);
        if !drag.active && dx * dx + dy * dy > 9.0 { drag.active = true; }
        if !drag.active { return false; }
        self.info.navigation.picker = false;
        self.chrome_dirty = true;
        true
    }

    /// 눌렀던 것을 놓았다. 문턱을 안 넘었으면 클릭(고르기·피커 열기), 넘었으면
    /// 사이드바 안에 놓았을 때만 아래 절로 붙인다.
    pub(crate) fn sidebar_navigation_release(&mut self, cursor: (f32, f32)) -> bool {
        let Some(drag) = self.info.navigation.drag.take() else { return false; };
        if let Some(window) = &self.window { window.set_cursor(CursorIcon::Default); }
        if !drag.active {
            if drag.from_picker {
                self.info.navigation.picker = false;
                self.select_navigation_machine(Some(drag.label));
            } else {
                self.open_navigation_picker();
            }
        } else {
            let width = self.tab_strip_w();
            let landed = self.info.navigation.full_view.is_some_and(|full| drop_target(cursor, width, full));
            if landed { self.pin_navigation_machine(drag.label); }
        }
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn sidebar_navigation_right_click(&mut self, cursor: (f32, f32)) -> bool {
        if self.info.navigation.picker { self.info.navigation.picker = false; self.chrome_dirty = true; return true; }
        let action = self.info.navigation.hits.iter().find(|(_, r)| hit(cursor, *r)).map(|(a, _)| a.clone());
        let label = match action {
            Some(Action::Menu(label) | Action::Pane(label, _) | Action::Section(label) | Action::Unpin(label)) => Some(label),
            Some(Action::Picker) => self.info.navigation.machine.clone(),
            _ => None,
        };
        let Some(label) = label else { return false; };
        self.info.machine_menu = Some((cursor.0, cursor.1, label));
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn sidebar_navigation_wheel(&mut self, delta: &MouseScrollDelta) -> bool {
        if self.tabs_on_top || self.tab_strip_w() <= 0.0 { return false; }
        let cursor = self.cursor_px;
        let nav = &mut self.info.navigation;
        let dy = match delta { MouseScrollDelta::LineDelta(_, y) => y * 42.0, MouseScrollDelta::PixelDelta(p) => p.y as f32 };
        let handled = if nav.picker {
            if nav.picker_view.is_some_and(|r| hit(cursor, r)) {
                let height = nav.picker_view.unwrap().3;
                nav.picker_scroll = (nav.picker_scroll - dy).clamp(0.0, (nav.picker_content_h - height).max(0.0));
            }
            true
        } else if let Some(pin) = nav.pinned.iter_mut().find(|p| p.view.is_some_and(|r| hit(cursor, r))) {
            let height = pin.view.unwrap().3;
            pin.scroll = (pin.scroll - dy).clamp(0.0, (pin.content_h - height).max(0.0));
            true
        } else if nav.machine.is_some() && nav.viewport.is_some_and(|r| hit(cursor, r)) {
            let height = nav.viewport.unwrap().3;
            nav.scroll = (nav.scroll - dy).clamp(0.0, (nav.content_h - height).max(0.0));
            true
        } else { false };
        if handled {
            self.chrome_dirty = true;
            if let Some(window) = &self.window { window.request_redraw(); }
        }
        handled
    }

    pub(crate) fn sidebar_navigation_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if event.state.is_pressed() && self.info.navigation.drag.is_some()
            && matches!(event.logical_key, Key::Named(NamedKey::Escape))
        {
            self.info.navigation.drag = None;
            if let Some(window) = &self.window { window.set_cursor(CursorIcon::Default); }
            self.chrome_dirty = true;
            return true;
        }
        if !self.info.navigation.picker || !event.state.is_pressed() { return false; }
        let max = self.info.machines_col.machines.len();
        let nav = &mut self.info.navigation;
        match event.logical_key {
            Key::Named(NamedKey::ArrowDown) => nav.picker_index = (nav.picker_index + 1).min(max),
            Key::Named(NamedKey::ArrowUp) => nav.picker_index = nav.picker_index.saturating_sub(1),
            Key::Named(NamedKey::Home) => nav.picker_index = 0,
            Key::Named(NamedKey::End) => nav.picker_index = max,
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let label = nav.picker_index.checked_sub(1)
                    .and_then(|index| self.info.machines_col.machines.get(index)).map(|m| m.label.clone());
                nav.picker = false;
                self.select_navigation_machine(label);
                self.chrome_dirty = true;
                return true;
            }
            Key::Named(NamedKey::Escape | NamedKey::Tab) => nav.picker = false,
            _ => return true,
        }
        let height = nav.picker_view.map_or(DEVICE_H, |r| r.3);
        let top = nav.picker_index as f32 * DEVICE_H;
        if top < nav.picker_scroll { nav.picker_scroll = top; }
        if top + DEVICE_H > nav.picker_scroll + height { nav.picker_scroll = (top + DEVICE_H - height).max(0.0); }
        self.chrome_dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> state::MachinesColMachine {
        let row = |pane: &str, remote: &str, room: &str| state::MachinesColRow {
            pane: pane.into(), remote_id: remote.into(), room: room.into(), remote_cwd: String::new(),
            name: String::new(), title: String::new(), status: String::new(), closed: false,
        };
        state::MachinesColMachine {
            label: "test".into(), online: true, ago_secs: None, outdated: false,
            host: String::new(), kvm: None, closed: 2,
            remote: vec![row("", "%12", "A"), row("", "%2", "A"), row("", "%8", "B")],
            mirrored: vec![row("%99", "%7", "A")],
        }
    }

    #[test]
    fn remote_navigation_keeps_source_order_and_mirror_destination() {
        let m = machine();
        let rows = rows(&m);
        assert_eq!(rows.iter().map(|r| r.remote_id.as_str()).collect::<Vec<_>>(), ["%2", "%7", "%12", "%8"]);
        assert_eq!(rows[1].pane, "%99");
    }

    #[test]
    fn remote_room_collapse_changes_scroll_height_without_losing_other_rooms() {
        let m = machine();
        let mut collapsed = std::collections::HashSet::new();
        let full = content_height(&m, &collapsed);
        collapsed.insert(room_key("test", "A"));
        assert_eq!(full - content_height(&m, &collapsed), PANE_H * 3.0);
        assert_eq!(rows(&m).len(), 4);
    }

    #[test]
    fn offscreen_remote_rows_cannot_receive_clicks() {
        let viewport = (0.0, 100.0, 200.0, 200.0);
        assert_eq!(clipped((10.0, 80.0, 180.0, 44.0), viewport), Some((10.0, 100.0, 180.0, 24.0)));
        assert!(clipped((10.0, 50.0, 180.0, 44.0), viewport).is_none());
        assert!(!hit((20.0, 99.0), clipped((10.0, 80.0, 180.0, 44.0), viewport).unwrap()));
    }

    #[test]
    fn opening_a_new_mirror_requires_two_clicks_on_the_same_source() {
        let mut last = None;
        let now = Instant::now();
        let next = now + std::time::Duration::from_millis(100);
        assert!(!mirror_double_click(&mut last, "one/%2".into(), now));
        assert!(!mirror_double_click(&mut last, "two/%2".into(), next));
        assert!(mirror_double_click(&mut last, "two/%2".into(), next));
        assert!(last.is_none());
        assert!(!mirror_double_click(&mut last, "two/%2".into(), next));
        assert!(!mirror_double_click(&mut last, "two/%2".into(), next + std::time::Duration::from_millis(500)));
    }

    #[test]
    fn pinned_sections_take_at_most_an_equal_share_and_shrink_to_content() {
        let pin = |label: &str| Pinned { label: label.into(), scroll: 0.0, content_h: 0.0, view: None };
        let collapsed = std::collections::HashSet::new();
        let m = machine();
        let body = content_height(&m, &collapsed);
        assert_eq!(section_heights(&[machine()], &[pin("test")], &collapsed, 1000.0), vec![SECTION_H + body]);
        assert_eq!(section_heights(&[machine()], &[pin("test")], &collapsed, 300.0), vec![150.0]);
        assert_eq!(section_heights(&[machine()], &[pin("없음")], &collapsed, 1000.0), vec![SECTION_H + PANE_H]);
        assert!(section_heights(&[machine()], &[], &collapsed, 1000.0).is_empty());
        let two = section_heights(&[machine()], &[pin("test"), pin("없음")], &collapsed, 240.0);
        assert_eq!(two, vec![SECTION_H + PANE_H, SECTION_H + PANE_H]);
    }

    #[test]
    fn drop_lands_only_inside_the_sidebar_below_the_header() {
        let full = (0.0, TITLE_HEIGHT + HEADER_H + 10.0, 220.0, 500.0);
        assert!(drop_target((30.0, full.1 + 300.0), 220.0, full));
        assert!(!drop_target((300.0, full.1 + 300.0), 220.0, full));
        assert!(!drop_target((30.0, TITLE_HEIGHT + 10.0), 220.0, full));
        assert!(!drop_target((30.0, full.1 + full.3 + 60.0), 220.0, full));
    }

    #[test]
    fn offline_devices_do_not_offer_stale_panes() {
        let mut m = machine();
        m.online = false;
        assert!(rows(&m).is_empty());
    }
}
