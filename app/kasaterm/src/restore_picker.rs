use crate::session::restore_selection::Plan;
use crate::{gpu, theme, App, RestoreBtn};
use serde_json::{json, Value};
use winit::keyboard::{Key, NamedKey};

mod welcome;

type Rect = (f32, f32, f32, f32);
const ROW_H: f32 = 76.0;
const GROUP_H: f32 = 34.0;

#[derive(Default)]
pub(crate) struct State {
    plan: Plan,
    selected: Vec<bool>,
    focus: usize,
    control_focus: Option<RestoreBtn>,
    scroll: f32,
    list_height: f32,
    group_height: f32,
    expanded: bool,
    reveal_next: bool,
    welcome_slots: usize,
}

impl State {
    pub(crate) fn new(state: &Value) -> Self {
        let mut plan = Plan::new(state);
        crate::testkit::restore_picker_portrait_fixture(&mut plan);
        Self {
            selected: vec![true; plan.rows.len()],
            plan,
            group_height: GROUP_H,
            welcome_slots: 1,
            ..Self::default()
        }
    }

    fn count(&self) -> usize {
        self.selected.iter().filter(|v| **v).count()
    }
    fn content_height(&self) -> f32 {
        self.plan.rows.len() as f32 * ROW_H + self.plan.groups.len() as f32 * self.group_height
    }
    fn clamp_scroll(&mut self) {
        self.scroll = self
            .scroll
            .clamp(0.0, (self.content_height() - self.list_height).max(0.0));
    }
    fn reveal(&mut self) {
        if !self.expanded {
            return;
        }
        let Some(row) = self.plan.rows.get(self.focus) else {
            return;
        };
        let top = self.focus as f32 * ROW_H + (row.group + 1) as f32 * self.group_height;
        if top < self.scroll {
            self.scroll = top;
        }
        if top + ROW_H > self.scroll + self.list_height {
            self.scroll = top + ROW_H - self.list_height;
        }
        self.clamp_scroll();
    }
    pub(crate) fn wheel(&mut self, delta: winit::event::MouseScrollDelta) {
        if !self.expanded {
            return;
        }
        use winit::event::MouseScrollDelta;
        self.scroll -= match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 36.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.clamp_scroll();
    }
    pub(crate) fn key(&mut self, key: &Key, shift: bool, repeat: bool) -> Option<RestoreBtn> {
        if matches!(key, Key::Named(NamedKey::Tab)) {
            let mut choices = if self.expanded {
                vec![RestoreBtn::Collapse, RestoreBtn::All, RestoreBtn::Clear]
            } else {
                vec![
                    RestoreBtn::Expand,
                    if self.count() == self.selected.len() {
                        RestoreBtn::Clear
                    } else {
                        RestoreBtn::All
                    },
                ]
            };
            if !self.expanded {
                let visible = welcome::range(self.selected.len(), self.focus, self.welcome_slots);
                if visible.start > 0 {
                    choices.push(RestoreBtn::Previous);
                }
                if visible.end < self.selected.len() {
                    choices.push(RestoreBtn::Next);
                }
            }
            if self.count() > 0 {
                choices.push(RestoreBtn::Restore);
            }
            choices.extend([RestoreBtn::Fresh, RestoreBtn::Dismiss]);
            let current = self
                .control_focus
                .and_then(|action| choices.iter().position(|a| *a == action));
            let index = match (current, shift) {
                (Some(i), true) => (i + choices.len() - 1) % choices.len(),
                (Some(i), false) => (i + 1) % choices.len(),
                (None, true) => choices.len() - 1,
                (None, false) => 0,
            };
            self.control_focus = Some(choices[index]);
            return None;
        }
        match key {
            Key::Named(NamedKey::Enter) if !repeat => {
                return Some(self.control_focus.unwrap_or(RestoreBtn::Restore))
            }
            Key::Named(NamedKey::Escape) if !repeat => {
                return Some(if self.expanded {
                    RestoreBtn::Collapse
                } else {
                    RestoreBtn::Dismiss
                })
            }
            Key::Named(NamedKey::Space) if !repeat => {
                return Some(self.control_focus.unwrap_or(RestoreBtn::Toggle(self.focus)))
            }
            Key::Character(value) if value.as_str() == " " && !repeat => {
                return Some(self.control_focus.unwrap_or(RestoreBtn::Toggle(self.focus)))
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                self.focus = self.focus.saturating_sub(1)
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                self.focus = (self.focus + 1).min(self.selected.len().saturating_sub(1))
            }
            Key::Named(NamedKey::Home) => self.focus = 0,
            Key::Named(NamedKey::End) => self.focus = self.selected.len().saturating_sub(1),
            Key::Named(NamedKey::PageDown) => {
                self.focus = (self.focus + (self.list_height / ROW_H).max(1.0) as usize)
                    .min(self.selected.len().saturating_sub(1))
            }
            Key::Named(NamedKey::PageUp) => {
                self.focus = self
                    .focus
                    .saturating_sub((self.list_height / ROW_H).max(1.0) as usize)
            }
            _ => return None,
        }
        self.control_focus = None;
        self.reveal();
        None
    }
    fn pick(&mut self, action: RestoreBtn) -> bool {
        match action {
            RestoreBtn::Previous | RestoreBtn::Next => {
                let visible = welcome::range(self.selected.len(), self.focus, self.welcome_slots);
                self.focus = if action == RestoreBtn::Next {
                    visible.end.min(self.selected.len().saturating_sub(1))
                } else {
                    visible.start.saturating_sub(self.welcome_slots.max(1))
                };
                self.control_focus = None;
            }
            RestoreBtn::Expand | RestoreBtn::Collapse => {
                self.expanded = action == RestoreBtn::Expand;
                self.control_focus = None;
                self.reveal_next = self.expanded;
            }
            RestoreBtn::Toggle(index) => {
                if let Some(selected) = self.selected.get_mut(index) {
                    *selected = !*selected;
                    self.focus = index;
                    self.control_focus = None;
                }
            }
            RestoreBtn::Group(index) => {
                if let Some(group) = self.plan.groups.get(index) {
                    let all = self.selected[group.start..group.end].iter().all(|s| *s);
                    self.selected[group.start..group.end].fill(!all);
                    self.focus = group.start;
                    self.control_focus = None;
                }
            }
            RestoreBtn::All | RestoreBtn::Clear => {
                self.selected.fill(action == RestoreBtn::All);
                if !self.expanded && self.control_focus == Some(action) {
                    self.control_focus = Some(if action == RestoreBtn::All {
                        RestoreBtn::Clear
                    } else {
                        RestoreBtn::All
                    });
                }
            }
            _ => return false,
        }
        true
    }
    fn selection(&self) -> Value {
        json!(self
            .selected
            .iter()
            .enumerate()
            .filter_map(|(i, selected)| selected.then_some(i))
            .collect::<Vec<_>>())
    }
}

impl App {
    pub(crate) fn restore_dialog_pick(&mut self, action: RestoreBtn) {
        if self.restore_prompt.is_none() {
            return;
        }
        self.chrome_dirty = true;
        if self.restore_picker.pick(action) {
            return;
        }
        if action == RestoreBtn::Restore && self.restore_picker.count() == 0 {
            return;
        }
        let Some(mut state) = self.restore_prompt.take() else {
            return;
        };
        match action {
            RestoreBtn::Restore => {
                state["restore_selection"] = self.restore_picker.selection();
                // Let the selected-work progress paint before rebuilding PTYs.
                self.restore_applying = Some((
                    state,
                    std::time::Instant::now() + std::time::Duration::from_millis(90),
                ));
            }
            RestoreBtn::Fresh | RestoreBtn::Dismiss => {
                crate::server_restore::import_pending_servers(&mut state);
                state["restore_selection"] = json!([]);
                self.restore_closed_history(&crate::session::restore_selection::apply(&state));
                self.release_reserved_characters();
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Debug)]
struct Layout {
    card: Rect,
    list: Rect,
    detail: Rect,
    footer: Rect,
    pad: f32,
    compact: bool,
    dense: bool,
}

fn layout(viewport: (f32, f32), content: f32) -> Layout {
    let width = (viewport.0 - 32.0).clamp(0.0, 620.0);
    let available = (viewport.1 - 32.0).max(0.0);
    let compact = available < 448.0;
    let dense = available < 280.0;
    let pad = if width < 420.0 { 16.0 } else { 24.0 };
    let header = if dense {
        62.0
    } else if compact {
        88.0
    } else {
        124.0_f32
    }
    .min(available * 0.42);
    let footer = if dense {
        36.0
    } else if compact {
        60.0
    } else {
        84.0_f32
    }
    .min(available * 0.34);
    let detail = if available >= 528.0 { 88.0 } else { 0.0 };
    let height = (header + content + detail + footer)
        .min(620.0)
        .min(available);
    let x = (viewport.0 - width) / 2.0;
    let y = (viewport.1 - height) / 2.0;
    let list_h = (height - header - detail - footer).max(0.0);
    Layout {
        card: (x, y, width, height),
        list: (x + pad, y + header, (width - 2.0 * pad).max(0.0), list_h),
        detail: (
            x + pad,
            y + header + list_h,
            (width - 2.0 * pad).max(0.0),
            detail,
        ),
        footer: (
            x + pad,
            y + height - footer,
            (width - 2.0 * pad).max(0.0),
            footer,
        ),
        pad,
        compact,
        dense,
    }
}

fn inside(r: Rect, cursor: (f32, f32)) -> bool {
    cursor.0 >= r.0 && cursor.0 < r.0 + r.2 && cursor.1 >= r.1 && cursor.1 < r.1 + r.3
}
fn clipped(rect: Rect, clip: Rect) -> Option<Rect> {
    let top = rect.1.max(clip.1);
    let bottom = (rect.1 + rect.3).min(clip.1 + clip.3);
    (bottom > top).then_some((rect.0, top, rect.2, bottom - top))
}
fn label(g: &mut gpu::GpuRenderer, rect: Rect, text: &str, size: f32, bold: bool, color: [u8; 4]) {
    let shown = crate::info::fit_text(g, text, rect.2.max(0.0), size, bold);
    g.draw_text(
        rect.0,
        rect.1,
        &shown,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}
fn check(g: &mut gpu::GpuRenderer, x: f32, y: f32, selected: bool, mixed: bool) {
    let fill = if selected || mixed {
        theme::accent()
    } else {
        theme::surface()
    };
    g.round_rect_fill(x, y, 18.0, 18.0, theme::radius_sm().min(5.0), fill);
    g.round_rect_stroke(
        x,
        y,
        18.0,
        18.0,
        theme::radius_sm().min(5.0),
        1.0,
        if selected || mixed {
            fill
        } else {
            theme::text_mute()
        },
    );
    if selected {
        g.queue_icon("check", x + 2.0, y + 2.0, 14.0, theme::foreground_on(fill));
    } else if mixed {
        g.rect(x + 4.0, y + 8.0, 10.0, 2.0, theme::foreground_on(fill));
    }
}
fn wrapped(g: &mut gpu::GpuRenderer, rect: Rect, value: &str, lines: usize) {
    let mut line = String::new();
    let mut y = rect.1;
    let mut line_no = 0;
    for ch in value.chars() {
        let mut next = line.clone();
        next.push(ch);
        if !line.is_empty() && g.measure_chrome_text(&next, 11.5, false) > rect.2 {
            label(
                g,
                (rect.0, y, rect.2, 16.0),
                &line,
                11.5,
                false,
                theme::text_dim(),
            );
            line_no += 1;
            y += 16.0;
            line.clear();
            if line_no >= lines {
                return;
            }
        }
        line.push(ch);
    }
    label(
        g,
        (rect.0, y, rect.2, 16.0),
        &line,
        11.5,
        false,
        theme::text_dim(),
    );
}

pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    state: &mut State,
    viewport: (f32, f32),
    cursor: (f32, f32),
) -> Vec<(RestoreBtn, Rect)> {
    if state.expanded {
        paint_selection(g, state, viewport, cursor)
    } else {
        welcome::paint(g, state, viewport, cursor)
    }
}

fn paint_selection(
    g: &mut gpu::GpuRenderer,
    state: &mut State,
    viewport: (f32, f32),
    cursor: (f32, f32),
) -> Vec<(RestoreBtn, Rect)> {
    use crate::native_controls::{icon_button, text_button, Style};
    state.group_height = if viewport.1 < 312.0 { 0.0 } else { GROUP_H };
    let l = layout(viewport, state.content_height());
    state.list_height = l.list.3;
    state.clamp_scroll();
    if state.reveal_next {
        state.reveal();
        state.reveal_next = false;
    }
    let mut hits = Vec::new();
    let (x, y, w, h) = l.card;
    g.rect(
        0.0,
        0.0,
        viewport.0,
        viewport.1,
        theme::with_alpha([0, 0, 0, 255], 160),
    );
    g.round_rect_fill(x, y, w, h, theme::radius_md(), theme::bg());
    g.round_rect_stroke(x, y, w, h, theme::radius_md(), 1.0, theme::border());
    g.push_clip(x, y, w, h);
    let tx = x + l.pad;
    let back_y = y + if l.dense {
        4.0
    } else if l.compact {
        10.0
    } else {
        18.0
    };
    let back = icon_button(
        g,
        (tx, back_y, 26.0, 28.0),
        cursor,
        "chevron-left",
        Style::default(),
    );
    hits.push((RestoreBtn::Collapse, back));
    label(
        g,
        (
            tx + 34.0,
            y + if l.dense {
                8.0
            } else if l.compact {
                12.0
            } else {
                20.0
            },
            w - l.pad * 2.0 - 74.0,
            28.0,
        ),
        "작업 고르기",
        if l.dense { 16.0 } else { 18.0 },
        true,
        theme::text(),
    );
    let close = icon_button(
        g,
        (
            x + w - l.pad - 28.0,
            y + if l.dense {
                4.0
            } else if l.compact {
                10.0
            } else {
                18.0
            },
            28.0,
            28.0,
        ),
        cursor,
        "x",
        Style::default(),
    );
    hits.push((RestoreBtn::Dismiss, close));
    if !l.compact {
        label(
            g,
            (tx, y + 52.0, w - l.pad * 2.0, 16.0),
            &format!("이 기기 · {}", state.plan.saved_at),
            12.0,
            false,
            theme::text_dim(),
        );
    }
    let tools_y = l.list.1 - if l.dense { 30.0 } else { 38.0 };
    let count_width = (l.list.2 - 174.0).max(0.0);
    label(
        g,
        (tx, tools_y + 7.0, count_width, 18.0),
        &if count_width < 110.0 {
            format!("{}/{}", state.count(), state.selected.len())
        } else {
            format!("{} / {}개 선택", state.count(), state.selected.len())
        },
        13.0,
        true,
        theme::text(),
    );
    for (action, name, dx) in [
        (RestoreBtn::All, "전체 선택", 162.0),
        (RestoreBtn::Clear, "전체 해제", 78.0),
    ] {
        let rect = text_button(
            g,
            (tx + l.list.2 - dx, tools_y, 78.0, 28.0),
            cursor,
            name,
            Style::default(),
        );
        hits.push((action, rect));
    }
    g.push_clip(l.list.0, l.list.1, l.list.2, l.list.3);
    let mut ry = l.list.1 - state.scroll;
    for (gi, group) in state.plan.groups.iter().enumerate() {
        let selected = state.selected[group.start..group.end]
            .iter()
            .filter(|s| **s)
            .count();
        let header = (l.list.0, ry, l.list.2 - 8.0, state.group_height);
        if let Some(hit) = clipped(header, l.list) {
            let hover = inside(hit, cursor);
            if hover {
                g.round_rect_fill(
                    header.0,
                    header.1,
                    header.2,
                    header.3,
                    theme::radius_sm(),
                    theme::surface_hover(),
                );
            }
            check(
                g,
                header.0 + 10.0,
                ry + 8.0,
                selected == group.end - group.start,
                selected > 0 && selected < group.end - group.start,
            );
            label(
                g,
                (header.0 + 40.0, ry + 10.0, header.2 - 112.0, 18.0),
                &group.title,
                12.0,
                true,
                theme::text_dim(),
            );
            label(
                g,
                (header.0 + header.2 - 65.0, ry + 10.0, 60.0, 18.0),
                &format!("{} / {}", selected, group.end - group.start),
                11.5,
                false,
                theme::text_mute(),
            );
            g.hover_pointer |= hover;
            hits.push((RestoreBtn::Group(gi), hit));
        }
        ry += state.group_height;
        for i in group.start..group.end {
            let row = &state.plan.rows[i];
            let rect = (l.list.0, ry, l.list.2 - 8.0, ROW_H - 4.0);
            if let Some(hit) = clipped(rect, l.list) {
                let hover = inside(hit, cursor);
                let fill = if hover {
                    theme::surface_hover()
                } else if state.selected[i] {
                    theme::lerp(theme::surface(), theme::accent(), 0.07)
                } else {
                    theme::surface()
                };
                g.round_rect_fill(rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), fill);
                if state.focus == i && state.control_focus.is_none() {
                    g.round_rect_stroke(
                        rect.0,
                        rect.1,
                        rect.2,
                        rect.3,
                        theme::radius_sm(),
                        1.0,
                        theme::with_alpha(theme::accent(), 170),
                    );
                }
                check(g, rect.0 + 10.0, ry + 26.0, state.selected[i], false);
                if row.character.is_empty()
                    || !crate::sprites::draw_student_face(
                        g,
                        &row.character,
                        rect.0 + 40.0,
                        ry + 23.0,
                        26.0,
                    )
                {
                    g.queue_icon(row.icon, rect.0 + 43.0, ry + 26.0, 20.0, theme::text_dim());
                }
                let left = rect.0 + 76.0;
                let width = (rect.2 - 86.0).max(0.0);
                label(
                    g,
                    (left, ry + 9.0, width, 18.0),
                    &row.title,
                    13.5,
                    true,
                    theme::text(),
                );
                let meta = if l.dense {
                    format!("방 {} · {}", row.group + 1, row.identity)
                } else {
                    format!("{} · 탭 {}", row.identity, row.tab + 1)
                };
                label(
                    g,
                    (left, ry + 30.0, width, 16.0),
                    &meta,
                    11.5,
                    false,
                    theme::text_dim(),
                );
                label(
                    g,
                    (left, ry + 49.0, width, 16.0),
                    if row.folder.is_empty() {
                        "작업 폴더 기록 없음"
                    } else {
                        &row.folder
                    },
                    11.5,
                    false,
                    theme::text_mute(),
                );
                g.hover_pointer |= hover;
                hits.push((RestoreBtn::Toggle(i), hit));
            }
            ry += ROW_H;
        }
    }
    if state.content_height() > l.list.3 && l.list.3 > 0.0 {
        let thumb = (l.list.3 * l.list.3 / state.content_height())
            .max(16.0)
            .min(l.list.3);
        let top =
            l.list.1 + (l.list.3 - thumb) * state.scroll / (state.content_height() - l.list.3);
        g.round_rect_fill(
            l.list.0 + l.list.2 - 4.0,
            top,
            3.0,
            thumb,
            1.5,
            theme::text_mute(),
        );
    }
    g.pop_clip();
    if l.detail.3 > 0.0 {
        g.rect(
            l.detail.0,
            l.detail.1 + 8.0,
            l.detail.2,
            1.0,
            theme::border(),
        );
        if let Some(row) = state.plan.rows.get(state.focus) {
            label(
                g,
                (l.detail.0, l.detail.1 + 18.0, l.detail.2, 16.0),
                &format!("{} · {}", row.kind, row.title),
                12.0,
                true,
                theme::text(),
            );
            let folder = if row.folder.is_empty() {
                "작업 폴더 기록 없음"
            } else {
                &row.folder
            };
            wrapped(
                g,
                (l.detail.0, l.detail.1 + 39.0, l.detail.2, 32.0),
                &format!("{folder}  ·  {}", row.detail),
                2,
            );
        }
    }
    let fy = l.footer.1 + if l.compact { 4.0 } else { 10.0 };
    if !l.dense {
        label(
            g,
            (l.footer.0, fy, l.footer.2, 16.0),
            "고르지 않은 작업은 ‘복원 보류’로 보관해요",
            11.5,
            false,
            theme::text_dim(),
        );
    }
    let button_y = fy + if l.dense { 0.0 } else { 24.0 };
    let restore_w = 134.0_f32.min(l.footer.2 * 0.5);
    let fresh_w = 90.0_f32.min((l.footer.2 - restore_w - 8.0).max(0.0));
    let restore_x = l.footer.0 + l.footer.2 - restore_w;
    let fresh = text_button(
        g,
        (restore_x - 8.0 - fresh_w, button_y, fresh_w, 28.0),
        cursor,
        "새로 시작",
        Style::default(),
    );
    hits.push((RestoreBtn::Fresh, fresh));
    let restore = text_button(
        g,
        (restore_x, button_y, restore_w, 28.0),
        cursor,
        &format!("{}개 복원하기", state.count()),
        Style {
            primary: true,
            enabled: state.count() > 0,
            ..Style::default()
        },
    );
    if state.count() > 0 {
        hits.push((RestoreBtn::Restore, restore));
    }
    if l.footer.2 > 570.0 {
        label(
            g,
            (l.footer.0, button_y + 8.0, 300.0, 16.0),
            "↑↓ 이동 · Space 선택 · Enter 복원",
            11.5,
            false,
            theme::text_mute(),
        );
    }
    for (action, rect) in &hits {
        if state.control_focus == Some(*action) {
            crate::native_controls::focus_ring(g, *rect);
        }
    }
    g.pop_clip();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State::new(&json!({"sessions":[{"windows":[
            {"leaf":{"title":"Same","tabs":[{"title":"Same"},{"title":"Third"}]}},
            {"leaf":{"title":"Another room"}}
        ]}]}))
    }

    #[test]
    fn group_and_individual_choices_are_independent_with_keyboard_reveal() {
        let mut state = state();
        state.pick(RestoreBtn::Expand);
        state.list_height = 110.0;
        assert_eq!(state.count(), 4);
        state.pick(RestoreBtn::Group(0));
        assert_eq!(state.selection(), json!([3]));
        state.pick(RestoreBtn::Toggle(1));
        assert_eq!(state.selection(), json!([1, 3]));
        state.key(&Key::Named(NamedKey::End), false, false);
        assert_eq!(state.focus, 3);
        assert!(state.scroll > 0.0);
        let action = state
            .key(&Key::Named(NamedKey::Space), false, false)
            .unwrap();
        state.pick(action);
        assert_eq!(state.selection(), json!([1]));
        state.pick(RestoreBtn::All);
        assert_eq!(state.count(), 4);
        state.pick(RestoreBtn::Clear);
        assert_eq!(state.count(), 0);
    }

    #[test]
    fn responsive_geometry_keeps_list_and_actions_inside_the_card() {
        for width in [280.0, 320.0, 390.0, 640.0, 1024.0, 1920.0] {
            for height in [200.0, 240.0, 360.0, 480.0, 600.0, 900.0] {
                for count in [1.0, 50.0] {
                    let l = layout((width, height), count * ROW_H + GROUP_H);
                    assert!(l.card.0 >= 0.0 && l.card.1 >= 0.0);
                    assert!(l.card.0 + l.card.2 <= width);
                    assert!(l.card.1 + l.card.3 <= height);
                    assert!(l.list.3 >= 0.0);
                    assert!(l.list.1 + l.list.3 <= l.footer.1);
                    assert!(l.footer.1 + l.footer.3 <= l.card.1 + l.card.3 + 0.01);
                    assert!(l.list.2 >= 216.0);
                    assert!(l.footer.3 >= if l.dense { 32.0 } else { 54.0 });
                    if l.dense {
                        assert!(l.list.3 >= 70.0);
                    }
                }
            }
        }
        assert!(layout((390.0, 844.0), 1000.0).detail.3 > 0.0);
        assert!(layout((640.0, 360.0), 1000.0).compact);
    }

    #[test]
    fn scrolling_rows_never_capture_clicks_over_fixed_footer() {
        assert_eq!(
            clipped((16.0, 180.0, 300.0, 76.0), (16.0, 100.0, 300.0, 100.0)),
            Some((16.0, 180.0, 300.0, 20.0))
        );
        assert!(clipped((16.0, 210.0, 300.0, 76.0), (16.0, 100.0, 300.0, 100.0)).is_none());
    }
    #[test]
    fn keyboard_controls_reverse_cycle_skip_disabled_restore_and_ignore_repeats() {
        let mut state = state();
        state.pick(RestoreBtn::Expand);
        state.key(&Key::Named(NamedKey::Tab), true, false);
        assert!(state.control_focus == Some(RestoreBtn::Dismiss));
        state.key(&Key::Named(NamedKey::Tab), false, false);
        assert!(state.control_focus == Some(RestoreBtn::Collapse));
        state.key(&Key::Named(NamedKey::Tab), false, false);
        assert!(state.control_focus == Some(RestoreBtn::All));
        state.key(&Key::Named(NamedKey::Tab), false, false);
        assert!(state.control_focus == Some(RestoreBtn::Clear));
        let clear = state
            .key(&Key::Named(NamedKey::Space), false, false)
            .unwrap();
        state.pick(clear);
        state.key(&Key::Named(NamedKey::Tab), false, false);
        assert!(state.control_focus == Some(RestoreBtn::Fresh));
        assert!(state
            .key(&Key::Named(NamedKey::Enter), false, true)
            .is_none());
        state.key(&Key::Named(NamedKey::ArrowDown), false, false);
        assert!(state.control_focus.is_none());
        assert!(state
            .key(&Key::Named(NamedKey::Space), false, true)
            .is_none());
        assert!(matches!(
            state.key(&Key::Character(" ".into()), false, false),
            Some(RestoreBtn::Toggle(1))
        ));
    }
    #[test]
    fn faces_toggle_directly_and_detail_round_trip_keeps_every_choice() {
        let mut state = state();
        assert!(!state.expanded);
        state.welcome_slots = 2;
        state.pick(RestoreBtn::Toggle(0));
        state.pick(RestoreBtn::Next);
        assert_eq!(state.focus, 2);
        state.pick(RestoreBtn::Toggle(2));
        assert_eq!(state.selection(), json!([1, 3]));
        state.pick(RestoreBtn::Expand);
        assert!(state.expanded && state.reveal_next);
        let back = state
            .key(&Key::Named(NamedKey::Escape), false, false)
            .unwrap();
        assert!(back == RestoreBtn::Collapse);
        state.pick(back);
        assert!(!state.expanded);
        assert_eq!(state.selection(), json!([1, 3]));
        state.pick(RestoreBtn::Previous);
        assert_eq!(state.focus, 0);
        assert!(
            state.key(&Key::Named(NamedKey::Escape), false, false) == Some(RestoreBtn::Dismiss)
        );
    }
    #[test]
    fn welcome_bulk_toggle_keeps_keyboard_focus_on_its_new_label() {
        let mut state = state();
        state.key(&Key::Named(NamedKey::Tab), false, false);
        state.key(&Key::Named(NamedKey::Tab), false, false);
        assert!(state.control_focus == Some(RestoreBtn::Clear));
        let action = state
            .key(&Key::Named(NamedKey::Space), false, false)
            .unwrap();
        state.pick(action);
        assert_eq!(state.count(), 0);
        assert!(state.control_focus == Some(RestoreBtn::All));
        let action = state
            .key(&Key::Named(NamedKey::Enter), false, false)
            .unwrap();
        state.pick(action);
        assert_eq!(state.count(), 4);
        assert!(state.control_focus == Some(RestoreBtn::Clear));
    }
}
