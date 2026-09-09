//! 터미널 pane 을 별도 OS 창으로 뗀다(undock).
//!
//! 문서 창(auxwin.rs)과 달리 창은 내용을 안 든다 — `pane_id` 하나뿐이다. 셀
//! 그리드·커서는 App.ws 의 그 pane 이, PTY 는 App.pty 가 그대로 소유하고,
//! undock 은 레이아웃 트리에서 leaf 만 뺀다. 그래서 창을 오가도 셸·claude
//! 세션이 끊기지 않는다. 되돌리기(dock)는 활성 pane 오른쪽에 `split_leaf` 로
//! 되꽂는다. 2026-09-03 에 걷어낸 옛 auxwin.rs(`f5264ebd^`)의 터미널 부분을
//! 새 문서 창 틀 위에 다시 올린 것이다(2026-09-09 지시).
//!
//! 그림은 본창과 **같은 함수**로 만든다 — `compose_terminal_pane` 이 학생
//! 스프라이트·상태줄·입력상자 붙잡기까지 한 번에 내주므로, 별도창만 다른
//! 모습이 되던 옛 사고(자리표시자 칩이 남던 것)가 구조적으로 안 난다.
use super::*;

/// 별도창 헤더 높이. macOS 는 본창 `TITLE_HEIGHT` 와 맞춰 두 창이 같은 앱으로
/// 읽히게 하고, 그 외는 OS 타이틀바가 따로 있으니 더 얇게 둔다.
#[cfg(target_os = "macos")]
const AUX_HEADER_H: f32 = TITLE_HEIGHT;
#[cfg(not(target_os = "macos"))]
const AUX_HEADER_H: f32 = 30.0;

/// 헤더 양끝 여백.
const AUX_HEADER_PAD: f32 = 6.0;
/// 헤더 버튼 한 변.
const AUX_HEADER_BTN: f32 = 26.0;
/// 라벨(학생·pane·방)이 시작되는 x. 신호등 숨김은 안 옮겼으므로 여백만큼만.
const AUX_HEADER_X: f32 = AUX_HEADER_PAD;
/// 셀 그리드가 시작되는 y — 헤더 띠 바로 아래. 행 수·커서·원점이 전부 이 하나를
/// 지나야 마지막 줄이 창 밖으로 밀리거나 커서가 헤더 위에 찍히지 않는다.
const AUX_CELL_TOP: f32 = PANE_INNER_Y + AUX_HEADER_H;
/// 칸수로 창 크기를 잴 때의 하한(논리 px).
const AUX_MIN_W: f32 = 420.0;
const AUX_MIN_H: f32 = 260.0;
/// 칸수 정보가 없을 때의 기본 창 크기.
const AUX_DEFAULT_W: f32 = 800.0;
const AUX_DEFAULT_H: f32 = 520.0;
/// 별도창 자리를 살릴 때 요구하는 최소 클라이언트 크기(물리 px).
const AUX_RESTORE_MIN_W: u32 = 320;
const AUX_RESTORE_MIN_H: u32 = 240;

/// `gpu` 가 `window` 앞에 선언된다 — surface 는 창이 살아 있을 때 먼저 놓아야 한다.
pub(crate) struct AuxTerminal {
    pub(crate) pane_id: String,
    /// 떠나온 방(창 인덱스). dock 이 돌아갈 곳이고 헤더 라벨에 쓴다. 방 순서가
    /// 바뀌거나 방이 닫히면 session.rs 의 remap 이 함께 옮긴다.
    pub(crate) home_window: usize,
    gpu: gpu::GpuRenderer,
    dirty: bool,
    cursor_px: (f32, f32),
    focused: bool,
    preedit: String,
    view_shift: Option<crate::PaneViewShift>,
    last_title: String,
    /// 이 창의 글꼴 크기(논리 px). Cmd+=/- 로 창마다 따로 움직인다.
    font_size: f32,
    pinned: bool,
    tab_bar_h: f32,
    tabs_out: crate::render::TabStripOut,
    header_hits: Vec<(TermHeaderBtn, (f32, f32, f32, f32))>,
    /// 하네스(`KASATERM_AUTOUNDOCK_CAP`)가 건 캡처 예약 — 때가 되면 `gpu.capture_next`.
    pub(crate) capture_at: Option<(Instant, String)>,
    pub(crate) window: Arc<Window>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TermHeaderBtn {
    Pin,
    Dock,
}

impl AuxTerminal {
    /// 창 client 영역의 논리 크기(px).
    fn logical_size(&self) -> (f32, f32) {
        let scale = self.gpu.scale().max(0.5);
        let phys = self.window.inner_size();
        (
            phys.width.max(1) as f32 / scale,
            phys.height.max(1) as f32 / scale,
        )
    }

    /// 셀 그리드가 시작되는 x. 셀 원점·커서·cols 계산이 전부 이 하나를 지난다.
    fn cell_left(&self) -> f32 {
        PANE_INNER_X
    }

    /// 본문이 시작하는 y. 헤더 아래 탭 띠가 서면 그만큼 내려간다.
    fn cell_top(&self) -> f32 {
        AUX_CELL_TOP + self.tab_bar_h
    }

    /// 창 client 를 셀수로 환산한 값. PTY resize 와 렌더가 **같은 식**을 쓴다 —
    /// 둘이 갈리면 입력상자 붙잡기가 다른 행 수를 보고 마지막 줄을 잘라 먹는다.
    fn grid_cells(&self) -> (u16, u16) {
        let (cw, ch) = (self.gpu.cell_w, self.gpu.cell_h);
        if cw <= 0.0 || ch <= 0.0 {
            return (80, 24);
        }
        let (w, h) = self.logical_size();
        let cols = (((w - self.cell_left() - PANE_INNER_X) / cw).floor() as i32).max(1) as u16;
        let rows = (((h - self.cell_top() - PANE_INNER_Y) / ch).floor() as i32).max(1) as u16;
        (cols, rows)
    }

    fn redraw(&mut self) {
        self.dirty = true;
        self.window.request_redraw();
    }
}

/// 별도창 외곽선. 학생이 있으면 그 색으로 — 메인 그리드의 active pane 테두리와
/// 같은 신호다. 없어도 두른다: 어두운 바탕 위에서 창 경계가 배경색 차이 하나뿐이면
/// 어디서 끝나는지 안 보인다. 세로 변은 가로 변 **사이만** 채운다 — 네 변을 통짜로
/// 그리면 모서리가 두 번 칠해져 포커스 없는 창의 꼭짓점이 점처럼 튄다.
fn draw_aux_border(t: &mut AuxTerminal, w: f32, h: f32, accent: Option<[u8; 4]>, focused: bool) {
    const T: f32 = 1.5;
    let base = accent.unwrap_or_else(theme::border);
    let col = theme::with_alpha(base, if focused { 0xFF } else { 0x66 });
    t.gpu.rect(0.0, 0.0, w, T, col);
    t.gpu.rect(0.0, h - T, w, T, col);
    t.gpu.rect(0.0, T, T, (h - T * 2.0).max(0.0), col);
    t.gpu.rect(w - T, T, T, (h - T * 2.0).max(0.0), col);
}

/// 헤더 오른쪽 버튼 — [접기][되돌리기]. 그리면서 히트 rect 을 창에 적어 두므로
/// 그림과 판정이 갈릴 수 없다. 되돌리기는 ⌘W 와 같은 동작이지만, 그 단축키를
/// 모르면 창에 갇힌다.
fn draw_term_header_btns(t: &mut AuxTerminal, w: f32) {
    const B: f32 = AUX_HEADER_BTN;
    const ICON: f32 = 14.0;
    // `corner-down-left` 은 아이콘 표에 없어 아무것도 안 그려졌던 전례가 있다 —
    // 뜻이 같고 실재하는 undo-2 를 쓴다.
    let btns: [(TermHeaderBtn, &str); 2] =
        [(TermHeaderBtn::Pin, "pin"), (TermHeaderBtn::Dock, "undo-2")];
    let n = btns.len();
    t.header_hits.clear();
    let (mx, my) = t.cursor_px;
    for (i, (kind, icon)) in btns.into_iter().enumerate() {
        let bx = w - B * (n - i) as f32 - AUX_HEADER_PAD;
        let by = (AUX_HEADER_H - B) / 2.0;
        let hov = mx >= bx && mx <= bx + B && my >= by && my <= by + B;
        if hov {
            crate::round_rect(
                &mut t.gpu,
                bx,
                by,
                B,
                B,
                theme::radius_sm(),
                theme::surface_hover(),
            );
        }
        let lit = kind == TermHeaderBtn::Pin && t.pinned;
        t.gpu.queue_icon(
            icon,
            bx + (B - ICON) / 2.0,
            by + (B - ICON) / 2.0,
            ICON,
            if lit {
                theme::accent()
            } else if hov {
                theme::text()
            } else {
                theme::text_mute()
            },
        );
        t.header_hits.push((kind, (bx, by, B, B)));
    }
}

/// Ctrl+글자 → 제어바이트(^A=0x01 … ^Z=0x1a). 메인 forward_key 의 같은 표를 winit
/// KeyCode 기준으로 옮긴 것.
fn ctrl_byte(code: winit::keyboard::KeyCode) -> Option<u8> {
    use winit::keyboard::KeyCode;
    let b = match code {
        KeyCode::KeyA => 0x01,
        KeyCode::KeyB => 0x02,
        KeyCode::KeyC => 0x03,
        KeyCode::KeyD => 0x04,
        KeyCode::KeyE => 0x05,
        KeyCode::KeyF => 0x06,
        KeyCode::KeyG => 0x07,
        KeyCode::KeyH => 0x08,
        KeyCode::KeyI => 0x09,
        KeyCode::KeyJ => 0x0a,
        KeyCode::KeyK => 0x0b,
        KeyCode::KeyL => 0x0c,
        KeyCode::KeyM => 0x0d,
        KeyCode::KeyN => 0x0e,
        KeyCode::KeyO => 0x0f,
        KeyCode::KeyP => 0x10,
        KeyCode::KeyQ => 0x11,
        KeyCode::KeyR => 0x12,
        KeyCode::KeyS => 0x13,
        KeyCode::KeyT => 0x14,
        KeyCode::KeyU => 0x15,
        KeyCode::KeyV => 0x16,
        KeyCode::KeyW => 0x17,
        KeyCode::KeyX => 0x18,
        KeyCode::KeyY => 0x19,
        KeyCode::KeyZ => 0x1a,
        _ => return None,
    };
    Some(b)
}

/// 저장된 창 틀이 지금 붙어 있는 모니터 안에 있나. 본창(handler.rs)과 같은 규칙 —
/// 좌상단 점이 아니라 **창 중심**으로 본다. 뗀 모니터 좌표로 화면 밖에 뜨는 것을
/// 막는다.
fn frame_on_screen(frame: &crate::auxwin::FrameRecord, event_loop: &ActiveEventLoop) -> bool {
    let cx = frame.x as f64 + frame.width as f64 / 2.0;
    let cy = frame.y as f64 + frame.height as f64 / 2.0;
    event_loop.available_monitors().any(|m| {
        let mp = m.position();
        let ms = m.size();
        cx >= mp.x as f64
            && cx < mp.x as f64 + ms.width as f64
            && cy >= mp.y as f64
            && cy < mp.y as f64 + ms.height as f64
    })
}

impl App {
    pub(crate) fn aux_terminal_index(&self, pane_id: &str) -> Option<usize> {
        self.aux
            .terminals
            .iter()
            .position(|t| t.pane_id == pane_id)
    }

    pub(crate) fn aux_terminal_window_index(&self, id: WindowId) -> Option<usize> {
        self.aux
            .terminals
            .iter()
            .position(|t| t.window.id() == id)
    }

    /// (pane, 떠나온 방) 전부 — 배치도·`pane_window` 미러·세션 저장이 쓴다.
    pub(crate) fn undocked_panes(&self) -> Vec<(String, usize)> {
        self.aux
            .terminals
            .iter()
            .map(|t| (t.pane_id.clone(), t.home_window))
            .collect()
    }

    /// `i` 번 방에서 떠난 별도창 pane 들.
    pub(crate) fn room_undocked(&self, i: usize) -> Vec<String> {
        self.aux
            .terminals
            .iter()
            .filter(|t| t.home_window == i)
            .map(|t| t.pane_id.clone())
            .collect()
    }

    /// 세션 저장용 (pane, 방, 창 틀). 틀은 문서 창 `AuxWindow::record` 와 같은
    /// 규칙 — macOS 는 `inner_position`(with_position 이 클라이언트 원점을 놓는다).
    pub(crate) fn undocked_frames(&self) -> Vec<(String, usize, crate::auxwin::FrameRecord)> {
        self.aux
            .terminals
            .iter()
            .map(|t| {
                let size = t.window.inner_size();
                #[cfg(target_os = "macos")]
                let pos = t
                    .window
                    .inner_position()
                    .or_else(|_| t.window.outer_position())
                    .unwrap_or_else(|_| winit::dpi::PhysicalPosition::new(0, 0));
                #[cfg(not(target_os = "macos"))]
                let pos = t
                    .window
                    .outer_position()
                    .unwrap_or_else(|_| winit::dpi::PhysicalPosition::new(0, 0));
                (
                    t.pane_id.clone(),
                    t.home_window,
                    crate::auxwin::FrameRecord {
                        x: pos.x,
                        y: pos.y,
                        width: size.width,
                        height: size.height,
                    },
                )
            })
            .collect()
    }

    /// 하네스 캡처 예약이 익은 창에 `capture_next` 를 건다 — 다음 프레임이 PNG 를
    /// 뽑는다(문서 창 `aux_capture` 와 같은 기전).
    pub(crate) fn consume_aux_terminal_captures(&mut self) {
        let now = Instant::now();
        for t in &mut self.aux.terminals {
            let due = matches!(&t.capture_at, Some((at, _)) if now >= *at);
            if !due {
                continue;
            }
            if let Some((_, path)) = t.capture_at.take() {
                eprintln!("[auxterm] capture {} → {path}", t.pane_id);
                t.gpu.capture_next = Some(path);
                t.redraw();
            }
        }
    }
}

// ---- 창 생성·크기 ----
impl App {
    /// `pane_id` 터미널 pane 을 별도창으로 띄운다. `near` Some 이면 그 물리좌표에,
    /// `want_cells` Some 이면 그 칸수가 들어가는 크기로(그리드에서 쓰던 폭·높이
    /// 유지), `frame` Some 이면 저장된 자리·크기 그대로(칸수 측정은 건너뜀). 새 창
    /// 인덱스 반환. 진입점(undock)은 이미 트리에서 leaf 를 빼고 pty·ws.panes 를
    /// 유지한 상태로 부른다.
    pub(crate) fn spawn_aux_terminal(
        &mut self,
        pane_id: String,
        home_window: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
        want_cells: Option<(u16, u16)>,
        frame: Option<crate::auxwin::FrameRecord>,
    ) -> Option<usize> {
        let title = pane_id.clone();
        let wants_focus = !crate::background_launch();
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            .with_theme(Some(theme::window_theme()))
            .with_active(wants_focus);
        match &frame {
            Some(f) => {
                attrs = attrs
                    .with_position(winit::dpi::PhysicalPosition::new(f.x, f.y))
                    .with_inner_size(winit::dpi::PhysicalSize::new(
                        f.width.max(AUX_RESTORE_MIN_W),
                        f.height.max(AUX_RESTORE_MIN_H),
                    ));
            }
            None => {
                attrs = attrs.with_inner_size(LogicalSize::new(AUX_DEFAULT_W, AUX_DEFAULT_H));
                if let Some(pos) = near {
                    attrs = attrs.with_position(pos);
                }
            }
        }
        let window = match crate::auxwin::create_untabbed(event_loop, attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxterm] 터미널 별도창 생성 실패: {e}");
                return None;
            }
        };
        // 메인 창과 같은 IME 정책: macOS 는 OS IME 끄고 in-process hangul, 그 외는 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let mut gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxterm] 터미널 별도창 렌더러 생성 실패: {e}");
                return None;
            }
        };
        // 그리드에서 쓰던 칸수를 그대로 담을 크기로 창을 다시 잰다. 고정 크기면
        // 넓게 쓰던 pane 이 꺼내는 순간 좁아져 오른쪽·아래가 잘린다. gpu 를 만든
        // 뒤라야 이 창의 셀 크기를 알고, 그래야 칸수→px 환산이 맞는다.
        if frame.is_none() {
            if let Some((wc, hc)) = want_cells {
                let (cw, ch) = (gpu.cell_w, gpu.cell_h);
                if cw > 0.0 && ch > 0.0 {
                    // 모니터 밖으로 나가는 창은 잘림을 옮겨 놓을 뿐이다 — 작업영역에 맞춘다.
                    let (max_w, max_h) = window
                        .current_monitor()
                        .map(|m| {
                            let s = m.scale_factor() as f32;
                            (
                                m.size().width as f32 / s * 0.94,
                                m.size().height as f32 / s * 0.88,
                            )
                        })
                        .unwrap_or((1600.0, 1000.0));
                    let want_w = (PANE_INNER_X * 2.0 + wc as f32 * cw).clamp(AUX_MIN_W, max_w);
                    let want_h = (AUX_CELL_TOP + PANE_INNER_Y + hc as f32 * ch)
                        .clamp(AUX_MIN_H, max_h);
                    if let Some(got) = window.request_inner_size(LogicalSize::new(want_w, want_h)) {
                        gpu.resize(got.width, got.height);
                    }
                }
            }
        }
        self.aux.terminals.push(AuxTerminal {
            pane_id: pane_id.clone(),
            home_window,
            gpu,
            dirty: true,
            cursor_px: (0.0, 0.0),
            focused: wants_focus,
            preedit: String::new(),
            view_shift: None,
            last_title: title,
            font_size: FONT_SIZE,
            pinned: false,
            tab_bar_h: 0.0,
            tabs_out: Default::default(),
            header_hits: Vec::new(),
            capture_at: None,
            window,
        });
        let idx = self.aux.terminals.len() - 1;
        eprintln!(
            "[auxterm] opened window #{idx} for {pane_id} 요청칸={want_cells:?} 창={:?}",
            self.aux.terminals[idx].logical_size()
        );
        // 창 client 크기에 맞춰 PTY 를 즉시 resize — 셸이 SIGWINCH 로 새 셀수에 리플로우.
        self.aux_terminal_resize_pty(idx);
        self.aux.terminals[idx].redraw();
        Some(idx)
    }

    /// 창 client 크기를 셀수로 환산해 이 창이 보는 pane 의 PTY 를 resize. 활성 탭만이
    /// 아니라 그 pane 의 **모든 터미널 탭**에 보낸다 — 탭을 바꿨을 때 옛 크기로
    /// 남은 셸이 화면을 찢지 않게.
    fn aux_terminal_resize_pty(&mut self, idx: usize) {
        // 탭 띠 높이를 먼저 갱신해야 아래 rows 가 맞는다.
        self.aux_sync_tab_bar(idx);
        let (pane_id, cols, rows) = {
            let Some(t) = self.aux.terminals.get(idx) else { return };
            if t.gpu.cell_w <= 0.0 || t.gpu.cell_h <= 0.0 {
                return;
            }
            let (c, r) = t.grid_cells();
            (t.pane_id.clone(), c, r)
        };
        let pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            match ws.panes.get(&pane_id) {
                Some(p) => p
                    .tabs
                    .iter()
                    .filter(|t| matches!(t.content, PaneContent::Terminal(_)))
                    .map(|t| t.pid.clone().unwrap_or_else(|| pane_id.clone()))
                    .collect(),
                None => vec![pane_id.clone()],
            }
        };
        for pid in pids {
            if let Some(pty) = self.pty.get(&pid) {
                let _ = pty.resize(cols, rows);
            }
        }
    }

    /// 이 창이 보는 pane 의 탭 수를 물어 `tab_bar_h` 를 갱신한다. **탭이 하나면 0** —
    /// 띠를 아예 안 세우므로 단일 탭 창은 메인 그리드와 같은 모습이다.
    fn aux_sync_tab_bar(&mut self, idx: usize) {
        let Some(pane_id) = self.aux.terminals.get(idx).map(|t| t.pane_id.clone()) else {
            return;
        };
        let multi = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(&pane_id).is_some_and(|p| p.tabs.len() > 1)
        };
        if let Some(t) = self.aux.terminals.get_mut(idx) {
            t.tab_bar_h = if multi { PANE_HEADER_HEIGHT } else { 0.0 };
        }
    }
}

// ---- 렌더 ----
impl App {
    /// 터미널 별도창 한 프레임. 셀·학생·상태줄은 본창과 같은 `compose_terminal_pane`
    /// 을 지나고, 그 위에 커서·조합·테두리를 얹는다.
    fn aux_terminal_render(&mut self, idx: usize, blink: bool) {
        self.aux_sync_tab_bar(idx);
        let (pane_id, home, scale, w, h, focused, cell_left, cell_top, cw, ch, cols, rows, cursor_px) = {
            let Some(t) = self.aux.terminals.get(idx) else { return };
            let (w, h) = t.logical_size();
            let (cols, rows) = t.grid_cells();
            (
                t.pane_id.clone(),
                t.home_window,
                t.gpu.scale(),
                w,
                h,
                t.focused,
                t.cell_left(),
                t.cell_top(),
                t.gpu.cell_w,
                t.gpu.cell_h,
                cols,
                rows,
                t.cursor_px,
            )
        };
        // compose 는 슬롯을 본창 셀 크기(`self.cell`)로 놓는다 — 이 창의 셀 크기와의
        // 비율을 font_scale 로 넘겨야 얼굴·아이콘이 이 창의 글자 위에 앉는다.
        let font_scale = if cw > 0.0 && self.cell.w > 0.0 { cw / self.cell.w } else { 1.0 };
        let (composition, student, labels, tab_state, tab_pid, cursor, pulled, cursor_color, alive) = {
            let ws = self.ws.lock().unwrap();
            let pane = ws.panes.get(&pane_id);
            let tab_pid = ws.active_tab_pid(&pane_id);
            // 이름·탭 제목은 **메인 그리드와 같은 함수**로 — 두 벌로 두면 이 창만
            // OSC 날제목으로 돌아가거나 셸만 도는 pane 이 학생으로 보인다.
            let student = self.display_pane_char(&ws, &pane_id);
            let labels = pane
                .map(|p| self.pane_tab_labels(&ws, &pane_id, p))
                .unwrap_or_default();
            let tab_state = pane
                .map(|p| (p.active_tab, p.tab_first, p.tab_last_active))
                .unwrap_or((0, 0, 0));
            let composition = pane.and_then(|p| {
                p.term().map(|term| {
                    self.compose_terminal_pane(
                        &ws,
                        p,
                        Some(term),
                        &pane_id,
                        tab_pid.clone(),
                        cols as usize,
                        rows as usize,
                        cell_left,
                        cell_top,
                        font_scale,
                        true,
                        &HashMap::new(),
                    )
                })
            });
            let cursor = pane.and_then(|p| p.term()).map(|t| {
                (
                    t.cursor_row,
                    t.cursor_col,
                    t.cursor_visible,
                    cursor_cell_width(&t.cells, t.cursor_row, t.cursor_col),
                )
            });
            // 화면을 아래로 당긴 pane 은 커서도 같은 만큼 내린다(본창 render 와 같은 근거).
            let pulled = pane
                .and_then(|p| p.term())
                .map(|t| self.bottom_pull_rows(&tab_pid, t, t.cells.len()).len() as u16)
                .unwrap_or(0);
            let cursor_color = ws
                .pane_character
                .get(&tab_pid)
                .and_then(|name| {
                    theme::character_accent_n(
                        name,
                        theme::character_ordinal(&ws.pane_character, &tab_pid),
                    )
                })
                .unwrap_or_else(theme::cursor);
            (composition, student, labels, tab_state, tab_pid, cursor, pulled, cursor_color, pane.is_some())
        };
        // 색은 실제로 학생이 도는 pane 만(본창 pane 테두리와 같은 규칙), 이름은
        // 배정만으로 뜬다(본창 pane 헤더와 같은 규칙) — 두 규칙이 원래 다르다.
        let runs_agent = self
            .pty
            .get(&tab_pid)
            .and_then(|p| p.active_agent())
            .is_some();
        let accent = runs_agent
            .then(|| student.as_deref().and_then(theme::character_accent_any))
            .flatten();
        let working = self
            .pane_activity
            .get(&pane_id)
            .is_some_and(|a| a.status == "working");
        let anim_ms = self.version_anim_start.elapsed().as_millis() as u64;
        let (cursor_shape, cursor_thickness) = (self.cursor_shape, self.cursor_thickness);
        let label = match &student {
            Some(name) => format!("{name} · {pane_id} · {}번 방", home + 1),
            None => format!("{pane_id} · {}번 방", home + 1),
        };
        let view_shift = composition.as_ref().and_then(|scene| {
            scene.view_shifts.iter().find(|(id, _)| id == &pane_id).map(|(_, shift)| shift.clone())
        });
        let cursor = cursor.and_then(|(row, col, visible, width)| {
            let position = view_shift.as_ref().map(|shift| shift.display_pos(row as usize, col as usize))
                .unwrap_or(Some((row as usize + pulled as usize, col as usize)));
            position.map(|(row, col)| (row, col, visible, width))
        });
        let Some(t) = self.aux.terminals.get_mut(idx) else { return };
        t.view_shift = view_shift;
        if t.last_title != label {
            t.window.set_title(&label);
            t.last_title = label.clone();
        }
        t.gpu.clear_chrome();
        t.gpu.rect(0.0, 0.0, w, h, theme::bg());
        // ── 헤더 ── 왼쪽에 「아루 · %3 · 2번 방」, 오른쪽에 접기·되돌리기. 배경은
        // 창 배경 그대로 두고 아래 hairline 만 남긴다.
        {
            t.gpu.rect(0.0, AUX_HEADER_H - 1.0, w, 1.0, theme::border());
            let label_col = accent.unwrap_or_else(theme::text_mute);
            t.gpu.draw_text(
                AUX_HEADER_X,
                (AUX_HEADER_H - 12.0) / 2.0,
                &label,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: theme::with_alpha(label_col, if focused { 0xF0 } else { 0x99 }),
                    bold: student.is_some(),
                    italic: false,
                },
            );
            // working 스윕바 — 메인 pane 상단의 그것과 같은 신호. 없으면 꺼낸 창만
            // 멈춘 것처럼 보인다.
            if working {
                const BAR_H: f32 = 2.5;
                let bar = accent.unwrap_or_else(theme::accent);
                let phase = crate::sprites::anim_phase_secs();
                t.gpu.rect(0.0, 0.0, w, BAR_H, theme::with_alpha(bar, 0x2e));
                let seg = (w * 0.32).clamp(36.0, 160.0);
                let span = w + seg;
                let off = (phase * 0.5).fract() * span - seg;
                let sx = off.max(0.0);
                let ex = (off + seg).min(w);
                if ex > sx {
                    t.gpu.rect(sx, 0.0, ex - sx, BAR_H, bar);
                }
            }
            draw_term_header_btns(t, w);
        }
        // ── 탭 띠 ── 탭이 둘 이상일 때만. 메인 그리드와 같은 함수를 지나므로 라벨
        // 자르기·× 자리·활성 표시가 저절로 같다.
        if t.tab_bar_h > 0.0 {
            let (cx, cy) = cursor_px;
            // hover 는 직전 프레임이 그린 rect 로 판정한다 — 그린 자리가 곧 히트라는
            // `header_hits` 규약 그대로.
            let hov = t
                .tabs_out
                .tab_hits
                .iter()
                .find(|(_, (rx, ry, rw, rh))| cx >= *rx && cx <= rx + rw && cy >= *ry && cy <= ry + rh)
                .map(|(i, _)| *i);
            let ctx = crate::render::TabStrip {
                tabs: &labels,
                label: &pane_id,
                x: 0.0,
                y: AUX_HEADER_H,
                w,
                active_tab: tab_state.0,
                tab_first: tab_state.1,
                tab_last_active: tab_state.2,
                is_active: focused,
                color: accent,
                right_reserve: 0.0,
                chrome_font: 12.0,
                icon_size: theme::ICON_SIZE,
                act_fg: theme::text_dim(),
                cursor_px: (cx, cy),
                hover_tab: hov,
                // 창 안 탭 재배치·창 간 드래그는 없다 — 메인 그리드에서 한다.
                drag_src: None,
                drag_target: None,
                frozen_slots: None,
            };
            t.tabs_out = crate::render::draw_pane_tabs(&mut t.gpu, &ctx);
        } else {
            t.tabs_out = Default::default();
        }
        // ── 본문 ──
        let mut animated = false;
        match composition {
            Some(c) => {
                // origin_px 는 물리 px(draw_cells 규약), 커서 rect 은 논리 px(gpu.rect 규약).
                let slot = gpu::PaneSlot {
                    rows: &c.rows,
                    origin_px: (cell_left * scale, cell_top * scale),
                    font_scale: 1.0,
                    dim: false,
                    links: Vec::new(),
                    default_fg: crate::cells::default_fg(),
                };
                t.gpu.draw_cells(&[slot]);
                crate::screenread::paint_status_model_icons(&mut t.gpu, &c.status_model_icons);
                // 학생 스프라이트 — 메인 그리드와 같은 함수·같은 이미지 키.
                let overlays = crate::screenread::StudentOverlays {
                    banner: c.banner_slots,
                    spinner: c.spinner_slots,
                    waiting: c.waiting_slots,
                    standing: c.standing_slots,
                    profile: c.profile_slots,
                };
                crate::screenread::paint_student_overlays(&mut t.gpu, &overlays, anim_ms);
                animated = c.animated_cells;
                // 커서 자리(논리 px). 조합 중 한글이 있으면 프리에딧을, 없으면 blink 커서.
                if let Some((row, col, vis, cur_w)) = cursor {
                    let px = cell_left + col as f32 * cw;
                    let py = cell_top + row as f32 * ch;
                    if t.preedit.is_empty() {
                        if vis && focused && blink {
                            let mut col = cursor_color;
                            col[3] = 140; // ~0.55 alpha (paint_gpu_overlays 와 동일)
                            for q in crate::cursor::cursor_primitives(
                                cursor_shape,
                                px,
                                py,
                                cw,
                                ch,
                                cur_w,
                                cursor_thickness,
                            )
                            .as_slice()
                            {
                                t.gpu.rect(q.x, q.y, q.width, q.height, col);
                            }
                        }
                    } else {
                        let pe = t.preedit.clone();
                        t.gpu.draw_preedit(px, py, &pe, cursor_color, 1.0);
                    }
                }
            }
            None if alive => {
                // 그림·마크다운·웹 탭 — 이 창은 v1 에서 못 그린다. 빈 창으로 두면
                // 고장으로 읽히므로 이유를 적는다.
                let msg = "이 탭은 별도 창에서 아직 못 봐요 — 메인 창에서 보세요";
                let fw = t.gpu.measure_chrome_text(msg, 13.0, false);
                let tx = ((w - fw) / 2.0).max(cell_left);
                let ty = (h / 2.0 - 8.0).max(cell_top);
                t.gpu.draw_text(
                    tx,
                    ty,
                    msg,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            None => {}
        }
        // 창 외곽선 — 셀 위에 얹어야 가장자리 글자에 안 먹힌다.
        draw_aux_border(t, w, h, accent, focused);
        let _ = t.gpu.render(&[], scale, 0.0, true);
        t.dirty = false;
        // 스윕바·애니 셀은 다음 프레임을 스스로 불러야 한다 — PTY 출력이 없으면
        // 아무도 이 창을 다시 그리지 않아 한 자리에 얼어붙는다.
        if working || animated {
            t.redraw();
        }
    }
}

// ---- 이벤트·입력 ----
impl App {
    /// 이 창의 **활성 탭**이 셸이면 그 탭의 PTY id. 셸이 아니면 `None`.
    ///
    /// `self.pty.get(pane_id)` 를 직접 쓰면 안 된다 — pane id 는 첫 탭의 pid 라,
    /// 그림·마크다운 탭을 보는 동안 친 키가 화면에 없는 첫 탭 셸에 박힌다.
    fn aux_active_term_pid(&self, pane_id: &str) -> Option<String> {
        let ws = self.ws.lock().ok()?;
        let p = ws.panes.get(pane_id)?;
        let t = p.tabs.get(p.active_tab)?;
        matches!(t.content, PaneContent::Terminal(_))
            .then(|| t.pid.clone().unwrap_or_else(|| pane_id.to_string()))
    }

    /// 터미널 별도창 이벤트 라우팅. Resized/Scale 는 PTY resize 까지, Close 는 dock 복귀.
    pub(crate) fn aux_terminal_event(
        &mut self,
        idx: usize,
        event: WindowEvent,
        _event_loop: &ActiveEventLoop,
    ) {
        match event {
            WindowEvent::CloseRequested => self.dock_pane_terminal(idx),
            WindowEvent::Resized(size) => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.gpu.resize(size.width, size.height);
                    t.redraw();
                }
                self.aux_terminal_resize_pty(idx);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    let sf = t.window.scale_factor() as f32;
                    t.gpu.set_scale(sf);
                    let fs = t.font_size;
                    t.gpu.set_font_size(fs);
                    let sz = t.window.inner_size();
                    t.gpu.resize(sz.width, sz.height);
                    t.redraw();
                }
                self.aux_terminal_resize_pty(idx);
            }
            WindowEvent::Focused(f) => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.focused = f;
                    t.redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    let scale = t.gpu.scale().max(0.5);
                    // 탭 띠도 hover 가 바뀌는 자리라 게이트를 헤더+띠로 잡는다.
                    let band = AUX_HEADER_H + t.tab_bar_h;
                    let was_header = t.cursor_px.1 <= band;
                    t.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                    // 헤더를 지나는 동안만 다시 그린다 — 셀 위에서 매 픽셀 재렌더는
                    // 낭비다. 띠를 벗어나는 프레임도 한 번은 그려야 hover 가 안 굳는다.
                    if was_header || t.cursor_px.1 <= band {
                        t.redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.aux_tab_click(idx) {
                    self.aux_header_click(idx);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.aux_terminal_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_terminal_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_terminal_ime(idx, ime),
            WindowEvent::RedrawRequested => {
                let blink = self.cursor_blink_on(Instant::now());
                self.aux_terminal_render(idx, blink);
            }
            _ => {}
        }
    }

    /// 탭 띠 클릭 — ×(닫기) → 알약(전환) → +(새 탭) 순. × 가 알약 **안**에 있으므로
    /// 순서가 뒤집히면 닫기를 눌러도 전환만 된다. 눌린 게 없으면 `false`.
    fn aux_tab_click(&mut self, idx: usize) -> bool {
        let Some(t) = self.aux.terminals.get(idx) else { return false };
        if t.tab_bar_h <= 0.0 {
            return false;
        }
        let (cx, cy) = t.cursor_px;
        let pane_id = t.pane_id.clone();
        let in_rect =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        let pick = |v: &[(usize, (f32, f32, f32, f32))]| {
            v.iter().find(|(_, r)| in_rect(r)).map(|(i, _)| *i)
        };
        let close = pick(&t.tabs_out.close_hits);
        let switch = pick(&t.tabs_out.tab_hits);
        let plus = t.tabs_out.plus_rect.filter(in_rect).is_some();

        if let Some(i) = close {
            self.close_tab(&pane_id, i);
        } else if let Some(i) = switch {
            let mut ws = self.ws.lock().unwrap();
            if let Some(p) = ws.panes.get_mut(&pane_id) {
                p.active_tab = i.min(p.tabs.len().saturating_sub(1));
                p.dirty = true;
            }
        } else if plus {
            if let Err(e) = self.spawn_new_tab(&pane_id, true) {
                eprintln!("[auxterm] spawn_new_tab: {e}");
            }
        } else {
            return false;
        }
        self.aux_terminal_resize_pty(idx);
        if let Some(t) = self.aux.terminals.get_mut(idx) {
            t.redraw();
        }
        true
    }

    /// 헤더 버튼 클릭. 눌린 게 없으면 `false`.
    fn aux_header_click(&mut self, idx: usize) -> bool {
        let Some(t) = self.aux.terminals.get(idx) else { return false };
        let (cx, cy) = t.cursor_px;
        let hit = t
            .header_hits
            .iter()
            .find(|(_, (bx, by, bw, bh))| cx >= *bx && cx <= bx + bw && cy >= *by && cy <= by + bh)
            .map(|(k, _)| *k);
        match hit {
            Some(TermHeaderBtn::Dock) => {
                self.dock_pane_terminal(idx);
                true
            }
            Some(TermHeaderBtn::Pin) => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.pinned = !t.pinned;
                    t.window.set_window_level(if t.pinned {
                        winit::window::WindowLevel::AlwaysOnTop
                    } else {
                        winit::window::WindowLevel::Normal
                    });
                    t.redraw();
                }
                true
            }
            None => false,
        }
    }

    /// 휠 → 활성 탭 셸의 스크롤백. 셸이 아닌 탭은 이 창이 못 그리니 굴릴 것도 없다.
    fn aux_terminal_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let Some(pane_id) = self.aux.terminals.get(idx).map(|t| t.pane_id.clone()) else {
            return;
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
        };
        if lines.abs() < 0.01 {
            return;
        }
        let Some(pid) = self.aux_active_term_pid(&pane_id) else { return };
        let step = lines.abs().ceil() as i32;
        let projection = self.aux.terminals.get(idx)
            .and_then(|term| term.view_shift.as_ref())
            .and_then(|shift| shift.projection.clone());
        if let Some(projection) = projection {
            if self.scroll_mirror_view(&pid, &projection, if lines > 0.0 { step } else { -step }) {
                if let Some(term) = self.aux.terminals.get_mut(idx) { term.redraw(); }
                return;
            }
        }
        if let Some(pty) = self.pty.get(&pid) {
            pty.scroll(if lines > 0.0 { step } else { -step });
        }
        if let Some(t) = self.aux.terminals.get_mut(idx) {
            t.redraw();
        }
    }

    /// 키 → PTY 바이트. forward_key 의 셸 전송부만 축약 재현(git/파일트리/이미지
    /// side effect 없음). 한글은 메인 창과 같은 in-process composer(self.hangul).
    fn aux_terminal_key(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed || crate::input::is_modifier_key(event) {
            return;
        }
        self.last_input_at = Instant::now();
        let Some(pane_id) = self.aux.terminals.get(idx).map(|t| t.pane_id.clone()) else {
            return;
        };
        // OS 키 자동반복은 Cmd 조합에선 삼킨다 — 메인 창(forward_key)과 같은 규칙.
        if self.host_mod() && event.repeat {
            return;
        }
        // Cmd/Ctrl+W: 이 창 닫기 → dock 복귀. 활성 탭이 셸이 아니어도 돼야 갇히지 않는다.
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.dock_pane_terminal(idx);
            return;
        }
        // 폰트 줌(Cmd+= / Cmd+- / Cmd+0). **한글 조합 분기보다 먼저** — 조합 중엔
        // 아래 경로가 이 키를 먼저 먹어 셸에 '-' 가 박힌다. 별도창은 자기 gpu 의
        // 폰트 크기가 전부라 여기서 직접 올리고 PTY 를 리플로우한다.
        let zoom_mod = if cfg!(target_os = "macos") {
            self.host_mod()
        } else {
            self.modifiers.control_key()
        };
        if zoom_mod {
            let logical_str = match &event.logical_key {
                Key::Character(s) => Some(s.as_str()),
                _ => None,
            };
            let code = match event.physical_key {
                PhysicalKey::Code(c) => Some(c),
                _ => None,
            };
            if let Some(z) = crate::input::zoom_key(code, logical_str) {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    let next = match z {
                        crate::input::ZoomKey::Reset => FONT_SIZE,
                        crate::input::ZoomKey::In => (t.font_size + 1.0).clamp(8.0, 40.0),
                        crate::input::ZoomKey::Out => (t.font_size - 1.0).clamp(8.0, 40.0),
                    };
                    t.font_size = next;
                    t.gpu.set_font_size(next);
                    t.redraw();
                }
                // 셀이 커졌으니 같은 창에 들어가는 칸 수가 달라진다.
                self.aux_terminal_resize_pty(idx);
                return;
            }
        }
        // 셸이 아닌 탭은 키를 받지 않는다 — 화면에 없는 첫 탭 셸로 새지 않게.
        let Some(tab_pid) = self.aux_active_term_pid(&pane_id) else { return };
        self.mirror_view_scroll.remove(&tab_pid);
        // **탭 pid** 로 겨눈다. outer 를 넣으면 조합 중 글자가 첫 탭으로 샌다.
        self.ime_retarget(crate::ImeFocus::Pane(tab_pid.clone()));
        // macOS in-process 한글 조합: 자모(U+3130..318F)면 composer 로, 완성 음절만 PTY.
        #[cfg(target_os = "macos")]
        if let Some(text) = &event.text {
            if text.chars().count() == 1 {
                if let Some(c) = text.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.send_bytes_to_surface(Some(&tab_pid), commit.as_bytes());
                        }
                        let pe = self.hangul.preedit().unwrap_or_default();
                        if let Some(t) = self.aux.terminals.get_mut(idx) {
                            t.preedit = pe;
                            t.redraw();
                        }
                        return;
                    }
                }
            }
        }
        // Ctrl(+letter) → 제어바이트. host_mod(Cmd)는 제외.
        if self.modifiers.control_key() && !self.host_mod() {
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(b) = ctrl_byte(code) {
                    self.aux_term_flush_hangul(idx, &tab_pid);
                    self.send_bytes_to_surface(Some(&tab_pid), &[b]);
                    return;
                }
            }
        }
        // 특수키 → ANSI 시퀀스.
        let seq: Option<&[u8]> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => Some(b"\r"),
            Key::Named(NamedKey::Tab) => Some(b"\t"),
            Key::Named(NamedKey::Escape) => Some(b"\x1b"),
            Key::Named(NamedKey::Backspace) => {
                // 조합 중이면 자모를 하나 빼고(셸로 안 보냄), 아니면 DEL.
                if self.hangul.backspace() {
                    let pe = self.hangul.preedit().unwrap_or_default();
                    if let Some(t) = self.aux.terminals.get_mut(idx) {
                        t.preedit = pe;
                        t.redraw();
                    }
                    return;
                }
                Some(b"\x7f")
            }
            Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A"),
            Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B"),
            Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C"),
            Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D"),
            Key::Named(NamedKey::Home) => Some(b"\x1b[H"),
            Key::Named(NamedKey::End) => Some(b"\x1b[F"),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~"),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~"),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~"),
            _ => None,
        };
        if let Some(bytes) = seq {
            self.aux_term_flush_hangul(idx, &tab_pid);
            self.send_bytes_to_surface(Some(&tab_pid), bytes);
            return;
        }
        // 평문(자모는 위에서 소비됨) — 조합 잔여를 먼저 확정하고 그대로 전송.
        if let Some(text) = &event.text {
            if !text.is_empty() {
                self.aux_term_flush_hangul(idx, &tab_pid);
                self.send_bytes_to_surface(Some(&tab_pid), text.as_bytes());
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.redraw();
                }
            }
        }
    }

    /// 조합 중인 음절을 확정해 PTY 로 보내고 프리에딧을 비운다(제어/특수/평문 전에).
    fn aux_term_flush_hangul(&mut self, idx: usize, tab_pid: &str) {
        if let Some(flushed) = self.hangul.flush() {
            self.send_bytes_to_surface(Some(tab_pid), flushed.as_bytes());
        }
        if let Some(t) = self.aux.terminals.get_mut(idx) {
            t.preedit.clear();
        }
    }

    /// 비-macOS(Windows/Linux) OS IME — Preedit 는 이 창 프리에딧, Commit 은 PTY 전송.
    fn aux_terminal_ime(&mut self, idx: usize, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.preedit.clear();
                }
            }
            Ime::Preedit(text, _) => {
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.preedit = text;
                }
            }
            Ime::Commit(text) => {
                let pane_id = self.aux.terminals.get(idx).map(|t| t.pane_id.clone());
                if let Some(t) = self.aux.terminals.get_mut(idx) {
                    t.preedit.clear();
                }
                if let Some(pid) = pane_id.and_then(|p| self.aux_active_term_pid(&p)) {
                    self.mirror_view_scroll.remove(&pid);
                    self.send_bytes_to_surface(Some(&pid), text.as_bytes());
                }
            }
        }
        if let Some(t) = self.aux.terminals.get_mut(idx) {
            t.redraw();
        }
    }
}

// ---- undock / dock ----
impl App {
    /// ⋮ 메뉴·헤더 단추의 진입점. 활성 탭 종류로 가른다 — 마크다운은 편집기 창,
    /// 자기 pid 를 가진 보조 터미널 탭은 그 탭만, 그 외는 pane 통째.
    pub(crate) fn undock_active_tab_of(&mut self, pid: &str, event_loop: &ActiveEventLoop) {
        enum Route {
            Markdown,
            TerminalTab,
            Pane,
            Skip,
        }
        let (outer, tab_idx, route) = {
            let ws = self.ws.lock().unwrap();
            let outer = ws
                .outer_for_pty(pid)
                .unwrap_or_else(|| pid.to_string());
            let Some(p) = ws.panes.get(&outer) else { return };
            let i = p.active_tab;
            let route = match p.tabs.get(i) {
                Some(t) => match &t.content {
                    PaneContent::Markdown(_) => Route::Markdown,
                    PaneContent::Terminal(_)
                        if t.pid.as_deref().is_some_and(|x| x != outer) =>
                    {
                        Route::TerminalTab
                    }
                    PaneContent::Terminal(_) => Route::Pane,
                    _ => Route::Skip,
                },
                None => Route::Skip,
            };
            (outer, i, route)
        };
        match route {
            Route::Markdown => self.popout_pane_tab(&outer, tab_idx, event_loop),
            Route::TerminalTab => self.undock_pane_tab(&outer, tab_idx, event_loop, None),
            Route::Pane => self.undock_pane_terminal(&outer, event_loop, None),
            Route::Skip => {}
        }
    }

    /// 다중탭 pane 에서 **탭 하나만** 별도창으로. 통째로 꺼내면 다른 탭까지 같이
    /// 나간다(2026-08-13 지적) — secondary 터미널 탭은 자기 pane 으로 승격하되
    /// 트리에는 안 꽂고 바로 별도창으로 세운다.
    ///
    /// 승격 pane 의 이름은 **그 탭의 pid** — 별도창 기계 전체가 pane_id ∈ App.pty
    /// 를 전제한다. pid 가 pane 이름과 같은 첫 탭은 못 가르고 pane undock 으로
    /// 폴백한다 — 소스가 이름을 쥔 채 pid 만 내보내면 소스가 닫힐 때
    /// `drop_pane_resources` 가 꺼내 둔 셸을 죽인다.
    pub(crate) fn undock_pane_tab(
        &mut self,
        pane_id: &str,
        idx: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        if self.tmux.is_some() {
            return;
        }
        let promote = {
            let ws = self.ws.lock().unwrap();
            let tab = ws
                .panes
                .get(pane_id)
                .filter(|p| p.tabs.len() > 1)
                .and_then(|p| p.tabs.get(idx));
            match tab.map(|t| (&t.content, t.pid.clone())) {
                Some((PaneContent::Terminal(_), Some(pid)))
                    if pid != pane_id
                        && !ws.panes.contains_key(&pid)
                        && self.pty.contains_key(&pid) =>
                {
                    Some(pid)
                }
                _ => None,
            }
        };
        let Some(tab_pid) = promote else {
            // 단일탭·첫 탭·PTY 없는 탭 — pane undock 그대로.
            self.undock_pane_terminal(pane_id, event_loop, near);
            return;
        };
        let home_window = self.window_of_pane(pane_id).unwrap_or(self.active_window);
        // 꺼낸 창은 탭이 보이던 pane 칸수를 그대로 담는다.
        let want_cells = self.leaf_cells_of(pane_id);
        {
            let mut ws = self.ws.lock().unwrap();
            // 들어올리기 — drop_tab_into_body 1단계와 같은 active 보정.
            let moved = {
                let Some(src) = ws.panes.get_mut(pane_id) else { return };
                if idx >= src.tabs.len() {
                    return;
                }
                let t = src.tabs.remove(idx);
                if idx < src.active_tab && src.active_tab > 0 {
                    src.active_tab -= 1;
                }
                if src.active_tab >= src.tabs.len() && !src.tabs.is_empty() {
                    src.active_tab = src.tabs.len() - 1;
                }
                src.dirty = true;
                t
            };
            let mut ps = PaneState::default();
            ps.tabs.clear();
            ps.tabs.push(moved);
            ps.active_tab = 0;
            ps.dirty = true;
            ws.panes.insert(tab_pid.clone(), ps);
            // pid→pane 재계산: ScreenUpdate 가 새 pane 으로 흐르고, 소스가 닫힐 때
            // secondary 정리가 이 pid 를 소스 것으로 안 본다.
            ws.rebuild_pid_map();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if self
            .spawn_aux_terminal(tab_pid.clone(), home_window, event_loop, near, want_cells, None)
            .is_none()
        {
            // 창을 못 만들었다 — 승격한 pane 을 잃지 않게 지금 방에 접붙인다.
            self.graft_pane_into_room(&tab_pid, self.active_window);
        }
    }

    /// 트리에서 이 leaf 가 지금 차지한 칸수(줌 반영). 트리에서 빼기 **전에** 재야 한다.
    fn leaf_cells_of(&self, pane_id: &str) -> Option<(u16, u16)> {
        let (cols, rows) = self.window_cells();
        self.effective_leaf_rects(cols, rows)
            .into_iter()
            .find(|(id, ..)| id == pane_id)
            .map(|(_, _, _, w, h)| (w, h))
    }

    /// `pane_id` 터미널 pane 을 별도창으로 undock. `remove_pane` 은 쓰지 않는다 —
    /// 그건 `self.pty.remove` 로 세션까지 죽인다. 트리에서 leaf 만 빼고
    /// `self.pty`·`ws.panes` 는 유지해, PtySession 이 살아 있고 그 셀 그리드를
    /// 별도창이 계속 본다.
    pub(crate) fn undock_pane_terminal(
        &mut self,
        pane_id: &str,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        // 이미 별도창이면 포커스만.
        if let Some(i) = self.aux_terminal_index(pane_id) {
            self.aux.terminals[i].window.focus_window();
            return;
        }
        // tmux 백엔드는 로컬 PTY 소유가 아니라 미지원. PTY 없는 pane(이미지/md)도 무시.
        if self.tmux.is_some() || !self.pty.contains_key(pane_id) {
            return;
        }
        // 나온 방을 지금 붙들어야 한다 — leaf 를 빼고 나면 `window_of_pane` 이 못 찾는다.
        let home_window = self.window_of_pane(pane_id).unwrap_or(self.active_window);
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        // 활성 트리에 없는 pane(스태시·다른 방)은 v1 범위 밖.
        if !leaves.iter().any(|l| l == pane_id) {
            return;
        }
        // 지금 이 pane 이 차지한 칸수 — leaf 를 빼기 전에.
        let want_cells = self.leaf_cells_of(pane_id);
        if self.zoomed_pane.as_deref() == Some(pane_id) {
            self.zoomed_pane = None;
        }
        let was_active = self.ws.lock().unwrap().active_pane.as_deref() == Some(pane_id);
        if leaves.len() > 1 {
            // 제거 pane 이 active 였으면 형제 leaf 로 포커스 이동(remove_pane 과 같은 규칙).
            let next_focus = was_active.then(|| {
                let i = leaves.iter().position(|l| l == pane_id).unwrap_or(0);
                if i + 1 < leaves.len() {
                    leaves[i + 1].clone()
                } else {
                    leaves[i - 1].clone()
                }
            });
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(pane_id);
            }
            let mut ws = self.ws.lock().unwrap();
            if let Some(next) = next_focus {
                ws.active_pane = Some(next);
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        } else {
            // 마지막 leaf — 빈 방을 셸로 채운다(`switch_window` 와 같은 수법). 옛
            // `pty_layout = None` 은 HEAD 의 빈 방 정리·단일 pane 폴백과 부딪힌다.
            if let Err(e) = self.spawn_session_pane() {
                eprintln!("[auxterm] 빈 방 되살리기 실패: {e:#}");
                return;
            }
            let mut ws = self.ws.lock().unwrap();
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if self
            .spawn_aux_terminal(pane_id.to_string(), home_window, event_loop, near, want_cells, None)
            .is_none()
        {
            // 창을 못 만들었다 — 방금 뺀 pane 을 되꽂아 잃지 않는다.
            self.graft_pane_into_room(pane_id, self.active_window);
        }
    }

    /// 트리 밖 pane 을 `home` 방 트리에 접붙인다(`reopen_pane_record` 와 같은 수법).
    /// 활성 방이면 활성 pane 오른쪽에, 다른 방이면 그 방 첫 leaf 오른쪽에. 트리가
    /// 비었으면 유일 leaf 로 세운다.
    fn graft_pane_into_room(&mut self, pane_id: &str, home: usize) {
        if home == self.active_window || home >= self.windows.len() {
            let anchor = self.ws.lock().unwrap().active_pane.clone();
            let grafted = match (anchor, self.pty_layout.as_mut()) {
                (Some(a), Some(tree)) => {
                    tree.split_leaf(&a, kasa_pty::SplitDir::Horizontal, pane_id.to_string())
                }
                _ => false,
            };
            if !grafted {
                self.pty_layout = Some(kasa_pty::PtyLayout::single(pane_id));
            }
            {
                let mut ws = self.ws.lock().unwrap();
                ws.active_pane = Some(pane_id.to_string());
                for pane in ws.panes.values_mut() {
                    pane.dirty = true;
                }
            }
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
        } else {
            let slot = &mut self.windows[home];
            let grafted = match slot.as_mut() {
                Some(tree) => {
                    let anchor = tree.leaves().first().map(|s| s.to_string());
                    anchor.is_some_and(|a| {
                        tree.split_leaf(&a, kasa_pty::SplitDir::Horizontal, pane_id.to_string())
                    })
                }
                None => false,
            };
            if !grafted {
                *slot = Some(kasa_pty::PtyLayout::single(pane_id));
            }
        }
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 세션 복원이 줄 세운 별도창 pane 을 연다(flush_aux_opens 에서). 창을 못 만들면
    /// 떠나온 방 트리에 접붙여 pane 을 잃지 않는다. 저장된 틀은 지금 붙은 모니터로
    /// 검증하고, 밖이면 기본 자리로 연다.
    pub(crate) fn open_restored_aux_terminal(
        &mut self,
        pane_id: String,
        home_window: usize,
        frame: Option<crate::auxwin::FrameRecord>,
        event_loop: &ActiveEventLoop,
    ) {
        if !self.pty.contains_key(&pane_id) || self.aux_terminal_index(&pane_id).is_some() {
            return;
        }
        let home = home_window.min(self.windows.len().saturating_sub(1));
        let frame = frame.filter(|f| {
            let ok = frame_on_screen(f, event_loop);
            if !ok {
                eprintln!("[auxterm] {pane_id} 저장된 자리가 화면 밖 — 기본 자리로 연다");
            }
            ok
        });
        if self
            .spawn_aux_terminal(pane_id.clone(), home, event_loop, None, None, frame)
            .is_none()
        {
            self.graft_pane_into_room(&pane_id, home);
        }
    }

    /// 별도창을 닫으며 그 pane 을 떠나온 방으로 되돌린다(dock). 살아있는 세션이면
    /// 활성 pane 오른쪽에 `split_leaf` 로 기존 pane_id 를 재삽입한다 — 새 세션을
    /// 만드는 split 과 달리 기존 PtySession 을 그대로 얹으므로 셸이 안 끊긴다.
    pub(crate) fn dock_pane_terminal(&mut self, idx: usize) {
        let Some(t) = self.aux.terminals.get(idx) else { return };
        let pane_id = t.pane_id.clone();
        let home = t.home_window;
        self.aux.terminals.remove(idx);
        // 셸이 이미 종료돼 세션이 사라졌으면 되돌릴 게 없다.
        if !self.pty.contains_key(&pane_id) {
            return;
        }
        // 나왔던 방으로 돌아간다. 이게 없으면 그때 보고 있던 방에 남의 pane 이
        // 튀어나온다 — 꺼낼 때와 되돌릴 때 활성 방이 같으리란 보장이 없다.
        if home < self.windows.len() && home != self.active_window {
            self.switch_window(home);
        }
        let in_tree = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().any(|l| *l == pane_id))
            .unwrap_or(false);
        if !in_tree {
            let active = self.ws.lock().unwrap().active_pane.clone();
            let inserted = match (active, self.pty_layout.as_mut()) {
                (Some(active), Some(tree)) => {
                    tree.split_leaf(&active, kasa_pty::SplitDir::Horizontal, pane_id.clone())
                }
                _ => false,
            };
            if !inserted {
                // 트리가 비었거나 active 가 트리에 없음 — 이 pane 을 유일 leaf 로.
                self.pty_layout = Some(kasa_pty::PtyLayout::single(pane_id.as_str()));
            }
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(pane_id.clone());
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.handoff_ime_to_active_surface();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 셸이 끝나 pty 도 화면도 사라진 별도창을 걷는다(reap_dead_panes 뒤에). 둘 중
    /// 하나라도 남아 있으면 둔다 — 같은 id 로 PTY 를 바꿔 끼우는 순간 pty 만 잠깐
    /// 비는 경우가 있다.
    pub(crate) fn reap_dead_aux_terminals(&mut self) {
        if self.aux.terminals.is_empty() {
            return;
        }
        let ws = self.ws.lock().unwrap();
        self.aux.terminals.retain(|t| {
            let alive = self.pty.contains_key(&t.pane_id) || ws.panes.contains_key(&t.pane_id);
            if !alive {
                eprintln!("[auxterm] {} 사라짐 — 별도창 닫음", t.pane_id);
            }
            alive
        });
    }
}
