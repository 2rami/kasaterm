//! 사이드바 옆에서 밀려 나오는 판 — 보드와 아로나 모드가 여기 뜬다.
//!
//! 보드는 방 하나를 통째로 차지해서 열면 터미널이 사라졌다. 「왼쪽에서 밀려 나오는 판」
//! (2026-09-28 지시)이 되려면 터미널이 오른쪽에 그대로 보여야 하므로, 사이드바와 파일트리
//! 사이의 기둥으로 세운다 — 폭 배분(`chrome_widths`)에 끼어 있어 격자가 그만큼 비켜 선다.
//! 격자는 여는 순간 한 번에 맞추고(PTY 크기를 프레임마다 바꾸면 claude 가 매번 다시 그린다)
//! 판 그림만 미끄러져 나온다. 치수는 `docs/design.md` 「왼쪽 판」.

use super::*;

pub(crate) const LEFT_PANEL_W: f32 = 600.0;
pub(crate) const LEFT_PANEL_W_MIN: f32 = 360.0;
pub(crate) const LEFT_PANEL_W_MAX: f32 = 1400.0;
/// 좁은 창에서 폭 배분이 깎아 내릴 수 있는 하한.
pub(crate) const LEFT_PANEL_W_AUTO_MIN: f32 = 320.0;
pub(crate) const PANEL_HEAD_H: f32 = 32.0;
const SLIDE: std::time::Duration = std::time::Duration::from_millis(160);

type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeftPanelKind {
    Board,
    Arona,
}

impl LeftPanelKind {
    fn title(self) -> &'static str {
        match self {
            Self::Board => "보드",
            Self::Arona => "아로나",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Board => "rows-2",
            Self::Arona => "users",
        }
    }
    fn shortcut(self) -> &'static str {
        match self {
            Self::Board => "⇧⌘B",
            Self::Arona => "⇧⌘A",
        }
    }
}

pub(crate) struct LeftPanel {
    pub(crate) kind: Option<LeftPanelKind>,
    /// 사람이 끌어 정한 폭. 창이 좁을 때 깎이는 것은 `chrome_widths` 의 돌려준 값뿐이다.
    pub(crate) w_logical: f32,
    opened_at: Option<Instant>,
    /// 키가 판으로 가나. 막 열었거나 판을 누르면 참, 터미널을 누르면 거짓 — 터미널이 보이는
    /// 채로 열려 있으니 판이 키를 통째로 삼키면 옆 pane 에 글을 칠 수 없다.
    pub(crate) focused: bool,
    /// 폭 손잡이를 잡은 `(x, 잡을 때 폭)`.
    pub(crate) resize: Option<(f32, f32)>,
    /// 렌더가 적는 × 자리. 안 그린 프레임은 `None`.
    pub(crate) close_rect: Option<Rect>,
}

impl LeftPanel {
    pub(crate) fn new(w: Option<f32>) -> Self {
        Self {
            kind: None,
            w_logical: w.unwrap_or(LEFT_PANEL_W).clamp(LEFT_PANEL_W_MIN, LEFT_PANEL_W_MAX),
            opened_at: None,
            focused: false,
            resize: None,
            close_rect: None,
        }
    }
}

/// 열린 지 `elapsed` 인 판이 얼마나 나왔나(0..1). 끝으로 갈수록 느려진다.
fn slide_progress(elapsed: Option<std::time::Duration>) -> f32 {
    let Some(elapsed) = elapsed else { return 1.0 };
    let t = (elapsed.as_secs_f32() / SLIDE.as_secs_f32()).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 폭 손잡이 — 판 오른쪽 가장자리를 가운데 두고 6px.
fn grip_hit(panel: Rect, cx: f32, cy: f32) -> bool {
    let edge = panel.0 + panel.2;
    cx >= edge - 3.0 && cx <= edge + 3.0 && cy > panel.1 && cy < panel.1 + panel.3
}

impl App {
    pub(crate) fn left_panel_kind(&self) -> Option<LeftPanelKind> {
        self.left_panel.kind.filter(|_| !self.lite)
    }

    /// 보드가 판에 떠 있나. 설정 방이 앞에 있으면 판 자리가 없어 안 그린다.
    pub(crate) fn board_panel_open(&self) -> bool {
        self.left_panel_kind() == Some(LeftPanelKind::Board) && !self.internal_room_active_any()
    }

    /// 판이 이번 프레임에 원하는 폭(0 이면 닫힘) — `chrome_wants` 가 읽는다.
    pub(crate) fn left_panel_want_w(&self) -> f32 {
        if self.left_panel_kind().is_some() && !self.tabs_on_top {
            self.left_panel.w_logical
        } else {
            0.0
        }
    }

    /// 판 전체 자리(머리 포함). 사이드바 바로 오른쪽, 상태줄 위까지.
    pub(crate) fn left_panel_rect(&self) -> Option<Rect> {
        let w = self.left_panel_col_w();
        if w <= 0.0 {
            return None;
        }
        let win_h = self.window.as_ref()?.inner_size().height as f32 / self.effective_scale();
        Some((self.tab_strip_w(), TITLE_HEIGHT, w, (win_h - TITLE_HEIGHT - self.status_h()).max(1.0)))
    }

    /// 머리 아래 본문 — 보드가 그리고 아로나 웹뷰가 앉는 자리. 오른쪽 1px 은 경계선 몫이다.
    pub(crate) fn left_panel_body_rect(&self) -> Option<Rect> {
        self.left_panel_rect()
            .map(|(x, y, w, h)| (x, y + PANEL_HEAD_H, (w - 1.0).max(1.0), (h - PANEL_HEAD_H).max(1.0)))
    }

    /// 밀려 나오는 동안 그림을 왼쪽으로 미는 거리(0 이면 다 나왔다).
    pub(crate) fn left_panel_slide_px(&self) -> f32 {
        let progress = slide_progress(self.left_panel.opened_at.map(|at| at.elapsed()));
        if progress >= 1.0 {
            return 0.0;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        self.left_panel_col_w() * (1.0 - progress)
    }

    /// 판이 미끄러지는 중인가 — `render_frame` 의 그릴 사유다. `chrome_dirty` 는 그리기 끝에
    /// 지워지므로 그리는 도중에 세우면 사라져, 판이 다 숨은 첫 프레임에서 멈췄다(실측).
    pub(crate) fn left_panel_sliding(&self) -> bool {
        self.left_panel.opened_at.is_some_and(|at| at.elapsed() < SLIDE + std::time::Duration::from_millis(40))
    }

    pub(crate) fn left_panel_contains(&self, x: f32, y: f32) -> bool {
        self.left_panel_rect()
            .is_some_and(|(px, py, pw, ph)| x >= px && x < px + pw && y >= py && y < py + ph)
    }

    /// 키보드가 보드로 가야 하나.
    pub(crate) fn board_panel_focused(&self) -> bool {
        self.board_panel_open() && self.left_panel.focused
    }

    pub(crate) fn toggle_board_panel(&mut self) {
        if self.board_panel_open() {
            self.close_left_panel();
        } else {
            self.open_board_panel();
        }
    }

    /// 보드를 판으로 연다. 아로나가 떠 있으면 그 자리를 넘겨받는다.
    pub(crate) fn open_board_panel(&mut self) -> bool {
        if self.lite {
            return false;
        }
        if self.internal_room_active_any() && !self.return_from_active_internal_room() {
            return false;
        }
        self.close_inline_web();
        let return_pane = self.active_user_pane();
        let target_window = return_pane
            .as_deref()
            .and_then(|pane| self.window_of_pane(pane))
            .unwrap_or(self.active_window);
        // 폴더는 **활성 탭**의 것이다 — 바깥 pane id 로 물으면 첫 탭(또는 없는 PTY)을 보게
        // 되어, 탭을 바꿔 둔 pane 에서 「저장소가 아니다」가 떴다(2026-09-14 지적).
        let target_cwd = return_pane
            .as_deref()
            .map(|pane| self.ws.lock().unwrap().active_tab_pid(pane))
            .and_then(|tab| self.pane_current_cwd(&tab))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.board_scene.enter(return_pane, target_window, target_cwd);
        self.show_left_panel(LeftPanelKind::Board);
        self.request_native_board_refresh();
        true
    }

    /// 판을 세우고 격자를 한 번에 맞춘 뒤 **지금** 한 프레임 그린다 — 다음 프레임을 기다리면
    /// 그동안 판의 클릭 자리가 비어 첫 클릭이 헛돈다(렌더 카탈로그 58번).
    pub(crate) fn show_left_panel(&mut self, kind: LeftPanelKind) {
        let reopening = self.left_panel.kind.is_some();
        self.left_panel.kind = Some(kind);
        self.left_panel.focused = true;
        if !reopening {
            self.left_panel.opened_at = Some(Instant::now());
            // 미끄러지는 동안 프레임을 깨울 박자. 그리는 도중의 `request_redraw` 는 다음 틱에
            // 얹힐 곳이 없어 판이 첫 프레임에서 멈췄다 — 걷는 학생·혜성과 같은 펌프를 짧게 둔다.
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                let until = Instant::now() + SLIDE + std::time::Duration::from_millis(40);
                while Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                    if proxy.send_event(UserEvent::Redraw).is_err() {
                        break;
                    }
                }
            });
        }
        self.reflow_for_left_panel();
        self.prime_hit_areas();
    }

    pub(crate) fn close_left_panel(&mut self) {
        let Some(kind) = self.left_panel.kind.take() else {
            return;
        };
        if kind == LeftPanelKind::Board {
            self.native_board_blur();
            self.board_scene.leave();
        } else {
            self.close_inline_web();
        }
        self.left_panel.focused = false;
        self.left_panel.opened_at = None;
        self.left_panel.close_rect = None;
        self.left_panel.resize = None;
        self.reflow_for_left_panel();
        self.handoff_ime_to_active_surface();
    }

    fn reflow_for_left_panel(&mut self) {
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// 판 위를 눌렀다 — × 는 닫고, 손잡이는 폭 끌기를 시작하고, 나머지는 판에 초점을 준다.
    /// 판 밖을 누르면 초점을 터미널로 돌려준다. 판이 처리했으면 true.
    pub(crate) fn left_panel_press(&mut self, cx: f32, cy: f32) -> bool {
        let Some(panel) = self.left_panel_rect() else {
            return false;
        };
        if grip_hit(panel, cx, cy) {
            self.left_panel.resize = Some((cx, self.left_panel.w_logical));
            return true;
        }
        if !self.left_panel_contains(cx, cy) {
            if self.left_panel.focused {
                self.left_panel.focused = false;
                if self.board_panel_open() {
                    self.native_board_blur();
                }
                self.chrome_dirty = true;
            }
            return false;
        }
        self.left_panel.focused = true;
        if self.left_panel.close_rect.is_some_and(|r| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3) {
            self.close_left_panel();
            return true;
        }
        if self.board_panel_open() {
            self.native_board_click(cx, cy);
        }
        self.chrome_dirty = true;
        true
    }

    /// 끄는 중이면 폭을 맞추고 true.
    pub(crate) fn left_panel_drag(&mut self) -> bool {
        let Some((start_x, start_w)) = self.left_panel.resize else {
            return false;
        };
        let new_w = (start_w + (self.cursor_px.0 - start_x)).clamp(LEFT_PANEL_W_MIN, LEFT_PANEL_W_MAX);
        if (new_w - self.left_panel.w_logical).abs() > 0.5 {
            self.left_panel.w_logical = new_w;
            self.reflow_for_left_panel();
        }
        true
    }

    pub(crate) fn left_panel_release(&mut self) -> bool {
        if self.left_panel.resize.take().is_none() {
            return false;
        }
        self.save_ui_state();
        true
    }

    /// 보드에서 고른 pane 으로 — 판은 연 채로 두고 초점만 터미널로 넘긴다. 다른 방이면 그 방으로 간다.
    pub(crate) fn focus_pane_beside_board(&mut self, pane: &str) -> bool {
        self.native_board_blur();
        let focused = self.focus_surface(pane);
        if focused {
            self.left_panel.focused = false;
        }
        focused
    }

    pub(crate) fn left_panel_grip_hover(&self, cx: f32, cy: f32) -> bool {
        self.left_panel.resize.is_some() || self.left_panel_rect().is_some_and(|panel| grip_hit(panel, cx, cy))
    }
}

impl App {
    /// 격리 앱에서 판을 **진짜 클릭·끌기**(winit 이벤트를 window_event 로)로 눌러 본다.
    /// 켜기: `KASATERM_PANEL_PROBE=1` + 검증 실행(`KASATERM_WINDOW_SIZE`). 결과는 `[panel-probe]` 줄.
    /// 트레이 보드 → 손잡이 끌기 → 터미널 누르기(초점) → 판 누르기 → × → 트레이 아로나 → ×.
    pub(crate) fn run_pending_panel_probe(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        if !cfg!(debug_assertions)
            || !crate::verification_run()
            || std::env::var("KASATERM_PANEL_PROBE").as_deref() != Ok("1")
        {
            return;
        }
        static START: OnceLock<Instant> = OnceLock::new();
        static STEP: AtomicUsize = AtomicUsize::new(0);
        let step = STEP.load(Ordering::Relaxed);
        if step > 7 || START.get_or_init(Instant::now).elapsed().as_millis() < 4000 + step as u128 * 1500 {
            return;
        }
        let Some(window) = self.window.clone() else { return };
        let wid = window.id();
        let scale = self.effective_scale() as f64;
        let press = |app: &mut App, at: (f32, f32)| {
            app.cursor_px = at;
            for state in [ElementState::Pressed, ElementState::Released] {
                app.window_event(event_loop, wid, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left });
            }
        };
        let center = |r: Rect| (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        let report = |app: &App, label: &str| {
            eprintln!(
                "[panel-probe] {label} kind={:?} focused={} panel={:?} grid_left={}",
                app.left_panel_kind(), app.left_panel.focused, app.left_panel_rect(), app.effective_sidebar_w()
            );
        };
        STEP.store(step + 1, Ordering::Relaxed);
        match step {
            0 => press(self, center(self.board_btn_rect)),
            1 => {
                report(self, "tray_board");
                let Some(panel) = self.left_panel_rect() else { return };
                let grip = (panel.0 + panel.2, panel.1 + panel.3 / 2.0);
                self.cursor_px = grip;
                self.window_event(event_loop, wid, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Pressed, button: MouseButton::Left });
                let to = winit::dpi::PhysicalPosition::new((grip.0 as f64 + 120.0) * scale, grip.1 as f64 * scale);
                self.window_event(event_loop, wid, WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position: to });
                self.window_event(event_loop, wid, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Released, button: MouseButton::Left });
            }
            2 => {
                report(self, "dragged_120");
                let grid = (self.effective_sidebar_w() + 200.0, 300.0);
                press(self, grid);
            }
            3 => {
                report(self, "terminal_click");
                if let Some(body) = self.left_panel_body_rect() {
                    press(self, (body.0 + body.2 - 40.0, body.1 + body.3 - 20.0));
                }
            }
            4 => {
                report(self, "panel_click");
                if let Some(close) = self.left_panel.close_rect {
                    press(self, center(close));
                }
            }
            5 => {
                report(self, "closed");
                press(self, center(self.arona_btn_rect));
            }
            6 => {
                report(self, "tray_arona");
                if let Some(close) = self.left_panel.close_rect {
                    press(self, center(close));
                }
            }
            _ => report(self, "arona_closed"),
        }
        self.chrome_dirty = true;
        window.request_redraw();
    }
}

/// 판 머리(아이콘·이름·단축키·×)와 경계선. 본문은 보드 페인터와 아로나 웹뷰 몫이다.
pub(crate) fn draw_frame(g: &mut gpu::GpuRenderer, panel: &mut LeftPanel, rect: Rect, slide: f32, cursor: (f32, f32)) {
    panel.close_rect = None;
    let Some(kind) = panel.kind else { return };
    let (x, y, w, h) = rect;
    // 바탕은 제자리에 먼저 깐다 — 바탕까지 밀면 첫 프레임에 판 자리가 빈 구멍으로 보인다.
    // 미끄러지는 것은 그 위의 글과 본문이다.
    g.rect(x, y, w, h, theme::panel_bg());
    g.rect(x, y + PANEL_HEAD_H - 1.0, w, 1.0, theme::border());
    g.push_clip(x, y, w, h);
    let ox = x - slide;
    let title_color = if panel.focused { theme::text() } else { theme::text_dim() };
    g.queue_icon(kind.icon(), ox + 14.0, y + (PANEL_HEAD_H - theme::ICON_SIZE) / 2.0, theme::ICON_SIZE, title_color);
    g.draw_text(ox + 38.0, y + (PANEL_HEAD_H - 12.0) / 2.0 - 1.0, kind.title(),
        gpu::DrawOpts { font_size: 12.0, color: title_color, bold: true, italic: false });
    let title_w = g.measure_chrome_text(kind.title(), 12.0, true);
    g.draw_text(ox + 38.0 + title_w + 8.0, y + (PANEL_HEAD_H - 10.5) / 2.0, kind.shortcut(),
        gpu::DrawOpts { font_size: 10.5, color: theme::text_mute(), bold: false, italic: false });
    let close = (ox + w - 32.0, y + (PANEL_HEAD_H - 22.0) / 2.0, 22.0, 22.0);
    let hover = cursor.0 >= close.0 && cursor.0 <= close.0 + close.2 && cursor.1 >= close.1 && cursor.1 <= close.1 + close.3;
    g.hover_pointer |= hover;
    if hover {
        hover_rect(g, close.0, close.1, close.2, close.3, theme::radius_sm());
    }
    g.queue_icon("x", close.0 + 4.0, close.1 + 4.0, 14.0, if hover { theme::text() } else { theme::text_mute() });
    g.pop_clip();
    g.rect(x + w - 1.0, y, 1.0, h, theme::border());
    panel.close_rect = Some(close);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_starts_hidden_and_settles_open() {
        assert_eq!(slide_progress(Some(std::time::Duration::ZERO)), 0.0);
        assert_eq!(slide_progress(Some(SLIDE)), 1.0);
        assert_eq!(slide_progress(None), 1.0, "시각이 없으면 다 나온 판");
        let half = slide_progress(Some(SLIDE / 2));
        assert!(half > 0.5 && half < 1.0, "앞이 빠르고 끝이 느린 곡선 {half}");
    }

    #[test]
    fn grip_straddles_only_the_right_edge() {
        let panel = (200.0, 36.0, 600.0, 700.0);
        assert!(grip_hit(panel, 800.0, 300.0));
        assert!(grip_hit(panel, 797.5, 300.0));
        assert!(!grip_hit(panel, 790.0, 300.0), "판 안쪽은 판을 누른 것이다");
        assert!(!grip_hit(panel, 800.0, 20.0), "타이틀 띠는 손잡이가 아니다");
    }

    #[test]
    fn saved_width_is_clamped_to_a_usable_panel() {
        assert_eq!(LeftPanel::new(None).w_logical, LEFT_PANEL_W);
        assert_eq!(LeftPanel::new(Some(10.0)).w_logical, LEFT_PANEL_W_MIN);
        assert_eq!(LeftPanel::new(Some(9000.0)).w_logical, LEFT_PANEL_W_MAX);
    }

    /// 입구는 그대로 셋(현황 줄·트레이·단축키)이고 모두 판을 연다 — 보드 방은 없다.
    /// 단축키는 설정 방이 키를 삼키기 전에 잡아야 설정 화면에서도 먹는다.
    #[test]
    fn every_board_entry_opens_the_panel_not_a_room() {
        let handler = include_str!("handler.rs");
        let shortcut = handler
            .split_once("winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyB)")
            .expect("보드 단축키")
            .1;
        let body = &shortcut[..shortcut.find("return;").expect("단축키 갈래 끝")];
        assert!(body.contains("self.toggle_board_panel();"), "⇧⌘B 는 판을 여닫는다");
        let at = handler.find("self.toggle_board_panel();").expect("보드 토글");
        assert!(at < handler.find("self.native_settings_key(&event);").expect("설정 키"));
        assert!(handler.contains("작업현황 켜기/끄기  ⇧⌘B"), "메뉴가 단축키를 알려 줘야 사람이 찾는다");
        assert!(handler.contains("if hit(self.board_btn_rect) {\n                        self.toggle_board_panel();"));
        for source in [handler, include_str!("chrome.rs"), include_str!("session.rs"), include_str!("native_board.rs")] {
            assert!(!source.contains("open_board_room") && !source.contains("return_from_board_room"));
        }
    }

    /// 아로나는 「지금 보는 pane」을 작업 대상으로 삼는다. 설정 방은 셸 없는 표식 pane 이라
    /// 거기서 열면 대상이 내부 id 로 굳는다 — 여는 쪽이 사용자 방 복귀를 먼저 태운다. 단축키는
    /// 설정 방이 키를 삼키기 전에 잡아야 그 화면에서도 먹는다.
    #[test]
    fn arona_leaves_the_settings_room_and_its_shortcut_outranks_it() {
        let chrome = include_str!("chrome.rs");
        let after = chrome.split_once("fn toggle_arona_panel").expect("toggle_arona_panel").1;
        let body = &after[..after.find("\n    }\n").expect("함수 끝")];
        assert!(body.contains("return_from_active_internal_room"));
        let handler = include_str!("handler.rs");
        let shortcut = handler.find("self.toggle_arona_panel(event_loop);").expect("아로나 단축키");
        assert!(shortcut < handler.find("self.native_settings_key(&event);").expect("설정 키"));
    }

    /// 아로나 웹뷰는 판 본문 자리에 앉는다 — 예전처럼 사이드바 오른쪽 전체를 덮으면 터미널이
    /// 가려진다.
    #[test]
    fn arona_webview_sits_in_the_panel_body() {
        let chrome = include_str!("chrome.rs");
        let sync = chrome.split_once("pub(crate) fn sync_inline_web(&mut self)").expect("sync_inline_web").1;
        let body = &sync[..sync.find("\n    }\n").expect("함수 끝")];
        assert!(body.contains("left_panel_body_rect()"), "웹뷰 자리는 판 본문이 정한다");
    }
}
