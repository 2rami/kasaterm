//! 화면 칸 — 여닫기와 그리기. 칸 탭은 `PaneContent::Screen`(기기 이름·보기 번호), 연결과 풀린 장은 `App.screen_views`.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Layout, ScreenView, Surface};
use crate::{gpu, theme, App, PaneContent, PaneTab, ScreenPane, UserEvent};

const STATUS_FONT: f32 = 13.0;
const HUD_FONT: f32 = 11.0;
const HUD_PAD: f32 = 6.0;
const EDGE: f32 = 2.0;
/// 안 보인 지 이만큼이면 연결을 닫는다 — 탭을 잠깐 넘겨 보는 동안 다시 붙느라 키프레임을 기다리지 않게.
const HIDDEN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// 보이는 칸의 연결이 닫히면 이만큼 뒤 다시 붙는다 — 그 기기 재시작·직통이 늦게 서는 경우.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

fn texture_key(view: u64) -> String {
    format!("screen:{view}")
}

/// 새로 풀린 장을 올리고 칸 본문 안에 비율을 지켜 그린다. 아직 장이 없으면 상태 글을 가운데에, 장이 흐르면 오른쪽
/// 위에 「조작」 단추와 `fps · 지연 · 길` 을 작게 얹는다. 장은 그 기기의 기기색 테로 두른다 — 이 기기 화면과 다른 기기
/// 화면이 한눈에 갈리게(사이드바 기기 묶음 둘레선과 같은 색). 조작 중이면 강조색 테가 그 자리를 대신하고, 조작을 못
/// 넣으면 그 까닭을 아래에 띄운다. 그린 자리는 누름 판정에 쓰도록 보기에 적어 둔다.
pub(crate) fn paint(g: &mut gpu::GpuRenderer, views: &mut HashMap<u64, ScreenView>, slots: &[(u64, (f32, f32, f32, f32))], on: Surface) {
    let white = [255, 255, 255, 235];
    let dim = [0, 0, 0, 150];
    let opts = |font_size, color| gpu::DrawOpts { font_size, color, bold: false, italic: false };
    for (view_id, (x, y, w, h)) in slots {
        let Some(view) = views.get_mut(view_id) else { continue };
        let key = texture_key(*view_id);
        view.surface = on;
        // 칸을 다른 창으로 옮기면 그 창 렌더러에는 장이 없다 — 멈춘 화면이면 새 장이 안 오니 마지막 장을 다시 올린다.
        let fresh = view.new_surface().or_else(|| if g.has_image(&key) { None } else { view.current_surface() });
        if let Some((surface, surface_key, fw, fh)) = fresh {
            let started = std::time::Instant::now();
            // 복사 없이 디코더 표면(IOSurface·D3D11 공유 텍스처)을 텍스처로. 못 하면(다른 백엔드 등) 장을 CPU 로 한 번 올린다.
            #[cfg(target_os = "macos")]
            let zero_copy = {
                let _ = surface_key;
                unsafe { g.set_image_iosurface(&key, surface, fw, fh) }
            };
            #[cfg(windows)]
            let zero_copy = unsafe { g.set_image_d3d11(&key, surface, surface_key, fw, fh) };
            #[cfg(not(any(target_os = "macos", windows)))]
            let zero_copy = {
                let _ = (surface, surface_key, fw, fh);
                false
            };
            if !zero_copy {
                view.copy_latest(|bgra, w, h, stride| g.update_image_bgra(&key, bgra, w, h, stride));
            }
            view.note_upload(started.elapsed(), zero_copy);
        }
        let edge = crate::render::pane_identity::device_section_border(theme::panel_bg(), &view.label);
        if !view.has_frame() {
            view.layout = None;
            g.round_rect_stroke(*x, *y, *w, *h, 0.0, EDGE, edge);
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
        g.round_rect_stroke(image.0, image.1, image.2, image.3, 0.0, EDGE, if view.control { theme::accent() } else { edge });
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
        let Some(view) = ScreenView::open(label, base, self.screen_fit(), wake) else {
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
            if let Some(view) = ScreenView::open(&label, &machine.base, self.screen_fit(), wake) {
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
