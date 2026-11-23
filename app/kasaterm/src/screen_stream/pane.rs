//! 화면 칸 — 여닫기와 그리기. 칸 탭은 `PaneContent::Screen`(기기 이름·보기 번호), 연결과 풀린 장은 `App.screen_views`.

use std::collections::HashMap;
use std::sync::Arc;

use super::{ScreenView, Surface};
use crate::{gpu, theme, App, PaneContent, PaneTab, ScreenPane, UserEvent};

/// 안 보인 지 이만큼이면 연결을 닫는다 — 탭을 잠깐 넘겨 보는 동안 다시 붙느라 키프레임을 기다리지 않게.
const HIDDEN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// 보이는 칸의 연결이 닫히면 이만큼 뒤 다시 붙는다 — 그 기기 재시작·직통이 늦게 서는 경우.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

fn texture_key(view: u64) -> String {
    format!("screen:{view}")
}

/// 칸마다 보기 하나를 그린다(`kasa_castview::paint`) — 글자는 이 앱 크롬 글꼴, 장 둘레는 그 기기 기기색(사이드바 기기 묶음
/// 둘레선과 같은 색)이다.
pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    views: &mut HashMap<u64, ScreenView>,
    slots: &[(u64, (f32, f32, f32, f32))],
    on: Surface,
    cursor: (f32, f32),
) {
    for (view_id, slot) in slots {
        let Some(view) = views.get_mut(view_id) else { continue };
        let style = kasa_castview::Style {
            accent: theme::accent(),
            mute: theme::text_mute(),
            edge: crate::render::pane_identity::device_section_border(theme::panel_bg(), &view.label),
            extra: &[],
        };
        kasa_castview::paint(g, view, &texture_key(*view_id), *slot, on, cursor, &style);
    }
}

impl kasa_castview::Canvas for gpu::GpuRenderer {
    fn grid(&mut self) -> &mut kasa_gridview::GridRenderer {
        &mut self.grid
    }
    fn measure(&mut self, text: &str, size: f32) -> f32 {
        self.measure_chrome_text(text, size, false)
    }
    fn text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
        self.draw_text(x, y, text, gpu::DrawOpts { font_size: size, color, bold: false, italic: false });
    }
}

impl App {
    /// 그 기기 화면을 이 창의 칸 탭으로 연다. 이미 열려 있으면 그 탭으로 간다. 지금 못 붙으면(옛 판·직통 아님) 칸에
    /// 까닭을 띄우고 길이 서면 붙는다. 칸을 둘 자리가 없을 때만 false.
    pub(crate) fn open_screen_pane(&mut self, label: &str, base: &str) -> bool {
        let existing = {
            let ws = self.ws.lock().unwrap();
            ws.panes.iter().find_map(|(id, p)| {
                p.tabs.iter().position(|t| t.screen().is_some_and(|s| s.label == label)).map(|ti| (id.clone(), ti))
            })
        };
        if let Some((id, tab_idx)) = existing.filter(|(id, _)| self.window_of_pane(id).is_some()) {
            if let Some(wi) = self.window_of_pane(&id) {
                self.switch_window(wi);
            }
            let mut ws = self.ws.lock().unwrap();
            if let Some(p) = ws.panes.get_mut(&id) {
                p.active_tab = tab_idx.min(p.tabs.len().saturating_sub(1));
                p.dirty = true;
            }
            ws.active_pane = Some(id);
            drop(ws);
            self.chrome_dirty = true;
            return true;
        }
        let Ok(anchor) = self.resolve_user_mutation_anchor(None, crate::settings_room::SettingsMutation::WebPane) else {
            return false;
        };
        let proxy = self.proxy.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || drop(proxy.send_event(UserEvent::Redraw)));
        let Some(view) = ScreenView::open(label, self.screen_fit(), None, super::silent_audio(), super::start(base), wake) else {
            return false;
        };
        let view_id = self.screen_view_seq;
        self.screen_view_seq += 1;
        self.screen_views.insert(view_id, view);
        let mut tab = PaneTab::default();
        tab.content = PaneContent::Screen(ScreenPane { label: label.to_string(), view_id });
        tab.title = Some(format!("화면 · {}", crate::render::pane_identity::device_name(label)));
        tab.title_pinned = true;
        {
            let mut ws = self.ws.lock().unwrap();
            let outer = ws.outer_for_pty(&anchor).unwrap_or_else(|| anchor.clone());
            let Some(pane) = ws.panes.get_mut(&outer) else {
                drop(ws);
                self.screen_views.remove(&view_id);
                return false;
            };
            pane.tabs.push(tab);
            pane.active_tab = pane.tabs.len() - 1;
            pane.dirty = true;
            ws.active_pane = Some(outer);
        }
        self.handoff_ime_to_active_surface();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    /// 보이는 화면 탭만 연결을 쥔다. 뒤 탭·다른 방으로 가 `HIDDEN_GRACE` 넘게 안 보이면 연결을 닫고(장을 계속 받아 풀지 않게),
    /// 닫혀 되살리기 목록으로 물러난 탭은 바로 닫는다. 다시 보이면 붙는다. 칸이 어디서 바뀌었든 다음 루프 턴에 여기서 따라잡는다.
    pub(crate) fn sync_screen_views(&mut self) {
        let tabs: Vec<(String, u64, String, bool)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .flat_map(|(id, p)| {
                    p.tabs.iter().enumerate().filter_map(move |(i, t)| {
                        t.screen().map(|s| (id.clone(), s.view_id, s.label.clone(), i == p.active_tab))
                    })
                })
                .collect()
        };
        if tabs.is_empty() && self.screen_views.is_empty() && self.screen_windows.is_empty() {
            return;
        }
        let shown = self.visible_pane_ids();
        let mut live: Vec<(u64, String, bool)> = tabs
            .into_iter()
            .filter(|(id, ..)| self.window_of_pane(id).is_some())
            .map(|(id, v, l, front)| (v, l, front && shown.contains(&id)))
            .collect();
        // 꺼낸 화면 창은 늘 보인다.
        live.extend(self.screen_windows.iter().map(|w| (w.view_id, w.label.clone(), true)));
        self.screen_display.retain(|id, _| live.iter().any(|(v, ..)| v == id));
        let now = std::time::Instant::now();
        let mut retry: Vec<(u64, String)> = Vec::new();
        let gone: Vec<u64> = self
            .screen_views
            .iter_mut()
            .filter_map(|(id, view)| {
                let Some((_, _, visible)) = live.iter().find(|(v, ..)| v == id) else { return Some(*id) };
                if *visible {
                    view.hidden_since = None;
                    if !view.closed() {
                        view.closed_since = None;
                        return None;
                    }
                    let since = *view.closed_since.get_or_insert(now);
                    if now.duration_since(since) < RETRY_AFTER {
                        return None;
                    }
                    retry.push((*id, view.status()));
                    return Some(*id);
                }
                let since = *view.hidden_since.get_or_insert(now);
                (now.duration_since(since) >= HIDDEN_GRACE).then_some(*id)
            })
            .collect();
        for id in gone {
            self.screen_views.remove(&id);
            if let Some(g) = self.gpu.as_mut() {
                g.drop_image(&texture_key(id));
            }
        }
        for (view_id, label, visible) in live {
            if !visible || self.screen_views.contains_key(&view_id) {
                continue;
            }
            let Some(machine) = kasa_device::machines::find(&label) else { continue };
            let proxy = self.proxy.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || drop(proxy.send_event(UserEvent::Redraw)));
            let display = self.screen_display.get(&view_id).copied();
            if let Some(view) = ScreenView::open(&label, self.screen_fit(), display, super::silent_audio(), super::start(&machine.base), wake) {
                if let Some((_, previous)) = retry.iter().find(|(v, _)| *v == view_id) {
                    view.carry_status(previous);
                }
                self.screen_views.insert(view_id, view);
            }
        }
    }

    /// 화면 칸이 커질 수 있는 끝 — 가장 큰 모니터. 칸 크기로 정하면 키우거나 꺼낼 때마다 다시 붙어야 한다.
    fn screen_fit(&self) -> (u32, u32) {
        self.window
            .as_ref()
            .map(|w| w.available_monitors().map(|m| m.size()).fold((0, 0), |a, s| if s.width * s.height > a.0 * a.1 { (s.width, s.height) } else { a }))
            .unwrap_or((0, 0))
    }

    /// 본창에 그려지는 보기에 새 장이 있나 — 꺼낸 창의 보기는 그 창만 다시 그린다(`redraw_screen_windows`).
    pub(crate) fn screen_views_have_new_frame(&self) -> bool {
        self.screen_views.values().any(|v| v.surface == Surface::Main && v.has_new_frame())
    }
}
