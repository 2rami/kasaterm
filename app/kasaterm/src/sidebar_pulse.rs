//! 사이드바 맨 위 현황 줄 — 모든 기기 보드를 「사람 차례 N · 작업 N · 끝 N」 한 줄로.
//!
//! 보드는 보기 메뉴와 ⇧⌘B 로만 열려서 찾기 어려웠다(2026-09-28 지시). 이 줄이 늘 떠 있는
//! 요약이자 보드 입구다. 수는 보드 목록과 같은 판정(`native_board::pulse_counts`)에서 나와야
//! 누르고 연 보드와 어긋나지 않는다. 치수는 `docs/design.md` 「사이드바 현황 줄·트레이」.

use super::*;
use kasa_socket::backend::Backend;

pub(crate) const PULSE_H: f32 = 30.0;
const PULSE_FONT: f32 = 11.5;
/// 보드 방이 열려 있을 때의 갱신 간격(`refresh_due` 2.2초)보다 느슨하게 — 요약이라
/// 몇 초 늦어도 뜻이 안 바뀌고, 사이드바가 열려 있는 내내 돈다.
const POLL: std::time::Duration = std::time::Duration::from_secs(3);

type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PulseCounts {
    /// 승인·질문으로 사람을 부르는 칸과 실패 보고 — 보드의 「확인 필요」.
    pub(crate) yours: usize,
    pub(crate) working: usize,
    /// 성공 완료 보고가 서 있는 칸. 다음 턴을 시작하면 보드에서 빠진다.
    pub(crate) done: usize,
}

type Mailbox = Arc<Mutex<Option<Result<PulseCounts, String>>>>;

#[derive(Default)]
pub(crate) struct Pulse {
    /// 마지막으로 읽은 수. 갱신이 실패해도 지우지 않는다 — 한 번 끊겼다고 0 으로 바뀌면
    /// 「사람 차례 없음」으로 읽혀 기다리는 학생을 놓친다.
    counts: Option<PulseCounts>,
    failed: bool,
    mailbox: Mailbox,
    inflight: bool,
    last_poll: Option<Instant>,
    /// 방 우클릭 메뉴 「현황 줄 숨기기」. settings.json `sidebar_pulse`.
    pub(crate) hidden: bool,
    /// 렌더가 적어 두는 누를 자리. 안 그린 프레임은 `None` 이라 유령 클릭이 없다.
    pub(crate) rect: Option<Rect>,
}

impl Pulse {
    pub(crate) fn from_settings() -> Self {
        Self {
            hidden: socket::read_settings().get("sidebar_pulse").and_then(|v| v.as_bool()) == Some(false),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Yours,
    Working,
    Done,
}

/// 줄에 실을 조각 `(이름, 수, 색)`. 좁으면 짧은 이름으로 접는다 — 수는 줄이지 않는다.
fn segments(counts: PulseCounts, short: bool) -> [(&'static str, usize, Tone); 3] {
    [
        (if short { "차례" } else { "사람 차례" }, counts.yours, Tone::Yours),
        ("작업", counts.working, Tone::Working),
        ("끝", counts.done, Tone::Done),
    ]
}

/// 0 은 흐리게 — 수가 있는 칸만 상태색이 떠야 눈이 그리로 간다.
fn tone_color(tone: Tone, n: usize) -> [u8; 4] {
    if n == 0 {
        return theme::text_mute();
    }
    match tone {
        Tone::Yours => theme::attention(),
        Tone::Working => theme::accent(),
        Tone::Done => theme::success(),
    }
}

const SEP: &str = " · ";

fn segments_w(g: &mut gpu::GpuRenderer, segs: &[(&str, usize, Tone); 3]) -> f32 {
    let mut w = g.measure_chrome_text(SEP, PULSE_FONT, false) * 2.0;
    for (label, n, _) in segs {
        w += g.measure_chrome_text(&format!("{label} "), PULSE_FONT, false)
            + g.measure_chrome_text(&n.to_string(), PULSE_FONT, true);
    }
    w
}

/// 방 우클릭 메뉴의 한 줄 — 이 기기 방과 다른 기기 방 메뉴가 같은 말을 쓴다.
pub(crate) fn menu_label(hidden: bool) -> &'static str {
    if hidden { "현황 줄 보이기" } else { "현황 줄 숨기기" }
}

/// 사이드바 맨 위, 기기 머리줄 위에 한 줄을 그리고 누를 자리를 적는다.
pub(crate) fn draw(g: &mut gpu::GpuRenderer, pulse: &mut Pulse, cursor: (f32, f32), width: f32, board_open: bool) {
    pulse.rect = None;
    if width <= 0.0 {
        return;
    }
    let row = (8.0, TITLE_HEIGHT + 2.0, width - 16.0, PULSE_H - 4.0);
    let hover = cursor.0 >= row.0 && cursor.0 <= row.0 + row.2 && cursor.1 >= row.1 && cursor.1 <= row.1 + row.3;
    g.hover_pointer |= hover;
    if hover {
        hover_rect(g, row.0, row.1, row.2, row.3, theme::radius_sm());
    }
    // 앞 아이콘을 두면 기본 폭(200)에서 「사람 차례」가 안 들어가 「차례」로 접힌다. 글자를 아래
    // 기기 머리줄의 아이콘 열(14)에 맞춰 세우고, 보드 입구라는 표시는 올렸을 때의 판과 툴팁이 맡는다.
    let text_x = if width < 180.0 { 12.0 } else { 14.0 };
    let ty = row.1 + (row.3 - PULSE_FONT) / 2.0 - 1.0;
    let avail = row.0 + row.2 - 6.0 - text_x;
    let opts = |color, bold| gpu::DrawOpts { font_size: PULSE_FONT, color, bold, italic: false };
    match pulse.counts {
        Some(counts) => {
            let long = segments(counts, false);
            let segs = if segments_w(g, &long) <= avail { long } else { segments(counts, true) };
            let mut x = text_x;
            // 보드가 열려 있으면 켜진 설정 버튼처럼 **색**으로 말한다(플랫 문법).
            let label_color = if board_open { theme::accent() } else if hover { theme::text() } else { theme::text_dim() };
            for (i, (label, n, tone)) in segs.iter().enumerate() {
                if i > 0 {
                    g.draw_text(x, ty, SEP, opts(theme::text_mute(), false));
                    x += g.measure_chrome_text(SEP, PULSE_FONT, false);
                }
                let label = format!("{label} ");
                g.draw_text(x, ty, &label, opts(label_color, false));
                x += g.measure_chrome_text(&label, PULSE_FONT, false);
                let num = n.to_string();
                g.draw_text(x, ty, &num, opts(tone_color(*tone, *n), true));
                x += g.measure_chrome_text(&num, PULSE_FONT, true);
            }
        }
        None => {
            let note = if pulse.failed { "현황을 못 읽었어요" } else { "현황 확인 중" };
            let note = crate::info::fit_text(g, note, avail.max(0.0), PULSE_FONT, false);
            g.draw_text(text_x, ty, &note, opts(theme::text_mute(), false));
        }
    }
    pulse.rect = Some(row);
}

impl App {
    /// 현황 줄이 먹는 높이. 세로 사이드바가 없거나 숨겼으면 0 — 기기 머리줄이 제자리로 올라간다.
    pub(crate) fn sidebar_pulse_h(&self) -> f32 {
        if self.lite || self.tabs_on_top || !self.sidebar_visible || self.pulse.hidden {
            0.0
        } else {
            PULSE_H
        }
    }

    /// 매 틱 — 받은 수를 반영하고, 때가 되면 다음 읽기를 백그라운드에 건다. 보드 스냅샷은
    /// 원격 기기까지 합친 것이라 GUI 스레드에서 기다리지 않는다.
    pub(crate) fn sidebar_pulse_tick(&mut self) {
        let arrived = self.pulse.mailbox.lock().unwrap().take();
        if let Some(result) = arrived {
            self.pulse.inflight = false;
            let before = (self.pulse.counts, self.pulse.failed);
            match result {
                Ok(counts) => {
                    self.pulse.counts = Some(counts);
                    self.pulse.failed = false;
                }
                Err(error) => {
                    if !self.pulse.failed {
                        eprintln!("[pulse] 보드 요약 실패: {error}");
                    }
                    self.pulse.failed = true;
                }
            }
            if before != (self.pulse.counts, self.pulse.failed) {
                self.chrome_dirty = true;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
        if self.sidebar_pulse_h() <= 0.0 || self.pulse.inflight {
            return;
        }
        if self.pulse.last_poll.is_some_and(|at| at.elapsed() < POLL) {
            return;
        }
        let Some(backend) = self.socket_backend.clone() else {
            return;
        };
        self.pulse.inflight = true;
        self.pulse.last_poll = Some(Instant::now());
        let mailbox = self.pulse.mailbox.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            #[cfg(debug_assertions)]
            let fixture = crate::native_board::pulse_fixture_counts();
            #[cfg(not(debug_assertions))]
            let fixture = None;
            let result = fixture.unwrap_or_else(|| {
                backend
                    .collab_snapshot(&serde_json::json!({"scope": "all"}))
                    .map_err(|e| e.to_string())
                    .and_then(crate::native_board::pulse_counts)
            });
            *mailbox.lock().unwrap() = Some(result);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 줄 위를 눌렀나 — 누르면 보드(⇧⌘B 와 같은 토글, 처리는 handler 의 트레이 버튼 옆).
    pub(crate) fn sidebar_pulse_hit(&self, cursor: (f32, f32)) -> bool {
        self.pulse.rect.filter(|_| self.sidebar_pulse_h() > 0.0).is_some_and(|r| {
            cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
        })
    }

    /// 방 메뉴 「현황 줄 숨기기·보이기」. 방 목록의 윗변이 움직이므로 다음 프레임에 새로 잰다.
    pub(crate) fn toggle_sidebar_pulse(&mut self) {
        self.pulse.hidden = !self.pulse.hidden;
        self.pulse.rect = None;
        socket::write_setting("sidebar_pulse", serde_json::json!(!self.pulse.hidden));
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl App {
    /// 격리 앱에서 입구를 **진짜 클릭**(winit MouseInput 을 window_event 로)으로 눌러 본다 —
    /// 상태를 손으로 세우면 handler 의 클릭 순서에 가려지는 버그를 못 잡는다.
    /// 켜기: `KASATERM_PULSE_PROBE=1` + 검증 실행(`KASATERM_WINDOW_SIZE`). 결과는 `[pulse-probe]` 줄.
    /// 현황 줄 → 보드 → 다시 눌러 복귀 → 트레이 보드 두 번 → 방 메뉴로 숨기기·보이기 → 트레이 아로나.
    pub(crate) fn run_pending_pulse_probe(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        if !cfg!(debug_assertions)
            || !crate::verification_run()
            || std::env::var("KASATERM_PULSE_PROBE").as_deref() != Ok("1")
        {
            return;
        }
        static START: OnceLock<Instant> = OnceLock::new();
        static STEP: AtomicUsize = AtomicUsize::new(0);
        let step = STEP.load(Ordering::Relaxed);
        if step > 9 || START.get_or_init(Instant::now).elapsed().as_millis() < 4000 + step as u128 * 1500 {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let room = || {
            self.window_tab_rects.iter().find(|(i, _)| self.internal_room_kind_at(*i).is_none()).map(|(_, r)| *r)
        };
        let toggle_item = || {
            self.sidebar_menu_rects.iter().find(|(a, _)| *a == SidebarMenuAction::TogglePulse).map(|(_, r)| *r)
        };
        let mut button = MouseButton::Left;
        let target = match step {
            0 => {
                eprintln!("[pulse-probe] counts={:?} line_h={}", self.pulse.counts, self.sidebar_pulse_h());
                self.pulse.rect
            }
            1 => {
                eprintln!("[pulse-probe] line_opens_board={}", self.board_room_active());
                self.pulse.rect
            }
            2 => {
                eprintln!("[pulse-probe] line_returns={}", !self.board_room_active());
                Some(self.board_btn_rect).filter(|r| r.2 > 0.0)
            }
            3 => {
                eprintln!("[pulse-probe] tray_opens_board={}", self.board_room_active());
                Some(self.board_btn_rect).filter(|r| r.2 > 0.0)
            }
            4 => {
                eprintln!("[pulse-probe] tray_returns={}", !self.board_room_active());
                button = MouseButton::Right;
                room()
            }
            5 => {
                eprintln!("[pulse-probe] room_menu_offers_toggle={}", toggle_item().is_some());
                toggle_item()
            }
            6 => {
                eprintln!(
                    "[pulse-probe] hidden={} head_top={} saved={:?}",
                    self.pulse.hidden,
                    self.sidebar_head_top(),
                    socket::read_settings().get("sidebar_pulse")
                );
                button = MouseButton::Right;
                room()
            }
            7 => toggle_item(),
            8 => {
                eprintln!("[pulse-probe] shown_again={} head_top={}", !self.pulse.hidden, self.sidebar_head_top());
                Some(self.arona_btn_rect).filter(|r| r.2 > 0.0)
            }
            _ => {
                let arona = self.inline_web.as_ref().is_some_and(|h| h.kind == crate::InlineWebKind::Arona);
                eprintln!("[pulse-probe] tray_arona_open={arona} toast={:?}", self.collab.toast.as_ref().map(|t| t.0.clone()));
                STEP.store(10, Ordering::Relaxed);
                return;
            }
        };
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(r) = target else {
            eprintln!("[pulse-probe] FAILED missing_target step={step}");
            STEP.store(10, Ordering::Relaxed);
            return;
        };
        self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.window_event(event_loop, wid, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button });
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_labels_keep_every_number() {
        let counts = PulseCounts { yours: 12, working: 3, done: 0 };
        let long = segments(counts, false);
        let short = segments(counts, true);
        assert_eq!(long.map(|s| s.1), short.map(|s| s.1), "좁아져도 수는 그대로다");
        assert_eq!(long[0].0, "사람 차례");
        assert_eq!(short[0].0, "차례");
    }

    #[test]
    fn zero_counts_stay_muted_and_live_ones_use_the_state_language() {
        assert_eq!(tone_color(Tone::Yours, 0), theme::text_mute());
        assert_eq!(tone_color(Tone::Yours, 1), theme::attention());
        assert_eq!(tone_color(Tone::Working, 2), theme::accent());
        assert_eq!(tone_color(Tone::Done, 1), theme::success());
    }
}
