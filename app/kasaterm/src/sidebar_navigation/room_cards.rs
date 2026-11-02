use super::*;
use std::collections::{BTreeMap, BTreeSet};
mod probe;

const MIN_HEIGHT: f32 = 46.0;
const MAX_HEIGHT: f32 = 960.0;
const HIDDEN_ROW: f32 = 32.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RoomId {
    Local(usize),
    Remote(String),
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Preferences {
    local_heights: BTreeMap<usize, f32>,
    local_hidden: BTreeSet<usize>,
    remote_heights: BTreeMap<String, f32>,
    remote_hidden: BTreeSet<String>,
}

impl Preferences {
    fn remap(&mut self, map: impl Fn(usize) -> Option<usize>) {
        self.local_heights = std::mem::take(&mut self.local_heights).into_iter()
            .filter_map(|(i, h)| Some((map(i)?, h))).collect();
        self.local_hidden = std::mem::take(&mut self.local_hidden).into_iter().filter_map(map).collect();
    }
    fn normalize(&mut self) {
        self.local_heights.retain(|_, h| h.is_finite());
        self.remote_heights.retain(|_, h| h.is_finite());
        for h in self.local_heights.values_mut().chain(self.remote_heights.values_mut()) {
            *h = h.clamp(MIN_HEIGHT, MAX_HEIGHT);
        }
    }
}

#[derive(Clone)]
pub(super) struct HiddenRoom {
    pub(super) id: RoomId,
    pub(super) name: String,
}

#[derive(Clone)]
pub(super) struct Grip {
    id: RoomId,
    rect: Rect,
    height: f32,
}

struct Resize {
    id: RoomId,
    start_y: f32,
    start_scroll: f32,
    initial: f32,
    height: f32,
}

#[derive(Default)]
pub(crate) struct RoomCards {
    prefs: Preferences,
    pub(super) local_hidden: Vec<HiddenRoom>,
    open: BTreeSet<String>,
    grips: Vec<Grip>,
    resize: Option<Resize>,
}

impl RoomCards {
    pub(super) fn hidden(&self, room: &RoomId) -> bool {
        match room {
            RoomId::Local(i) => self.prefs.local_hidden.contains(i),
            RoomId::Remote(key) => self.prefs.remote_hidden.contains(key),
        }
    }
    pub(super) fn height(&self, room: &RoomId, natural: f32, listed: bool) -> f32 {
        if listed { return natural; }
        if let Some(resize) = self.resize.as_ref().filter(|r| r.id == *room) { return resize.height; }
        match room {
            RoomId::Local(i) => self.prefs.local_heights.get(i),
            RoomId::Remote(key) => self.prefs.remote_heights.get(key),
        }.copied().unwrap_or(natural)
    }
    fn set_height(&mut self, room: RoomId, height: Option<f32>) {
        match room {
            RoomId::Local(i) => if let Some(h) = height { self.prefs.local_heights.insert(i, h); } else { self.prefs.local_heights.remove(&i); },
            RoomId::Remote(key) => if let Some(h) = height { self.prefs.remote_heights.insert(key, h); } else { self.prefs.remote_heights.remove(&key); },
        }
    }
    pub(super) fn hidden_height(&self, label: &str, count: usize) -> f32 {
        if count == 0 { 0.0 } else { HIDDEN_ROW * (1 + if self.open.contains(label) { count } else { 0 }) as f32 }
    }
    pub(super) fn push_grip(&mut self, id: RoomId, card: Rect, height: f32, view: Rect) {
        let rect = (card.0 + 8.0, card.1 + card.3 - 7.0, (card.2 - 16.0).max(0.0), 6.0);
        if let Some(rect) = clipped(rect, view).filter(|r| r.3 >= 3.0) { self.grips.push(Grip { id, rect, height }); }
    }
    pub(super) fn clear_grips(&mut self) { self.grips.clear(); }
    pub(super) fn cancel_resize(&mut self) -> bool { self.resize.take().is_some() }
}

pub(super) fn remote_hidden(machine: &state::MachinesColMachine, cards: &RoomCards) -> Vec<HiddenRoom> {
    rooms(machine).into_iter().filter_map(|(name, rows)| {
        let id = RoomId::Remote(room_key(&machine.label, rows[0].window, &name));
        cards.hidden(&id).then(|| HiddenRoom { id, name: room_title(&name).0.to_string() })
    }).collect()
}

pub(super) fn draw_hidden(g: &mut gpu::GpuRenderer, hits: &mut Vec<(Action, Rect)>, cards: &RoomCards,
    label: &str, rows: &[HiddenRoom], cursor: (f32, f32), width: f32, y: f32, view: Rect) -> f32 {
    if rows.is_empty() { return 0.0; }
    let open = cards.open.contains(label);
    let head = (8.0, y, (width - 16.0).max(0.0), HIDDEN_ROW);
    g.queue_icon("folder", 14.0, y + 9.0, 14.0, theme::text_mute());
    text(g, &format!("숨긴 방 {}", rows.len()), 36.0, y + 9.0, width - 66.0, 11.0, theme::text_dim(), false);
    g.queue_icon(if open { "chevron-down" } else { "chevron-right" }, width - 26.0, y + 10.0, 12.0, theme::text_mute());
    if let Some(rect) = clipped(head, view) {
        g.hover_pointer |= hit(cursor, rect);
        hits.push((Action::HiddenRooms(label.to_string()), rect));
    }
    if open {
        for (n, row) in rows.iter().enumerate() {
            let rect = (16.0, y + HIDDEN_ROW * (n + 1) as f32, (width - 32.0).max(0.0), HIDDEN_ROW);
            if let Some(visible) = clipped(rect, view) {
                if hit(cursor, visible) { g.rect(rect.0, rect.1, rect.2, rect.3, theme::surface_hover()); }
                g.hover_pointer |= hit(cursor, visible);
                text(g, &row.name, rect.0 + 6.0, rect.1 + 9.0, rect.2 - 36.0, 11.5, theme::text(), false);
                g.queue_icon("undo-2", rect.0 + rect.2 - 22.0, rect.1 + 9.0, 14.0, theme::text_dim());
                hits.push((Action::ShowHiddenRoom(row.id.clone()), visible));
            }
        }
    }
    cards.hidden_height(label, rows.len())
}

pub(crate) fn draw_grips(g: &mut gpu::GpuRenderer, info: &state::InfoState, cursor: (f32, f32)) {
    for grip in &info.navigation.room_cards.grips {
        let hot = hit(cursor, grip.rect) || info.navigation.room_cards.resize.as_ref().is_some_and(|r| r.id == grip.id);
        g.rect(grip.rect.0 + grip.rect.2 * 0.25, grip.rect.1 + (grip.rect.3 - 2.0) / 2.0, grip.rect.2 * 0.5, 2.0,
            if hot { theme::accent() } else { theme::with_alpha(theme::border(), 140) });
    }
}

impl App {
    pub(crate) fn sidebar_room_hidden(&self, i: usize) -> bool {
        self.info.navigation.room_cards.hidden(&RoomId::Local(i))
    }
    pub(crate) fn sidebar_room_height(&self, i: usize, natural: f32, listed: bool) -> f32 {
        self.info.navigation.room_cards.height(&RoomId::Local(i), natural, listed)
    }
    pub(crate) fn refresh_hidden_sidebar_rooms(&mut self) {
        self.info.navigation.room_cards.local_hidden = (0..self.windows.len())
            .filter(|i| self.sidebar_room_hidden(*i) && self.remote_view_of_window(*i).is_none())
            .map(|i| HiddenRoom { id: RoomId::Local(i), name: self.window_labels.get(i)
                .map(|(name, _)| name.clone()).unwrap_or_else(|| format!("방 {}", i + 1)) }).collect();
    }
    fn sidebar_room_cards_changed(&mut self) {
        self.close_freeze.thaw();
        self.refresh_hidden_sidebar_rooms();
        if let Some(window) = &self.window {
            let height = window.inner_size().height as f32 / self.effective_scale();
            self.sidebar_scroll_px = self.sidebar_scroll_px.clamp(0.0, self.sidebar_max_scroll(height));
        }
        self.session_touched = true;
        self.chrome_dirty = true;
    }
    pub(crate) fn hide_sidebar_room(&mut self, room: RoomId) {
        match room {
            RoomId::Local(i) => { if i < self.windows.len() { self.info.navigation.room_cards.prefs.local_hidden.insert(i); } }
            RoomId::Remote(key) => { self.info.navigation.room_cards.prefs.remote_hidden.insert(key); }
        }
        self.sidebar_room_cards_changed();
    }
    pub(super) fn show_sidebar_room(&mut self, room: RoomId) {
        match room {
            RoomId::Local(i) => { self.info.navigation.room_cards.prefs.local_hidden.remove(&i); }
            RoomId::Remote(key) => { self.info.navigation.room_cards.prefs.remote_hidden.remove(&key); }
        }
        self.sidebar_room_cards_changed();
    }
    pub(crate) fn reset_sidebar_room_height(&mut self, room: RoomId) {
        self.info.navigation.room_cards.set_height(room, None);
        self.sidebar_room_cards_changed();
    }
    pub(super) fn toggle_hidden_sidebar_rooms(&mut self, label: String) {
        let open = &mut self.info.navigation.room_cards.open;
        if !open.remove(&label) { open.insert(label); }
        self.sidebar_room_cards_changed();
    }
    pub(crate) fn refresh_local_room_grips(&mut self, tabs: &[(usize, Rect)], view: Rect) {
        self.info.navigation.room_cards.clear_grips();
        if self.tabs_on_top || self.tab_strip_w() <= 0.0 { return; }
        for &(i, rect) in tabs {
            if !self.room_body_is_list(i) && self.expand_progress(i) > 0.99 && self.internal_room_kind_at(i).is_none() {
                let height = self.sidebar_card_metrics(i).0;
                self.info.navigation.room_cards.push_grip(RoomId::Local(i), rect, height, view);
            }
        }
    }
    pub(crate) fn sidebar_room_grip_at(&self, point: (f32, f32)) -> bool {
        self.info.navigation.room_cards.grips.iter().any(|g| hit(point, g.rect))
    }
    pub(crate) fn sidebar_room_resize_press(&mut self, point: (f32, f32)) -> bool {
        let Some(grip) = self.info.navigation.room_cards.grips.iter().find(|g| hit(point, g.rect)).cloned() else { return false };
        self.close_freeze.thaw();
        self.info.navigation.device_drag = None;
        self.info.navigation.cell_drag = None;
        self.info.navigation.room_cards.resize = Some(Resize { id: grip.id, start_y: point.1,
            start_scroll: self.sidebar_scroll_px, initial: grip.height, height: grip.height });
        self.chrome_dirty = true;
        true
    }
    pub(crate) fn sidebar_room_resize_move(&mut self) -> bool {
        let Some(resize) = self.info.navigation.room_cards.resize.as_mut() else { return false };
        resize.height = (resize.initial + self.cursor_px.1 - resize.start_y + self.sidebar_scroll_px - resize.start_scroll)
            .clamp(MIN_HEIGHT, MAX_HEIGHT);
        self.chrome_dirty = true;
        true
    }
    pub(crate) fn sidebar_room_resize_release(&mut self) -> bool {
        self.sidebar_room_resize_move();
        let Some(resize) = self.info.navigation.room_cards.resize.take() else { return false };
        if (resize.height - resize.initial).abs() > 0.5 {
            self.info.navigation.room_cards.set_height(resize.id, Some(resize.height));
            self.sidebar_room_cards_changed();
        }
        true
    }
    pub(crate) fn remap_sidebar_room_cards(&mut self, map: impl Fn(usize) -> Option<usize>) {
        let cards = &mut self.info.navigation.room_cards;
        cards.prefs.remap(&map);
        cards.resize = None;
        cards.local_hidden = std::mem::take(&mut cards.local_hidden).into_iter().filter_map(|mut room| {
            if let RoomId::Local(i) = room.id { room.id = RoomId::Local(map(i)?); }
            Some(room)
        }).collect();
    }
    pub(crate) fn sidebar_room_cards_snapshot(&self, indices: &[Option<usize>]) -> serde_json::Value {
        let mut prefs = self.info.navigation.room_cards.prefs.clone();
        prefs.remap(|i| indices.get(i).copied().flatten());
        serde_json::to_value(prefs).unwrap_or_default()
    }
    pub(crate) fn restore_sidebar_room_cards(&mut self, state: &serde_json::Value, indices: &[Option<usize>]) {
        let active = state["active_session"].as_u64().unwrap_or(0) as usize;
        let saved = state["sessions"].as_array().and_then(|s| s.get(active).or_else(|| s.first()))
            .and_then(|s| s.get("sidebar_room_cards")).cloned().unwrap_or_default();
        let mut prefs: Preferences = serde_json::from_value(saved).unwrap_or_default();
        prefs.normalize();
        prefs.remap(|i| indices.get(i).copied().flatten());
        self.info.navigation.room_cards.prefs = prefs;
        self.refresh_hidden_sidebar_rooms();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_room_preferences_follow_reorders_and_skip_omitted_rooms() {
        let mut p = Preferences::default();
        p.local_heights.extend([(0, 200.0), (2, 360.0)]);
        p.local_hidden.extend([0, 1]);
        p.remote_hidden.insert("remote".into());
        p.remap(|i| [Some(1), None, Some(0)].get(i).copied().flatten());
        assert_eq!(p.local_heights, [(0, 360.0), (1, 200.0)].into());
        assert_eq!(p.local_hidden, [1].into());
        let restored: Preferences = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(restored.remote_hidden, ["remote".to_string()].into());
        assert_eq!(restored.local_hidden, p.local_hidden);
    }
    #[test]
    fn resizing_a_map_does_not_stretch_the_list_or_mutate_saved_height_until_release() {
        let mut cards = RoomCards::default();
        cards.set_height(RoomId::Local(0), Some(200.0));
        cards.resize = Some(Resize { id: RoomId::Local(0), start_y: 0.0, start_scroll: 0.0, initial: 200.0, height: 320.0 });
        assert_eq!(cards.height(&RoomId::Local(0), 50.0, false), 320.0);
        assert_eq!(cards.height(&RoomId::Local(0), 80.0, true), 80.0);
        assert_eq!(cards.prefs.local_heights[&0], 200.0);
        cards.cancel_resize();
        assert_eq!(cards.height(&RoomId::Local(0), 50.0, false), 200.0);
    }
}
