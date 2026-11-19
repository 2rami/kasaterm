//! 꺼낸 화면 창 — 화면 칸(`pane.rs`)의 보기를 별도 OS 창 하나로 띄운다. 창은 보기 번호만 들고, 연결·풀린 장은 칸과 같이
//! `App.screen_views` 가 쥔다. 머리는 별도 터미널 창(`auxterm.rs`)과 같은 꼴 — 왼쪽 기기 이름, 오른쪽 「칸으로」 단추.
//! 본문은 칸과 같은 `paint` 라 상태 글·「조작」·HUD 가 그대로다. 창을 닫으면 보기도 닫힌다(다시 열면 된다).

use std::sync::Arc;

use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes, WindowId};

use super::Surface;
use crate::{gpu, theme, App, PaneContent, PaneTab, ScreenPane, FONT_SIZE};

/// 머리 높이 — macOS 는 본창 제목줄과 맞춰 두 창이 같은 앱으로 읽히게, 그 밖은 OS 제목줄이 따로 있어 얇게.
#[cfg(target_os = "macos")]
const HEADER_H: f32 = 36.0;
#[cfg(not(target_os = "macos"))]
const HEADER_H: f32 = 30.0;
const PAD: f32 = 6.0;
const BTN: f32 = 26.0;
const ICON: f32 = 14.0;
/// 처음 크기 — 장 비율을 알면 이 폭에 맞춰 높이를 정한다.
const START_W: f32 = 1280.0;
const START_H: f32 = 800.0;

/// `gpu` 가 `window` 앞에 선언된다 — surface 는 창이 살아 있을 때 먼저 놓아야 한다.
pub(crate) struct ScreenWindow {
    pub(crate) view_id: u64,
    pub(crate) label: String,
    gpu: gpu::GpuRenderer,
    pub(crate) window: Arc<Window>,
    cursor_px: (f32, f32),
    pub(crate) focused: bool,
    dock_btn: (f32, f32, f32, f32),
}

impl ScreenWindow {
    fn logical_size(&self) -> (f32, f32) {
        let s = self.window.inner_size();
        let scale = self.gpu.scale().max(0.5);
        (s.width as f32 / scale, s.height as f32 / scale)
    }

    fn on(&self) -> Surface {
        Surface::Window(self.window.id())
    }
}

impl App {
    pub(crate) fn screen_window_index(&self, id: WindowId) -> Option<usize> {
        self.screen_windows.iter().position(|w| w.window.id() == id)
    }

    /// 그 칸의 화면 탭을 별도 창으로 꺼낸다. 탭은 칸에서 빠지고 보기(연결)는 그대로 이어진다.
    pub(crate) fn pop_out_screen(&mut self, outer: &str, tab_idx: usize, event_loop: &ActiveEventLoop) {
        let Some((label, view_id)) = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(outer)
            .and_then(|p| p.tabs.get(tab_idx))
            .and_then(|t| t.screen().map(|s| (s.label.clone(), s.view_id)))
        else {
            return;
        };
        let title = format!("화면 · {}", crate::render::pane_identity::device_name(&label));
        let (fw, fh) = self.screen_views.get(&view_id).map(|v| v.frame_size()).unwrap_or((0, 0));
        let height = if fw > 0 && fh > 0 { START_W * fh as f32 / fw as f32 + HEADER_H } else { START_H };
        let attrs = WindowAttributes::default()
            .with_title(title)
            .with_theme(Some(theme::window_theme()))
            .with_inner_size(LogicalSize::new(START_W, height.min(START_H * 1.2)));
        let window = match crate::auxwin::create_untabbed(event_loop, attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[screen-window] 창을 못 만든다: {e}");
                return;
            }
        };
        // 본창과 같은 입력기 길 — macOS 는 앱이 한글을 조합하고, 그 밖은 OS 입력기.
        window.set_ime_allowed(cfg!(not(target_os = "macos")));
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[screen-window] 렌더러를 못 만든다: {e}");
                return;
            }
        };
        // 창을 세운 뒤에 탭을 뺀다 — 못 세웠으면 칸에 그대로 남는다. 보기는 이 창이 살아 있는 동안 sync 가 놓지 않는다.
        self.screen_windows.push(ScreenWindow {
            view_id,
            label,
            gpu,
            window,
            cursor_px: (0.0, 0.0),
            focused: true,
            dock_btn: (0.0, 0.0, 0.0, 0.0),
        });
        self.close_tab(outer, tab_idx);
        self.chrome_dirty = true;
        if let Some(w) = self.screen_windows.last() {
            w.window.request_redraw();
        }
    }

    /// 꺼낸 화면을 지금 칸의 탭으로 되돌린다.
    pub(crate) fn dock_screen_window(&mut self, idx: usize) {
        let Some(w) = self.screen_windows.get(idx) else { return };
        let (view_id, label) = (w.view_id, w.label.clone());
        let Ok(anchor) = self.resolve_user_mutation_anchor(None, crate::settings_room::SettingsMutation::WebPane) else {
            return;
        };
        let mut tab = PaneTab::default();
        tab.content = PaneContent::Screen(ScreenPane { label: label.clone(), view_id });
        tab.title = Some(format!("화면 · {}", crate::render::pane_identity::device_name(&label)));
        tab.title_pinned = true;
        {
            let mut ws = self.ws.lock().unwrap();
            let outer = ws.outer_for_pty(&anchor).unwrap_or_else(|| anchor.clone());
            let Some(pane) = ws.panes.get_mut(&outer) else { return };
            pane.tabs.push(tab);
            pane.active_tab = pane.tabs.len() - 1;
            pane.dirty = true;
            ws.active_pane = Some(outer);
        }
        if let Some(view) = self.screen_views.get_mut(&view_id) {
            view.surface = Surface::Main;
        }
        self.screen_windows.remove(idx);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.focus_window();
            w.request_redraw();
        }
    }

    /// 창을 닫으면 보기도 닫는다 — sync 가 표에 없는 보기를 놓는다.
    fn close_screen_window(&mut self, idx: usize) {
        if idx < self.screen_windows.len() {
            self.screen_windows.remove(idx);
        }
    }

    /// 새 장이 풀린 꺼낸 창만 다시 그린다.
    pub(crate) fn redraw_screen_windows(&self) {
        for w in &self.screen_windows {
            if self.screen_views.get(&w.view_id).is_some_and(|v| v.has_new_frame()) {
                w.window.request_redraw();
            }
        }
    }

    pub(crate) fn screen_window_event(&mut self, idx: usize, event: WindowEvent) {
        let Some(w) = self.screen_windows.get_mut(idx) else { return };
        let (on, view_id) = (w.on(), w.view_id);
        match &event {
            WindowEvent::CloseRequested => {
                self.close_screen_window(idx);
                return;
            }
            WindowEvent::Resized(size) => {
                w.gpu.resize(size.width, size.height);
                w.window.request_redraw();
                return;
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                w.gpu.set_scale(w.window.scale_factor() as f32);
                w.gpu.set_font_size(FONT_SIZE);
                let size = w.window.inner_size();
                w.gpu.resize(size.width, size.height);
                w.window.request_redraw();
                return;
            }
            WindowEvent::Focused(f) => {
                w.focused = *f;
                w.window.request_redraw();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.render_screen_window(idx);
                return;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = w.gpu.scale().max(0.5);
                let was_header = w.cursor_px.1 <= HEADER_H;
                w.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                if was_header || w.cursor_px.1 <= HEADER_H {
                    w.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = w.cursor_px;
                let (bx, by, bw, bh) = w.dock_btn;
                if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
                    self.dock_screen_window(idx);
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{KeyCode, PhysicalKey};
                // 창 닫기·붙여넣기는 본창과 같은 단축키(맥 ⌘, 윈도우 Ctrl+Shift — 붙여넣기는 Ctrl+V 도)로 앱이 받는다.
                let host = self.host_mod();
                let pressed = event.state == ElementState::Pressed;
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };
                if pressed && host && code == Some(KeyCode::KeyW) {
                    self.close_screen_window(idx);
                    return;
                }
                let is_v = code == Some(KeyCode::KeyV);
                let ctrl = self.modifiers.control_key();
                let paste = pressed
                    && ((host && is_v) || crate::input::windows_paste_chord(cfg!(windows), ctrl, host, self.modifiers.alt_key(), is_v));
                if paste {
                    if !event.repeat {
                        self.screen_paste_for(view_id);
                    }
                } else {
                    let event = event.clone();
                    self.screen_key_for(view_id, &event);
                }
                self.redraw_screen_window(idx);
                return;
            }
            WindowEvent::Ime(ime) => {
                let ime = ime.clone();
                self.screen_ime_for(view_id, &ime);
                self.redraw_screen_window(idx);
                return;
            }
            _ => {}
        }
        let Some(w) = self.screen_windows.get(idx) else { return };
        let p = (f64::from(w.cursor_px.0), f64::from(w.cursor_px.1));
        if self.screen_pointer_on(&event, p, on) {
            self.redraw_screen_window(idx);
        }
    }

    fn redraw_screen_window(&self, idx: usize) {
        if let Some(w) = self.screen_windows.get(idx) {
            w.window.request_redraw();
        }
    }

    fn render_screen_window(&mut self, idx: usize) {
        let Some(w) = self.screen_windows.get_mut(idx) else { return };
        let (ww, wh) = w.logical_size();
        let scale = w.gpu.scale();
        let on = w.on();
        let name = crate::render::pane_identity::device_name(&w.label);
        w.gpu.clear_chrome();
        w.gpu.rect(0.0, 0.0, ww, wh, theme::bg());
        w.gpu.rect(0.0, HEADER_H - 1.0, ww, 1.0, theme::border());
        w.gpu.draw_text(
            PAD,
            (HEADER_H - 12.0) / 2.0,
            &format!("화면 · {name}"),
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::with_alpha(theme::text_mute(), if w.focused { 0xF0 } else { 0x99 }),
                bold: false,
                italic: false,
            },
        );
        let (bx, by) = (ww - BTN - PAD, (HEADER_H - BTN) / 2.0);
        let (mx, my) = w.cursor_px;
        let hover = mx >= bx && mx <= bx + BTN && my >= by && my <= by + BTN;
        if hover {
            crate::round_rect(&mut w.gpu, bx, by, BTN, BTN, theme::radius_sm(), theme::surface_hover());
        }
        w.gpu.queue_icon("undo-2", bx + (BTN - ICON) / 2.0, by + (BTN - ICON) / 2.0, ICON, if hover { theme::text() } else { theme::text_mute() });
        w.dock_btn = (bx, by, BTN, BTN);
        let body = (PAD, HEADER_H + PAD, (ww - PAD * 2.0).max(0.0), (wh - HEADER_H - PAD * 2.0).max(0.0));
        super::paint(&mut w.gpu, &mut self.screen_views, &[(w.view_id, body)], on, w.cursor_px);
        let _ = w.gpu.render(&[], scale, 0.0, true);
    }
}
