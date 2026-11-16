//! 화면 칸 — 여닫기와 그리기. 칸 탭은 `PaneContent::Screen`(기기 이름·보기 번호), 연결과 풀린 장은 `App.screen_views`.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Layout, ScreenView};
use crate::{gpu, theme, App, PaneContent, PaneTab, ScreenPane, UserEvent};

const STATUS_FONT: f32 = 13.0;
const HUD_FONT: f32 = 11.0;
const HUD_PAD: f32 = 6.0;

fn texture_key(view: u64) -> String {
    format!("screen:{view}")
}

/// 새로 풀린 장을 올리고 칸 본문 안에 비율을 지켜 그린다. 아직 장이 없으면 상태 글을 가운데에, 장이 흐르면 오른쪽
/// 위에 「조작」 단추와 `fps · 지연 · 길` 을 작게 얹는다. 조작 중이면 장을 강조색 테로 두르고, 조작을 못 넣으면 그 까닭을
/// 아래에 띄운다. 그린 자리는 누름 판정에 쓰도록 보기에 적어 둔다.
pub(crate) fn paint(g: &mut gpu::GpuRenderer, views: &mut HashMap<u64, ScreenView>, slots: &[(u64, (f32, f32, f32, f32))]) {
    let white = [255, 255, 255, 235];
    let dim = [0, 0, 0, 150];
    let opts = |font_size, color| gpu::DrawOpts { font_size, color, bold: false, italic: false };
    for (view_id, (x, y, w, h)) in slots {
        let Some(view) = views.get_mut(view_id) else { continue };
        let key = texture_key(*view_id);
        view.take_new(|bgra, fw, fh, stride| g.update_image_bgra(&key, bgra, fw, fh, stride));
        if !view.has_frame() {
            view.layout = None;
            let text = view.status();
            let tw = g.measure_chrome_text(&text, STATUS_FONT, false);
            g.draw_text(x + (w - tw) / 2.0, y + (h - STATUS_FONT) / 2.0, &text, opts(STATUS_FONT, theme::text_mute()));
            continue;
        }
        g.queue_image_contain(&key, *x, *y, *w, *h);
        let (fw, fh) = view.frame_size();
        let fit = (w / fw.max(1) as f32).min(h / fh.max(1) as f32);
        let (dw, dh) = (fw as f32 * fit, fh as f32 * fit);
        let image = (x + (w - dw) / 2.0, y + (h - dh) / 2.0, dw, dh);
        let box_h = HUD_FONT + HUD_PAD * 1.5;
        let hud = view.hud();
        let hw = g.measure_chrome_text(&hud, HUD_FONT, false) + HUD_PAD * 2.0;
        let hud_box = (x + w - hw - HUD_PAD, y + HUD_PAD, hw, box_h);
        let label = if view.control { "조작 중" } else { "조작" };
        let tw = g.measure_chrome_text(label, HUD_FONT, false) + HUD_PAD * 2.0;
        let toggle = (hud_box.0 - HUD_PAD - tw, y + HUD_PAD, tw, box_h);
        if view.control {
            g.round_rect_stroke(image.0, image.1, image.2, image.3, 0.0, 2.0, theme::accent());
        }
        g.round_rect_fill(toggle.0, toggle.1, toggle.2, toggle.3, 4.0, if view.control { theme::accent() } else { dim });
        g.draw_text(toggle.0 + HUD_PAD, toggle.1 + HUD_PAD * 0.75, label, opts(HUD_FONT, white));
        g.round_rect_fill(hud_box.0, hud_box.1, hud_box.2, hud_box.3, 4.0, dim);
        g.draw_text(hud_box.0 + HUD_PAD, hud_box.1 + HUD_PAD * 0.75, &hud, opts(HUD_FONT, white));
        if view.control && !view.preedit.is_empty() {
            let pw = g.measure_chrome_text(&view.preedit, STATUS_FONT, false) + HUD_PAD * 2.0;
            let ph = STATUS_FONT + HUD_PAD * 1.5;
            let (px, py) = (image.0 + HUD_PAD, image.1 + image.3 - ph - HUD_PAD);
            g.round_rect_fill(px, py, pw, ph, 4.0, dim);
            g.draw_text(px + HUD_PAD, py + HUD_PAD * 0.75, &view.preedit, opts(STATUS_FONT, white));
            g.rect(px + HUD_PAD, py + ph - HUD_PAD * 0.5, pw - HUD_PAD * 2.0, 1.0, white);
        }
        if let Some(note) = view.input_note().filter(|_| view.control) {
            let nw = g.measure_chrome_text(&note, HUD_FONT, false) + HUD_PAD * 2.0;
            let (nx, ny) = (x + (w - nw) / 2.0, y + h - box_h - HUD_PAD);
            g.round_rect_fill(nx, ny, nw, box_h, 4.0, dim);
            g.draw_text(nx + HUD_PAD, ny + HUD_PAD * 0.75, &note, opts(HUD_FONT, white));
        }
        view.layout = Some(Layout { image, toggle });
    }
}

impl App {
    /// 그 기기 화면을 이 창의 칸 탭으로 연다. 이미 열려 있으면 그 탭으로 간다. 못 열면 false — 부른 쪽이 JPEG
    /// 웹 칸으로 연다.
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
        let Some(view) = ScreenView::open(label, base, wake) else {
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

    /// 창에 실재하는 화면 탭만 연결을 쥔다. 닫혀 되살리기 목록으로 물러난 탭은 연결을 버리고(장을 계속 풀지 않게),
    /// 되살아난 탭은 다시 붙는다. 칸이 어디서 닫혔든 다음 루프 턴에 여기서 따라잡는다.
    pub(crate) fn sync_screen_views(&mut self) {
        let tabs: Vec<(String, u64, String)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .flat_map(|(id, p)| p.tabs.iter().filter_map(|t| t.screen().map(|s| (id.clone(), s.view_id, s.label.clone()))))
                .collect()
        };
        if tabs.is_empty() && self.screen_views.is_empty() {
            return;
        }
        let live: Vec<(u64, String)> =
            tabs.into_iter().filter(|(id, ..)| self.window_of_pane(id).is_some()).map(|(_, v, l)| (v, l)).collect();
        let gone: Vec<u64> = self.screen_views.keys().copied().filter(|id| !live.iter().any(|(v, _)| v == id)).collect();
        for id in gone {
            self.screen_views.remove(&id);
            if let Some(g) = self.gpu.as_mut() {
                g.drop_image(&texture_key(id));
            }
        }
        for (view_id, label) in live {
            if self.screen_views.contains_key(&view_id) {
                continue;
            }
            let Some(machine) = kasa_device::machines::find(&label) else { continue };
            let proxy = self.proxy.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || drop(proxy.send_event(UserEvent::Redraw)));
            if let Some(view) = ScreenView::open(&label, &machine.base, wake) {
                self.screen_views.insert(view_id, view);
            }
        }
    }

    pub(crate) fn screen_views_have_new_frame(&self) -> bool {
        self.screen_views.values().any(ScreenView::has_new_frame)
    }
}
