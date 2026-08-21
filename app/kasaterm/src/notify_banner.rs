//! 완료 알림 배너 — **우리가 직접 그리는** 알림 창.
//!
//! macOS 알림 센터를 못 쓴다. 자체 서명 번들은 `UNUserNotificationCenter` 등록이
//! `Notifications are not allowed for this application` 으로 거절되고, 그래서
//! `osascript` 로 떨어지면 배너에 **스크립트 편집기 아이콘**이 붙는다. 원인이 우리
//! 코드가 아니라는 것까지 배제 실측으로 확인했다(chrome.rs `notify_native` 주석).
//! 거노 2026-08-21 선택: 「앱이 직접 배너를 그린다」.
//!
//! ⚠️ **이 창은 절대 포커스를 뺏으면 안 된다.** 배너가 뜨는 순간은 정의상 사용자가
//! **다른 앱을 보고 있을 때**다 — 타이핑 중에 키 포커스를 가져가면 그 타이핑이
//! 끊긴다. 이 레포엔 그 사고 이력이 있다(헤드리스 캡처가 창을 튀어나오게 해 쓰던
//! 앱이 뒤로 밀렸다). `with_active(false)` 로 뜨고, 클릭도 받지 않는다 — 배너의
//! 일은 「알린다」 하나다. **어디로 갈지는 사이드바가 이미 말하고 있다**(아래).
//!
//! ## 이 방식이 잃는 것과, 그걸 이미 메우고 있는 것
//!
//! 자체 배너는 **알림 센터에 안 쌓인다**. 자리를 비웠다 오면 놓친 배너가 없다.
//! 그런데 이 앱은 그 자리를 이미 다른 언어로 말하고 있다 — `unread_panes`(못 본
//! 완료), Dock 배지, 사이드바 줄의 숨쉬기. 배너는 **「지금 알린다」만** 맡고
//! 「놓친 것」은 그쪽이 든다. 그래서 배너를 띄우는 판정을 새로 만들지 않고
//! `handle_notify` 안, 그 셋과 **같은 자리**에서 띄운다 — 판정이 두 벌이면 배너는
//! 떴는데 배지는 안 서는 식으로 갈린다.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes, WindowLevel};

use super::*;

/// 배너 한 장의 논리 크기. macOS 기본 배너와 비슷한 비례 — 낯선 물건을 새 규격으로
/// 내놓을 이유가 없다.
const W: f32 = 360.0;
const H: f32 = 78.0;
/// 쌓일 때의 세로 간격.
const GAP: f32 = 10.0;
/// 화면 오른쪽 여백.
const MARGIN_R: f32 = 14.0;
/// 화면 위 여백. 메뉴바(및 노치)를 피해야 한다 — winit 은 「가려지지 않는 영역」을
/// 안 알려주므로 넉넉히 잡는다. macOS 알림도 이쯤에 뜬다.
const MARGIN_T: f32 = 44.0;
/// 한 장이 떠 있는 시간.
const LIFE: Duration = Duration::from_millis(4500);
/// 동시에 쌓이는 최대 장수. 넘으면 **가장 오래된 것부터** 걷는다 — 무한히 쌓으면
/// 학생이 여럿 끝나는 순간 화면이 배너로 덮이고, 그건 알림이 아니라 방해다.
const MAX: usize = 3;

pub(crate) struct Banner {
    /// 자체 wgpu 렌더러. `window` 보다 먼저 드롭돼야 한다(auxwin 과 같은 규약).
    pub(crate) gpu: gpu::GpuRenderer,
    pub(crate) window: Arc<Window>,
    title: String,
    body: String,
    /// 학생 이름 — 있으면 색으로 「누구」를 말한다.
    character: Option<String>,
    /// 이 시각이 지나면 걷는다.
    pub(crate) until: Instant,
}

impl App {
    /// 완료 배너를 한 장 띄운다. 실패하면 조용히 아무 일도 안 한다 — 알림을 못 띄운
    /// 것이 앱을 세울 이유는 아니다.
    pub(crate) fn push_notify_banner(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: &str,
        body: &str,
        character: Option<String>,
    ) {
        // 오래된 것부터 걷어 자리를 낸다.
        while self.banners.len() >= MAX {
            self.banners.remove(0);
        }
        // **메인 창이 있는 모니터**에 띄운다. 커서가 있는 화면을 쓰고 싶지만 winit 은
        // 전역 커서 위치를 안 준다. 그리고 화면이 여럿일 때 「내가 보고 있는 화면」에
        // 가장 가까운 답은 작업하던 창이 있는 쪽이다 — primary 로 고정하면 외장
        // 모니터로 일하는 동안 배너만 내장 화면에 뜬다.
        let mon = self
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .or_else(|| event_loop.primary_monitor());
        let (ox, oy, mw) = match &mon {
            Some(m) => {
                let sf = m.scale_factor();
                let p = m.position();
                let s = m.size();
                (
                    p.x as f64 / sf,
                    p.y as f64 / sf,
                    s.width as f64 / sf,
                )
            }
            None => (0.0, 0.0, 1440.0),
        };
        let idx = self.banners.len() as f64;
        let x = ox + mw - W as f64 - MARGIN_R as f64;
        let y = oy + MARGIN_T as f64 + idx * (H as f64 + GAP as f64);

        let attrs = WindowAttributes::default()
            .with_title("kasaterm 알림")
            .with_inner_size(LogicalSize::new(W, H))
            .with_position(LogicalPosition::new(x, y))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            // 다른 앱 위에 떠야 값이 있다. 알림이 필요한 순간은 정의상 이 앱을
            // 안 보고 있을 때다.
            .with_window_level(WindowLevel::AlwaysOnTop)
            // ★키 포커스를 가져가지 않는다. 이 한 줄이 이 기능의 전제다.
            .with_active(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[banner] 창 생성 실패 — 배너 건너뜀: {e}");
                return;
            }
        };
        window.set_ime_allowed(false);
        // ⚠️ 셀 폰트 크기는 **다른 창과 같은 `FONT_SIZE`** 여야 한다. 여기만 13.0 을
        // 줬더니 한글 자간이 불규칙하게 벌어졌다(「학 생 이끝났 습 니다」) — 이 값이
        // 격자와 폴백 글리프 메트릭의 기준이라, 배너가 chrome 텍스트만 그리는데도
        // 어긋난다. 배너 글자 크기는 `draw_text` 의 `font_size` 로 따로 정한다.
        let gpu = match gpu::GpuRenderer::new(window.clone(), crate::FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[banner] gpu 초기화 실패 — 배너 건너뜀: {e}");
                return;
            }
        };
        self.banners.push(Banner {
            gpu,
            window,
            title: title.to_string(),
            body: body.to_string(),
            character,
            until: Instant::now() + LIFE,
        });
        let n = self.banners.len() - 1;
        self.draw_banner(n);
    }

    /// 배너 한 장을 그린다.
    pub(crate) fn draw_banner(&mut self, idx: usize) {
        let Some(b) = self.banners.get_mut(idx) else { return };
        let scale = b.window.scale_factor() as f32;
        b.gpu.set_scale(scale);
        let (pw, ph) = b.gpu.surface_size();
        if pw == 0 || ph == 0 {
            return;
        }
        let (w, h) = (pw as f32 / scale, ph as f32 / scale);
        // 학생색으로 「누구」를 말한다. 배정이 없으면 평소 강조색.
        let accent = b
            .character
            .as_deref()
            .and_then(theme::character_accent)
            .unwrap_or_else(theme::accent);
        let title = b.title.clone();
        let body = b.body.clone();

        b.gpu.clear_chrome();
        // 창을 투명으로 띄웠으므로 배경을 안 칠하면 **판 바깥이 그대로 비친다** —
        // 둥근 모서리가 사는 건 그 덕이다. 알파 합성이 안 먹는 환경에서는 이 판이
        // 그냥 사각형으로 보일 뿐, 내용은 그대로다.
        b.gpu.round_rect_fill(0.0, 0.0, w, h, 14.0, theme::panel_bg());
        // 왼쪽 색띠 — 알림 여럿이 쌓였을 때 누구 것인지 글자보다 먼저 읽힌다.
        b.gpu.round_rect_fill(0.0, 0.0, 4.0, h, 2.0, accent);

        let x0 = 18.0;
        b.gpu.draw_text(
            x0,
            16.0,
            &title,
            gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false },
        );
        b.gpu.draw_text(
            x0,
            40.0,
            &body,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        // 헤드리스 검증: 배너는 **별도 창**이라 메인 창을 찍는 `KASATERM_AUTOCAPTURE`
        // 로는 안 잡힌다. auxwin 이 자기 gpu 로 따로 readback 하는 것과 같은 이유다.
        if let Ok(p) = std::env::var("KASATERM_AUTOBANNER_CAP") {
            let dot = p.rfind('.').unwrap_or(p.len());
            b.gpu.capture_next = Some(format!("{}-{idx}{}", &p[..dot], &p[dot..]));
        }
        let _ = b.gpu.render(&[], scale, 0.0, true);
    }

    /// 수명이 다한 배너를 걷고, 남은 것을 위로 당긴다. `about_to_wait` 에서 매 틱.
    pub(crate) fn expire_banners(&mut self) {
        if self.banners.is_empty() {
            return;
        }
        let now = Instant::now();
        let before = self.banners.len();
        self.banners.retain(|b| b.until > now);
        if self.banners.len() != before {
            self.reflow_banners();
        }
    }

    /// 한 장이 걷힌 뒤 남은 것을 제자리로 올린다. 안 하면 가운데가 빈 채로 남아
    /// 「아직 뭔가 있나」로 보인다.
    fn reflow_banners(&mut self) {
        for (i, b) in self.banners.iter().enumerate() {
            let Some(pos) = b.window.outer_position().ok() else { continue };
            let sf = b.window.scale_factor();
            let cur_y = pos.y as f64 / sf;
            // 첫 장의 y 를 기준으로 삼지 않고 각자 자기 화면 원점을 다시 못 박는다 —
            // 배너가 뜬 사이 창이 다른 모니터로 갔을 수 있다.
            let base = b
                .window
                .current_monitor()
                .map(|m| m.position().y as f64 / m.scale_factor())
                .unwrap_or(0.0);
            let want = base + MARGIN_T as f64 + i as f64 * (H as f64 + GAP as f64);
            if (cur_y - want).abs() > 0.5 {
                let x = pos.x as f64 / sf;
                b.window.set_outer_position(LogicalPosition::new(x, want));
            }
        }
    }

    /// 배너 창으로 온 이벤트. 그릴 것과 닫을 것만 본다 — 입력은 안 받는다.
    pub(crate) fn banner_window_event(&mut self, idx: usize, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::RedrawRequested => {
                self.draw_banner(idx);
                true
            }
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                if idx < self.banners.len() {
                    self.banners.remove(idx);
                    self.reflow_banners();
                }
                true
            }
            _ => true,
        }
    }
}
