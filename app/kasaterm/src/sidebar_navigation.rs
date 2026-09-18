use super::*;

type Rect = (f32, f32, f32, f32);
pub(crate) const HEADER_H: f32 = 48.0;
const PANE_H: f32 = 44.0;
const DEVICE_H: f32 = 42.0;
/// 아래에 끌어다 둔 기기 절의 머리줄 높이.
const SECTION_H: f32 = 36.0;
/// 끌고 있는 동안 바닥에 그리는 착지 안내 띠.
const DROP_H: f32 = 40.0;

/// 연결이 끊겨도 기기 상태를 목록에서 확인할 수 있도록 보관한다.
pub(crate) struct Pinned {
    pub(crate) label: String,
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
    pub(crate) local_content_h: f32,
    pub(crate) shared_scroll: f32,
    pub(crate) room_numbers: Vec<(String, Option<u64>, String, usize)>,
    pub(crate) pinned: Vec<Pinned>,
    /// x 로 닫은 기기 — 연결돼 있어도 저절로 다시 서지 않는다. 끌어다 놓으면 풀린다.
    dismissed: std::collections::HashSet<String>,
    pub(crate) drag: Option<NavDrag>,
    /// 지금 보는 창이 어느 기계의 어떤 원격 pane 들인가 — 그 방 카드를 활성으로 그린다.
    /// 렌더가 프레임마다 `App::remote_view_of_window` 로 채운다.
    pub(crate) viewing: Option<(String, Vec<String>)>,
    /// 보는 중인 원격 방에서 지금 포커스한 칸의 원격 pane id.
    pub(crate) viewing_cur: Option<String>,
    picker_scroll: f32,
    picker_index: usize,
    content_h: f32,
    viewport: Option<Rect>,
    /// 위 목록과 아래 절을 합친 세로 구간 — 끌어다 놓을 자리 판정에 쓴다.
    full_view: Option<Rect>,
    picker_view: Option<Rect>,
    picker_content_h: f32,
    collapsed_rooms: std::collections::HashSet<String>,
    /// 목록 본문으로 보는 방(키) — 본기기 카드의 「목록으로 보기」와 같다.
    list_rooms: std::collections::HashSet<String>,
    /// 방 카드 우클릭 메뉴. 본기기 방 메뉴와 같은 항목(본문 보기·이름·닫기).
    /// 칸(pane)에서 열면 칸 메뉴가 된다.
    pub(crate) room_menu: Option<RoomMenu>,
    /// 배치도 칸을 잡아 끄는 중 — 놓은 자리의 칸과 자리를 바꾼다(그쪽 `surface.move`).
    pub(crate) cell_drag: Option<CellDrag>,
    room_menu_rects: Vec<(RoomMenuAction, Rect)>,
    /// 이름을 고치는 중인 원격 방 — (카드 키, 캐럿 포함 글). 렌더가 프레임마다 채운다.
    pub(crate) rename: Option<(String, String)>,
    hits: Vec<(Action, Rect)>,
    picker_hits: Vec<(Option<String>, Rect)>,
}

/// 방 카드 우클릭 메뉴 — 어느 기기의 어느 방인가와 뜬 자리. `pane` 이 있으면 **칸 메뉴**다.
pub(crate) struct RoomMenu {
    x: f32,
    y: f32,
    label: String,
    window: Option<u64>,
    room: String,
    key: String,
    pane: Option<String>,
}

/// 잡은 칸. 문턱을 넘어야 `active` — 그 전에 놓으면 그냥 여는 클릭이다(머리줄 끌기와 같은 규칙).
pub(crate) struct CellDrag {
    label: String,
    room: String,
    window: Option<u64>,
    pane: String,
    start: (f32, f32),
    pub(crate) active: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum RoomMenuAction {
    ListBody,
    MapBody,
    Rename,
    Close,
    ClosePane,
    SplitBeside,
    NewTab,
}

#[derive(Clone)]
enum Action {
    Picker,
    Menu(String),
    /// 방 접기·펴기(배지).
    Room(String),
    /// 그 기기의 방을 여기 방으로 연다 — 카드 머리나 칸. `focus` 는 누른 칸의 원격 id.
    Open { label: String, window: Option<u64>, room: String, focus: Option<String> },
    /// 그 기기에 새 방을 만들고 바로 보기 창으로 연다(절 머리의 +).
    NewRoom(String),
    /// 이 기기에 새 방 — 머리줄의 +. 기기 절과 같은 자리·같은 모양(2026-09-17 지시).
    NewLocalRoom,
    /// 아래 절의 머리줄 — 누르는 일은 없고 우클릭 메뉴의 대상만 된다.
    Section(String),
    Unpin(String),
    /// 카드 머리의 × — 그 기기의 방을 닫는다(확인은 본기기 방과 같은 모달).
    CloseRoom { label: String, window: Option<u64>, room: String },
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

fn room_key(machine: &str, window: Option<u64>, room: &str) -> String {
    let identity = match window {
        Some(window) => format!("window:{window}"),
        None => format!("name:{room}"),
    };
    format!("{}:{machine}{identity}", machine.len())
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

/// 기기의 줄을 방으로 묶는다 — 정본은 원본 기기의 방 번호(`window`)다. 이름으로 묶으면
/// 옛 판 기기가 pane 마다 폴더 꼬리로 이름을 달아 한 방이 셋으로 갈라진다. 번호가 없는
/// 줄(옛 판·닫힘)만 이름으로 묶는다. 방 이름은 그 방 첫 줄의 것.
pub(crate) fn rooms(machine: &state::MachinesColMachine) -> Vec<(String, Vec<&state::MachinesColRow>)> {
    let mut out: Vec<(Option<u64>, String, Vec<&state::MachinesColRow>)> = Vec::new();
    for row in rows(machine) {
        let slot = out.iter_mut().find(|(w, room, _)| match (w, row.window) {
            (Some(a), Some(b)) => *a == b,
            (None, None) => *room == row.room,
            _ => false,
        });
        match slot {
            Some((_, _, list)) => list.push(row),
            None => out.push((row.window, row.room.clone(), vec![row])),
        }
    }
    out.sort_by_key(|(w, _, _)| w.unwrap_or(u64::MAX));
    out.into_iter().map(|(_, room, list)| (room, list)).collect()
}

/// 배치도 칸 하나에 앉는 줄들 — 바깥 pane 과 그 탭들. 첫 줄이 칸의 대표(좌표·얼굴).
fn decks<'a>(list: &[&'a state::MachinesColRow]) -> Vec<Vec<&'a state::MachinesColRow>> {
    let mut out: Vec<(String, Vec<&'a state::MachinesColRow>)> = Vec::new();
    for row in list {
        let key = row.tab_of.clone().unwrap_or_else(|| row.remote_id.clone());
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, deck)) => {
                if row.tab_of.is_none() { deck.insert(0, row); } else { deck.push(row); }
            }
            None => out.push((key, vec![row])),
        }
    }
    out.into_iter().map(|(_, deck)| deck).collect()
}

/// 원본이 준 방 이름은 「이름 · 폴더」 한 줄이다 — 본기기 카드처럼 두 줄로 가른다.
fn room_title(room: &str) -> (&str, &str) {
    if room.is_empty() {
        return ("방 이름 없음", "");
    }
    room.split_once(" · ").unwrap_or((room, ""))
}

/// 방 카드 본문(배치도) 높이 — 본기기 `sidebar_card_metrics` 와 같은 식이라 두 기기의
/// 카드가 같은 키로 선다.
/// 카드 본문 높이 — 본기기 `sidebar_card_metrics` 와 같은 식. 목록이면 줄 수만큼.
fn room_body_h(panes: usize, listed: bool) -> f32 {
    if listed {
        panes as f32 * SIDEBAR_ROW_H + SIDEBAR_ROW_PAD
    } else {
        (36.0 + 13.0 * panes as f32).clamp(46.0, 150.0)
    }
}

fn content_height(
    machine: &state::MachinesColMachine,
    collapsed: &std::collections::HashSet<String>,
    listed: &std::collections::HashSet<String>,
) -> f32 {
    let groups = rooms(machine);
    let mut height = 0.0;
    for (room, list) in &groups {
        height += SIDEBAR_TAB_H;
        let key = room_key(&machine.label, list[0].window, room);
        if !collapsed.contains(&key) { height += room_body_h(decks(list).len(), listed.contains(&key)); }
        height += SIDEBAR_TAB_GAP;
    }
    if groups.is_empty() { height = PANE_H; }
    if machine.closed > 0 { height += PANE_H; }
    height + 8.0
}

/// 배치도 칸 — 원본 기기가 준 칸(0..1)을 상자에 얹는다. 옛 판 기기라 칸이 없으면
/// 가로로 고르게 나눈다. 1px 씩 깎아 칸 사이 틈을 낸다(본기기 배치도와 같은 규칙).
fn cell_rects(list: &[&state::MachinesColRow], ma: Rect) -> Vec<Rect> {
    let n = list.len().max(1) as f32;
    let placed = list.iter().all(|r| r.rect.is_some());
    list.iter().enumerate().map(|(k, row)| {
        let [x, y, w, h] = match row.rect {
            Some(r) if placed => r,
            _ => [k as f32 / n, 0.0, 1.0 / n, 1.0],
        };
        (ma.0 + x * ma.2, ma.1 + y * ma.3, (w * ma.2 - 1.0).max(2.0), (h * ma.3 - 1.0).max(2.0))
    }).collect()
}

/// 방과 기기가 같은 스크롤을 쓰므로 각 절은 내용의 실제 높이를 유지한다.
fn section_heights(
    machines: &[state::MachinesColMachine],
    pinned: &[Pinned],
    collapsed: &std::collections::HashSet<String>,
    listed: &std::collections::HashSet<String>,
    _avail: f32,
) -> Vec<f32> {
    if pinned.is_empty() { return Vec::new(); }
    pinned.iter().map(|pin| {
        let body = machines.iter().find(|m| m.label == pin.label)
            .map_or(PANE_H, |m| content_height(m, collapsed, listed));
        SECTION_H + body
    }).collect()
}

/// 렌더 전에 읽어도 같은 순서를 돌려줘야 첫 프레임부터 단축키와 높이가 일치한다.
pub(crate) fn section_labels(info: &state::InfoState) -> Vec<String> {
    let nav = &info.navigation;
    let mut labels: Vec<String> = info.machines_col.machines.iter()
        .filter(|m| (m.online || nav.pinned.iter().any(|p| p.label == m.label))
            && !nav.dismissed.contains(&m.label))
        .map(|m| m.label.clone()).collect();
    for pin in &nav.pinned {
        if !labels.contains(&pin.label) {
            labels.push(pin.label.clone());
        }
    }
    labels
}

pub(crate) fn remote_content_h(info: &state::InfoState) -> f32 {
    section_labels(info).iter().map(|label| {
        SECTION_H + info.machines_col.machines.iter().find(|m| &m.label == label)
            .map_or(PANE_H, |m| content_height(m, &info.navigation.collapsed_rooms, &info.navigation.list_rooms))
    }).sum()
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

/// 배치도 칸 하나 — 본기기 배치도 칸(render.rs)과 **같은 문법·같은 치수**: 테두리 2px 둥근
/// 사각, 기다림이면 칸째 숨쉬기, 얼굴·걷기, 바닥 working 바, 탭은 점 줄. 눌리는 사각은
/// 잘린 칸이다 — 보이지 않는 부분이 눌리면 안 된다.
fn draw_cell(
    g: &mut gpu::GpuRenderer,
    hits: &mut Vec<(Action, Rect)>,
    machine: &state::MachinesColMachine,
    room: &str,
    deck: &[&state::MachinesColRow],
    cur: bool,
    cell: Rect,
    cursor: (f32, f32),
    view: Rect,
    drag: Option<&str>,
) {
    let row = deck[0];
    let (mx, my, mw, mh) = cell;
    let Some(visible) = clipped(cell, view) else { return };
    let hover = hit(cursor, visible);
    // 끌고 있는 칸은 옅게, 놓일 칸은 테두리로 — 본기기 pane 끌기와 같은 신호.
    let dragging = drag.is_some_and(|d| d == row.remote_id.as_str());
    let drop_here = drag.is_some_and(|d| d != row.remote_id.as_str()) && hover;
    g.hover_pointer |= hover && !row.closed;
    let busy = deck.iter().any(|r| matches!(r.status.as_str(), "working" | "compacting"));
    let waiting = deck.iter().any(|r| r.status.contains("wait") || r.status.contains("attention"));
    let signal = waiting.then(|| (theme::attention(), 0.9));
    round_rect(g, mx, my, mw, mh, 2.0, if drop_here {
        theme::accent()
    } else if let Some((c, _)) = signal {
        c
    } else if cur {
        theme::accent()
    } else if hover {
        theme::surface_hover()
    } else {
        // 칸은 기기색을 문다 — 본기기 배치도의 다른 기기 칸과 같은 규칙.
        crate::render::pane_identity::minimap_border(theme::panel_bg(), Some(&machine.label), theme::with_alpha(theme::border(), 0x66))
    });
    if mw > 5.0 && mh > 5.0 {
        round_rect(g, mx + 1.5, my + 1.5, mw - 3.0, mh - 3.0, 1.5,
            crate::render::pane_identity::minimap_background(theme::panel_bg(), Some(&machine.label)));
    }
    if let Some((col, period)) = signal {
        if mw > 5.0 && mh > 5.0 {
            let mut c = col;
            c[3] = (30.0 + 120.0 * crate::sprites::blink(crate::sprites::anim_phase_secs(), period)) as u8;
            round_rect(g, mx + 1.5, my + 1.5, mw - 3.0, mh - 3.0, 1.5, c);
        }
    }
    let (fx, fy, face) = crate::render::minimap_face_box(mx, my, mw, mh);
    let phase = crate::sprites::anim_phase_secs();
    let who = deck.iter().find(|r| !r.name.is_empty()).map_or("", |r| r.name.as_str());
    let walked = busy && crate::sprites::draw_student_walk(g, who, fx - 2.0, fy - 2.0, face + 4.0, phase);
    if !walked && !crate::sprites::draw_student_face_anim(g, who, fx, fy, face, phase) {
        let size = face.min(16.0);
        g.queue_icon("terminal", mx + (mw - size) / 2.0, my + (mh - size) / 2.0, size, theme::text_dim());
    }
    let has_bar = crate::render::minimap_has_bar(mw, mh);
    if has_bar && busy {
        let (bar_h, pad) = (crate::render::MINI_BAR_H, crate::render::MINI_BAR_PAD);
        g.working_bar(mx + 2.0, my + mh - bar_h - pad, mw - 4.0, bar_h, theme::accent());
    }
    // 탭은 바닥의 점 줄 — 첫 점(바깥 자리)이 넓고, 나머지는 흐리게. 본기기와 같은 그림.
    if deck.len() > 1 && mw > 16.0 && mh > 16.0 {
        let (dot, gap) = (2.5, 1.5);
        let dy = if has_bar {
            my + mh - crate::render::MINI_BAR_H - crate::render::MINI_BAR_PAD - dot - 2.0
        } else {
            my + mh - dot - 2.0
        };
        let mut dx = mx + 3.0;
        for (k, _) in deck.iter().take(6).enumerate() {
            let w = if k == 0 { dot * 1.8 } else { dot };
            let col = if k == 0 { theme::text_dim() } else { theme::with_alpha(theme::text_mute(), 0x70) };
            round_rect(g, dx, dy, w, dot, dot / 2.0, col);
            dx += w + gap;
        }
    }
    if dragging && mw > 5.0 && mh > 5.0 {
        round_rect(g, mx + 1.5, my + 1.5, mw - 3.0, mh - 3.0, 1.5, theme::with_alpha(theme::bg(), 0x88));
    }
    if !row.closed {
        hits.push((Action::Open {
            label: machine.label.clone(), window: row.window, room: room.to_string(),
            focus: Some(deck.iter().find(|r| !r.name.is_empty()).unwrap_or(&row).remote_id.clone()),
        }, visible));
    }
}

/// 목록 본문의 한 줄 — 본기기 목록 줄(render.rs)과 같은 문법: 얼굴(도는 중이면 걷기)·
/// 이름·줄 끝 상태 점(기다림이면 깜빡). 누르면 그 자리로 보기 창을 연다.
fn draw_list_row(
    g: &mut gpu::GpuRenderer,
    hits: &mut Vec<(Action, Rect)>,
    machine: &state::MachinesColMachine,
    room: &str,
    deck: &[&state::MachinesColRow],
    cur: bool,
    row: Rect,
    separator: bool,
    cursor: (f32, f32),
    view: Rect,
) {
    let head = deck[0];
    let (rx, ry, rw, rh) = row;
    let Some(visible) = clipped(row, view) else { return };
    if separator {
        g.rect(rx + 6.0, ry, rw - 12.0, 1.0, theme::with_alpha(theme::border(), 0x50));
    }
    let hover = hit(cursor, visible);
    g.hover_pointer |= hover && !head.closed;
    if hover { round_rect(g, rx, ry, rw, rh, theme::radius_sm(), theme::surface_hover()); }
    if cur { g.rect(rx, ry + 3.0, 2.0, rh - 6.0, theme::accent()); }
    let busy = deck.iter().any(|r| matches!(r.status.as_str(), "working" | "compacting"));
    let waiting = deck.iter().any(|r| r.status.contains("wait") || r.status.contains("attention"));
    let who = deck.iter().find(|r| !r.name.is_empty()).map_or("", |r| r.name.as_str());
    let phase = crate::sprites::anim_phase_secs();
    let face = rh - 6.0;
    let walked = busy && crate::sprites::draw_student_walk(g, who, rx + 5.0, ry + 1.0, rh - 2.0, phase);
    let has_face = walked || crate::sprites::draw_student_face_anim(g, who, rx + 7.0, ry + 3.0, face, phase);
    let col = if head.closed { theme::text_mute() } else if busy { theme::accent() } else { theme::success() };
    if !has_face { circle_rect(g, rx + 9.0, ry + rh / 2.0 - 3.0, 6.0, col); }
    let name_x = rx + 7.0 + face + 6.0;
    let label = if !who.is_empty() { who } else if !head.title.is_empty() { head.title.as_str() } else { head.remote_id.as_str() };
    text(g, label, name_x, ry + (rh - 11.0) / 2.0, (rx + rw - 14.0 - name_x).max(0.0), 11.0,
        if head.closed { theme::text_mute() } else if cur { theme::text() } else { theme::text_dim() }, false);
    let (dot_x, dot_y) = (rx + rw - 6.0, ry + rh / 2.0 - 3.0);
    if waiting {
        crate::sprites::blink_dot(g, dot_x, dot_y, 6.0, theme::attention(), 0.9);
    } else {
        circle_rect(g, dot_x, dot_y, 6.0, col);
    }
    if !head.closed {
        hits.push((Action::Open {
            label: machine.label.clone(), window: head.window, room: room.to_string(),
            focus: Some(deck.iter().find(|r| !r.name.is_empty()).unwrap_or(&head).remote_id.clone()),
        }, visible));
    }
}

/// 방 카드 우클릭 메뉴 — 본기기 방 메뉴와 같은 골격·치수·자리 규칙(사이드바 안에 가둔다).
/// 렌더가 칼럼의 맨 끝에 부른다 — 카드 위에 떠야 한다.
pub(crate) fn draw_menu(g: &mut gpu::GpuRenderer, info: &mut state::InfoState, cursor: (f32, f32), width: f32, bottom: f32) {
    let nav = &mut info.navigation;
    nav.room_menu_rects.clear();
    let Some(menu) = nav.room_menu.as_ref() else { return };
    let listed = nav.list_rooms.contains(&menu.key);
    // 칸에서 열었으면 칸 메뉴 — 본기기 pane 메뉴와 같은 자리에 같은 골격으로 뜬다.
    let items: Vec<(RoomMenuAction, &str)> = if menu.pane.is_some() {
        vec![
            (RoomMenuAction::SplitBeside, "옆에 세우기"),
            (RoomMenuAction::NewTab, "탭으로 세우기"),
            (RoomMenuAction::ClosePane, "pane 닫기"),
        ]
    } else {
        vec![
            if listed { (RoomMenuAction::MapBody, "배치도로 보기") } else { (RoomMenuAction::ListBody, "목록으로 보기") },
            (RoomMenuAction::Rename, "이름 바꾸기"),
            (RoomMenuAction::Close, "방 닫기"),
        ]
    };
    const MIH: f32 = 28.0;
    let widest = items.iter().map(|(_, l)| g.measure_chrome_text(l, 13.0, false)).fold(0.0f32, f32::max);
    let mw = (widest + 32.0).min((width - 8.0).max(80.0));
    let mh = 12.0 + items.len() as f32 * MIH;
    let mx = menu.x.min((width - mw - 4.0).max(4.0)).max(4.0);
    let my = menu.y.min((bottom - mh - 6.0).max(TITLE_HEIGHT)).max(TITLE_HEIGHT);
    panel_rect_outlined(g, mx, my, mw, mh, theme::radius_md(), theme::surface());
    for (i, (action, label)) in items.iter().enumerate() {
        let r = (mx + 4.0, my + 6.0 + i as f32 * MIH, mw - 8.0, MIH);
        let hov = hit(cursor, r);
        g.hover_pointer |= hov;
        if hov { hover_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm()); }
        g.draw_text(r.0 + 12.0, r.1 + (MIH - 13.0) / 2.0, label,
            gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
        nav.room_menu_rects.push((*action, r));
    }
}

/// 한 기기의 방을 본기기와 같은 **카드 + 배치도**로 `view` 안에 그린다. 위 목록과
/// 아래 절이 같은 함수를 쓴다 — 두 자리의 방이 달리 보이면 같은 방인지 헷갈린다.
/// 카드를 그리는 데 필요한 nav 상태 — 위 목록과 아래 절이 같은 것을 본다.
struct RowsCtx<'a> {
    numbers: &'a [(String, Option<u64>, String, usize)],
    collapsed: &'a std::collections::HashSet<String>,
    listed: &'a std::collections::HashSet<String>,
    /// 지금 끌고 있는 칸의 원격 pane id.
    drag: Option<&'a str>,
    viewing: Option<&'a (String, Vec<String>)>,
    viewing_cur: Option<&'a str>,
    rename: Option<&'a (String, String)>,
}

fn draw_rows(
    g: &mut gpu::GpuRenderer,
    hits: &mut Vec<(Action, Rect)>,
    ctx: &RowsCtx,
    machine: &state::MachinesColMachine,
    cursor: (f32, f32),
    width: f32,
    view: Rect,
    scroll: f32,
) {
    g.push_clip(view.0, view.1, view.2, view.3);
    let tab_x = SIDEBAR_TAB_INSET;
    let tab_w = (width - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
    let mut y = view.1 - scroll;
    let groups = rooms(machine);
    if groups.is_empty() {
        let message = if machine.online { "열린 pane 없음" } else { "기기에 연결할 수 없음" };
        text(g, message, 16.0, y + 12.0, width - 32.0, 11.0, theme::text_dim(), false);
        y += PANE_H;
    }
    let active_room = groups.iter().position(|(_, list)| ctx.viewing.is_some_and(|(label, ids)| {
        *label == machine.label && list.iter().any(|r| ids.contains(&r.remote_id))
    }));
    for (index, (room, list)) in groups.iter().enumerate() {
        let key = room_key(&machine.label, list[0].window, room);
        let collapsed = ctx.collapsed.contains(&key);
        let listed = ctx.listed.contains(&key);
        let stacks = decks(list);
        let body_h = if collapsed { 0.0 } else { room_body_h(stacks.len(), listed) };
        let h = SIDEBAR_TAB_H + body_h;
        let head = (tab_x, y, tab_w, SIDEBAR_TAB_H);
        let active = active_room == Some(index);
        if let Some(head_visible) = clipped(head, view) {
            let hover = hit(cursor, head_visible);
            g.hover_pointer |= hover;
            if active {
                panel_rect(g, tab_x, y, tab_w, h, theme::radius_md(), theme::surface_active());
            } else if hover {
                panel_rect(g, tab_x, y, tab_w, h, theme::radius_md(), theme::surface_hover());
            }
            // 본기기 카드와 같은 문법: 카드 사이 흐린 구분선(활성 카드 앞뒤엔 없음).
            if !active && index + 1 < groups.len() && active_room != Some(index + 1) {
                let ly = (y + h + SIDEBAR_TAB_GAP / 2.0).round();
                g.rect(tab_x + 10.0, ly, tab_w - 20.0, 1.0, theme::with_alpha(theme::border(), 0x60));
            }
            // 접힌 방의 기다림은 머리의 숨쉬는 점이 말한다 — 펴진 방은 칸이 말하므로 조용히.
            let waits = list.iter().any(|r| r.status.contains("wait") || r.status.contains("attention"));
            if collapsed && waits {
                crate::sprites::blink_dot(g, tab_x + 12.0, y + 13.0, 9.0, theme::attention(), 0.9);
            }
            // 펼침 배지 — 본기기 `window_expand_rect` 와 같은 pill(칸 수 포함).
            let badge_w = if stacks.len() >= 10 { 44.0 } else { 37.0 };
            let badge = (tab_x + tab_w - 8.0 - badge_w, y + 26.0, badge_w, 20.0);
            let (name, folder) = room_title(room);
            let compact = tab_w < 180.0;
            let text_x = tab_x + if compact { 8.0 } else { 26.0 };
            let tab_right = tab_x + tab_w;
            // 이름 줄 오른쪽엔 ×(본기기 카드와 같은 자리: 이름 줄 가운데, 오른쪽 끝 3px).
            let cs = 14.0;
            let close = (tab_x + tab_w - cs - 3.0, y + 11.0, cs, cs);
            let number = ctx.numbers.iter().find(|(label, window, fallback, _)| {
                label == &machine.label && match (*window, list[0].window) {
                    (Some(a), Some(b)) => a == b,
                    (None, None) => fallback == room,
                    _ => false,
                }
            }).map(|(_, _, _, n)| *n);
            let kbd = number.filter(|n| *n < 9).map(|n| format!("⌘{}", n + 1));
            let kbd_w = kbd.as_ref().map_or(0.0, |k| g.measure_chrome_text(k, 11.0, false) + 6.0);
            let name_budget = if compact { tab_right - 8.0 - text_x } else { close.0 - 6.0 - kbd_w - text_x }.max(0.0);
            if let Some(kbd) = kbd {
                let (kx, ky, kw) = if compact { (text_x, y + 31.0, (badge.0 - 4.0 - text_x).max(0.0)) }
                    else { (close.0 - 6.0 - kbd_w, y + 13.0, kbd_w) };
                text(g, &kbd, kx, ky, kw, 11.0, theme::text_mute(), false);
            }
            let folder_budget = (tab_right - 8.0 - (badge_w + 14.0) - text_x).max(0.0);
            let name_y = if folder.is_empty() && !compact { y + ((SIDEBAR_TAB_H - 17.0) / 2.0).round() } else { y + 11.0 };
            // 이름을 고치는 중이면 본기기처럼 버퍼(캐럿 포함)가 이름 자리에 선다.
            let renaming = ctx.rename.filter(|(k, _)| *k == key).map(|(_, t)| t.as_str());
            text(g, renaming.unwrap_or(name), text_x, name_y, name_budget, 13.5,
                if active || renaming.is_some() { theme::text() } else { theme::text_dim() }, active);
            if !compact {
                let x_hover = hit(cursor, close);
                if x_hover { hover_rect(g, close.0, close.1, close.2, close.3, theme::radius_sm()); }
                g.queue_icon("x", close.0 + (cs - 12.0) / 2.0, close.1 + (cs - 12.0) / 2.0, 12.0,
                    if x_hover { theme::text() } else { theme::text_mute() });
            }
            if !folder.is_empty() && !compact {
                text(g, folder, text_x, y + 30.0, folder_budget, 11.0, theme::text_dim(), false);
            }
            let badge_hover = hit(cursor, badge);
            if badge_hover { hover_rect(g, badge.0, badge.1, badge.2, badge.3, theme::radius_sm()); }
            let fg = if badge_hover { theme::text() } else { theme::lerp(theme::text_dim(), theme::text(), 0.55) };
            g.queue_icon(if collapsed { "chevron-right" } else { "chevron-down" }, badge.0 + 5.0, badge.1 + 3.0, 14.0, fg);
            g.draw_text(badge.0 + 21.0, badge.1 + 5.0, &stacks.len().to_string(),
                gpu::DrawOpts { font_size: 11.0, color: fg, bold: false, italic: false });
            // 배지는 접고 펴고, 머리 나머지는 본기기 방 탭처럼 그 방으로 간다. 배지가
            // 앞이어야 한다 — 맞춤은 앞선 것이 이긴다.
            if let Some(c) = (!compact).then(|| clipped(close, view)).flatten() {
                hits.push((Action::CloseRoom { label: machine.label.clone(), window: list[0].window, room: room.clone() }, c));
            }
            if let Some(b) = clipped(badge, view) { hits.push((Action::Room(key.clone()), b)); }
            hits.push((Action::Open {
                label: machine.label.clone(), window: list[0].window, room: room.clone(), focus: None,
            }, head_visible));
        }
        if !collapsed && listed {
            // 목록 보기 — 배치도 자리에 학생 줄(본기기 목록 줄과 같은 기하).
            for (k, deck) in stacks.iter().enumerate() {
                let row = (tab_x + 8.0, y + SIDEBAR_TAB_H + SIDEBAR_ROW_PAD / 2.0 + k as f32 * SIDEBAR_ROW_H,
                    tab_w - 16.0, SIDEBAR_ROW_H);
                let cur = active && ctx.viewing_cur.is_some_and(|c| deck.iter().any(|r| r.remote_id == c));
                draw_list_row(g, hits, machine, room, deck, cur, row, k > 0, cursor, view);
            }
        } else if !collapsed {
            let ma = (tab_x + 10.0, y + SIDEBAR_TAB_H + 3.0, tab_w - 20.0, body_h - 8.0);
            let heads: Vec<&state::MachinesColRow> = stacks.iter().map(|d| d[0]).collect();
            for (deck, cell) in stacks.iter().zip(cell_rects(&heads, ma)) {
                let cur = active && ctx.viewing_cur.is_some_and(|c| deck.iter().any(|r| r.remote_id == c));
                draw_cell(g, hits, machine, room, deck, cur, cell, cursor, view, ctx.drag);
            }
        }
        y += h + SIDEBAR_TAB_GAP;
    }
    if machine.closed > 0 {
        text(g, &format!("닫힌 pane {} · 원본에서 되살리기", machine.closed), 16.0, y + 12.0, width - 32.0, 10.0, theme::text_dim(), false);
    }
    g.pop_clip();
}

pub(crate) fn draw(g: &mut gpu::GpuRenderer, info: &mut state::InfoState, cursor: (f32, f32), width: f32, viewport: Rect) {
    let labels = section_labels(info);
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
    // 고른 기기엔 「…」 메뉴와 「+ 새 방」 이, 이 기기엔 「+ 새 방」 이 머리 오른쪽에 선다 —
    // 방 추가 단추가 어느 기기든 같은 자리에 있어야 한다.
    let menu_w = if selected.is_some() { 56.0 } else { 28.0 };
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
        let plus = (head.0 + head.2 - menu_w, head.1, 28.0, head.3);
        let r = (head.0 + head.2 - 28.0, head.1, 28.0, head.3);
        for (rect, icon) in [(plus, "plus"), (r, "ellipsis-horizontal")] {
            if hit(cursor, rect) { g.rect(rect.0, rect.1, rect.2, rect.3, theme::surface_hover()); }
            g.hover_pointer |= hit(cursor, rect);
            g.queue_icon(icon, rect.0 + 7.0, rect.1 + 13.0, 14.0, theme::text_dim());
        }
        nav.hits.push((Action::NewRoom(machine.label.clone()), plus));
        nav.hits.push((Action::Menu(machine.label.clone()), r));
    } else {
        let plus = (head.0 + head.2 - 28.0, head.1, 28.0, head.3);
        if hit(cursor, plus) { g.rect(plus.0, plus.1, plus.2, plus.3, theme::surface_hover()); }
        g.hover_pointer |= hit(cursor, plus);
        g.queue_icon("plus", plus.0 + 7.0, plus.1 + 13.0, 14.0, theme::text_dim());
        nav.hits.push((Action::NewLocalRoom, plus));
    }
    g.rect(12.0, TITLE_HEIGHT + HEADER_H, (width - 24.0).max(0.0), 1.0, theme::border());

    // 끊긴 기기도 머리줄을 남겨 일시적인 연결 실패가 방 삭제처럼 보이지 않게 한다.
    for m in machines.iter().filter(|m| m.online) {
        if nav.machine.as_deref() == Some(m.label.as_str())
            || nav.dismissed.contains(&m.label)
            || nav.pinned.iter().any(|p| p.label == m.label)
        {
            continue;
        }
        nav.pinned.push(Pinned { label: m.label.clone(), content_h: 0.0, view: None });
    }
    nav.pinned.sort_by_key(|p| labels.iter().position(|l| l == &p.label).unwrap_or(usize::MAX));
    let NavigationState { pinned, collapsed_rooms, hits, machine: main, scroll, content_h, viewport: main_view, viewing, viewing_cur, list_rooms, rename, cell_drag, local_content_h, shared_scroll, room_numbers, .. } = nav;
    let ctx = RowsCtx {
        numbers: room_numbers,
        collapsed: collapsed_rooms,
        listed: list_rooms,
        drag: cell_drag.as_ref().filter(|d| d.active).map(|d| d.pane.as_str()),
        viewing: viewing.as_ref(),
        viewing_cur: viewing_cur.as_deref(),
        rename: rename.as_ref(),
    };
    let heights = section_heights(machines, pinned, collapsed_rooms, list_rooms, viewport.3);
    let view = viewport;
    *main_view = Some(view);
    if main.is_some() {
        match selected {
            None => text(g, "기기를 다시 선택해 주세요", 16.0, view.1 + 12.0, width - 32.0, 11.0, theme::text_dim(), false),
            Some(machine) => {
                *content_h = content_height(machine, collapsed_rooms, list_rooms);
                *scroll = scroll.clamp(0.0, (*content_h - view.3).max(0.0));
                draw_rows(g, hits, &ctx, machine, cursor, width, view, *scroll);
                scrollbar(g, view, *content_h, *scroll, width);
            }
        }
    }
    if main.is_some() { return; }
    let mut y = viewport.1 + *local_content_h - *shared_scroll;
    g.push_clip(viewport.0, viewport.1, viewport.2, viewport.3);
    for (pin, h) in pinned.iter_mut().zip(heights) {
        let machine = machines.iter().find(|m| m.label == pin.label);
        g.rect(12.0, y, (width - 24.0).max(0.0), 1.0, theme::border());
        let head = (8.0, y + 2.0, width - 16.0, SECTION_H - 4.0);
        let unpin = (width - 32.0, y + 7.0, 22.0, 22.0);
        let menu = (width - 54.0, y + 7.0, 22.0, 22.0);
        let plus = (width - 76.0, y + 7.0, 22.0, 22.0);
        if hit(cursor, head) { g.rect(head.0, head.1, head.2, head.3, theme::surface_hover()); }
        let tint = if machine.is_some() { crate::render::machine_tint(&pin.label) } else { theme::text_dim() };
        g.queue_icon("monitor", 14.0, y + 11.0, 14.0, tint);
        text(g, &pin.label, 36.0, y + 4.0, width - 118.0, 11.5, theme::text(), true);
        let sub = machine.map_or_else(|| "등록되지 않은 기기".to_string(), status);
        text(g, &sub, 36.0, y + 20.0, width - 118.0, 9.5, theme::text_dim(), false);
        g.hover_pointer |= hit(cursor, menu) || hit(cursor, unpin) || hit(cursor, plus);
        // 「+」 = 그 기기에 새 방(본기기 사이드바의 + 와 같은 뜻). 연결돼 있을 때만.
        if machine.is_some_and(|m| m.online) {
            g.queue_icon("plus", plus.0 + 4.0, plus.1 + 4.0, 14.0, if hit(cursor, plus) { theme::text() } else { theme::text_dim() });
        }
        g.queue_icon("ellipsis-horizontal", menu.0 + 4.0, menu.1 + 4.0, 14.0, theme::text_dim());
        g.queue_icon("x", unpin.0 + 5.0, unpin.1 + 5.0, 12.0, if hit(cursor, unpin) { theme::attention() } else { theme::text_dim() });
        // 작은 단추가 머리줄보다 앞이어야 한다 — 맞춤은 앞선 것이 이긴다.
        if let Some(r) = clipped(unpin, viewport) { hits.push((Action::Unpin(pin.label.clone()), r)); }
        if let Some(r) = clipped(menu, viewport) { hits.push((Action::Menu(pin.label.clone()), r)); }
        if machine.is_some_and(|m| m.online) {
            if let Some(r) = clipped(plus, viewport) { hits.push((Action::NewRoom(pin.label.clone()), r)); }
        }
        if let Some(r) = clipped(head, viewport) { hits.push((Action::Section(pin.label.clone()), r)); }
        let body = (viewport.0, y + SECTION_H, viewport.2, (h - SECTION_H).max(0.0));
        pin.view = clipped(body, viewport);
        match machine {
            None => text(g, "기기 목록에서 사라졌어요", 16.0, body.1 + 12.0, width - 32.0, 11.0, theme::text_dim(), false),
            Some(machine) => {
                pin.content_h = content_height(machine, collapsed_rooms, list_rooms);
                if let Some(visible) = pin.view {
                    draw_rows(g, hits, &ctx, machine, cursor, width, visible, visible.1 - body.1);
                }
            }
        }
        y += h;
    }
    g.pop_clip();
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
        // Autoinfo rebuilds its display-only fixture every tick, including between probe stages.
        if step.1 >= 6 {
            if let Some(machine) = self.info.machines_col.machines.iter_mut().find(|m| m.label == "맥북") {
                if !machine.remote.iter().any(|row| row.remote_id == "%nav-probe-0") {
                    if let Some(template) = machine.remote.first().or(machine.mirrored.first()).cloned() {
                        for number in 0..6 {
                            let mut row = template.clone();
                            row.pane.clear();
                            row.remote_id = format!("%nav-probe-{number}");
                            row.window = Some(9000 + number);
                            row.room = format!("탐색 검사 {}", number + 1);
                            row.tab_of = None;
                            machine.remote.push(row);
                        }
                    }
                }
            }
        }
        if step.1 >= 19 || step.0.elapsed().as_millis() < if step.1 == 0 { 10000 } else { 700 } { return; }
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
                report = format!("shared_scroll={}", self.sidebar_scroll_px > 0.0);
            }
            7 => {
                report = format!("remote_rooms_reachable={} no_remote_sessions={}", self.info.navigation.hits.iter().any(|(a, _)| matches!(a, Action::Open { .. })), self.pty.keys().all(|id| !kasa_mcp::remote::is_remote_pane(id)));
                report.push_str(&format!(" room_navigation_count={} remote_room_count={}", self.room_navigation_count(), self.remote_room_navigation().len()));
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
                report = format!("pinned_view={} shared_viewport={} pinned_hits={}",
                    nav.pinned.first().is_some_and(|p| p.view.is_some()),
                    self.sidebar_avail_h(win_h) == self.sidebar_full_avail_h(win_h),
                    nav.hits.iter().any(|(a, _)| matches!(a, Action::Open { label, .. } if label == "맥북")));
            }
            12 => {
                // 아래 절의 방 카드 머리를 우클릭 — 본기기 방 메뉴와 같은 메뉴가 떠야 한다.
                let head = self.info.navigation.hits.iter()
                    .find_map(|(a, r)| matches!(a, Action::Open { label, .. } if label == "맥북").then_some(*r));
                if let Some(r) = head {
                    self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    self.sidebar_navigation_right_click(self.cursor_px);
                }
                report = format!("room_menu={}", self.info.navigation.room_menu.is_some());
            }
            13 => {
                // 첫 항목(목록으로 보기) — 본문이 학생 줄로 바뀌고 메뉴는 닫힌다.
                let item = self.info.navigation.room_menu_rects.first().map(|(_, r)| *r);
                if let Some(r) = item { self.sidebar_navigation_menu_click((r.0 + r.2 / 2.0, r.1 + r.3 / 2.0)); }
                let nav = &self.info.navigation;
                report = format!("listed={} menu_closed={}", !nav.list_rooms.is_empty(), nav.room_menu.is_none());
            }
            14 => {
                // 다시 우클릭 — 항목 사각은 다음 프레임의 그리기가 채우므로 누르기는 다음 단계.
                let head = self.info.navigation.hits.iter()
                    .find_map(|(a, r)| matches!(a, Action::Open { label, .. } if label == "맥북").then_some(*r));
                if let Some(r) = head {
                    self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    self.sidebar_navigation_right_click(self.cursor_px);
                }
                let close = self.info.navigation.hits.iter()
                    .find_map(|(a, r)| matches!(a, Action::CloseRoom { label, .. } if label == "맥북").then_some(*r));
                report = format!("room_menu_again={} close_hit={} close_accessible={} close={:?} scale={}", self.info.navigation.room_menu.is_some(),
                    close.is_some(), close.is_some() || self.info.navigation.room_menu.as_ref().is_some_and(|m| m.pane.is_none()), close, self.effective_scale());
            }
            15 => {
                // 둘째 항목(이름 바꾸기) — 편집칸(캐럿)이 카드 머리에 선다.
                let item = self.info.navigation.room_menu_rects.get(1).map(|(_, r)| *r);
                if let Some(r) = item { self.sidebar_navigation_menu_click((r.0 + r.2 / 2.0, r.1 + r.3 / 2.0)); }
                report = format!("renaming={}", self.room_rename.remote.is_some());
            }
            16 => {
                self.cancel_room_rename();
                report = format!("rename_cancelled={}", self.room_rename.remote.is_none() && self.room_rename.editing.is_none());
            }
            17 => {
                // 배치도 칸 우클릭 — 방 메뉴가 아니라 **칸 메뉴**가 떠야 한다.
                let cell = self.info.navigation.hits.iter().find_map(|(a, r)|
                    matches!(a, Action::Open { label, focus: Some(_), .. } if label == "맥북").then_some(*r));
                if let Some(r) = cell {
                    self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    self.sidebar_navigation_right_click(self.cursor_px);
                }
                report = format!("cell_menu={}", self.info.navigation.room_menu.as_ref().is_some_and(|m| m.pane.is_some()));
            }
            18 => {
                let items = self.info.navigation.room_menu_rects.len();
                let first = self.info.navigation.room_menu_rects.first().map(|(_, r)| *r);
                if let Some(r) = first { self.sidebar_navigation_menu_click((r.0 + r.2 / 2.0, r.1 + r.3 / 2.0)); }
                report = format!("cell_menu_items={items} menu_closed={}", self.info.navigation.room_menu.is_none());
            }
            _ => {}
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window { window.request_redraw(); }
        let capture = std::env::var("KASATERM_AUTONAV_CAPTURE_STAGES")
            .map(|stages| stages.split(',').filter_map(|v| v.trim().parse::<usize>().ok()).any(|v| v == stage))
            .unwrap_or(true);
        if capture {
            if let Some(g) = &mut self.gpu { g.capture_next = Some(format!("{folder}/stage-{stage}.png")); }
        }
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

    fn select_navigation_machine(&mut self, label: Option<String>) {
        let nav = &mut self.info.navigation;
        if let Some(label) = &label { nav.dismissed.remove(label); }
        nav.machine = label;
        nav.scroll = 0.0;
    }

    /// 기기를 아래 절로 붙인다. 위 목록에 서 있던 기기면 위는 이 기기로 돌아간다 —
    /// 「밑으로 끌어 내린」 것이지 복사한 것이 아니다.
    fn pin_navigation_machine(&mut self, label: String) {
        let nav = &mut self.info.navigation;
        if nav.machine.as_deref() == Some(label.as_str()) {
            nav.machine = None;
            nav.scroll = 0.0;
        }
        nav.dismissed.remove(&label);
        if !nav.pinned.iter().any(|p| p.label == label) {
            nav.pinned.push(Pinned { label, content_h: 0.0, view: None });
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
            Action::Open { label, window, room, focus } => match focus {
                // 칸은 잡아 끌 수 있다 — 문턱을 안 넘고 놓으면 그냥 연다.
                Some(pane) => {
                    if let Some(machine) = kasa_mcp::machines::find(&label) {
                        self.begin_drag_identity(crate::drag_transfer::DragEndpoint::RemotePane {
                            label: label.clone(), base: machine.base, pane: pane.clone(),
                        });
                    }
                    self.info.navigation.cell_drag = Some(CellDrag {
                        label, room, window, pane, start: cursor, active: false,
                    });
                }
                None => {
                    if let Err(e) = self.open_remote_room(&label, window, &room, None) {
                        self.set_toast(format!("방 열기 실패 — {e:#}"));
                    }
                    self.info.machines_col.last_refresh = None;
                }
            },
            Action::NewRoom(label) => {
                if let Err(e) = self.new_remote_room(&label) {
                    self.set_toast(format!("{label} 에 새 방 실패 — {e:#}"));
                }
                self.info.machines_col.last_refresh = None;
            }
            Action::NewLocalRoom => self.new_window(),
            Action::Unpin(label) => {
                self.info.navigation.pinned.retain(|p| p.label != label);
                self.info.navigation.dismissed.insert(label);
            }
            Action::CloseRoom { label, window, room } => {
                self.confirm_or_close_remote_room(&label, window, &room);
            }
            Action::Room(key) => {
                if !self.info.navigation.collapsed_rooms.remove(&key) { self.info.navigation.collapsed_rooms.insert(key); }
            }
        }
        self.chrome_dirty = true;
        true
    }

    /// 커서가 움직일 때 — 잡은 기기가 문턱을 넘으면 끌기가 되고 피커는 닫힌다.
    /// 끌기 중이면 true 를 돌려 다른 hover 처리가 끼어들지 않게 한다.
    pub(crate) fn sidebar_navigation_drag_move(&mut self) -> bool {
        let cursor = self.cursor_px;
        if let Some(cell) = self.info.navigation.cell_drag.as_mut() {
            let (dx, dy) = (cursor.0 - cell.start.0, cursor.1 - cell.start.1);
            if !cell.active && dx * dx + dy * dy > 9.0 { cell.active = true; }
            if cell.active { self.chrome_dirty = true; }
            return cell.active;
        }
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
        if let Some(cell) = self.info.navigation.cell_drag.take() {
            if !cell.active {
                if let Err(e) = self.open_remote_room(&cell.label, cell.window, &cell.room, Some(&cell.pane)) {
                    self.set_toast(format!("방 열기 실패 — {e:#}"));
                }
                self.info.machines_col.last_refresh = None;
            } else {
                self.drop_remote_cell(&cell, cursor);
            }
            self.chrome_dirty = true;
            return true;
        }
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
        let action = self.info.navigation.hits.iter()
            .find(|(a, r)| matches!(a, Action::Open { .. }) && hit(cursor, *r))
            .or_else(|| self.info.navigation.hits.iter().find(|(_, r)| hit(cursor, *r)))
            .map(|(a, _)| a.clone());
        // 방 카드(머리·칸·×)는 방 메뉴, 절 머리·피커는 기기 메뉴.
        let action = match action {
            Some(Action::Open { label, window, room, focus }) => {
                let key = room_key(&label, window, &room);
                self.info.navigation.room_menu = Some(RoomMenu { x: cursor.0, y: cursor.1, label, window, room, key, pane: focus });
                self.info.machine_menu = None;
                self.chrome_dirty = true;
                return true;
            }
            Some(Action::CloseRoom { label, window, room }) => {
                let key = room_key(&label, window, &room);
                self.info.navigation.room_menu = Some(RoomMenu { x: cursor.0, y: cursor.1, label, window, room, key, pane: None });
                self.info.machine_menu = None;
                self.chrome_dirty = true;
                return true;
            }
            other => other,
        };
        let label = match action {
            Some(Action::Menu(label) | Action::Section(label) | Action::Unpin(label) | Action::NewRoom(label)) => Some(label),
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
        if event.state.is_pressed() && self.info.navigation.room_menu.is_some()
            && matches!(event.logical_key, Key::Named(NamedKey::Escape))
        {
            self.info.navigation.room_menu = None;
            self.chrome_dirty = true;
            return true;
        }
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
            name: String::new(), title: String::new(), status: String::new(), closed: false, rect: None,
            window: None, tab_of: None,
        };
        state::MachinesColMachine {
            label: "test".into(), online: true, ago_secs: None, outdated: false,
            host: String::new(), kvm: None, closed: 2,
            remote: vec![row("", "%12", "A"), row("", "%2", "A"), row("", "%8", "B")],
            mirrored: vec![row("%99", "%7", "A")],
        }
    }

    #[test]
    fn list_body_is_a_row_per_deck_like_the_local_card() {
        let m = machine();
        let (room, list) = rooms(&m).into_iter().next().unwrap();
        let key = room_key(&m.label, list[0].window, &room);
        let listed: std::collections::HashSet<String> = [key].into_iter().collect();
        let map = content_height(&m, &Default::default(), &Default::default());
        let as_list = content_height(&m, &Default::default(), &listed);
        let n = decks(&list).len();
        assert_eq!(as_list - map, room_body_h(n, true) - room_body_h(n, false));
        assert_eq!(room_body_h(n, true), n as f32 * SIDEBAR_ROW_H + SIDEBAR_ROW_PAD);
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
        let full = content_height(&m, &collapsed, &Default::default());
        collapsed.insert(room_key("test", None, "A"));
        assert_eq!(full - content_height(&m, &collapsed, &Default::default()), room_body_h(3, false));
        assert_eq!(rows(&m).len(), 4);
        assert_eq!(rooms(&m).iter().map(|(r, l)| (r.as_str(), l.len())).collect::<Vec<_>>(), [("A", 3), ("B", 1)]);
    }

    #[test]
    fn rooms_group_by_source_window_and_tabs_fold_into_one_cell() {
        let mut m = machine();
        for (row, (w, tab)) in m.remote.iter_mut().zip([(Some(3u64), None), (Some(3), Some("%12".to_string())), (Some(3), None)]) {
            row.window = w;
            row.room = if tab.is_some() { "다른 폴더".into() } else { "같은 방".into() };
            row.tab_of = tab;
        }
        let grouped = rooms(&m);
        assert_eq!(grouped.iter().map(|(r, l)| (r.as_str(), l.len())).collect::<Vec<_>>(), [("같은 방", 3), ("A", 1)]);
        let stacks = decks(&grouped[0].1);
        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks[0].iter().map(|r| r.remote_id.as_str()).collect::<Vec<_>>(), ["%8"]);
        assert_eq!(stacks[1].iter().map(|r| r.remote_id.as_str()).collect::<Vec<_>>(), ["%12", "%2"]);
        assert_eq!(content_height(&m, &Default::default(), &Default::default()),
            2.0 * (SIDEBAR_TAB_H + SIDEBAR_TAB_GAP) + room_body_h(2, false) + room_body_h(1, false) + PANE_H + 8.0);
    }

    #[test]
    fn cells_follow_source_layout_and_split_evenly_without_one() {
        let m = machine();
        let list = rooms(&m).remove(0).1;
        let ma = (10.0, 20.0, 100.0, 50.0);
        let even = cell_rects(&list, ma);
        assert_eq!(even.len(), 3);
        assert!((even[1].0 - (10.0 + 100.0 / 3.0)).abs() < 0.01);
        assert!((even[2].3 - 49.0).abs() < 0.01);
        let mut placed: Vec<state::MachinesColRow> = list.iter().map(|r| (*r).clone()).collect();
        placed[0].rect = Some([0.0, 0.0, 0.5, 1.0]);
        placed[1].rect = Some([0.5, 0.0, 0.5, 0.5]);
        placed[2].rect = Some([0.5, 0.5, 0.5, 0.5]);
        let refs: Vec<&state::MachinesColRow> = placed.iter().collect();
        let cells = cell_rects(&refs, ma);
        assert_eq!(cells[0], (10.0, 20.0, 49.0, 49.0));
        assert_eq!(cells[2], (60.0, 45.0, 49.0, 24.0));
    }

    #[test]
    fn offscreen_remote_rows_cannot_receive_clicks() {
        let viewport = (0.0, 100.0, 200.0, 200.0);
        assert_eq!(clipped((10.0, 80.0, 180.0, 44.0), viewport), Some((10.0, 100.0, 180.0, 24.0)));
        assert!(clipped((10.0, 50.0, 180.0, 44.0), viewport).is_none());
        assert!(!hit((20.0, 99.0), clipped((10.0, 80.0, 180.0, 44.0), viewport).unwrap()));
    }

    #[test]
    fn device_sections_keep_content_height_in_the_shared_scroll() {
        let pin = |label: &str| Pinned { label: label.into(), content_h: 0.0, view: None };
        let collapsed = std::collections::HashSet::new();
        let m = machine();
        let body = content_height(&m, &collapsed, &Default::default());
        assert_eq!(section_heights(&[machine()], &[pin("test")], &collapsed, &Default::default(), 1000.0), vec![SECTION_H + body]);
        assert_eq!(section_heights(&[machine()], &[pin("test")], &collapsed, &Default::default(), 300.0), vec![SECTION_H + body]);
        assert_eq!(section_heights(&[machine()], &[pin("없음")], &collapsed, &Default::default(), 1000.0), vec![SECTION_H + PANE_H]);
        assert!(section_heights(&[machine()], &[], &collapsed, &Default::default(), 1000.0).is_empty());
    }

    #[test]
    fn device_order_is_available_before_render_and_retains_offline_headers() {
        let mut info = state::InfoState::default();
        info.machines_col.machines = vec![machine()];
        assert_eq!(section_labels(&info), ["test"]);
        let height = remote_content_h(&info);
        assert!(height > SECTION_H);
        info.navigation.machine = Some("test".into());
        assert_eq!(section_labels(&info), ["test"]);
        info.navigation.pinned.push(Pinned { label: "test".into(), content_h: 0.0, view: None });
        info.machines_col.machines[0].online = false;
        assert_eq!(section_labels(&info), ["test"]);
        assert!(rooms(&info.machines_col.machines[0]).is_empty());
        info.navigation.pinned.clear();
        info.navigation.dismissed.insert("test".into());
        info.machines_col.machines[0].online = true;
        assert!(section_labels(&info).is_empty());
    }

    #[test]
    fn same_named_rooms_keep_distinct_source_windows_in_numeric_order() {
        let mut m = machine();
        m.mirrored.clear();
        for (row, window) in m.remote.iter_mut().zip([8, 2, 8]) {
            row.room = "same".into();
            row.window = Some(window);
        }
        let groups = rooms(&m);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.iter().map(|(_, rows)| rows[0].window).collect::<Vec<_>>(), [Some(2), Some(8)]);
        assert_eq!(groups[1].1.len(), 2);
        let key = room_key(&m.label, Some(8), "same");
        assert_ne!(key, room_key(&m.label, Some(2), "same"));
        assert_eq!(key, room_key(&m.label, Some(8), "renamed"));
        assert_ne!(key, room_key(&m.label, None, "window:8"));
        assert_ne!(room_key("test", None, "same"), room_key("other", None, "same"));
        let collapsed = [key.clone()].into_iter().collect();
        let full = content_height(&m, &Default::default(), &Default::default());
        assert_eq!(full - content_height(&m, &collapsed, &Default::default()), room_body_h(2, false));
        let listed = [key].into_iter().collect();
        assert_eq!(content_height(&m, &Default::default(), &listed) - full,
            room_body_h(2, true) - room_body_h(2, false));
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
    fn room_title_splits_name_and_folder_like_the_local_card() {
        assert_eq!(room_title("mission-control · …mo/sionic/mission-control"), ("mission-control", "…mo/sionic/mission-control"));
        assert_eq!(room_title("방 3"), ("방 3", ""));
        assert_eq!(room_title(""), ("방 이름 없음", ""));
    }

    #[test]
    fn offline_devices_do_not_offer_stale_panes() {
        let mut m = machine();
        m.online = false;
        assert!(rows(&m).is_empty());
    }
}

impl App {
    /// 그 기기의 그 방에 앉은 줄들 — 카드가 묶는 규칙(`rooms`)과 같다: 방 번호가 있으면
    /// 번호로, 없으면 이름으로.
    fn remote_room_panes(&self, label: &str, window: Option<u64>, room: &str) -> Vec<&state::MachinesColRow> {
        self.info.machines_col.machines.iter().find(|m| m.label == label)
            .map(|m| m.remote.iter().chain(m.mirrored.iter())
                .filter(|r| !r.closed && match (window, r.window) {
                    (Some(a), Some(b)) => a == b,
                    (None, None) => r.room == room,
                    _ => false,
                })
                .collect())
            .unwrap_or_default()
    }

    /// 방 카드 메뉴가 떠 있을 때의 클릭 — 항목이면 실행하고, 어디를 눌렀든 메뉴는 닫힌다.
    pub(crate) fn sidebar_navigation_menu_click(&mut self, cursor: (f32, f32)) {
        let Some(menu) = self.info.navigation.room_menu.take() else { return };
        let action = self.info.navigation.room_menu_rects.iter().find(|(_, r)| hit(cursor, *r)).map(|(a, _)| *a);
        self.info.navigation.room_menu_rects.clear();
        self.chrome_dirty = true;
        match action {
            Some(RoomMenuAction::ListBody) => { self.info.navigation.list_rooms.insert(menu.key); }
            Some(RoomMenuAction::MapBody) => { self.info.navigation.list_rooms.remove(&menu.key); }
            Some(RoomMenuAction::Rename) => match menu.window {
                Some(window) => {
                    let name = room_title(&menu.room).0.to_string();
                    self.begin_remote_room_rename(&menu.label, window, &menu.key, &name);
                }
                None => self.set_toast("옛 판 기기의 방은 이름을 바꿀 수 없어요".to_string()),
            },
            Some(RoomMenuAction::Close) => self.confirm_or_close_remote_room(&menu.label, menu.window, &menu.room),
            Some(RoomMenuAction::ClosePane) => {
                if let Some(pane) = menu.pane { self.close_remote_cell(&menu.label, &pane); }
            }
            Some(RoomMenuAction::SplitBeside) => {
                if let Some(pane) = menu.pane { self.spawn_beside_remote_cell(&menu.label, &pane, false); }
            }
            Some(RoomMenuAction::NewTab) => {
                if let Some(pane) = menu.pane { self.spawn_beside_remote_cell(&menu.label, &pane, true); }
            }
            None => {}
        }
    }

    /// 다른 기기의 방 닫기 — 본기기 방과 같은 문: 누가 도는 중이면 확인 모달, 아니면 바로.
    pub(crate) fn confirm_or_close_remote_room(&mut self, label: &str, window: Option<u64>, room: &str) {
        let busy = self.remote_room_panes(label, window, room).into_iter()
            .find(|r| matches!(r.status.as_str(), "working" | "compacting"))
            .map(|r| if r.name.is_empty() { "claude".to_string() } else { r.name.clone() });
        let action = PendingClose::RemoteRoom { label: label.to_string(), window, room: room.to_string() };
        match busy {
            Some(proc) => self.open_confirm_close(proc, action),
            None => self.do_close(action),
        }
    }

    /// 그 기계에 `window.close` 를 보낸다(방 번호가 없는 옛 판은 pane 하나씩). 답은 폴링
    /// 대신 `poke` 로 바로 받는다 — 카드가 5초 뒤에 사라지면 안 닫힌 줄 안다.
    pub(crate) fn close_remote_room(&mut self, label: &str, window: Option<u64>, room: &str) {
        let Some(m) = kasa_mcp::machines::find(label) else {
            self.set_toast(format!("{label} 가 명부(machines.json)에 없어요"));
            return;
        };
        let mut ids: Vec<String> = self.remote_room_panes(label, window, room).into_iter().map(|r| r.remote_id.clone()).collect();
        ids.sort();
        ids.dedup();
        if window.is_none() && ids.is_empty() { return; }
        self.set_toast(format!("{label} 의 방 닫는 중…"));
        let base = m.base.clone();
        std::thread::spawn(move || {
            let result = match window {
                Some(idx) => kasa_mcp::remote::remote_cmd(&base, "window.close", serde_json::json!({ "idx": idx })).map(|_| ()),
                None => ids.iter().try_for_each(|id| kasa_mcp::remote::close_remote_pane(&base, id, None, true)),
            };
            if let Err(e) = result {
                eprintln!("[remote] room close failed: {e:#}");
            }
            kasa_mcp::machines::poke();
        });
    }

    /// 편집칸의 이름을 그 기계의 `window.rename` 으로 보낸다 — 그쪽은 pane 으로 방을 짚으므로
    /// 그 방의 첫 pane 을 함께 준다.
    pub(crate) fn rename_remote_room(&mut self, label: &str, window: u64, name: &str) {
        let Some(m) = kasa_mcp::machines::find(label) else {
            self.set_toast(format!("{label} 가 명부(machines.json)에 없어요"));
            return;
        };
        let Some(pane) = self.remote_room_panes(label, Some(window), "").first().map(|r| r.remote_id.clone()) else {
            self.set_toast("그 방의 pane 을 못 찾아 이름을 못 바꿨어요".to_string());
            return;
        };
        let (base, name) = (m.base.clone(), name.to_string());
        std::thread::spawn(move || {
            let params = serde_json::json!({ "surface_id": pane, "title": name });
            if let Err(e) = kasa_mcp::remote::remote_cmd(&base, "window.rename", params) {
                eprintln!("[remote] room rename failed: {e:#}");
            }
            kasa_mcp::machines::poke();
        });
    }

    /// 그 기기의 칸 하나를 닫는다 — 사람이 그 기계에서 pane 을 닫은 것과 같다(되살리기 대열에 남는다).
    pub(crate) fn close_remote_cell(&mut self, label: &str, pane: &str) {
        let Some(m) = kasa_mcp::machines::find(label) else {
            self.set_toast(format!("{label} 가 명부(machines.json)에 없어요"));
            return;
        };
        let (base, pane) = (m.base.clone(), pane.to_string());
        self.set_toast(format!("{label} 의 pane 닫는 중…"));
        std::thread::spawn(move || {
            if let Err(e) = kasa_mcp::remote::close_remote_pane(&base, &pane, None, false) {
                eprintln!("[remote] pane close failed: {e:#}");
            }
            kasa_mcp::machines::poke();
        });
    }

    /// 그 칸 **옆에**(`as_tab` 이면 그 칸의 탭으로) 새 셸을 세운다. 자리는 저쪽이 정한다 —
    /// 우리 배치도는 그 기계 배치의 사본이라, 여기서 자리를 지어 보내면 어긋난다.
    pub(crate) fn spawn_beside_remote_cell(&mut self, label: &str, pane: &str, as_tab: bool) {
        let Some(m) = kasa_mcp::machines::find(label) else {
            self.set_toast(format!("{label} 가 명부(machines.json)에 없어요"));
            return;
        };
        let at = kasa_socket::backend::SpawnShellAt {
            beside: (!as_tab).then(|| pane.to_string()),
            tab_of: as_tab.then(|| pane.to_string()),
            ..Default::default()
        };
        let (base, label) = (m.base.clone(), label.to_string());
        self.set_toast(format!("{label} 에 {} 세우는 중…", if as_tab { "탭을" } else { "옆자리를" }));
        std::thread::spawn(move || {
            if let Err(e) = kasa_mcp::remote::spawn_shell_pane_at(&base, &at, None) {
                eprintln!("[remote] spawn beside failed: {e:#}");
            }
            kasa_mcp::machines::poke();
        });
    }

    /// 커서 아래가 어느 기기의 카드·칸·절 머리인가 — 이 기기 pane 을 끌어다 놓을 때
    /// 「그 기기로 이사」의 과녁이다(2026-09-18 지시).
    pub(crate) fn navigation_machine_at(&self, cursor: (f32, f32)) -> Option<String> {
        let nav = &self.info.navigation;
        let by_hit = nav.hits.iter().find(|(_, r)| hit(cursor, *r)).and_then(|(a, _)| match a {
            Action::Open { label, .. } | Action::Menu(label) | Action::Section(label)
            | Action::NewRoom(label) | Action::Unpin(label) | Action::CloseRoom { label, .. } => Some(label.clone()),
            _ => None,
        });
        by_hit
            .or_else(|| nav.pinned.iter().find(|p| p.view.is_some_and(|v| hit(cursor, v))).map(|p| p.label.clone()))
            .or_else(|| nav.machine.clone().filter(|_| nav.viewport.is_some_and(|v| hit(cursor, v))))
    }

    pub(crate) fn navigation_drop_target(&self, cursor: (f32, f32)) -> Option<(crate::drag_transfer::DragEndpoint, DropZone)> {
        let nav = &self.info.navigation;
        // Minimap cells take precedence over their enclosing room card.
        let pick = nav.hits.iter().rev().find_map(|(action, rect)| match action {
            Action::Open { label, window, room, focus: Some(pane) } if hit(cursor, *rect) =>
                Some((label, window, room, Some(pane.clone()), *rect)),
            _ => None,
        }).or_else(|| nav.hits.iter().rev().find_map(|(action, rect)| match action {
            Action::Open { label, window, room, focus: None } if hit(cursor, *rect) =>
                Some((label, window, room, None, *rect)),
            _ => None,
        }))?;
        let (label, window, room, focused, rect) = pick;
        let machine = self.info.machines_col.machines.iter().find(|m| m.label == *label && m.online)?;
        let anchor = focused.clone().or_else(|| machine.remote.iter().chain(&machine.mirrored)
            .find(|row| window.map_or(row.room == *room, |id| row.window == Some(id)))
            .map(|row| row.remote_id.clone()))?;
        let peer = kasa_mcp::machines::find(label)?;
        let zone = if focused.is_some() {
            crate::layout::drop_edge_for_offsets(
                (cursor.0 - rect.0 - rect.2 / 2.0) / (rect.2 / 2.0).max(1.0),
                (cursor.1 - rect.1 - rect.3 / 2.0) / (rect.3 / 2.0).max(1.0),
            )
        } else { DropZone::Right };
        Some((crate::drag_transfer::DragEndpoint::RemotePane { label: label.clone(), base: peer.base, pane: anchor }, zone))
    }

    fn drop_remote_cell(&mut self, cell: &CellDrag, cursor: (f32, f32)) {
        let Some(machine) = kasa_mcp::machines::find(&cell.label) else {
            self.set_toast("출발 기기의 연결을 확인할 수 없어요".into());
            return;
        };
        let target = self.sidebar_pane_drop_target(cursor.0, cursor.1)
            .or_else(|| self.drop_target_at(cursor.0, cursor.1).map(|(pane, zone)| (self.drag_endpoint(&pane), zone)));
        let Some((destination, mut zone)) = target else {
            self.set_toast("이사할 방이나 칸 위에 놓아 주세요".into());
            return;
        };
        let same_room_cell = self.info.navigation.hits.iter().any(|(action, rect)| match action {
            Action::Open { label, window, room, focus: Some(pane) } =>
                hit(cursor, *rect) && *label == cell.label && *pane != cell.pane
                    && match (cell.window, *window) {
                        (Some(a), Some(b)) => a == b,
                        (None, None) => *room == cell.room,
                        _ => false,
                    },
            _ => false,
        });
        if same_room_cell { zone = DropZone::Center; }
        self.route_drag_move(crate::drag_transfer::DragEndpoint::RemotePane {
            label: cell.label.clone(), base: machine.base, pane: cell.pane.clone(),
        }, destination, zone);
        self.info.machines_col.last_refresh = None;
    }
    /// 원격 방 이름 편집칸의 글(캐럿 포함) — 본기기 `overlay_room_rename_label` 과 같은 식.
    pub(crate) fn remote_rename_overlay(&self) -> Option<(String, String)> {
        let (_, _, key) = self.room_rename.remote.as_ref()?;
        let (idx, buf) = self.room_rename.editing.as_ref()?;
        let composing = match self.ime_focus {
            Some(crate::ImeFocus::RoomRename(i)) if i == *idx => self.preedit.as_str(),
            _ => "",
        };
        let (before, after) = crate::lineedit::split(buf, self.room_rename.cursor);
        Some((key.clone(), format!("{before}{composing}\u{258c}{after}")))
    }
}
