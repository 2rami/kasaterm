//! 자동 테스트 하네스 — env 기반 auto-split/window/toggle/drag/tabs + schedule 타이머.
use super::*;

/// md 스크립트에 아직 실행할 단계가 남았는지. `about_to_wait` 이 이걸 보고
/// 프레임을 펌프한다 — 자세한 사정은 `run_pending_automdscript` 참고.
static MDSCRIPT_LEFT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `KASATERM_AUTOPANEMERGE` 예약 슬롯 — (발사 시각, 대상 leaf).
static AUTO_MERGE: std::sync::OnceLock<std::sync::Mutex<Option<(Instant, String)>>> =
    std::sync::OnceLock::new();

fn auto_merge_slot() -> &'static std::sync::Mutex<Option<(Instant, String)>> {
    AUTO_MERGE.get_or_init(|| std::sync::Mutex::new(None))
}

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
    /// `KASATERM_AUTOCURSOR="x,y"` (+ `_MS`) — 커서를 그 논리 좌표에 놓는다.
    ///
    /// hover 는 정적 캡처로 볼 방법이 없다. 들림도 손가락 커서도 커서가 그 위에
    /// 있을 때만 생기는데, 헤드리스는 마우스를 못 움직여 "hover 를 넣었다" 는
    /// 주장이 눈으로 확인되지 않은 채 남는다. 캡처 직전에 커서만 옮겨 두면
    /// 그 프레임이 곧 hover 스크린샷이 된다.
    pub(crate) fn run_pending_autocursor(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, f32, f32)>> = OnceLock::new();
        static MOVED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let spec = std::env::var("KASATERM_AUTOCURSOR").ok()?;
            let (xs, ys) = spec.split_once(',')?;
            let ms: u64 = std::env::var("KASATERM_AUTOCURSOR_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6000);
            Some((
                Instant::now() + std::time::Duration::from_millis(ms),
                xs.trim().parse().ok()?,
                ys.trim().parse().ok()?,
            ))
        });
        let Some((due, x, y)) = *due else { return };
        if Instant::now() < due || MOVED.swap(true, Ordering::Relaxed) {
            return;
        }
        self.cursor_px = (x, y);
        self.chrome_dirty = true;
        eprintln!("[autocursor] ({x:.0},{y:.0})");
    }
    /// `KASATERM_AUTOEXPANDCLICK="<방idx>"` (+ `_MS`) — 그 방의 **펼치기 버튼**을
    /// 진짜로 누른다. `"2:body"` 는 같은 카드의 이름줄, `"2:dots"` 는 버튼 바로
    /// 오른쪽 상태 점 자리 — 둘 다 방 전환으로 흘러야 하는 곳이다.
    ///
    /// 상태를 직접 세우는 `AUTOEXPAND` 와 갈리는 건 좌표 판정을 지난다는 점이다.
    /// 버튼과 전환이 한 카드 안에서 갈리므로, 정작 검증해야 할 것이 그 갈림
    /// 자체다 — 예전엔 클릭 쪽이 "아랫줄 오른쪽 100px" 라는 자기 공식을 갖고 있어
    /// 눈에 보이는 삼각형보다 훨씬 넓은 구역이 전환을 삼켰다.
    pub(crate) fn run_pending_autoexpandclick(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, usize, u8)>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let spec = std::env::var("KASATERM_AUTOEXPANDCLICK").ok()?;
            let (idx, rest) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
            let ms: u64 = std::env::var("KASATERM_AUTOEXPANDCLICK_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000);
            Some((
                Instant::now() + std::time::Duration::from_millis(ms),
                idx.trim().parse().ok()?,
                match rest.trim() {
                    "body" => 1,
                    "dots" => 2,
                    _ => 0,
                },
            ))
        });
        let Some((due, idx, spot)) = *due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(tab) = self.window_tab_rects.iter().find(|(i, _)| *i == idx).map(|(_, r)| *r)
        else {
            eprintln!("[autoexpandclick] 방 {idx} 없음");
            return;
        };
        let btn = self.window_expand_rect(idx, tab);
        let (x, y) = match (spot, btn) {
            (1, _) => (tab.0 + 40.0, tab.1 + 14.0),
            // 배지 **왼쪽** 여백 — 아랫줄에서 드래그를 시작할 수 있는 자리다.
            // 오른쪽은 배지가 카드 끝에 붙어 있어 카드 밖으로 나간다(실측 handled=false).
            (2, Some(r)) => (r.0 - 10.0, r.1 + r.3 / 2.0),
            (_, Some(r)) => (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0),
            _ => {
                eprintln!("[autoexpandclick] 방 {idx} 는 pane 이 하나라 버튼이 없음");
                return;
            }
        };
        let before = self.active_window;
        let handled = self.window_strip_click(x, y);
        // 드래그 장전 여부까지 찍는다 — 버튼을 카드에서 도려내는 변경은 그 자리의
        // tear-off 를 조용히 죽일 수 있고(빌드도 클릭도 멀쩡하다), 신호가 여기뿐이다.
        eprintln!(
            "[autoexpandclick] ({x:.0},{y:.0}) handled={handled} 활성 {before}->{} 펼침={:?} 드래그장전={}",
            self.active_window,
            self.expanded_windows,
            self.win_tab_drag.is_some()
        );
        // 펼침 모션 프레임은 **클릭 기준**으로 잡아야 한다. 시작 기준
        // `AUTOCAPTURE_MS` 로는 못 잡는다 — 이 클릭 자체가 이벤트 루프가 깨어날 때
        // 나가서, 예약보다 한참 늦게 발화한다(실측: 캡처가 먼저 찍혀 네 장 모두
        // 접힌 그림이 나왔다). `_CAP` 에 경로, `_CAP_MS` 에 클릭 후 ms 를 콤마로.
        //
        // ⚠️ 여러 장을 걸 때는 **간격을 readback 보다 넓게**. 캡처는 `capture_next`
        // 한 칸을 거쳐 다음 렌더에 찍히는데 그 전에 다음 만기가 오면 앞엣것을
        // 덮어써 파일이 조용히 빈다(실측: 30·70·120·300 중 1·4 번만 남았다).
        // 0.16초짜리 이 모션은 오프셋을 바꿔 가며 한 실행에 한 장이 확실하다.
        let Ok(path) = std::env::var("KASATERM_AUTOEXPANDCLICK_CAP") else { return };
        let offs = std::env::var("KASATERM_AUTOEXPANDCLICK_CAP_MS")
            .unwrap_or_else(|_| "40,90,140,260".into());
        let now = Instant::now();
        for (i, ms) in offs.split(',').filter_map(|s| s.trim().parse::<u64>().ok()).enumerate() {
            let p = match path.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}-{}.{ext}", i + 1),
                None => format!("{path}-{}", i + 1),
            };
            self.pending_capture.push((now + std::time::Duration::from_millis(ms), p));
        }
    }
    /// `KASATERM_AUTOROWDRAG="<src줄>:<dst줄>[:before]"` (+ `_MS`) — 사이드바
    /// 목록의 src 번째 줄을 잡아 dst 번째 줄 위(`before`)나 아래에 떨어뜨린다.
    ///
    /// 누르기는 진짜 클릭 판정(`window_strip_click`)을 지나고, 떨어질 자리는
    /// handler 와 같은 규칙(대상 줄의 위/아래 절반)으로 잡는다. 확인할 건 "옮겼다"가
    /// 아니라 **아무것도 잃지 않았나**다 — pane 이동은 트리에서 leaf 를 떼어 다른
    /// 트리에 붙이는 일이라, 어긋나면 캡처는 멀쩡한데 pane 하나가 조용히 사라진다.
    pub(crate) fn run_pending_autorowdrag(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, usize, usize, bool)>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let spec = std::env::var("KASATERM_AUTOROWDRAG").ok()?;
            let mut it = spec.split(':');
            let src: usize = it.next()?.trim().parse().ok()?;
            let dst: usize = it.next()?.trim().parse().ok()?;
            let before = it.next().map(|s| s.trim() == "before").unwrap_or(false);
            let ms: u64 = std::env::var("KASATERM_AUTOROWDRAG_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), src, dst, before))
        });
        let Some((due, src, dst, before)) = *due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        let rows = self.sidebar_row_rects.clone();
        let Some((_, sid, sr)) = rows.get(src) else {
            eprintln!("[autorowdrag] 줄 {src} 없음 (총 {})", rows.len());
            return;
        };
        // dst 가 줄 범위를 넘으면 **방 카드**에 떨어뜨린 것으로 본다(넘긴 만큼이 방
        // 인덱스) — pane 하나짜리 방은 목록에 줄이 없어 이 경로로만 닿는다.
        let did = match rows.get(dst) {
            Some((_, id, _)) => id.clone(),
            None => {
                let wi = dst - rows.len();
                match self.window_leaves(wi).into_iter().last() {
                    Some(id) => id,
                    None => {
                        eprintln!("[autorowdrag] 방 {wi} 가 비었음");
                        return;
                    }
                }
            }
        };
        let did = &did;
        let all = |s: &Self| -> Vec<String> {
            (0..s.windows.len()).flat_map(|i| s.window_leaves(i)).collect()
        };
        let before_leaves = all(self);
        self.window_strip_click(sr.0 + sr.2 / 2.0, sr.1 + sr.3 / 2.0);
        let armed = self.sidebar_row_drag.is_some();
        if let Some(d) = self.sidebar_row_drag.as_mut() {
            d.active = true;
            d.target = Some((did.clone(), before));
        }
        let zone = if before { crate::DropZone::Up } else { crate::DropZone::Down };
        let (sid, did) = (sid.clone(), did.clone());
        self.move_pane(&sid, &did, zone);
        self.sidebar_row_drag = None;
        self.render_frame();
        let after = all(self);
        eprintln!(
            "[autorowdrag] {sid} → {did} ({}) 장전={armed} leaves {}개→{}개 {:?}",
            if before { "위" } else { "아래" },
            before_leaves.len(),
            after.len(),
            after
        );
        eprintln!("[autorowdrag] 기대: 장전=true · leaves 수 그대로 · 모든 pane 살아 있음");
    }
    /// `KASATERM_AUTOTHEME="<키>"` (+ `_MS`) — 그 시각에 테마를 갈아 끼운다.
    ///
    /// 전환 디졸브는 0.4초짜리라 손으로는 중간을 못 잡는다. 바뀌는 시각을 못박아
    /// 두면 `AUTOCAPTURE_MS` 를 그 뒤 몇십 ms 에 붙여 원하는 진행도의 한 장을
    /// 정확히 찍을 수 있다(콤마로 여러 시각을 주면 연속 프레임).
    pub(crate) fn run_pending_autotheme(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, String)>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let key = std::env::var("KASATERM_AUTOTHEME").ok()?;
            let ms: u64 = std::env::var("KASATERM_AUTOTHEME_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), key))
        });
        let Some((due, key)) = due.as_ref() else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        self.begin_theme_fx();
        crate::theme::set_theme(key);
        self.repaint_all();
        eprintln!("[autotheme] → {key}");
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
    /// Headless 방 재배치 repro: `KASATERM_AUTOWINREORDER_MS` 뒤에 방 셋을 만들어
    /// 이름·알림을 심고, 가운데 탭을 **실제 hit-test 경로**(`window_strip_click`)로
    /// 잡아 맨 뒤로 끌어 놓는다.
    ///
    /// 여기서 깨지기 쉬운 건 순서가 아니라 신원이다 — 활성 방의 트리는 슬롯이 아니라
    /// `pty_layout` 에 얹혀 있고, 이름·알림은 인덱스가 키다. 그래서 옮기기 전후로 각
    /// 방의 leaf 수와 이름을 같이 찍는다. leaf 가 0 이면 그 방의 내용이 증발한
    /// 것이고, 이름이 어긋나면 남의 이름을 단 것이다 — 캡처로는 둘 다 멀쩡해 보인다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autowinreorder(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWINREORDER_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let dump = |app: &App, tag: &str| {
            let rows: Vec<String> = (0..app.windows.len())
                .map(|i| {
                    let layout = if i == app.active_window {
                        app.pty_layout.as_ref()
                    } else {
                        app.windows[i].as_ref()
                    };
                    format!(
                        "{i}{}:{} leaves={}{}{}",
                        if i == app.active_window { "*" } else { "" },
                        app.window_name_override.get(&i).map(|s| s.as_str()).unwrap_or("-"),
                        layout.map(|l| l.leaves().len()).unwrap_or(0),
                        if app.window_alert.contains(&i) { " alert" } else { "" },
                        if app.window_is_undocked(i) { " 밖" } else { "" },
                    )
                })
                .collect();
            eprintln!("[autowinreorder] {tag}: {}", rows.join(" | "));
        };
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        while self.windows.len() < 3 {
            self.new_window();
        }
        for (i, name) in ["A", "B", "C"].iter().enumerate() {
            self.window_name_override.insert(i, name.to_string());
        }
        // 알림은 안 잡을 방(2번)에 건다 — 잡은 방은 press 가 전환하면서 지운다.
        self.window_alert.insert(2);
        self.window_labels_at = None;
        self.render_frame();
        dump(self, "before");
        let Some((_, r)) = self.window_tab_rects.get(1).copied() else {
            eprintln!("[autowinreorder] 탭 rect 없음 — 사이드바 창 탭이 안 그려졌다");
            return;
        };
        let handled = self.window_strip_click(r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        let armed = self.win_tab_drag.as_ref().map(|d| d.from);
        // 마지막 탭 아래까지 끌었다고 친다(문턱 통과 + 삽입 슬롯 = 방 개수).
        let end = self.windows.len();
        if let Some(d) = self.win_tab_drag.as_mut() {
            d.active = true;
            d.target = end;
        }
        // HOLD 면 놓지 않고 잡은 채로 둔다 — `AUTOCAPTURE_MS` 를 뒤에 붙여 삽입선이
        // 그려지는 프레임을 잡기 위한 것(놓고 나면 선은 사라져 캡처할 수 없다).
        if std::env::var("KASATERM_AUTOWINREORDER_HOLD").is_ok() {
            self.chrome_dirty = true;
            eprintln!("[autowinreorder] hold — 잡은 채로 유지, 삽입선 프레임 대기");
            return;
        }
        if let Some(d) = self.win_tab_drag.take() {
            self.reorder_window(d.from, d.target);
        }
        self.refresh_window_labels();
        eprintln!("[autowinreorder] press handled={handled} armed_from={armed:?} target={end}");
        dump(self, "after ");
        eprintln!(
            "[autowinreorder] 기대: A,C,B / 잡은 B 가 활성인 채 맨 뒤 / alert 는 C 를 따라 1번 / 모든 leaves>0"
        );
    }
    /// Headless 닫기→되살리기 repro: `KASATERM_AUTOCLOSEREOPEN_MS` 뒤에 pane 을 쪼갠 뒤
    /// 하나를 닫고, 되살리기 스택에 남았는지 찍고, 다시 되살린다.
    ///
    /// "새 셸이 하나 뜬다"로는 되살렸는지 알 수 없다 — 원래 자리·cwd·대화를 되찾아야
    /// 되살린 것이다. 그래서 닫기 전후 leaf 목록과 스택 내용(어느 pane·어느 폴더)을 같이
    /// 찍는다. `_HOLD=1` 이면 되살리지 않고 멈춘다(인포의 대기 줄을 캡처하기 위한 것).
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoclosereopen(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOCLOSEREOPEN_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let leaves = |app: &App| -> Vec<String> {
            app.pty_layout
                .as_ref()
                .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        };
        let stack = |app: &App| -> Vec<String> {
            app.closed_panes
                .iter()
                .map(|c| {
                    format!(
                        "{}({}{})",
                        c.pane_id,
                        c.folder,
                        if c.alive { ",살아있음" } else { ",죽음" }
                    )
                })
                .collect()
        };
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        self.render_frame();
        let before = leaves(self);
        let Some(victim) = before.last().cloned() else { return };
        eprintln!("[autoclosereopen] before: leaves={before:?}");
        self.close_pane(&victim);
        self.render_frame();
        eprintln!(
            "[autoclosereopen] {victim} 닫음 → leaves={:?} 스택={:?} PTY생존={}",
            leaves(self),
            stack(self),
            self.pty.contains_key(&victim)
        );
        if std::env::var("KASATERM_AUTOCLOSEREOPEN_HOLD").is_ok() {
            // 되살리기는 Info 섹션이 맡으므로 hold 는 그 화면에서 멈춘다 — 하단바가
            // 0 이라는 것만 찍고 끝내면 "되살릴 길이 사라진 것"과 구분이 안 된다.
            if !self.git.col_visible {
                self.toggle_git_col();
            }
            self.info.tab = crate::state::SideTab::Info;
            self.render_frame();
            eprintln!(
                "[autoclosereopen] hold — 하단바 칩={:?} 예약={} · Info 되살리기 줄={:?}",
                self.dock_chip_rects,
                self.dock_reserve_h(),
                self.info.closed_rects
            );
            return;
        }
        self.reopen_closed_pane();
        self.render_frame();
        let after = leaves(self);
        eprintln!(
            "[autoclosereopen] 되살린 뒤: leaves={:?} 스택={:?} 같은id복귀={}",
            after,
            stack(self),
            after.iter().any(|l| *l == victim)
        );
        // × 경로 — 다시 닫고 이번엔 되살리는 대신 끈다. 여기서만 프로세스가 죽어야
        // 한다. "끄기전=true, 끈뒤=false" 가 아니면 × 가 목록만 지우고 셸을 남긴
        // 것이고, 그건 닫을수록 프로세스가 쌓인다는 뜻이다.
        self.close_pane(&victim);
        self.render_frame();
        let before_kill = self.pty.contains_key(&victim);
        let last = self.closed_panes.len().saturating_sub(1);
        self.discard_closed_pane_at(last);
        self.render_frame();
        eprintln!(
            "[autoclosereopen] × 로 끔 → 끄기전PTY={before_kill} 끈뒤PTY={} 스택={:?}",
            self.pty.contains_key(&victim),
            stack(self)
        );
        eprintln!(
            "[autoclosereopen] 기대: 닫아도 PTY생존=true(죽이지 않는다) · 되살리면 같은id복귀=true(새로 띄우는 게 아니라 다시 붙인다) · × 만 끄기전true→끈뒤false"
        );
    }

    /// Headless 방 꺼내기 repro: `KASATERM_AUTOWINUNDOCK_MS` 뒤에 방 둘을 만들어
    /// 하나를 pane 셋짜리로 쪼갠 다음, 그 방을 통째로 별도 창으로 꺼낸다.
    ///
    /// 확인할 건 "창이 떴다"가 아니라 **아무것도 잃지 않았나**다. 트리를 옮기지 않는
    /// 설계라 방 목록은 줄지 않아야 하고, 메인은 남은 방으로 옮겨가야 하고, 꺼낸 방의
    /// leaf 셋이 별도 창에서 rect 셋으로 서야 한다. 하나라도 어긋나면 캡처는 멀쩡해
    /// 보이는데 pane 이 조용히 사라진 상태다 — 그래서 숫자로 찍는다.
    /// `_DOCK=1` 이면 곧바로 되돌려 방이 메인으로 무사히 돌아오는지까지 본다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autowinundock(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWINUNDOCK_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let dump = |app: &App, tag: &str| {
            let rooms: Vec<String> = (0..app.windows.len())
                .map(|i| {
                    let layout = if i == app.active_window {
                        app.pty_layout.as_ref()
                    } else {
                        app.windows[i].as_ref()
                    };
                    format!(
                        "{i}{}: leaves={}{}",
                        if i == app.active_window { "*" } else { "" },
                        layout.map(|l| l.leaves().len()).unwrap_or(0),
                        if app.window_is_undocked(i) { " 밖" } else { "" },
                    )
                })
                .collect();
            eprintln!(
                "[autowinundock] {tag}: windows={} aux={} | {}",
                app.windows.len(),
                app.aux_windows.len(),
                rooms.join(" | ")
            );
        };
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        while self.windows.len() < 2 {
            self.new_window();
        }
        // 꺼낼 방을 pane 셋으로 — 한 개짜리로는 "pane 들이 제 자리에 선다"를 못 본다.
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        let _ = self.split_active_pane(kasa_pty::SplitDir::Vertical);
        self.window_labels_at = None;
        self.render_frame();
        let target = self.active_window;
        let before = self.pty_layout.as_ref().map(|l| l.leaves().len()).unwrap_or(0);
        dump(self, "before");
        self.undock_window_room(target, event_loop, None);
        dump(self, "after ");
        let aux_idx = self.aux_windows.iter().position(|a| a.room_window() == Some(target));
        let rects = aux_idx.map(|i| self.room_leaf_rects(i)).unwrap_or_default();
        eprintln!(
            "[autowinundock] 방 {target}: 꺼내기 전 leaf={before} → 별도 창 rect={} {:?}",
            rects.len(),
            rects
                .iter()
                .map(|(p, x, y, w, h)| format!("{p}@{x},{y} {w}x{h}"))
                .collect::<Vec<_>>()
        );
        // 창이 떴어도 macOS 가 탭으로 합쳐 버렸으면 「별도 창」이 아니다. mode 2 =
        // Disallowed, 탭수 0 = 어디에도 안 묶임. 하나라도 어긋나면 드래그로 떼는
        // 순간 형제까지 딸려 나온다(거노).
        #[cfg(target_os = "macos")]
        {
            let main = self.window.as_ref().map(|w| crate::auxwin::tabbing_probe(w));
            let aux = aux_idx.map(|i| crate::auxwin::tabbing_probe(&self.aux_windows[i].window));
            eprintln!("[autowinundock] 탭(mode,묶인수): 메인={main:?} 꺼낸창={aux:?} · 기대 (2,0)");
        }
        eprintln!(
            "[autowinundock] 기대: windows 그대로 · 활성은 남은 방 · 꺼낸 방에 「밖」 · rect 수 = 꺼내기 전 leaf 수"
        );
        if let Ok(mode) = std::env::var("KASATERM_AUTOWINUNDOCK_DOCK") {
            // `click` 은 사이드바 빈 슬롯의 되돌리기 버튼을 실제로 눌러 본다 —
            // `dock_window_room` 직접 호출은 aux 인덱스로 말하고 사이드바는 방
            // 인덱스로 말해서, 둘이 어긋나면 엉뚱한 창이 돌아온다.
            if mode == "click" {
                self.render_frame();
                let hit = self.window_dock_rects.first().copied();
                eprintln!("[autowinundock] 되돌리기 버튼={hit:?}");
                if let Some((_, r)) = hit {
                    let handled = self.window_strip_click(r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    eprintln!("[autowinundock] 버튼 클릭 handled={handled}");
                }
            } else if let Some(i) = aux_idx {
                self.dock_window_room(i);
            }
            self.render_frame();
            dump(self, "docked");
            eprintln!("[autowinundock] 기대(되돌린 뒤): aux=0 · 그 방이 다시 활성 · leaves 그대로");
            return;
        }
        let cap = std::env::var("KASATERM_AUTOWINUNDOCK_CAP").unwrap_or_else(|_| {
            std::env::temp_dir().join("undock-room.png").to_string_lossy().into_owned()
        });
        if let Some(a) = aux_idx.and_then(|i| self.aux_windows.get_mut(i)) {
            a.pending_capture =
                Some((Instant::now() + std::time::Duration::from_millis(2500), cap));
        }
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
        self.log_window_placement(&format!("이동#{i} 전"));
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
            self.log_window_placement("정착 후");
        }
    }
    /// 저장될 창 좌표가 **복원 때 살아남는지**를 그 자리에서 판정해 찍는다.
    /// 저장은 조용히 성공하고 복원만 조용히 실패하므로, 둘을 따로 보면
    /// "위치가 왜 안 돌아오지" 를 영영 못 잡는다 — `resumed` 의 on_screen
    /// 판정을 그대로 재현해 같은 줄에 놓는 것이 요점이다.
    pub(crate) fn log_window_placement(&mut self, tag: &str) {
        let Some(w) = self.window.clone() else { return };
        let Ok(p) = w.outer_position() else { return };
        let (px, py) = (p.x as f64, p.y as f64);
        let mut restorable = false;
        for m in w.available_monitors() {
            let mp = m.position();
            let ms = m.size();
            let ok = px >= mp.x as f64
                && px < (mp.x as f64 + ms.width as f64 - 60.0)
                && py >= mp.y as f64
                && py < (mp.y as f64 + ms.height as f64 - 60.0);
            eprintln!(
                "[winpos]   모니터 @({},{}) {}x{} sf={} → {}",
                mp.x,
                mp.y,
                ms.width,
                ms.height,
                m.scale_factor(),
                if ok { "통과" } else { "탈락" }
            );
            restorable |= ok;
        }
        eprintln!(
            "[winpos] {tag}: outer=({px},{py}) inner={}x{} sf={} → 복원 {}",
            w.inner_size().width,
            w.inner_size().height,
            w.scale_factor(),
            if restorable { "됨" } else { "★버려짐★" }
        );
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
            self.info.ctx_menu = Some((cx, cy, crate::state::InfoTarget::Proc(pid)));
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
    /// ("appearance" / "shell" / "claude" / "students" / "feedback", default General), then arm
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
            "feedback" => SettingsCat::Feedback,
            _ => SettingsCat::General,
        };
        // 딥링크 검증: KASATERM_AUTOSETTINGS_STUDENT 로 특정 학생 선택 상태(=프사
        // 클릭 결과)를 헤드리스로 재현 — persona 편집기가 뜬 화면을 캡처한다.
        let student = std::env::var("KASATERM_AUTOSETTINGS_STUDENT")
            .ok()
            .filter(|s| !s.is_empty());
        eprintln!("[autosettings] open settings window cat={cat_env} student={student:?}");
        self.open_settings_window(event_loop, Some(cat), student);
        // 피드백 본문은 키 이벤트로만 채워지는데 헤드리스엔 그 경로가 없다 —
        // 버퍼를 직접 심어 wrap·캐럿·활성 버튼을 캡처로 본다.
        // KASATERM_AUTOFEEDBACK_SAVE=1 이면 저장까지 눌러, 캡처엔 비워진 폼과
        // 토스트가 남는다(파일이 실제로 떨어졌는지는 폴더로 확인).
        // 한글 조합 검증: KASATERM_AUTOSETTINGS_TYPE 의 자모를 계정 이름 필드에
        // 한 글자씩 먹여, 조합기가 완성 음절을 만드는지 낱자로 흘리는지 찍는다.
        // 실제 IME 없이 재현할 수 있는 건 macOS 가 OS IME 를 끄고 자모를 그대로
        // 받기 때문 — 그 경로가 곧 거노가 치는 경로다.
        // 페이지 아래쪽 항목은 창을 아무리 키워도 첫 화면에 안 들어온다 —
        // 스크롤 위치를 직접 심어 그 자리를 캡처한다(휠 이벤트는 aux 창으로
        // 안 간다).
        if let Ok(s) = std::env::var("KASATERM_AUTOSETTINGS_SCROLL") {
            if let Ok(v) = s.parse::<f32>() {
                self.settings_scroll = v;
            }
        }
        // 배율/폰트를 흐트러뜨린 뒤 "1:1 로 되돌리기"가 둘 다 되돌리는지. 되돌린
        // 값이 맞아도 격자를 다시 안 재면 화면만 옛 크기로 남으므로 cells 도 찍는다.
        if std::env::var("KASATERM_AUTOSETTINGS_RESET").is_ok() {
            self.change_ui_zoom(0.3);
            self.font_size = 22.0;
            self.apply_effective_scale();
            eprintln!(
                "[autoreset] 흐트러뜨림: zoom={:.2} font={} cells={:?}",
                self.ui_zoom,
                self.font_size,
                self.window_cells()
            );
            self.settings_apply(crate::SettingsAction::ResetScale);
            eprintln!(
                "[autoreset] 되돌린 뒤: zoom={:.2} font={} cells={:?}",
                self.ui_zoom,
                self.font_size,
                self.window_cells()
            );
        }
        if let Ok(t) = std::env::var("KASATERM_AUTOSETTINGS_TYPE") {
            self.settings_input = Some(SettingsInput::ClaudeAccountLabel(0));
            self.settings_caret = 0;
            if let Some(a) = self.set_claude_accounts.first_mut() {
                a.label.clear();
            }
            for c in t.chars() {
                if !self.settings_hangul_char(c) {
                    self.settings_hangul_flush();
                    self.settings_insert_text(&c.to_string());
                }
            }
            let pre = self.hangul.preedit().unwrap_or_default();
            eprintln!(
                "[autotype] label={:?} caret={} preedit={pre:?}",
                self.set_claude_accounts.first().map(|a| a.label.clone()),
                self.settings_caret
            );
        }
        if let Ok(t) = std::env::var("KASATERM_AUTOFEEDBACK_TEXT") {
            self.feedback_caret = t.chars().count();
            self.feedback_body = t;
            self.settings_input = Some(SettingsInput::FeedbackBody);
            if std::env::var("KASATERM_AUTOFEEDBACK_SAVE").is_ok_and(|v| v == "1") {
                self.save_feedback();
            }
        }
        // Aux capture (main autocapture only reaches the main window). +1500ms so
        // the new window renders a full frame first. 화면에 든 값이 subprocess 를
        // 기다리는 자리(계정 슬롯의 `claude auth status`)면 한 프레임으로는 부족해
        // 늘 빈칸만 찍힌다 — `_CAP_MS` 로 그 지연을 연다.
        let cap = std::env::var("KASATERM_AUTOSETTINGS_CAP").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("settings-window.png")
                .to_string_lossy()
                .into_owned()
        });
        let delay = std::env::var("KASATERM_AUTOSETTINGS_CAP_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1500);
        if let Some(a) = self.settings_window_idx().and_then(|i| self.aux_windows.get_mut(i)) {
            a.pending_capture =
                Some((Instant::now() + std::time::Duration::from_millis(delay), cap));
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
                let px: f32 = v.parse().unwrap_or(0.0);
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
            // 렌더 뷰 선택을 문서 좌표로 직접 세운다 — `sel:<ax>,<ay>,<bx>,<by>`.
            // 마우스 드래그는 밖에서 만들 수 없어(winit) 상태를 세워 띠 렌더와
            // 복사 추출만 확인한다. `selcopy` 는 그 결과를 로그로 찍는다(클립보드는
            // 건드리지 않는다 — 검증이 사용자 클립보드를 덮으면 안 된다).
            Some(("sel", v)) => {
                let n: Vec<f32> =
                    v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                if n.len() != 4 {
                    eprintln!("[mdscript] sel: 좌표 4개 필요(ax,ay,bx,by)");
                    return;
                }
                self.md_render_sel = Some(crate::MdRenderSel {
                    pane: id.clone(),
                    anchor: (n[0], n[1]),
                    end: (n[2], n[3]),
                    dragging: false,
                });
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(&id) {
                        pane.dirty = true;
                    }
                }
                eprintln!("[mdscript] sel=({},{})-({},{})", n[0], n[1], n[2], n[3]);
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
                // 실물 press 경로와 같은 함수를 쓴다 — 캐럿·드래그 앵커·연타
                // 선택의 순서가 계약이라, 여기서 md_click_caret 만 부르면
                // 더블클릭이 재현되지 않아 검증이 실물과 어긋난다. 같은
                // 좌표로 짧은 간격(`_STEP_MS` 450 이하)에 두 번 주면 단어
                // 선택, 세 번이면 줄 선택이 걸린다.
                // 실물 press 와 같은 순서: 접기 삼각형을 먼저 본다. 이걸 빼면
                // 하네스가 삼각형 클릭을 캐럿 클릭으로 재현해 검증이 거짓말을 한다.
                if self.md_fold_click(&id, bx + dx, by + dy) {
                    let f = self.ws.lock().ok().and_then(|w| {
                        w.panes.get(&id).and_then(|p| p.markdown()).map(|m| m.folds.clone())
                    });
                    eprintln!("[mdscript] fold click=({dx},{dy}) folds={f:?}");
                    self.wake_after_mdstep();
                    return;
                }
                let clicks = self.md_press_caret(&id, bx + dx, by + dy);
                let at = self.ws.lock().ok().and_then(|w| {
                    w.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .map(|m| (m.cur_line, m.cur_col, m.sel_anchor, m.selected_text()))
                });
                eprintln!("[mdscript] click=({dx},{dy}) clicks={clicks} caret={at:?}");
            }
            // 누른 채로 끌고 간 자리 — `drag:<dx>,<dy>`(앞에 `click:` 이 앵커를
            // 세워 둔 뒤에 쓴다). CursorMoved 의 드래그 갈래가 부르는 것과 같은
            // 함수라, 접힌 줄을 가로지르는 선택 밴드도 실물과 같은 경로로 선다.
            Some(("drag", v)) => {
                let (dx, dy) = v.split_once(',').unwrap_or((v, "0"));
                let (dx, dy): (f32, f32) =
                    (dx.trim().parse().unwrap_or(0.0), dy.trim().parse().unwrap_or(0.0));
                let Some(&(bx, by, _, _)) = self.md_body_rects.get(&id) else {
                    eprintln!("[mdscript] drag: 본문 박스 없음(raw 모드인지 확인)");
                    return;
                };
                self.md_click_caret(&id, bx + dx, by + dy);
                let at = self.ws.lock().ok().and_then(|w| {
                    w.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .map(|m| (m.cur_line, m.cur_col, m.sel_anchor))
                });
                eprintln!("[mdscript] drag=({dx},{dy}) caret={at:?}");
            }
            // Cmd+D — `occ:`. 첫 번은 캐럿 낱말, 그 뒤로는 다음 출현에 커서 추가.
            // Cmd+Opt+↑↓ 는 `vcaret:up|down`. 실물 키가 부르는 것과 같은 함수다.
            Some(("occ", _)) | Some(("vcaret", _)) => {
                let down = step.split_once(':').map(|(_, v)| v) != Some("up");
                let occ = step.starts_with("occ");
                let got = {
                    let mut ws = self.ws.lock().unwrap();
                    ws.panes.get_mut(&id).and_then(|p| {
                        p.dirty = true;
                        p.markdown_mut()
                    })
                    .map(|m| {
                        let ok =
                            if occ { m.select_next_occurrence() } else { m.add_caret_vert(down) };
                        (ok, m.carets())
                    })
                };
                eprintln!("[mdscript] {step} → {got:?}");
            }
            // 편집기에 글자를 넣어 본다 — `type:<문자열>`. 키 이벤트를 밖에서
            // 만들 수 없어(winit `KeyEvent`) 삽입 진입점을 직접 부른다.
            // 실타이핑의 비용 구조를 재려면 **한 단계에 한 글자**로 써야 한다
            // (`type:a|type:b|…`): 한 단계에 여러 글자를 넣으면 그 사이에
            // 프레임이 안 그려져 버퍼 재파싱이 한 번으로 접혀 버린다.
            Some(("type", v)) => {
                self.md_insert_into(&id, v);
                let at = self.ws.lock().ok().and_then(|w| {
                    w.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .map(|m| (m.cur_line, m.cur_col))
                });
                eprintln!("[mdscript] type={v:?} caret={at:?}");
            }
            // 자동완성 팝업을 캐럿 앞 낱말로 열어 본다 — `complete:`. 실물 키가
            // 부르는 것과 **같은 함수**를 직접 부른다(winit `KeyEvent` 를 밖에서
            // 못 만들어 타이핑으로는 팝업까지 갈 수 없다).
            Some(("complete", _)) => {
                let got = {
                    let mut ws = self.ws.lock().unwrap();
                    ws.panes.get_mut(&id).and_then(|p| {
                        p.dirty = true;
                        p.markdown_mut()
                    })
                    .map(|m| {
                        m.complete_refresh();
                        m.complete.as_ref().map(|c| (c.items.clone(), c.sel, c.from_col))
                    })
                };
                eprintln!("[mdscript] complete={got:?}");
                // 실물 키 경로가 하는 그대로 서버에도 물어 둔다. 응답은 틱에서
                // 받으므로 뒤에 `citems:` 단계를 두고 갈아끼워진 후보를 본다.
                self.lsp_complete_request(&id);
            }
            // 그 자리에 마우스를 멈춘 것으로 친다 — `hover:<dx>,<dy>`(본문 박스
            // 기준). 실제 커서를 밖에서 못 움직여서 상태를 직접 세우고, 멈춘
            // 시각을 과거로 둬 대기 시간을 건너뛴다. 답은 틱이 받으므로 뒤에
            // `tip:` 단계를 둔다.
            Some(("hover", v)) => {
                let (dx, dy) = v.split_once(',').unwrap_or((v, "0"));
                let (dx, dy): (f32, f32) =
                    (dx.trim().parse().unwrap_or(0.0), dy.trim().parse().unwrap_or(0.0));
                let Some(&(bx, by, _, _)) = self.md_body_rects.get(&id) else {
                    eprintln!("[mdscript] hover: 본문 박스 없음(raw 모드인지 확인)");
                    return;
                };
                let past = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::time::Instant::now);
                self.hover = Some(crate::HoverState {
                    at: (bx + dx, by + dy),
                    since: past,
                    req: None,
                    text: None,
                });
                self.lsp_hover_tick();
                eprintln!("[mdscript] hover=({dx},{dy}) 요청={}", self.hover.is_some());
            }
            // 지금 떠 있는 툴팁 글 — `tip:`.
            Some(("tip", _)) => {
                let t = self.hover.as_ref().and_then(|h| h.text.clone());
                eprintln!("[mdscript] tip={t:?}");
            }
            // 줄 접기(word wrap) 토글 — `wrap:`. Alt+Z 가 부르는 것과 같은
            // 상태를 직접 세운다(수정키 조합을 밖에서 만들 수 없다).
            Some(("wrap", _)) => {
                let on = {
                    let mut ws = self.ws.lock().unwrap();
                    ws.panes.get_mut(&id).and_then(|p| {
                        p.dirty = true;
                        p.markdown_mut()
                    })
                    .map(|m| {
                        m.wrap = !m.wrap;
                        if m.wrap {
                            m.h_scroll = 0.0;
                        }
                        m.wrap
                    })
                };
                eprintln!("[mdscript] wrap={on:?}");
            }
            // 정의로 뛴다 — `goto:`. Cmd+클릭이 부르는 것과 같은 함수를 직접
            // 부른다(수정키 상태를 밖에서 만들 수 없다). 응답은 틱이 받으므로
            // 뒤에 `caret:` 단계를 두고 옮겨진 자리를 본다.
            Some(("goto", _)) => {
                self.lsp_goto_request(&id);
                eprintln!("[mdscript] goto 요청");
            }
            // 지금 캐럿이 어느 파일 몇 줄인지 — `caret:`.
            Some(("caret", _)) => {
                let got = {
                    let ws = self.ws.lock().unwrap();
                    ws.active_pane
                        .as_ref()
                        .and_then(|a| ws.panes.get(a))
                        .and_then(|p| p.markdown())
                        .map(|m| {
                            (
                                m.doc.path.rsplit('/').next().unwrap_or("").to_string(),
                                m.cur_line,
                                m.cur_col,
                            )
                        })
                };
                eprintln!("[mdscript] caret={got:?}");
            }
            // 지금 팝업에 들어 있는 후보만 찍는다 — `citems:`. `complete:` 를 다시
            // 부르면 버퍼 낱말로 덮어써서 서버 답이 왔는지 알 수 없다.
            Some(("citems", _)) => {
                let got = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .and_then(|m| m.complete.as_ref())
                        .map(|c| (c.items.clone(), c.sel, c.lsp_req))
                };
                eprintln!("[mdscript] citems={got:?}");
            }
            // LSP 진단 확인 — `diags:`. rust-analyzer 의 첫 인덱싱은 수 초~수십 초라
            // 이 단계 앞에 넉넉한 `_STEP_MS` 를 두거나 여러 번 찍어야 한다.
            Some(("diags", _)) => {
                let path = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes.get(&id).and_then(|p| p.markdown()).map(|m| m.doc.path.clone())
                };
                match path {
                    Some(p) => {
                        let ds = self.lsp_diags(&p);
                        eprintln!(
                            "[mdscript] diags n={} {:?}",
                            ds.len(),
                            ds.iter()
                                .take(4)
                                .map(|d| (d.line, d.col, d.severity, d.message.clone()))
                                .collect::<Vec<_>>()
                        );
                    }
                    None => eprintln!("[mdscript] diags: 편집기 pane 이 아님"),
                }
            }
            // 선택 텍스트 확인. 클립보드 대신 로그로 찍는다(위 `sel:` 주석 참고).
            Some(("selcopy", _)) => {
                match self.md_render_selection_text() {
                    Some(t) => eprintln!("[mdscript] selcopy={t:?}"),
                    None => eprintln!("[mdscript] selcopy=<없음>"),
                }
            }
            // `[[이름]]` 링크를 눌러 본다 — `wiki:<이름>`. 클릭 좌표 대신 목적지를
            // 직접 넘긴다: 링크 글자의 화면 위치는 창 폭과 스크롤에 따라 움직여
            // 좌표로 짚으면 검증이 창 크기에 묶인다. 확인하려는 건 히트테스트가
            // 아니라 **어느 파일이 열리는가** 다(볼트가 주제 폴더로 갈라져 있다).
            Some(("wiki", v)) => {
                self.open_md_dest(&format!("wiki:{v}"));
                let opened = self.ws.lock().ok().map(|w| {
                    w.panes
                        .values()
                        .filter_map(|p| p.markdown().map(|m| m.doc.path.clone()))
                        .collect::<Vec<_>>()
                });
                eprintln!("[mdscript] wiki={v} 열린문서={opened:?}");
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
            // 한글 조합. `md_editor_input` 이 자모에 대해 하는 일과 **같은
            // 코드**(소유권 주장 → `md_feed_jamo`)를 탄다 — winit KeyEvent 를
            // 못 만들어 조합 경로만 검증 사각지대였던 걸 여기서 메운다.
            Some(("jamo", v)) => {
                for c in v.chars() {
                    self.ime_retarget(crate::ImeFocus::Editor(id.clone()));
                    let took = self.md_feed_jamo(c);
                    eprintln!(
                        "[mdscript] jamo {c} took={took} preedit={:?} focus={:?}",
                        self.preedit, self.ime_focus
                    );
                }
                self.md_ensure_caret_visible();
            }
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
                crate::auxwin::AuxWindowKind::Terminal { pane_id, .. } if *pane_id == pid)
        }) {
            a.pending_capture =
                Some((Instant::now() + std::time::Duration::from_millis(2500), cap));
        }
        // `_HIDE=1` 이면 이어서 접기→되살리기까지 본다. 접기는 창만 없애는 것이라
        // **PTY 가 살아 있는지**가 판정의 전부다 — 창 수만 세면 "접었다"와 "죽였다"가
        // 똑같아 보인다.
        if std::env::var("KASATERM_AUTOUNDOCK_HIDE").is_err() {
            return;
        }
        self.hide_aux_window(0);
        self.render_frame();
        eprintln!(
            "[autoundock] 접음 → aux={} 접힌목록={:?} 하단바예약={} PTY생존={}",
            self.aux_windows.len(),
            self.hidden_aux.iter().map(|h| h.label.clone()).collect::<Vec<_>>(),
            self.dock_reserve_h(),
            self.pty.contains_key(&pid)
        );
        eprintln!("[autoundock] 하단바 칩={:?}", self.dock_chip_rects);
        self.unhide_aux(0, event_loop);
        self.render_frame();
        eprintln!(
            "[autoundock] 되살림 → aux={} 접힌목록={} 하단바예약={} PTY생존={}",
            self.aux_windows.len(),
            self.hidden_aux.len(),
            self.dock_reserve_h(),
            self.pty.contains_key(&pid)
        );
        eprintln!(
            "[autoundock] 기대: 접으면 aux=0·예약=40·PTY생존=true / 되살리면 aux=1·예약=0·PTY생존=true"
        );
    }
    /// Headless 드래그-tear repro: `KASATERM_AUTOTEARDRAG_MS` 뒤에 활성 pane 의 탭
    /// 드래그를 세우고 커서를 창 **밖으로** 옮긴다 — 마우스를 놓지 않은 채 별도창이
    /// 떨어지는지, 그 창이 커서를 따라오는지, 놓았을 때 되꽂히지 않는지까지 본다.
    /// 상태를 손으로 세팅하지 않고 `CursorMoved`/`MouseInput` 을 그대로 `window_event`
    /// 에 흘려보내 handler 라우팅까지 태운다(automenuclick 과 같은 이유).
    /// autoundock 처럼 함수-로컬 static — struct App 은 건드리지 않는다.
    /// 단계 사이에 900ms 를 두는 이유: macOS 의 `set_outer_position` 은 윈도우
    /// 서버에 비동기로 전달돼, 같은 이벤트 루프 반복 안에서 `outer_position()` 을
    /// 다시 읽으면 **옮기기 전 값**이 나온다("따라오지 않는다"로 오판했던 자리).
    pub(crate) fn run_pending_autoteardrag(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        static POS1: AtomicI32 = AtomicI32::new(i32::MIN);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOTEARDRAG_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        if step > 3 || Instant::now() < *due + std::time::Duration::from_millis(900 * step as u64)
        {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let scale = self.effective_scale() as f64;
        let moved = |app: &mut Self, x: f32, y: f32| {
            app.window_event(
                event_loop,
                wid,
                WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(
                        x as f64 * scale,
                        y as f64 * scale,
                    ),
                },
            );
        };
        let dragged = self.tab_drag.as_ref().map(|d| d.pane.clone());
        match step {
            // 0) 메인 창을 화면보다 작게 줄인다. 창 밖 좌표가 **화면 안**이어야
            //    별도창이 커서 자리에 그대로 놓인다 — 화면 밖을 노리면 macOS 가
            //    두 요청을 같은 자리로 물려 "안 따라온다"로 오독된다.
            0 => {
                if let Some(w) = self.window.as_ref() {
                    let _ = w.request_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
                }
                STEP.store(1, Ordering::Relaxed);
            }
            // 1) 드래그를 세우고 커서를 창 밖으로 — 여기서 이미 뜯겨야 한다(놓기 전).
            1 => {
                // 뜯긴 뒤에도 메인 창에 뭐가 남아 있어야 리플로우를 볼 수 있다.
                if self.pty_layout.as_ref().map(|t| t.leaves().len()).unwrap_or(0) < 2 {
                    let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
                }
                let Some(pid) = self.ws.lock().unwrap().active_pane.clone() else { return };
                self.tab_drag = Some(TabDrag {
                    pane: pid.clone(),
                    from: 0,
                    start: (120.0, 10.0),
                    active: true,
                    target: 0,
                    drop_pane: pid.clone(),
                });
                moved(self, 1000.0, 120.0);
                let torn = self.torn_aux_window(&pid);
                if let Some(p) =
                    torn.and_then(|i| self.aux_windows[i].window.outer_position().ok())
                {
                    POS1.store(p.y, Ordering::Relaxed);
                }
                eprintln!(
                    "[autoteardrag] 1) pane={pid} win={:?} 놓기전뜯김={} 트리에남음={}",
                    self.logical_win_size(),
                    torn.is_some(),
                    self.pty_layout
                        .as_ref()
                        .map(|t| t.leaves().iter().any(|l| *l == pid))
                        .unwrap_or(false),
                );
                STEP.store(2, Ordering::Relaxed);
            }
            // 2) 커서를 더 내린다 — 창이 따라오는지.
            2 => {
                moved(self, 1000.0, 400.0);
                STEP.store(3, Ordering::Relaxed);
            }
            // 3) 옮겨진 자리를 읽고 놓는다 — 되꽂히면 안 된다.
            _ => {
                STEP.store(4, Ordering::Relaxed);
                let Some(pid) = dragged else {
                    eprintln!("[autoteardrag] FAIL — 드래그 상태가 사라졌다");
                    return;
                };
                let y2 = self
                    .torn_aux_window(&pid)
                    .and_then(|i| self.aux_windows[i].window.outer_position().ok())
                    .map(|p| p.y);
                let y1 = POS1.load(Ordering::Relaxed);
                let followed = matches!(y2, Some(y) if y1 != i32::MIN && y != y1);
                self.window_event(
                    event_loop,
                    wid,
                    WindowEvent::MouseInput {
                        device_id: DeviceId::dummy(),
                        state: ElementState::Released,
                        button: MouseButton::Left,
                    },
                );
                let still_torn = self.torn_aux_window(&pid).is_some();
                let back_in_tree = self
                    .pty_layout
                    .as_ref()
                    .map(|t| t.leaves().iter().any(|l| *l == pid))
                    .unwrap_or(false);
                eprintln!(
                    "[autoteardrag] 2) 따라옴={followed}(y {y1}→{y2:?}) 3) 놓은뒤유지={still_torn} 트리복귀={back_in_tree}"
                );
                // 뜯긴 창이 살아 있는 셸을 계속 그리는지는 눈으로만 확인된다.
                if let Some(cap) = std::env::var("KASATERM_AUTOTEARDRAG_CAP").ok() {
                    if let Some(i) = self.torn_aux_window(&pid) {
                        self.aux_windows[i].pending_capture = Some((
                            Instant::now() + std::time::Duration::from_millis(2000),
                            cap,
                        ));
                    }
                }
                eprintln!(
                    "[autoteardrag] {}",
                    if followed && still_torn && !back_in_tree { "PASS" } else { "FAIL" }
                );
            }
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
            eprintln!("[autoopen] open_file {}", p.display());
            // 사람 경로(`open_file_split`)가 아니라 미리보기 경로로 연다: "파일 열기"
            // 설정이 App/Terminal 이면 사람 경로는 파일을 외부 앱으로 넘겨버려,
            // 내장 뷰를 증명하려는 이 하네스가 아무것도 열지 못한다.
            self.open_file(p, None, true);
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
    /// 사이드바 방 펼치기 헤드리스 재현 — `KASATERM_AUTOEXPAND` 에 방 인덱스를
    /// 콤마로(`0,2`). 펼침은 클릭 손잡이가 유일한 입구라, 상태를 직접 세워야
    /// 목록의 배치·잘림·넘침을 캡처로 볼 수 있다. 방이 아직 없어도 인덱스만
    /// 담아 두면 나중에 생기는 방에 그대로 적용된다.
    /// `KASATERM_AUTOALERT="0,2"` — 그 방들에 "못 본 알림"을 세운다.
    ///
    /// 알림·대기 표시는 밖에서 일이 일어나야(claude 가 끝나거나 물어봐야) 켜지는데,
    /// 헤드리스에는 그 일이 없다. 상태만 세워 두면 캡처가 곧 그 표시의 스크린샷이
    /// 된다 — 색·자리·속도가 정말 갈리는지는 눈으로만 확인된다.
    pub(crate) fn arm_autoalert(&mut self) {
        let Ok(v) = std::env::var("KASATERM_AUTOALERT") else { return };
        for i in v.split(',').filter_map(|s| s.trim().parse::<usize>().ok()) {
            self.window_alert.insert(i);
        }
        eprintln!("[autoalert] {:?}", self.window_alert);
    }
    /// `KASATERM_AUTOWAIT="%2"` — 그 pane 을 "손을 기다리는 중"으로 세운다.
    ///
    /// 한 번 세우고 끝낼 수 없다. `refresh_pane_activity` 가 틱마다 transcript 를
    /// 다시 읽어 `pane_activity` 를 통째로 덮어쓰므로, 캡처가 뜰 즈음엔 세워 둔
    /// 상태가 이미 지워져 있다(실측: 띠가 한 장도 안 나왔다). 그래서 매 틱 덮는다.
    pub(crate) fn apply_autowait(&mut self) {
        use std::sync::OnceLock;
        static IDS: OnceLock<Vec<String>> = OnceLock::new();
        let ids = IDS.get_or_init(|| {
            std::env::var("KASATERM_AUTOWAIT")
                .map(|v| {
                    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                })
                .unwrap_or_default()
        });
        for id in ids {
            self.pane_activity.entry(id.clone()).or_default().status = "waiting".into();
        }
    }
    /// Headless 별도창 파일트리 repro: `KASATERM_AUTOAUXTREE_MS` 뒤에 첫 aux 창의
    /// 트리 패널을 연다(헤더 버튼 클릭은 그 창 좌표라 헤드리스 주입이 번거롭다).
    /// `KASATERM_AUTOUNDOCK_MS` 로 창을 먼저 띄워 두고 쓴다.
    ///
    /// 트리를 여는 것만으론 반쪽이다 — 진짜 확인할 것은 **셀이 트리만큼 밀렸는가**라,
    /// 열기 전후 cols 를 같이 찍는다. 원점만 밀고 cols 를 안 줄이면 오른쪽이 창 밖으로
    /// 나가는데, 그건 스크린샷에서 "글자가 좀 잘렸네"로 흘려보내기 쉽다.
    pub(crate) fn run_pending_autoauxtree(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOAUXTREE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if self.aux_windows.is_empty() {
            eprintln!("[autoauxtree] FAIL — 별도창이 없다(KASATERM_AUTOUNDOCK_MS 를 앞에 둬라)");
            return;
        }
        let cols_before = self
            .aux_windows
            .first()
            .and_then(|a| a.term_pane_id())
            .and_then(|p| self.pty.get(p))
            .map(|p| p.size().0);
        self.toggle_aux_tree(0);
        // 한 프레임 그려야 `tree_rows` 가 찬다 — 안 그리고 세면 항상 0 이라 "행이
        // 그려졌나"를 묻는 척만 하게 된다.
        self.aux_render(0);
        let (open, rows) = self
            .aux_windows
            .first()
            .map(|a| (a.tree_open, a.tree_rows.len()))
            .unwrap_or((false, 0));
        let cols_after = self
            .aux_windows
            .first()
            .and_then(|a| a.term_pane_id())
            .and_then(|p| self.pty.get(p))
            .map(|p| p.size().0);
        eprintln!(
            "[autoauxtree] 열림={open} 트리노드={} 그린줄={rows} cols {cols_before:?}→{cols_after:?}",
            self.file_tree.nodes.len()
        );
        let narrowed = matches!((cols_before, cols_after), (Some(b), Some(a)) if a < b);
        eprintln!(
            "[autoauxtree] {}",
            if open && rows > 0 && narrowed { "PASS" } else { "FAIL" }
        );
        if let Ok(cap) = std::env::var("KASATERM_AUTOAUXTREE_CAP") {
            if let Some(a) = self.aux_windows.first_mut() {
                a.pending_capture =
                    Some((Instant::now() + std::time::Duration::from_millis(1500), cap));
            }
        }
    }
    /// `KASATERM_AUTOUNREAD="%2"` — 그 pane 을 "끝났는데 아직 안 본" 상태로 세운다.
    /// 방 단위인 `KASATERM_AUTOALERT` 의 pane 판 — 완료 숨쉬기가 방 전체가 아니라
    /// **그 세션 줄** 에만 걸리는지 보려면 둘을 따로 세울 수 있어야 한다.
    ///
    /// autowait 과 같은 이유로 매 틱 다시 넣는다: `sync_dock_badge` 가 활성 pane 을
    /// 지우고 지나가므로 한 번 세워 두면 캡처 전에 사라질 수 있다.
    pub(crate) fn apply_autounread(&mut self) {
        use std::sync::OnceLock;
        static IDS: OnceLock<Vec<String>> = OnceLock::new();
        let ids = IDS.get_or_init(|| {
            std::env::var("KASATERM_AUTOUNREAD")
                .map(|v| {
                    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                })
                .unwrap_or_default()
        });
        for id in ids {
            self.unread_panes.insert(id.clone());
        }
    }
    pub(crate) fn arm_autoexpand(&mut self) {
        let Ok(v) = std::env::var("KASATERM_AUTOEXPAND") else { return };
        for i in v.split(',').filter_map(|s| s.trim().parse::<usize>().ok()) {
            self.expanded_windows.insert(i);
        }
        eprintln!("[autoexpand] {:?}", self.expanded_windows);
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
    /// 헤드리스 pane 병합 검증. `KASATERM_AUTOPANEMERGE="%N"`(빈값=첫 leaf) 이면
    /// 그 leaf 를 header_drag 로 집어 **형제의 본문 중앙**에 커서를 두고 라이브
    /// 프리뷰를 적용한 뒤, 릴리즈 핸들러와 똑같은 경로(`take_center_drop`)로 놓는다.
    /// 예약 상태를 `struct App` 필드가 아니라 모듈 static 에 두는 이유는 검증
    /// 전용 스캐폴딩이 병렬 작업의 충돌 핫스팟(App 필드 정의)을 늘리지 않게 하려는
    /// 것이다.
    pub(crate) fn arm_auto_pane_merge(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTOPANEMERGE") else { return };
        let ms: u64 = std::env::var("KASATERM_AUTOPANEMERGE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        *auto_merge_slot().lock().unwrap() =
            Some((Instant::now() + std::time::Duration::from_millis(ms), env));
        eprintln!("[panemerge] armed in {ms}ms");
    }
    pub(crate) fn run_pending_auto_pane_merge(&mut self) {
        let due = {
            let mut slot = auto_merge_slot().lock().unwrap();
            match slot.as_ref() {
                Some((t, _)) if Instant::now() >= *t => slot.take().map(|(_, w)| w),
                _ => None,
            }
        };
        let Some(want) = due else { return };
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().into_iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if leaves.len() < 2 {
            eprintln!("[panemerge] need 2+ panes, have {}", leaves.len());
            return;
        }
        let pane = if leaves.iter().any(|s| *s == want) { want } else { leaves[0].clone() };
        // carried pane 을 빼면 형제가 창을 통째로 채운다 — 그 중앙이 곧 Center 존.
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let win_w = cols as f32 * self.cell.w;
        let win_h = rows as f32 * self.cell.h;
        self.cursor_px = (pad + win_w / 2.0, TITLE_HEIGHT + win_h * 0.5);
        self.header_drag =
            Some(HeaderDrag { pane: pane.clone(), start: (0.0, 0.0), active: true, from_handle: false });
        self.update_live_drag();
        let hit = self.live_drag_hit(&pane);
        eprintln!("[panemerge] src={pane} cursor=({:.0},{:.0}) hit={hit:?} preview_leaves={:?}",
            self.cursor_px.0, self.cursor_px.1,
            self.pty_layout.as_ref().map(|l| l.leaves().len()));
        let dst = hit.as_ref().map(|(t, _)| t.clone());
        let before = dst.as_ref().and_then(|d| {
            self.ws.lock().ok().and_then(|w| w.panes.get(d).map(|p| p.tabs.len()))
        });
        let merged = self.take_center_drop(&pane);
        self.header_drag = None;
        let after = dst.as_ref().and_then(|d| {
            self.ws.lock().ok().and_then(|w| w.panes.get(d).map(|p| p.tabs.len()))
        });
        let src_gone = self
            .pty_layout
            .as_ref()
            .map(|l| !l.leaves().iter().any(|s| *s == pane))
            .unwrap_or(true);
        let routed = dst
            .as_ref()
            .map(|d| {
                self.ws
                    .lock()
                    .ok()
                    .map(|w| w.pid_to_pane.values().filter(|v| *v == d).count())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        eprintln!(
            "[panemerge] merged={merged} dst={dst:?} tabs={before:?}→{after:?} src_gone={src_gone} pids_routed_to_dst={routed} leaves={:?}",
            self.pty_layout.as_ref().map(|l| l.leaves().len())
        );
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
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
        // `KASATERM_FORCE_DRAG_AT=center` 면 중앙(병합 프리뷰) 자리에 park —
        // 소스가 그리드에서 빠지고 타깃에 "안에 넣기" 박스가 뜬 순간을 캡처한다.
        let fy = match std::env::var("KASATERM_FORCE_DRAG_AT").as_deref() {
            Ok("center") => 0.5,
            _ => 0.8,
        };
        self.cursor_px = (pad + win_w / 2.0, TITLE_HEIGHT + win_h * fy);
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
    /// 본문 중앙에 커서를 놓고 휠을 `KASATERM_AUTOWHEEL`(기본 10) 번 보낸다. 음수면
    /// 아래로 굴린다. `KASATERM_AUTOWHEEL_PX=<px>` 를 주면 노치(LineDelta) 대신 그
    /// 픽셀만큼의 트랙패드 델타로 보낸다 — 문서 뷰의 픽셀 스크롤 경로는 노치로는
    /// 밟히지 않아, 이것 없이는 헤드리스로 확인할 방법이 없다.
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
        let n: i32 = std::env::var("KASATERM_AUTOWHEEL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let px: Option<f32> = std::env::var("KASATERM_AUTOWHEEL_PX")
            .ok()
            .and_then(|s| s.parse().ok());
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        self.cursor_px = (
            pad + cols as f32 * self.cell.w / 2.0,
            TITLE_HEIGHT + rows as f32 * self.cell.h / 2.0,
        );
        let dir = if n < 0 { -1.0 } else { 1.0 };
        eprintln!(
            "[autowheel] {} ticks {} px_mode={px:?} cursor=({:.0},{:.0})",
            n.abs(),
            if n < 0 { "down" } else { "up" },
            self.cursor_px.0,
            self.cursor_px.1
        );
        let before = self.autowheel_md_scroll();
        for _ in 0..n.abs() {
            let delta = match px {
                Some(v) => winit::event::MouseScrollDelta::PixelDelta(
                    winit::dpi::PhysicalPosition::new(0.0, (v * dir) as f64),
                ),
                None => winit::event::MouseScrollDelta::LineDelta(0.0, dir),
            };
            self.handle_wheel(delta);
        }
        eprintln!(
            "[autowheel] md scroll {before:?} -> {:?}",
            self.autowheel_md_scroll()
        );
    }

    /// active pane 이 문서 뷰면 그 스크롤 오프셋(logical px). autowheel 로그가
    /// "몇 픽셀 움직였나" 를 찍어야 셀 단위로 튀는 회귀를 눈이 아니라 숫자로 잡는다.
    fn autowheel_md_scroll(&self) -> Option<f32> {
        let ws = self.ws.lock().ok()?;
        let id = ws.active_pane.as_ref()?;
        ws.panes.get(id)?.markdown().map(|m| m.scroll)
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

impl App {
    /// Info 패널 그룹 머리 **더블클릭 → 그 학생으로 포커스** 헤드리스 검증.
    /// `KASATERM_AUTOINFODBL_MS` 뒤에, 지금 활성이 아닌 첫 pane 그룹을 두 번
    /// 눌러 `active_pane` 이 실제로 옮겨갔는지 로그로 남긴다. 사람 손 없이
    /// 확인할 수 있는 유일한 경로다 — 포커스는 그려지는 값이 아니라 상태다.
    pub(crate) fn run_pending_autoinfodbl(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOINFODBL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let before = self.ws.lock().ok().and_then(|w| w.active_pane.clone()).unwrap_or_default();
        // `KASATERM_AUTOINFODBL=win` 이면 방 머리를, 아니면 지금 활성이 아닌
        // pane 머리를 겨눈다 — 두 경로가 서로 다른 동작(방 전환 / pane 포커스)이라
        // 따로 재야 한다.
        let want_win = std::env::var("KASATERM_AUTOINFODBL").is_ok_and(|v| v == "win");
        let target = self
            .info
            .group_rects
            .iter()
            .find(|(k, _)| {
                if want_win {
                    k.strip_prefix("win:").is_some_and(|n| n != self.active_window.to_string())
                } else {
                    !k.starts_with("win:") && *k != before
                }
            })
            .map(|(k, r)| (k.clone(), *r));
        let Some((key, r)) = target else {
            eprintln!("[autoinfodbl] 비활성 pane 그룹이 없다(그룹 {})", self.info.group_rects.len());
            return;
        };
        let (x, y) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for _ in 0..2 {
            self.cursor_px = (x, y);
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
        }
        let after = self.ws.lock().ok().and_then(|w| w.active_pane.clone()).unwrap_or_default();
        eprintln!(
            "[autoinfodbl] {key} 더블클릭 → active_pane {before} → {after} (win={}) 접힘={}",
            self.active_window,
            if key.starts_with("win:") {
                self.info.group_collapsed.contains(&key)
            } else {
                !self.info.pane_expanded.contains(&key)
            }
        );
    }
}
