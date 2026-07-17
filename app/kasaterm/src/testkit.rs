//! 자동 테스트 하네스 — env 기반 auto-split/window/toggle/drag/tabs + schedule 타이머.
use super::*;

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
    /// Headless settings-screen repro: open the settings overlay after
    /// `KASATERM_AUTOSETTINGS_MS`, on the category named in
    /// `KASATERM_AUTOSETTINGS` ("appearance" / "shell" / "claude", default
    /// General), so a background run can screenshot it without a click. State
    /// is function-local statics — deliberately no App field (parallel-work
    /// rule: struct App stays untouched).
    pub(crate) fn run_pending_autosettings(&mut self) {
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
        let cat = std::env::var("KASATERM_AUTOSETTINGS").unwrap_or_default();
        self.settings_cat = match cat.as_str() {
            "appearance" => SettingsCat::Appearance,
            "shell" => SettingsCat::Shell,
            "claude" => SettingsCat::Claude,
            _ => SettingsCat::General,
        };
        eprintln!("[autosettings] open_settings cat={cat}");
        self.open_settings();
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
}
