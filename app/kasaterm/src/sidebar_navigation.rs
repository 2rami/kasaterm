use super::*;

type Rect = (f32, f32, f32, f32);
pub(crate) const HEADER_H: f32 = 48.0;
const ROOM_H: f32 = 30.0;
const PANE_H: f32 = 44.0;
const DEVICE_H: f32 = 42.0;

#[derive(Default)]
pub(crate) struct NavigationState {
    pub(crate) machine: Option<String>,
    pub(crate) picker: bool,
    pub(crate) scroll: f32,
    picker_scroll: f32,
    picker_index: usize,
    content_h: f32,
    viewport: Option<Rect>,
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

pub(crate) fn draw(g: &mut gpu::GpuRenderer, info: &mut state::InfoState, cursor: (f32, f32), width: f32, viewport: Rect) {
    let nav = &mut info.navigation;
    nav.hits.clear();
    nav.viewport = (width > 0.0).then_some(viewport);
    if width <= 0.0 {
        nav.picker = false;
        nav.picker_hits.clear();
        return;
    }
    let selected = nav.machine.as_deref().and_then(|name| info.machines_col.machines.iter().find(|m| m.label == name));
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
    if nav.machine.is_none() { return; }
    let Some(machine) = selected else {
        text(g, "기기를 다시 선택해 주세요", 16.0, viewport.1 + 12.0, width - 32.0, 11.0, theme::text_dim(), false);
        return;
    };
    nav.content_h = content_height(machine, &nav.collapsed_rooms);
    nav.scroll = nav.scroll.clamp(0.0, (nav.content_h - viewport.3).max(0.0));
    g.push_clip(viewport.0, viewport.1, viewport.2, viewport.3);
    let mut y = viewport.1 - nav.scroll;
    let mut last_room = None;
    let machine_rows = rows(machine);
    if machine_rows.is_empty() {
        let message = if machine.online { "열린 pane 없음" } else { "기기에 연결할 수 없음" };
        text(g, message, 16.0, y + 12.0, width - 32.0, 11.0, theme::text_dim(), false);
        y += PANE_H;
    }
    for row in machine_rows {
        let key = room_key(&machine.label, &row.room);
        let collapsed = nav.collapsed_rooms.contains(&key);
        if last_room != Some(row.room.as_str()) {
            let r = (8.0, y, width - 16.0, ROOM_H);
            if let Some(r) = clipped(r, viewport) {
                if hit(cursor, r) { g.rect(r.0, r.1, r.2, r.3, theme::surface_hover()); }
                g.hover_pointer |= hit(cursor, r);
                g.queue_icon(if collapsed { "chevron-right" } else { "chevron-down" }, 14.0, y + 9.0, 12.0, theme::text_dim());
                text(g, if row.room.is_empty() { "방 이름 없음" } else { &row.room }, 34.0, y + 8.0, width - 50.0, 11.0, theme::text_dim(), true);
                nav.hits.push((Action::Room(key.clone()), r));
            }
            last_room = Some(row.room.as_str());
            y += ROOM_H;
        }
        if collapsed { continue; }
        let full = (14.0, y, width - 24.0, PANE_H);
        if let Some(r) = clipped(full, viewport) {
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
                    if let Some(r) = clipped(close, viewport) {
                        nav.hits.push((Action::Close(state::MachinesColBtn::Close {
                            label: machine.label.clone(), remote_id: row.remote_id.clone(),
                            name: row.name.clone(), pane: row.pane.clone(),
                        }), r));
                    }
                }
                nav.hits.push((Action::Pane(machine.label.clone(), row.clone()), r));
            }
        }
        y += PANE_H;
    }
    if machine.closed > 0 {
        text(g, &format!("닫힌 pane {} · 원본에서 되살리기", machine.closed), 16.0, y + 12.0, width - 32.0, 10.0, theme::text_dim(), false);
    }
    g.pop_clip();
    if nav.content_h > viewport.3 {
        let h = (viewport.3 * viewport.3 / nav.content_h).max(20.0).min(viewport.3);
        let y = viewport.1 + (viewport.3 - h) * nav.scroll / (nav.content_h - viewport.3);
        g.rect(width - 4.0, y, 2.0, h, theme::text_mute());
    }
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
        if selected || index == nav.picker_index || hit(cursor, r) { g.rect(full.0, full.1, full.2, full.3, if selected { theme::surface_active() } else { theme::surface_hover() }); }
        g.hover_pointer |= hit(cursor, r);
        let label = machine.map(|m| m.label.as_str()).unwrap_or_else(|| crate::info::local_machine_name());
        text(g, if label.is_empty() { "이 기기" } else { label }, 20.0, y + 5.0, width - 52.0, 12.0, theme::text(), true);
        let detail = machine.map(status).unwrap_or_else(|| "이 기기".into());
        text(g, &detail, 20.0, y + 24.0, width - 44.0, 10.0, theme::text_dim(), false);
        if selected { g.queue_icon("check", width - 32.0, y + 13.0, 14.0, theme::accent()); }
        nav.picker_hits.push((selection, r));
    }
    g.pop_clip();
    g.rect(viewport.0, top + height, viewport.2, 1.0, theme::border());
}

impl App {
    pub(crate) fn run_pending_sidebar_navigation_probe(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::{Mutex, OnceLock};
        if !crate::verification_run()
            || !std::env::var("KASATERM_AUTOINFO").is_ok_and(|v| v == "rooms")
            || !std::env::var("KASATERM_MACHINES").is_ok_and(|v| v == "[]")
        { return; }
        let Ok(folder) = std::env::var("KASATERM_AUTONAV_DIR") else { return; };
        static STEP: OnceLock<Mutex<(Instant, usize)>> = OnceLock::new();
        let mut step = STEP.get_or_init(|| Mutex::new((Instant::now(), 0))).lock().unwrap();
        if step.1 >= 8 || step.0.elapsed().as_millis() < if step.1 == 0 { 10000 } else { 700 } { return; }
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

    pub(crate) fn sidebar_navigation_click(&mut self, cursor: (f32, f32)) -> bool {
        if self.tabs_on_top || self.tab_strip_w() <= 0.0 { return false; }
        if self.info.navigation.picker {
            let selected = self.info.navigation.picker_hits.iter().find(|(_, r)| hit(cursor, *r)).map(|(s, _)| s.clone());
            self.info.navigation.picker = false;
            if let Some(selected) = selected {
                self.info.navigation.machine = selected;
                self.info.navigation.scroll = 0.0;
                self.info.navigation.last_click = None;
            }
            self.chrome_dirty = true;
            return true;
        }
        let action = self.info.navigation.hits.iter().find(|(_, r)| hit(cursor, *r)).map(|(a, _)| a.clone());
        let Some(action) = action else { return false; };
        match action {
            Action::Picker => {
                self.info.navigation.picker = true;
                self.info.navigation.picker_index = self.info.navigation.machine.as_ref()
                    .and_then(|name| self.info.machines_col.machines.iter().position(|m| &m.label == name))
                    .map_or(0, |index| index + 1);
            }
            Action::Menu(label) => {
                self.info.machine_menu = Some((cursor.0, cursor.1, label));
                self.info.machines_col.last_refresh = None;
            }
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

    pub(crate) fn sidebar_navigation_right_click(&mut self, cursor: (f32, f32)) -> bool {
        if self.info.navigation.picker { self.info.navigation.picker = false; self.chrome_dirty = true; return true; }
        let action = self.info.navigation.hits.iter().find(|(_, r)| hit(cursor, *r)).map(|(a, _)| a.clone());
        let label = match action {
            Some(Action::Menu(label) | Action::Pane(label, _)) => Some(label),
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
        let nav = &mut self.info.navigation;
        let dy = match delta { MouseScrollDelta::LineDelta(_, y) => y * 42.0, MouseScrollDelta::PixelDelta(p) => p.y as f32 };
        let handled = if nav.picker {
            if nav.picker_view.is_some_and(|r| hit(self.cursor_px, r)) {
                let height = nav.picker_view.unwrap().3;
                nav.picker_scroll = (nav.picker_scroll - dy).clamp(0.0, (nav.picker_content_h - height).max(0.0));
            }
            true
        } else if nav.machine.is_some() && nav.viewport.is_some_and(|r| hit(self.cursor_px, r)) {
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
        if !self.info.navigation.picker || !event.state.is_pressed() { return false; }
        use winit::keyboard::{Key, NamedKey};
        let max = self.info.machines_col.machines.len();
        let nav = &mut self.info.navigation;
        match event.logical_key {
            Key::Named(NamedKey::ArrowDown) => nav.picker_index = (nav.picker_index + 1).min(max),
            Key::Named(NamedKey::ArrowUp) => nav.picker_index = nav.picker_index.saturating_sub(1),
            Key::Named(NamedKey::Home) => nav.picker_index = 0,
            Key::Named(NamedKey::End) => nav.picker_index = max,
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                nav.machine = nav.picker_index.checked_sub(1)
                    .and_then(|index| self.info.machines_col.machines.get(index)).map(|m| m.label.clone());
                nav.scroll = 0.0;
                nav.last_click = None;
                nav.picker = false;
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
    fn offline_devices_do_not_offer_stale_panes() {
        let mut m = machine();
        m.online = false;
        assert!(rows(&m).is_empty());
    }
}
