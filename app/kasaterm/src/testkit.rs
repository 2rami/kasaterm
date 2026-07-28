//! 자동 테스트 하네스 — env 기반 auto-split/window/toggle/drag/tabs + schedule 타이머.
use super::*;

/// md 스크립트에 아직 실행할 단계가 남았는지. `about_to_wait` 이 이걸 보고
/// 프레임을 펌프한다 — 자세한 사정은 `run_pending_automdscript` 참고.
static MDSCRIPT_LEFT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn mdscript_pending() -> bool {
    MDSCRIPT_LEFT.load(std::sync::atomic::Ordering::Relaxed)
}

/// 모니터 이동 프로브의 "정착 후" 재측정 예약 시각. 검증 전용이라
/// `struct App` 을 늘리지 않는다(병렬 작업 충돌 핫스팟).
static LAYERGEOM_DUE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

impl App {
    /// Headless verification: arm a clean exit after KASATERM_AUTOQUIT_MS so a
    /// background run exercises the save-on-exit path (and thus the next
    /// launch's restore). No-op when the env var is unset.
    pub(crate) fn schedule_autoquit(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOQUIT_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        eprintln!("[autoquit] clean exit in {ms}ms");
        self.autoquit_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn schedule_autocapture(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOCAPTURE_MS") else { return; };
        // 콤마로 여러 시각 지정 가능("14000,14300") — 애니메이션처럼 시간에
        // 따라 그림이 바뀌는 기능을 프레임 비교로 검증할 때 쓴다.
        let deadlines: Vec<u64> = ms_str
            .split(',')
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .collect();
        if deadlines.is_empty() {
            return;
        }
        let path = std::env::var("KASATERM_AUTOCAPTURE_PATH").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("kasaterm.png")
                .to_string_lossy()
                .into_owned()
        });
        // Optional git-panel demo before the capture: expand the first changed
        // file's inline diff ("diff") or open the commit modal ("modal").
        if let Ok(action) = std::env::var("KASATERM_AUTOGIT") {
            let gms: u64 = std::env::var("KASATERM_AUTOGIT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(deadlines[0].saturating_sub(1500));
            self.pending_autogit = Some((
                std::time::Instant::now() + std::time::Duration::from_millis(gms),
                action,
            ));
        }
        // GPU frame readback (gpu::render → save_rgba_png) needs no OS
        // screen-record permission, so it works headless on every platform —
        // replacing the old screencapture (macOS, permission-blocked) and
        // PrintWindow (Windows, can't grab the Vulkan/Metal surface) paths.
        let multi = deadlines.len() > 1;
        for (i, ms) in deadlines.into_iter().enumerate() {
            // 단일 캡처는 기존 경로 그대로, 다중이면 "-1", "-2" suffix.
            let p = if multi {
                match path.rsplit_once('.') {
                    Some((stem, ext)) => format!("{stem}-{}.{ext}", i + 1),
                    None => format!("{path}-{}", i + 1),
                }
            } else {
                path.clone()
            };
            eprintln!("[autocapture] in {ms}ms → {p} (gpu readback)");
            self.pending_capture.push((
                std::time::Instant::now() + std::time::Duration::from_millis(ms),
                p,
            ));
        }
    }
    /// Run a queued git-panel demo action (KASATERM_AUTOGIT) so headless capture
    /// can show the inline diff / commit modal without a real click.
    pub(crate) fn run_autogit(&mut self, action: &str) {
        // The demo actions assume the column is up; open it for headless capture.
        if !self.git.col_visible {
            self.toggle_git_col();
        }
        match action {
            "diff" => {
                let pick = self.git.col_data.lock().ok().and_then(|g| {
                    g.unstaged
                        .first()
                        .map(|(_, p)| (false, p.clone()))
                        .or_else(|| g.staged.first().map(|(_, p)| (true, p.clone())))
                });
                if let Some((staged, path)) = pick {
                    self.toggle_git_diff(staged, path);
                }
            }
            "modal" => self.open_commit_modal(),
            "menu" => self.git.commit_menu_open = true,
            "spin" => self.git.op = Some("Pushing"),
            "hover" => {
                // Park the cursor over the first file row so its action cluster
                // (open / discard / stage) renders for a headless capture.
                let gx = self.git_col_x();
                let gw = self.git_col_w();
                self.cursor_px = (gx + gw - 30.0, TITLE_HEIGHT + 150.0);
            }
            _ => {}
        }
    }
    pub(crate) fn schedule_autosend(&self) {
        let Ok(text) = std::env::var("KASATERM_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        // Capture whichever backend is wired so we don't need access
        // to self inside the timer thread.
        let tmux = self.tmux.clone();
        // Autosend always targets the currently-focused pane. In tmux
        // mode we leave pane targeting to the daemon; in pty mode we
        // grab the active session here so the closure doesn't need
        // self access.
        let pty = self.active_pty().cloned();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let mut payload = text.clone();
            // Enter is CR on Windows' ConPTY — PowerShell reads a bare LF as
            // "line unfinished" and parks on its `>>` continuation prompt, so
            // the autosent command never runs. POSIX shells want LF.
            let eol = if cfg!(windows) { '\r' } else { '\n' };
            if !payload.ends_with(eol) {
                payload.push(eol);
            }
            if let Some(t) = tmux.as_ref() {
                let hex: String = payload
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = t.send_keys_hex(None, &hex);
            } else if let Some(p) = pty.as_ref() {
                let _ = p.send_bytes(payload.as_bytes());
            }
        });
    }
    /// Headless confirm-modal repro: after `KASATERM_TEST_CONFIRM_MS` fire the
    /// window-close confirm path, so a background run can screenshot the modal
    /// (pair with AUTOSEND="sleep 300" to give a pane a real foreground job).
    pub(crate) fn arm_autoconfirm(&mut self) {
        let Ok(ms) = std::env::var("KASATERM_TEST_CONFIRM_MS") else { return };
        let Ok(ms) = ms.parse::<u64>() else { return };
        self.autoconfirm_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autoconfirm(&mut self) {
        let Some(due) = self.autoconfirm_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoconfirm_at = None;
        let raised = self.confirm_or_close_window();
        eprintln!("[autoconfirm] confirm_or_close_window -> raised={raised}");
    }
    /// Headless 사이드바 창-닫기 repro: `KASATERM_AUTOWINCLOSE_MS` 뒤에 사이드바
    /// 창 탭의 ×를 **실제 hit-test 경로**(`window_strip_click`)로 누른다.
    ///
    /// 그 ×는 한 번 확인 모달을 잃은 적이 있다 — 사이드바 strip 과 상단 탭 strip 을
    /// 한 함수로 합치면서 `close_window` 를 직접 부르게 돼, 돌고 있는 claude 가
    /// 말없이 죽었다. `confirm_or_close_session` 을 직접 부르는 테스트로는 그 회귀를
    /// 못 잡으므로(끊긴 건 라우팅이지 모달이 아니다) 좌표 클릭으로 재현한다.
    /// AUTOSEND="sleep 300" 과 같이 써서 pane 에 진짜 작업을 물려둘 것.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autowinclose(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWINCLOSE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        // close rect 는 ①사이드바가 보이고 ②창이 2개 이상일 때만 그려진다
        // (sidebar_layout 의 `n > 1` — 마지막 창은 닫을 수 없으니 ×가 없다).
        // 좌표는 렌더가 채우므로 조건을 맞춘 뒤 한 프레임 그린다.
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        if self.windows.len() < 2 {
            self.new_window();
        }
        self.render_frame();
        let Some((_, r)) = self.window_tab_close_rects.first().copied() else {
            eprintln!("[autowinclose] close rect 없음 — 사이드바 창 탭이 안 그려졌다");
            return;
        };
        let handled = self.window_strip_click(r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        eprintln!(
            "[autowinclose] handled={handled} confirm_raised={} why={:?}",
            self.confirm_close.is_some(),
            self.confirm_close.as_ref().map(|c| match &c.why {
                CloseWhy::Busy(p) => format!("busy:{p}"),
                CloseWhy::Dirty(d) =>
                    format!("dirty:{}", d.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(",")),
            }),
        );
    }
    /// 상단바 토글 프로브. `KASATERM_AUTOHEADER_MS` 뒤에 활성 pane 의 헤더 띠를
    /// 켜고, PTY 행 수가 실제로 줄었는지 찍는다.
    ///
    /// 띠를 켜면 셀 그리드가 그만큼 밀리므로 render·hit-test·PTY 가 같은 값을 봐야
    /// 한다. 행 수가 그대로면 PTY 만 옛 크기로 남아 클릭이 한 행씩 어긋나는데,
    /// 캡처로는 멀쩡해 보인다 — 그래서 숫자로 찍는다.
    pub(crate) fn run_pending_autoheader(&mut self) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicUsize = AtomicUsize::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOHEADER_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        // 단계마다 1.5초 — SIGWINCH → 셸 재그림 → apply_screen_update 가 비동기라
        // 토글 직후 읽으면 옛 격자가 그대로 보인다.
        let step = STEP.load(Ordering::Relaxed);
        if step > 4
            || Instant::now() < *due + std::time::Duration::from_millis(1500 * step as u64)
        {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(target) = self.ws.lock().unwrap().active_pane.clone() else { return };
        let snap = |app: &Self| {
            let ws = app.ws.lock().unwrap();
            ws.panes.get(&target).map(|p| {
                (p.has_header(), p.term().map_or((0, 0), |t| (t.cols, t.rows)))
            })
        };
        // 0: 그대로 읽기 → 1: resize_backend 만(토글 없이) → 2: 읽기 → 3: 헤더 켜기
        // → 4: 읽기. 1번이 있어야 "격자가 변한 게 헤더 때문인지, 그냥 리사이즈가
        // 처음 밀린 것인지" 를 가른다 — 이걸 안 갈라서 한 번 오판했다.
        match step {
            0 => eprintln!("[autoheader] 0 초기 {:?}", snap(self)),
            1 => {
                let (c, r) = self.window_cells();
                self.resize_backend(c, r);
                eprintln!("[autoheader] 1 헤더 없이 resize_backend 만 호출");
            }
            2 => eprintln!("[autoheader] 2 리사이즈 후 {:?}", snap(self)),
            3 => {
                self.toggle_pane_header(&target);
                eprintln!("[autoheader] 3 헤더 켬");
            }
            _ => eprintln!("[autoheader] 4 헤더 켠 뒤 {:?}", snap(self)),
        }
    }
    /// surface 크기 어긋남 재현. `KASATERM_FORCE_SURFACE_HALF_MS` 뒤에 스왑체인만
    /// 창의 절반 크기로 다시 잡는다 — 모니터를 옮길 때 Resized/ScaleFactorChanged
    /// 가 코얼레스되며 실제로 벌어지는 상태를 인위적으로 만든 것이다.
    /// 거노 스크린샷 실측(창 1510x950 안에 콘텐츠 754x472, 빈 영역은 우리
    /// 배경색이 아닌 NSWindow 기본색)이 바로 이 상태다.
    pub(crate) fn run_pending_forcesurfacehalf(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_FORCE_SURFACE_HALF_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(size) = self.window.as_ref().map(|w| w.inner_size()) else { return };
        // `view` = 뷰 자체를 줄인다(거노가 본 상태 — UI 가 온전한 채로 축소).
        // 그 외 = 스왑체인만 줄인다(UI 가 잘림). 두 증상이 다르다는 게
        // 원인 판별의 핵심이었다.
        if std::env::var("KASATERM_FORCE_SURFACE_HALF_KIND").as_deref() == Ok("view") {
            if let Some(w) = self.window.as_ref() {
                gpu::shrink_view_for_test(w);
            }
        } else if let Some(g) = self.gpu.as_mut() {
            g.resize(size.width / 2, size.height / 2);
            eprintln!(
                "[forcehalf] 스왑체인만 {}x{} 로 축소(창은 {}x{} 그대로)",
                size.width / 2,
                size.height / 2,
                size.width,
                size.height
            );
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 모니터 이동 재현. `KASATERM_AUTOMOVESCREEN_MS="7000,11000"` 처럼 콤마로
    /// 여러 시각을 주면 그때마다 창을 **다른 물리 모니터로** 옮긴다(핑퐁).
    /// `KASATERM_AUTOCAPTURE_MS` 를 그 사이사이에 끼워 이동 전/후 프레임을
    /// 비교하면 "큰 모니터로 옮기면 화면이 구석에 처박힌다" 를 헤드리스에서
    /// 그대로 볼 수 있다. 레이어 속성만 흉내 내는 재현은 실패했다 — AppKit 이
    /// 진짜로 backing scale 을 바꿔야 한다.
    pub(crate) fn run_pending_automovescreen(&mut self) {
        use std::sync::OnceLock;
        static DUE: OnceLock<Vec<Instant>> = OnceLock::new();
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOMOVESCREEN_MS")
                .ok()
                .map(|s| {
                    let now = Instant::now();
                    s.split(',')
                        .filter_map(|p| p.trim().parse::<u64>().ok())
                        .map(|ms| now + std::time::Duration::from_millis(ms))
                        .collect()
                })
                .unwrap_or_default()
        });
        let i = NEXT.load(std::sync::atomic::Ordering::Relaxed);
        let Some(at) = due.get(i) else { return };
        if Instant::now() < *at {
            return;
        }
        NEXT.store(i + 1, std::sync::atomic::Ordering::Relaxed);
        let Some(w) = self.window.clone() else { return };
        eprintln!("[movescreen] #{i} 이동 시작");
        gpu::log_layer_geometry(&w, &format!("이동#{i} 전"));
        gpu::move_window_to_other_screen(&w);
        gpu::log_layer_geometry(&w, &format!("이동#{i} 직후"));
        *LAYERGEOM_DUE.lock().unwrap() = Some(Instant::now() + std::time::Duration::from_millis(1200));
    }
    /// 이동 뒤 AppKit 이 프레임/스케일을 정착시킬 시간을 준 다음 한 번 더 실측.
    /// `ScaleFactorChanged` 는 `setFrame:` 과 같은 턴에 안 올 수 있어서
    /// "직후" 값만 보면 정상으로 오판한다. (검증 전용이라 struct App 필드를
    /// 늘리지 않는다 — 병렬 작업 충돌 핫스팟이다.)
    pub(crate) fn run_pending_layergeom(&mut self) {
        let due = { *LAYERGEOM_DUE.lock().unwrap() };
        let Some(at) = due else { return };
        if Instant::now() < at {
            return;
        }
        *LAYERGEOM_DUE.lock().unwrap() = None;
        if let Some(w) = self.window.clone() {
            gpu::log_layer_geometry(&w, "정착 후");
        }
    }
    /// 줌 클릭 매핑 프로브. `KASATERM_AUTOZOOMPROBE_MS` 뒤에 활성 pane 을 줌하고,
    /// 작업영역 전체에 격자로 점을 찍어 `px_to_pane_cell` 이 어디로 보내는지 찍는다.
    ///
    /// 지켜야 할 불변식은 하나다 — **줌 중엔 작업영역 안 모든 점이 줌된 pane 으로
    /// 가야 한다.** 예전엔 원본 split 박스로 판정해 아래 절반이 숨은 pane 으로
    /// 샜고(거노: "최대화하고 위치 매핑이 이상해"), 화면엔 그 pane 이 안 보이니
    /// 클릭이 사라지는 것처럼 보였다. 눈으로 보는 캡처로는 절대 안 잡히는 종류라
    /// 좌표를 직접 찍는 프로브를 남긴다.
    pub(crate) fn run_pending_autozoomprobe(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOZOOMPROBE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(target) = self.ws.lock().unwrap().active_pane.clone() else {
            eprintln!("[zoomprobe] 활성 pane 없음");
            return;
        };
        let Some(size) = self.window.as_ref().map(|w| w.inner_size()) else {
            eprintln!("[zoomprobe] 창 없음");
            return;
        };
        let scale = self.effective_scale();
        let (lw, lh) = (size.width as f32 / scale, size.height as f32 / scale);
        let probe = |app: &Self, tag: &str| {
            // 작업영역 안쪽만 — 패딩·타이틀바 밖은 애초에 pane 이 아니다.
            let x0 = app.effective_sidebar_w() + WINDOW_PADDING + 4.0;
            for i in 0..5 {
                for j in 0..5 {
                    let px = x0 + (lw - x0 - 8.0) * (i as f32 / 4.0);
                    let py = TITLE_HEIGHT + 4.0 + (lh - TITLE_HEIGHT - 40.0) * (j as f32 / 4.0);
                    let hit = app.px_to_pane_cell(px, py);
                    eprintln!(
                        "[zoomprobe] {tag} ({px:.0},{py:.0}) → {}",
                        hit.map_or("(없음)".to_string(), |(p, c, r)| format!("{p} {c},{r}"))
                    );
                }
            }
        };
        probe(self, "before");
        self.toggle_pane_zoom(&target);
        eprintln!("[zoomprobe] zoomed={target}");
        probe(self, "after");
    }
    /// Headless Info 패널 repro: `KASATERM_AUTOINFO_MS` 뒤에 우측 칼럼을 열고
    /// Info 탭으로 넘긴다. 그 탭은 클릭으로만 갈 수 있어 캡처 하네스에서 볼
    /// 방법이 없었다 — 프로세스·포트 목록은 눈으로 봐야 폭·정렬을 판단한다.
    /// AUTOSEND 로 pane 에 자식 프로세스를 물려두면 목록이 채워진 채 찍힌다.
    ///
    /// `KASATERM_AUTOINFO=hover|menu` 를 주면 탭이 열리고 1.5초 뒤(첫 수집이
    /// 끝나 행 좌표가 생긴 뒤) 첫 프로세스 행에 커서를 올리거나 우클릭 메뉴를
    /// 띄운다. 종료(×) 버튼과 메뉴는 호버·우클릭에만 나타나 정적 캡처로는
    /// 존재 자체를 확인할 수 없다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoinfo(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static OPENED: AtomicBool = AtomicBool::new(false);
        static ACTED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOINFO_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due {
            return;
        }
        if !OPENED.swap(true, Ordering::Relaxed) {
            if !self.git.col_visible {
                self.toggle_git_col();
            }
            self.info.tab = crate::state::SideTab::Info;
            self.info.scroll = 0.0;
            eprintln!("[autoinfo] Info 탭 열림 (col_visible={})", self.git.col_visible);
            return;
        }
        let act = match std::env::var("KASATERM_AUTOINFO").ok() {
            Some(v) if v == "hover" || v == "menu" => v,
            _ => return,
        };
        if ACTED.load(Ordering::Relaxed)
            || Instant::now() < *due + std::time::Duration::from_millis(1500)
        {
            return;
        }
        ACTED.store(true, Ordering::Relaxed);
        let Some((pid, r)) = self.info.proc_rects.first().copied() else {
            eprintln!("[autoinfo] 프로세스 행 없음 — 좌표가 아직 안 생겼다");
            return;
        };
        let (cx, cy) = (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        if act == "menu" {
            self.info.ctx_menu = Some((cx, cy, pid));
        }
        // hover 든 menu 든 커서는 행 위에 둔다 — menu 도 그 행이 하이라이트된
        // 상태로 찍혀야 어느 프로세스를 겨눈 메뉴인지 보인다.
        self.cursor_px = (cx, cy);
        eprintln!("[autoinfo] act={act} pid={pid} at ({cx:.0},{cy:.0})");
    }
    /// `KASATERM_FORCE_HANDLE_MENU=*` → 활성 pane 의 ⋮ 메뉴를 연다. 이 env 는
    /// 생성자에서 pane id 를 그대로 받는데(main.rs), 로컬 PTY 모드의 leaf id 는
    /// 곧 셸 pid 라 실행 전에는 알 수가 없다 — 그래서 `*` 만 여기서 한 번
    /// 실제 id 로 바꿔 준다. pane 이 생긴 뒤에 도는 틱에서 호출된다.
    pub(crate) fn resolve_force_handle_menu(&mut self) {
        if self.handle_menu.as_deref() != Some("*") {
            return;
        }
        let Some(id) = self.ws.lock().unwrap().active_pane.clone() else { return };
        self.handle_menu = Some(id);
        self.chrome_dirty = true;
    }
    /// `KASATERM_AUTOMENUPICK=<idx>` — 열려 있는 ⋮ 메뉴의 idx 번째 항목을
    /// **진짜 클릭**한다(`KASATERM_FORCE_HANDLE_MENU=*` 로 연 뒤). 화면
    /// 새로고침처럼 "깨진 화면을 고치는" 동작은 고쳐지는 걸 캡처로 봐야
    /// 검증이 되는데, winit `KeyEvent` 는 외부에서 만들 수 없어 단축키로는
    /// 하네스를 못 짠다 — 마우스 경로가 유일한 자동 검증 통로다.
    pub(crate) fn run_pending_automenuclick(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<(Instant, usize)>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let idx = std::env::var("KASATERM_AUTOMENUPICK")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())?;
            let ms = std::env::var("KASATERM_AUTOMENUCLICK_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), idx))
        });
        let Some((due, idx)) = *due else { return };
        if DONE.load(Ordering::Relaxed) || Instant::now() < due {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let Some(&(act, r)) = self.handle_menu_hits.get(idx) else {
            eprintln!(
                "[automenuclick] idx{idx} 없음 (rects={})",
                self.handle_menu_hits.len()
            );
            DONE.store(true, Ordering::Relaxed);
            return;
        };
        DONE.store(true, Ordering::Relaxed);
        self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.window_event(
                event_loop,
                wid,
                WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state,
                    button: MouseButton::Left,
                },
            );
        }
        eprintln!("[automenuclick] idx{idx} {act:?} 클릭 @({:.0},{:.0})", self.cursor_px.0, self.cursor_px.1);
    }
    /// `KASATERM_AUTOPILLCLICK_MS` 뒤에 타이틀바 사용량 pill 을 **진짜로 클릭**한다.
    /// 다른 probe 처럼 상태를 손으로 세팅하지 않고 winit `MouseInput` 을 그대로
    /// `window_event` 에 흘려보내 handler 디스패치까지 태운다 — "render 는 그렸는데
    /// handler 가 안 잡는다"(⋮ 메뉴의 상단바 토글이 실제로 그랬다) 종류의 버그는
    /// 이 경로로만 잡히기 때문이다. 두 번째 클릭 좌표를 `KASATERM_AUTOPILLPICK`
    /// (드롭다운 행 인덱스)으로 주면 그 항목까지 눌러 전환 결과를 확인한다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autopillclick(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOPILLCLICK_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        // 단계마다 800ms 간격 — 클릭 결과가 다음 프레임에 그려질 시간을 준다.
        if Instant::now() < *due + std::time::Duration::from_millis(800 * step as u64) {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let click = |app: &mut Self, x: f32, y: f32| {
            app.cursor_px = (x, y);
            for state in [ElementState::Pressed, ElementState::Released] {
                app.window_event(
                    event_loop,
                    wid,
                    WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left },
                );
            }
        };
        match step {
            0 => {
                let Some(r) = self.account_chip_rect else {
                    eprintln!("[autopillclick] chip rect 없음 — pill 자체가 안 그려졌다");
                    STEP.store(9, Ordering::Relaxed);
                    return;
                };
                let (x, y) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                click(self, x, y);
                eprintln!(
                    "[autopillclick] pill({x:.0},{y:.0}) 클릭 → account_menu={} rows={}",
                    self.account_menu,
                    self.account_menu_hits.len()
                );
                STEP.store(1, Ordering::Relaxed);
            }
            1 => {
                let pick = std::env::var("KASATERM_AUTOPILLPICK")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok());
                if let Some(i) = pick {
                    match self.account_menu_hits.get(i).map(|(_, r)| *r) {
                        Some(r) => {
                            click(self, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                            eprintln!(
                                "[autopillclick] row{i} 클릭 → account='{}' menu={}",
                                self.set_claude_account, self.account_menu
                            );
                        }
                        None => eprintln!("[autopillclick] row{i} 없음"),
                    }
                }
                STEP.store(9, Ordering::Relaxed);
            }
            _ => {}
        }
    }
    /// Headless settings-window repro: open the settings *window* (auxwin) after
    /// `KASATERM_AUTOSETTINGS_MS`, on the category named in `KASATERM_AUTOSETTINGS`
    /// ("appearance" / "shell" / "claude" / "students", default General), then arm
    /// a self-capture of that aux surface at +1500ms (path `KASATERM_AUTOSETTINGS_CAP`,
    /// default scratchpad `settings-window.png`). Function-local statics — no App
    /// field (parallel-work rule: struct App stays untouched).
    pub(crate) fn run_pending_autosettings(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOSETTINGS_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let cat_env = std::env::var("KASATERM_AUTOSETTINGS").unwrap_or_default();
        let cat = match cat_env.as_str() {
            "appearance" => SettingsCat::Appearance,
            "shell" => SettingsCat::Shell,
            "claude" => SettingsCat::Claude,
            "students" => SettingsCat::Students,
            _ => SettingsCat::General,
        };
        // 딥링크 검증: KASATERM_AUTOSETTINGS_STUDENT 로 특정 학생 선택 상태(=프사
        // 클릭 결과)를 헤드리스로 재현 — persona 편집기가 뜬 화면을 캡처한다.
        let student = std::env::var("KASATERM_AUTOSETTINGS_STUDENT")
            .ok()
            .filter(|s| !s.is_empty());
        eprintln!("[autosettings] open settings window cat={cat_env} student={student:?}");
        self.open_settings_window(event_loop, Some(cat), student);
        // Aux capture (main autocapture only reaches the main window). +1500ms so
        // the new window renders a full frame first.
        let cap = std::env::var("KASATERM_AUTOSETTINGS_CAP").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("settings-window.png")
                .to_string_lossy()
                .into_owned()
        });
        if let Some(a) = self.settings_window_idx().and_then(|i| self.aux_windows.get_mut(i)) {
            a.pending_capture =
                Some((Instant::now() + std::time::Duration::from_millis(1500), cap));
        }
    }
    /// Headless raw-editor selection seed: KASATERM_TEST_MD_SELECT="al,ac,cl,cc"
    /// plants a selection (anchor line/col → cursor line/col) on the active
    /// raw editor after KASATERM_TEST_MD_SELECT_MS (default 6000 — pair with
    /// KASATERM_AUTOOPEN so the editor exists first). Mouse drags aren't
    /// injectable headlessly; this lets a capture prove the selection band.
    /// Function-local statics — no App field (parallel-work rule).
    pub(crate) fn run_pending_automdselect(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_TEST_MD_SELECT").ok().map(|_| {
                let ms: u64 = std::env::var("KASATERM_TEST_MD_SELECT_MS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(6000);
                Instant::now() + std::time::Duration::from_millis(ms)
            })
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let spec = std::env::var("KASATERM_TEST_MD_SELECT").unwrap_or_default();
        let nums: Vec<usize> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let [al, ac, cl, cc] = nums[..] else {
            eprintln!("[automdselect] expected al,ac,cl,cc — got {spec:?}");
            return;
        };
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            pane.dirty = true;
            if let Some(m) = pane.markdown_mut() {
                m.sel_anchor = Some((al, ac));
                m.cur_line = cl;
                m.cur_col = cc;
            }
        }
        self.md_ensure_caret_visible();
        eprintln!("[automdselect] anchor=({al},{ac}) cursor=({cl},{cc})");
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless 편집기 스크립트: `KASATERM_TEST_MD_SCRIPT` 에 `|` 로 이은 단계를
    /// `KASATERM_TEST_MD_STEP_MS`(기본 700) 간격으로 하나씩 실행한다. 첫 단계는
    /// `KASATERM_TEST_MD_SCRIPT_MS`(기본 5000, `KASATERM_AUTOOPEN` 이 편집기를
    /// 띄운 뒤여야 한다) 에 시작.
    ///
    /// 단계: `scroll:<px>` 절대 스크롤 · `mode:raw|render` 토글 · `cap:<경로>` 캡처.
    /// 키 입력이 아니라 상태를 직접 건드리는 이유는 winit `KeyEvent` 가 밖에서
    /// 만들 수 없어서다(비공개 필드) — 키 경로 자체는 유닛 테스트가 맡는다.
    /// autosettings 처럼 함수-로컬 static 이라 `struct App` 은 안 건드린다.
    pub(crate) fn run_pending_automdscript(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        static PLAN: OnceLock<Option<(Instant, u64, Vec<String>)>> = OnceLock::new();
        static DONE: AtomicUsize = AtomicUsize::new(0);
        // 이 함수는 about_to_wait 에서만 도는데, 앱은 할 일이 없으면 `Wait` 로
        // 완전히 잠들어 about_to_wait 자체가 안 돈다. 그러면 다음 단계 시각이
        // 와도 아무도 깨우지 않아 스크립트가 중간에 멎는다. 남은 단계가 있는
        // 동안은 펌프를 켠다(`MDSCRIPT_LEFT`).
        //
        // 그리고 **밀린 단계는 한 번에 다 소화한다.** 한 패스에 한 단계만 처리하면
        // 스크립트가 사실상 "프레임 수"로 페이싱된다 — 디버그 빌드의 raw 편집기는
        // 한 프레임이 swash 글리프 힌팅에 수 초를 쓰므로(샘플러로 확인), 같은
        // 스크립트가 판마다 3~7단계에서 제멋대로 끊겼다. 코드 문제로 보였지만
        // 실은 하네스가 프레임을 못 따라간 것이다.
        let plan = PLAN.get_or_init(|| {
            let spec = std::env::var("KASATERM_TEST_MD_SCRIPT").ok()?;
            let start: u64 = std::env::var("KASATERM_TEST_MD_SCRIPT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000);
            let step: u64 = std::env::var("KASATERM_TEST_MD_STEP_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(700);
            let steps: Vec<String> = spec.split('|').map(|s| s.trim().to_string()).collect();
            Some((
                Instant::now() + std::time::Duration::from_millis(start),
                step,
                steps,
            ))
        });
        let Some((start, step_ms, steps)) = plan else { return };
        loop {
            let n = DONE.load(Ordering::Relaxed);
            MDSCRIPT_LEFT.store(n < steps.len(), Ordering::Relaxed);
            if n >= steps.len() {
                return;
            }
            let due = *start + std::time::Duration::from_millis(*step_ms * n as u64);
            if Instant::now() < due {
                return;
            }
            DONE.store(n + 1, Ordering::Relaxed);
            // 마지막 단계가 캡처면 거기서 멈춘다 — 캡처는 *다음* 프레임에
            // 찍히므로, 뒤에 밀린 단계를 이어서 돌리면 찍히기도 전에 화면이
            // 바뀐다.
            let stop_after = steps[n].starts_with("cap:");
            self.run_one_mdstep(&steps[n].clone(), event_loop);
            if stop_after {
                return;
            }
        }
    }

    fn run_one_mdstep(&mut self, step: &str, event_loop: &ActiveEventLoop) {
        let step = step.to_string();
        // pane 이 필요 없는 단계를 먼저 — 닫기 확인을 해소한 뒤엔 마크다운 pane
        // 자체가 사라지는데, 정작 그 "닫힌 뒤 화면"이 캡처하고 싶은 것이다.
        match step.split_once(':') {
            Some(("cap", p)) => {
                // `pending_capture` 큐를 거치지 않고 바로 무장한다 — 그 큐의 드레인은
                // 이 함수보다 **앞에서** 돌아, 큐에 넣으면 빨라야 다음 패스에나
                // 집히고 그 사이에 다음 단계가 끼어들면 바뀐 화면이 찍힌다
                // (실제로 raw 캡처가 render 로 되돌린 뒤 화면을 담았다).
                if let Some(g) = self.gpu.as_mut() {
                    g.capture_next = Some(p.to_string());
                }
                eprintln!("[mdscript] cap → {p}");
                self.wake_after_mdstep();
                return;
            }
            // 모달 버튼 누르기 — 저장/저장 안 함/취소.
            Some(("pick", v)) => {
                let btn = match v {
                    "save" => ConfirmBtn::Save,
                    "cancel" => ConfirmBtn::Cancel,
                    _ => ConfirmBtn::Close,
                };
                self.confirm_dialog_pick(btn, event_loop);
                eprintln!("[mdscript] pick={v} modal_left={}", self.confirm_close.is_some());
                self.wake_after_mdstep();
                return;
            }
            _ => {}
        }
        // 활성 pane 이 아니라 **마크다운 pane** 을 찾는다. 옆 셸이 먼저 죽으면
        // 포커스가 그쪽으로 넘어가고, 그러면 단계들이 아무 말 없이 반환돼
        // 스크립트가 중간에 멈춘 것처럼 보였다(실제로 4단계에서 끊겼다).
        let Some(id) = self.ws.lock().ok().and_then(|w| {
            let act = w.active_pane.clone();
            let is_md = |i: &String| w.panes.get(i).is_some_and(|p| p.markdown().is_some());
            act.filter(&is_md)
                .or_else(|| w.panes.keys().find(|i| is_md(i)).cloned())
        }) else {
            eprintln!("[mdscript] {step}: 마크다운 pane 없음");
            return;
        };
        match step.split_once(':') {
            Some(("scroll", v)) => {
                let px: usize = v.parse().unwrap_or(0);
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(&id) {
                        pane.dirty = true;
                        if let Some(m) = pane.markdown_mut() {
                            m.scroll = px;
                        }
                    }
                }
                eprintln!("[mdscript] scroll={px}");
            }
            Some(("mode", v)) => {
                let want_raw = v == "raw";
                self.set_md_mode(&id, want_raw);
                let at = self.md_anchor_line(&id);
                eprintln!("[mdscript] mode={v} anchor_line={at:?}");
            }
            // Raw 편집기 클릭 → 캐럿. 좌표는 **본문 박스 기준 상대 px**
            // (`click:<dx>,<dy>`) 이라 창 크기가 달라도 같은 자리를 가리킨다.
            // 마우스 이벤트를 밖에서 만들 수 없어 히트테스트 진입점을 직접 부른다.
            Some(("click", v)) => {
                let (dx, dy) = v.split_once(',').unwrap_or((v, "0"));
                let (dx, dy): (f32, f32) =
                    (dx.trim().parse().unwrap_or(0.0), dy.trim().parse().unwrap_or(0.0));
                let Some(&(bx, by, _, _)) = self.md_body_rects.get(&id) else {
                    eprintln!("[mdscript] click: 본문 박스 없음(raw 모드인지 확인)");
                    return;
                };
                self.md_click_caret(&id, bx + dx, by + dy);
                let at = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.panes.get(&id).and_then(|p| p.markdown()).map(|m| (m.cur_line, m.cur_col)));
                eprintln!("[mdscript] click=({dx},{dy}) caret={at:?}");
            }
            // 이 마크다운 탭을 닫아 본다 — 저장 안 한 편집분이 있으면 확인
            // 모달이 떠야 하고, 그 화면이 이 단계의 관찰 대상이다.
            Some(("close", _)) => {
                let tab = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.panes.get(&id).map(|p| p.active_tab))
                    .unwrap_or(0);
                self.confirm_or_close_tab(&id, tab);
                let why = self.confirm_close.as_ref().map(|c| match &c.why {
                    CloseWhy::Busy(p) => format!("busy:{p}"),
                    CloseWhy::Dirty(d) => format!(
                        "dirty:{}",
                        d.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(",")
                    ),
                });
                eprintln!("[mdscript] close why={why:?}");
            }
            // 편집 키는 winit `KeyEvent` 를 밖에서 만들 수 없어(비공개 필드)
            // 순수 메서드를 직접 부른다. 키→메서드 배선은 유닛 테스트가 아니라
            // 코드 경로로 확인하고, 여기선 **결과가 화면에 어떻게 그려지는지**만
            // 본다 — 들여쓴 항목이 실제로 한 단 들어가 보이는지 같은 것.
            Some(("edit", v)) => {
                {
                    let Ok(mut ws) = self.ws.lock() else { return };
                    let Some(pane) = ws.panes.get_mut(&id) else { return };
                    pane.dirty = true;
                    let Some(m) = pane.markdown_mut() else { return };
                    match v {
                        "tab" => m.indent(false),
                        "untab" => m.indent(true),
                        "enter" => m.newline(),
                        "find" => m.find_open(false),
                        "replace" => m.find_open(true),
                        "next" => m.find_step(false),
                        "prev" => m.find_step(true),
                        // `at <line>,<col>` 은 캐럿 이동, 나머지는 그대로 타이핑.
                        _ => match v.strip_prefix("at ") {
                            Some(pos) => {
                                let (l, c) = pos.split_once(',').unwrap_or((pos, "0"));
                                m.cur_line = l.trim().parse().unwrap_or(0);
                                m.cur_col = c.trim().parse().unwrap_or(0);
                            }
                            // 찾기 바가 열려 있으면 타이핑은 검색어로 — 실제
                            // 키 경로(md_editor_insert)와 같은 갈림길이다.
                            None if m.find.is_some() => m.find_type(v),
                            None => m.insert_at_caret(v),
                        },
                    }
                    eprintln!("[mdscript] edit={v} caret=({},{})", m.cur_line, m.cur_col);
                }
                self.md_ensure_caret_visible();
            }
            _ => eprintln!("[mdscript] 모르는 단계: {step:?}"),
        }
        self.wake_after_mdstep();
    }

    fn wake_after_mdstep(&mut self) {
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless undock repro: `KASATERM_AUTOUNDOCK_MS` 후 활성 터미널 pane 을
    /// 별도창으로 undock(헤더 아이콘 클릭은 헤드리스 주입 불가) 하고, 그 aux 창을
    /// +2500ms 에 자체 캡처(`KASATERM_AUTOUNDOCK_CAP`, 기본 temp undock-window.png).
    /// autosettings 처럼 함수-로컬 static(병렬 작업 규칙: struct App 무접촉).
    pub(crate) fn run_pending_autoundock(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOUNDOCK_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let Some(pid) = self.ws.lock().unwrap().active_pane.clone() else { return };
        eprintln!("[autoundock] undock pane {pid}");
        self.undock_pane_terminal(&pid, event_loop, None);
        let cap = std::env::var("KASATERM_AUTOUNDOCK_CAP").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("undock-window.png")
                .to_string_lossy()
                .into_owned()
        });
        if let Some(a) = self.aux_windows.iter_mut().find(|a| {
            matches!(&a.kind,
                crate::auxwin::AuxWindowKind::Terminal { pane_id } if *pane_id == pid)
        }) {
            a.pending_capture =
                Some((Instant::now() + std::time::Duration::from_millis(2500), cap));
        }
    }
    /// Headless "+" 셸 피커 repro: `KASATERM_AUTOSHELLMENU_MS` 후 피커 팝업을 연다 —
    /// 항목(기본 셸·Claude 학생 등)을 클릭 없이 캡처. autosettings 처럼 함수-로컬
    /// static(병렬 작업 규칙: struct App 무접촉).
    pub(crate) fn run_pending_autoshellmenu(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOSHELLMENU_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        eprintln!("[autoshellmenu] open shell picker");
        self.shell_menu_open = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless 파일트리 우클릭 메뉴 repro: `KASATERM_TEST_FTMENU_MS` 후 트리
    /// 첫 파일을 선택하고 컨텍스트 메뉴를 연다. 우클릭은 마우스 이벤트라 헤드리스
    /// 주입이 안 되는데, "…에서 열기" 항목은 기기에 설치된 앱 수만큼 늘어나므로
    /// 눈으로 한 번은 확인해야 한다. autoshellmenu 처럼 함수-로컬 static.
    pub(crate) fn run_pending_autoftmenu(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_TEST_FTMENU_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        // 트리가 아직 안 채워졌으면 다음 프레임에 다시 본다 — 파일트리는 워커가
        // 비동기로 채우므로 고정 지연만으로는 빈 트리에 메뉴를 띄울 수 있다.
        let Some(first) = self.file_tree.nodes.first().map(|n| n.path.clone()) else {
            return;
        };
        FIRED.store(true, Ordering::Relaxed);
        eprintln!("[autoftmenu] context menu on {}", first.display());
        self.file_tree.selected = Some(first);
        self.file_tree.ctx_menu = Some((260.0, 200.0));
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless file-open repro: schedule `open_file_split` on the path in
    /// `KASATERM_AUTOOPEN` after `KASATERM_AUTOOPEN_MS` (default 4000ms), so a
    /// background run can prove the preview pane + file-tree highlight without
    /// a real double-click (mouse events aren't injectable headlessly).
    pub(crate) fn arm_autoopen(&mut self) {
        let Ok(p) = std::env::var("KASATERM_AUTOOPEN") else { return };
        let ms: u64 = std::env::var("KASATERM_AUTOOPEN_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        self.autoopen_path = Some(std::path::PathBuf::from(p));
        self.autoopen_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autoopen(&mut self) {
        let Some(due) = self.autoopen_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoopen_at = None;
        if let Some(p) = self.autoopen_path.take() {
            eprintln!("[autoopen] open_file_split {}", p.display());
            self.open_file_split(p);
        }
    }
    /// Headless verification helper. Reads `KASATERM_AUTOSPLIT` ("h" / "v"
    /// / "hv" / "vh" ...) and fires the matching splits from
    /// `about_to_wait` after `KASATERM_AUTOSPLIT_MS` (default 2500ms),
    /// so a background `cargo run` can prove multi-pane rendering
    /// without a human pressing Cmd+D.
    pub(crate) fn run_pending_autosplits(&mut self) {
        if self.autosplit_plan.is_empty() {
            return;
        }
        let now = Instant::now();
        let due = match self.autosplit_at {
            Some(t) => t,
            None => return,
        };
        if now < due {
            return;
        }
        let dir = self.autosplit_plan.remove(0);
        if let Err(e) = self.split_active_pane(dir) {
            eprintln!("[autosplit] split failed: {e}");
        }
        // Chain the next split 500ms later so the renderer has time to
        // settle and a screenshot can capture intermediate states.
        self.autosplit_at = if self.autosplit_plan.is_empty() {
            None
        } else {
            Some(now + std::time::Duration::from_millis(500))
        };
    }
    /// Headless repro for the window sidebar: spawn KASATERM_AUTOWINDOWS extra
    /// windows, one every 600ms, so a screenshot can capture the multi-tab
    /// sidebar without a human pressing Cmd+T.
    pub(crate) fn run_pending_autowindows(&mut self) {
        if self.autowindow_left == 0 {
            return;
        }
        let now = Instant::now();
        let Some(due) = self.autowindow_at else { return };
        if now < due {
            return;
        }
        self.new_window();
        self.autowindow_left -= 1;
        self.autowindow_at = if self.autowindow_left == 0 {
            None
        } else {
            Some(now + std::time::Duration::from_millis(600))
        };
    }
    pub(crate) fn arm_autowindows(&mut self) {
        let Ok(n_str) = std::env::var("KASATERM_AUTOWINDOWS") else { return };
        let Ok(n) = n_str.parse::<usize>() else { return };
        if n == 0 {
            return;
        }
        let ms: u64 = std::env::var("KASATERM_AUTOWINDOWS_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        eprintln!("[autowindow] armed: {n} window(s) in {ms}ms");
        self.autowindow_left = n;
        self.autowindow_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autotoggle(&mut self) {
        let Some(due) = self.autotoggle_sidebar_at else { return };
        if Instant::now() < due {
            return;
        }
        self.toggle_sidebar();
        eprintln!(
            "[autotoggle] flipped → visible={} remaining={}",
            self.sidebar_visible, self.autotoggle_left
        );
        if self.autotoggle_left > 0 {
            self.autotoggle_left -= 1;
            self.autotoggle_sidebar_at =
                Some(Instant::now() + std::time::Duration::from_millis(1500));
        } else {
            self.autotoggle_sidebar_at = None;
        }
    }
    pub(crate) fn arm_autotoggle(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOTOGGLE_SIDEBAR_MS") else { return };
        let Ok(ms) = ms_str.parse::<u64>() else { return };
        self.autotoggle_left = std::env::var("KASATERM_AUTOTOGGLE_SIDEBAR_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        eprintln!("[autotoggle] sidebar flip in {ms}ms (repeat={})", self.autotoggle_left);
        self.autotoggle_sidebar_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    /// Headless arona-panel verification: open the arona window after
    /// `KASATERM_AUTOARONA_MS` (아로나 게이트 + webview load 포함 전체 경로).
    pub(crate) fn arm_autoarona(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOARONA_MS") else { return };
        let Ok(ms) = ms_str.parse::<u64>() else { return };
        eprintln!("[autoarona] toggle in {ms}ms");
        self.autoarona_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autoarona(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        let Some(due) = self.autoarona_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoarona_at = None;
        self.toggle_arona_panel(event_loop);
        eprintln!("[autoarona] toggled → open={}", self.arona_panel_window.is_some());
    }
    pub(crate) fn arm_autosplit(&mut self) {
        let Ok(plan) = std::env::var("KASATERM_AUTOSPLIT") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSPLIT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        let dirs: Vec<kasa_pty::SplitDir> = plan
            .chars()
            .filter_map(|c| match c {
                'h' | 'H' => Some(kasa_pty::SplitDir::Horizontal),
                'v' | 'V' => Some(kasa_pty::SplitDir::Vertical),
                _ => None,
            })
            .collect();
        if dirs.is_empty() {
            return;
        }
        eprintln!("[autosplit] armed: {plan:?} in {ms}ms");
        self.autosplit_plan = dirs;
        self.autosplit_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    /// Headless cross-pane tab-merge simulation. Reads
    /// KASATERM_AUTODRAG="src:from:dst" (e.g. "%2:0:%0") and fires
    /// `simulate_tab_merge` after KASATERM_AUTODRAG_MS (default 5500).
    pub(crate) fn arm_autodrag(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTODRAG") else { return };
        let parts: Vec<&str> = env.split(':').collect();
        if parts.len() < 3 {
            eprintln!("[autodrag] expected src:from:dst, got {env:?}");
            return;
        }
        let from: usize = parts[1].parse().unwrap_or(0);
        let ms: u64 = std::env::var("KASATERM_AUTODRAG_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5500);
        self.autodrag_plan = Some((parts[0].to_string(), from, parts[2].to_string()));
        self.autodrag_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[autodrag] armed: src={} from={} dst={} fire_in={ms}ms",
            parts[0], from, parts[2]);
    }
    pub(crate) fn run_pending_autodrag(&mut self) {
        let Some(t) = self.autodrag_at else { return };
        if Instant::now() < t { return; }
        self.autodrag_at = None;
        let Some((src, from, dst)) = self.autodrag_plan.take() else { return };
        self.simulate_tab_merge(&src, from, &dst);
    }
    /// Headless cross-window pane move. KASATERM_AUTOPANEMOVE=<dst window idx>
    /// relocates the active window's first leaf beside that window's first leaf
    /// via `move_pane`, exercising the sidebar-chip drop path without a drag.
    pub(crate) fn arm_autopanemove(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTOPANEMOVE") else { return };
        let Ok(dst) = env.parse::<usize>() else {
            eprintln!("[autopanemove] expected a window index, got {env:?}");
            return;
        };
        let ms: u64 = std::env::var("KASATERM_AUTOPANEMOVE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5500);
        self.autopanemove_dst = Some(dst);
        self.autopanemove_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[autopanemove] armed: dst_window={dst} fire_in={ms}ms");
    }
    pub(crate) fn run_pending_autopanemove(&mut self) {
        let Some(t) = self.autopanemove_at else { return };
        if Instant::now() < t { return; }
        self.autopanemove_at = None;
        let Some(dst_win) = self.autopanemove_dst.take() else { return };
        let moving = self
            .pty_layout
            .as_ref()
            .and_then(|l| l.leaves().first().map(|s| s.to_string()));
        let target = self
            .windows
            .get(dst_win)
            .and_then(|w| w.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()));
        match (moving, target) {
            (Some(m), Some(tg)) => {
                eprintln!("[autopanemove] move {m} → window {dst_win} (target {tg})");
                self.move_pane(&m, &tg, DropZone::Right);
            }
            (m, tg) => eprintln!("[autopanemove] skipped: moving={m:?} target={tg:?}"),
        }
    }
    /// Headless drag-preview repro. KASATERM_FORCE_DRAG="%N" (or empty = first
    /// leaf) parks that leaf in an active header_drag with the cursor in a
    /// sibling pane's lower half (Down zone), then stops — so a capture shows
    /// the floating ghost + vacated-slot scrim mid-drag.
    pub(crate) fn arm_force_drag(&mut self) {
        let Ok(env) = std::env::var("KASATERM_FORCE_DRAG") else { return };
        let ms: u64 = std::env::var("KASATERM_FORCE_DRAG_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        self.force_drag_leaf = Some(env);
        self.force_drag_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[force_drag] armed in {ms}ms");
    }
    pub(crate) fn run_pending_force_drag(&mut self) {
        let Some(t) = self.force_drag_at else { return };
        if Instant::now() < t { return; }
        self.force_drag_at = None;
        let Some(want) = self.force_drag_leaf.take() else { return };
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().into_iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if leaves.len() < 2 {
            eprintln!("[force_drag] need 2+ panes, have {}", leaves.len());
            return;
        }
        let pane = if leaves.iter().any(|s| *s == want) { want } else { leaves[0].clone() };
        // carried pane 을 제거하면 형제가 창 전체를 채운다 — 라이브 hit-test 는 그
        // base 기준이므로 커서를 *창 전체*의 가로 중앙·하단(80%)에 둬야 Down 쐐기에
        // 확실히 떨어진다(거노가 말한 1→2 밑). 형제의 옛 rect 기준으로 두면 정규화
        // 좌표상 대각선 경계라 Right 로 새기도 했다.
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let win_w = cols as f32 * self.cell.w;
        let win_h = rows as f32 * self.cell.h;
        self.cursor_px = (pad + win_w / 2.0, TITLE_HEIGHT + win_h * 0.8);
        self.header_drag = Some(HeaderDrag { pane, start: (0.0, 0.0), active: true, from_handle: false });
        // 라이브 이동을 실제로 적용 — 실드래그의 mouse-move 가 하는 일을 흉내.
        self.update_live_drag();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        eprintln!("[force_drag] parked drag; cursor=({:.0},{:.0})", self.cursor_px.0, self.cursor_px.1);
    }
    /// Pane header centre in logical px, mirroring `drop_target_at`'s box
    /// expansion. Used by `simulate_tab_merge` to land the synthetic
    /// cursor exactly where a user would aim "drop on header band".
    pub(crate) fn pane_header_center(&self, id: &str) -> Option<(f32, f32)> {
        let tree = self.pty_layout.as_ref()?;
        let leaves = tree.leaves().len();
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        let (_, cx, cy, cw, _) = rects.into_iter().find(|(i, ..)| i == id)?;
        let bx = pad + cx as f32 * self.cell.w;
        let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
        let bw = cw as f32 * self.cell.w;
        Some((bx + bw / 2.0, by - header_band / 2.0))
    }
    /// Simulate dragging `src.tabs[from]` onto `dst`'s header. Mirrors the
    /// release-handler's cross_pane merge branch so we can verify the
    /// path without a real mouse. Logs to stderr.
    pub(crate) fn simulate_tab_merge(&mut self, src: &str, from: usize, dst: &str) {
        let Some((mx, my)) = self.pane_header_center(dst) else {
            eprintln!("[autodrag] no rect for dst={dst}");
            return;
        };
        eprintln!("[autodrag] simulate src={src} from={from} dst={dst} mouse=({mx:.0},{my:.0})");
        let mut moved_pid: Option<String> = None;
        let mut moved: Option<PaneTab> = None;
        let mut src_empty = false;
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(s) = ws.panes.get_mut(src) {
                if from < s.tabs.len() {
                    let tab = s.tabs.remove(from);
                    moved_pid = tab.pid.clone();
                    moved = Some(tab);
                    if s.active_tab >= s.tabs.len() && !s.tabs.is_empty() {
                        s.active_tab = s.tabs.len() - 1;
                    }
                    src_empty = s.tabs.is_empty();
                    s.dirty = true;
                }
            }
            if let (Some(tab), Some(pid)) = (moved.take(), moved_pid.clone()) {
                ws.pid_to_pane.insert(pid, dst.to_string());
                if let Some(d) = ws.panes.get_mut(dst) {
                    let to = d.tabs.len();
                    d.tabs.insert(to, tab);
                    d.active_tab = to;
                    d.dirty = true;
                }
            }
            if src_empty {
                ws.panes.remove(src);
            }
        }
        if src_empty {
            self.collapse_layout_only(src);
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        let dst_tabs = self.ws.lock().unwrap()
            .panes.get(dst).map(|p| p.tabs.len()).unwrap_or(0);
        eprintln!("[autodrag] done; src_empty={src_empty} dst_tabs={dst_tabs}");
    }
    /// Headless repro for the in-pane tab header: queue N dummy tabs on the
    /// active pane KASATERM_AUTOTABS_MS (default 3200, after autosplit) later.
    pub(crate) fn arm_autotabs(&mut self) {
        let Ok(n_str) = std::env::var("KASATERM_AUTOTABS") else { return };
        let Ok(n) = n_str.parse::<usize>() else { return };
        if n == 0 {
            return;
        }
        let ms: u64 = std::env::var("KASATERM_AUTOTABS_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3200);
        eprintln!("[autotabs] armed: {n} tab(s) in {ms}ms");
        self.autotabs_n = n;
        self.autotabs_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autotabs(&mut self) {
        if self.autotabs_n == 0 {
            return;
        }
        let Some(due) = self.autotabs_at else { return };
        if Instant::now() < due {
            return;
        }
        let n = self.autotabs_n;
        // Spawn N real PTY-backed tabs so the headless verify cycle exercises
        // the stage-3 path (each tab has its own shell behind it). Falls back
        // to dummy label-only tabs if the spawn fails (e.g. tmux mode).
        let active = self.ws.lock().unwrap().active_pane.clone();
        if let Some(outer) = active {
            for i in 1..=n {
                if self.spawn_new_tab(&outer).is_err() {
                    if let Some(pane) = self.ws.lock().unwrap().panes.get_mut(&outer) {
                        let mut t = PaneTab::default();
                        t.title = Some(format!("탭 {}", i + 1));
                        pane.tabs.push(t);
                        pane.dirty = true;
                    }
                }
            }
            if let Some(pane) = self.ws.lock().unwrap().panes.get_mut(&outer) {
                pane.active_tab = 0;
                pane.dirty = true;
            }
        }
        eprintln!("[autotabs] added {n} tab(s) to active pane");
        self.autotabs_n = 0;
        self.autotabs_at = None;
        self.chrome_dirty = true;
    }

    /// Headless pop-out editor repro: `KASATERM_TEST_POPOUT=<file path>` opens
    /// that file in a separate wgpu editor window after `KASATERM_TEST_POPOUT_MS`
    /// (default 6000), then arms a self-capture of the *aux* surface at +1500ms
    /// (path `KASATERM_TEST_POPOUT_CAP`, default scratchpad `auxwin-popout.png`).
    /// Function-local statics — no App field (parallel-work rule).
    pub(crate) fn run_pending_auxpopout(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_TEST_POPOUT").ok().map(|_| {
                let ms: u64 = std::env::var("KASATERM_TEST_POPOUT_MS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(6000);
                Instant::now() + std::time::Duration::from_millis(ms)
            })
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let path = std::env::var("KASATERM_TEST_POPOUT").unwrap_or_default();
        eprintln!("[auxpopout] pop out {path}");
        self.popout_file_window(std::path::PathBuf::from(path), event_loop);
        // Arm the aux surface capture (main autocapture only reaches the main
        // window). +1500ms so the new window has rendered a full frame first.
        let cap = std::env::var("KASATERM_TEST_POPOUT_CAP").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("auxwin-popout.png")
                .to_string_lossy()
                .into_owned()
        });
        if let Some(a) = self.aux_windows.last_mut() {
            a.pending_capture =
                Some((Instant::now() + std::time::Duration::from_millis(1500), cap));
        }
    }

    /// [임시·검증용] Headless 스크롤 주입: `KASATERM_AUTOWHEEL_MS` 후 active pane
    /// 본문 중앙에 커서를 놓고 휠 up 을 `KASATERM_AUTOWHEEL`(기본 10) 노치 보낸다.
    /// mouse-tracking TUI(claude)면 SGR 로 그 pane 에 전달돼 실제 스크롤 경로를 밟아
    /// sticky prompt 를 재현한다 — sticky pill 감지/표시를 헤드리스로 확인하려는 용도.
    pub(crate) fn run_pending_autowheel(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWHEEL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let n: usize = std::env::var("KASATERM_AUTOWHEEL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        self.cursor_px = (
            pad + cols as f32 * self.cell.w / 2.0,
            TITLE_HEIGHT + rows as f32 * self.cell.h / 2.0,
        );
        eprintln!("[autowheel] scroll up {n} notches, cursor=({:.0},{:.0})",
            self.cursor_px.0, self.cursor_px.1);
        for _ in 0..n {
            self.handle_wheel(winit::event::MouseScrollDelta::LineDelta(0.0, 1.0));
        }
    }

    /// Arm any due aux-window self-capture: set its gpu `capture_next` and wake
    /// the window so the next frame reads it back. Mirrors the main
    /// `pending_capture` drain but per aux window (each owns its own surface).
    pub(crate) fn drain_aux_captures(&mut self) {
        let now = Instant::now();
        for a in self.aux_windows.iter_mut() {
            if let Some((at, _)) = a.pending_capture.as_ref() {
                if now >= *at {
                    let (_, path) = a.pending_capture.take().unwrap();
                    eprintln!("[auxpopout] capture → {path}");
                    a.gpu.capture_next = Some(path);
                    a.window.request_redraw();
                }
            }
        }
    }
}
