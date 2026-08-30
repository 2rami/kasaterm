//! 키/마우스/휠 입력 + 클립보드 + claude 상태 글리프/타이틀.
use super::*;

fn claude_launch_screen(text: &str) -> bool {
    (text.contains("Claude Code")
        && text.contains("Welcome back")
        && text.contains("Using Opus"))
        || (text.contains("Accessing workspace:") && text.contains("Quick safety check:"))
}

fn deferred_account_restart_toast(restarted: usize) -> String {
    format!("계정 전환을 적용해 대기 중이던 pane {restarted}개를 다시 띄웠어요")
}

impl App {
    pub(crate) fn send_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Route to whichever backend owns the *active tab*. In-pane tabs
        // (`spawn_new_tab`) are always GUI-local PtySessions in `self.pty`,
        // even when the GUI is daemon-attached: the daemon owns only the
        // primary tab (pid == outer id). So a GUI-local hit must win over the
        // daemon path — otherwise keystrokes for a secondary tab get sent to
        // the daemon, which has no such surface and routes them to the primary
        // tab instead (the "typing in another tab lands in the first tab" bug).
        let surface = self.target_surface();
        if let Some(pid) = surface.as_deref() {
            if let Some(pty) = self.pty.get(pid) {
                let _ = pty.send_bytes(bytes);
                return;
            }
        }
        // Dispatch by which backend is wired up. The hex encoding is
        // a tmux send-keys quirk (the daemon decodes hex pairs back
        // to bytes itself); for the pty backend we hand the raw bytes
        // straight to the PTY writer.
        let _ = &surface;
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let target = self.target_pane();
            let _ = tmux.send_keys_hex(target.as_deref(), &hex);
        } else if let Some(pty) = self.active_pty() {
            let _ = pty.send_bytes(bytes);
        }
    }
    /// True when the pane has mouse reporting + SGR encoding enabled
    /// (claude code / vim / less in alt-screen). Shift-held overrides
    /// to false so the user has an iTerm-style escape hatch back to
    /// our own selection logic.
    pub(crate) fn pane_takes_mouse(&self, pane_id: &str) -> bool {
        if self.modifiers.shift_key() {
            return false;
        }
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .and_then(|p| p.term())
            .map(|t| t.mouse_enabled && t.mouse_sgr)
            .unwrap_or(false)
    }
    /// True if the pane shows a terminal (vs a markdown / image document
    /// view). Document panes are scrolled with the wheel, not dragged —
    /// terminal cell text-selection must not start on them. Unknown pane
    /// (e.g. a leaf that hasn't produced a ScreenUpdate yet) defaults to
    /// terminal so the normal split flow is never blocked.
    pub(crate) fn pane_is_terminal(&self, pane_id: &str) -> bool {
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .map(|p| matches!(p.content, PaneContent::Terminal(_)))
            .unwrap_or(true)
    }
    /// True if the pane's active tab is an image view — a drag here pans the
    /// zoomed image instead of selecting text / following a markdown link.
    pub(crate) fn pane_is_image(&self, pane_id: &str) -> bool {
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .map(|p| matches!(p.content, PaneContent::Image(_)))
            .unwrap_or(false)
    }
    /// Max pan offset (logical px, per axis) for the image pane's current
    /// zoom/rotation against its last-known body box — the slack between the
    /// zoomed image and the box. Clamping the stored pan to this keeps a drag
    /// past the edge from building dead-zone slack. Returns (0,0) when the
    /// image fits (no room to pan) or the box/image is unknown.
    pub(crate) fn image_pan_bounds(&self, pane_id: &str) -> (f32, f32) {
        // Body box ≈ the pane's leaf cell span in logical px. Close enough for
        // clamping the stored pan; `queue_image` does the pixel-exact crop.
        let (cols, rows) = self.window_cells();
        let Some((bw, bh)) = self
            .effective_leaf_rects(cols, rows)
            .into_iter()
            .find(|(id, _, _, _, _)| id == pane_id)
            .map(|(_, _, _, cw, ch)| (cw as f32 * self.cell.w, ch as f32 * self.cell.h))
        else {
            return (0.0, 0.0);
        };
        self.image_pan_bounds_in(pane_id, bw, bh)
    }
    /// `image_pan_bounds` 의 본체 — 본문 박스를 **밖에서 받는다**. 별도창이 이걸 쓴다:
    /// 꺼낸 pane 은 메인 트리(`effective_leaf_rects`)에 없어 위 경로가 늘 (0,0) 을
    /// 내고, 그러면 확대해도 이미지가 가운데 붙박여 움직일 수 없다.
    pub(crate) fn image_pan_bounds_in(&self, pane_id: &str, bw: f32, bh: f32) -> (f32, f32) {
        let ws = self.ws.lock().unwrap();
        let Some(pane) = ws.panes.get(pane_id) else {
            return (0.0, 0.0);
        };
        let Some(img) = pane.image() else {
            return (0.0, 0.0);
        };
        // Rotation by an odd quarter swaps the texture's width/height.
        let (iw, ih) = if pane.image_rot % 2 == 1 {
            (img.h as f32, img.w as f32)
        } else {
            (img.w as f32, img.h as f32)
        };
        if iw <= 0.0 || ih <= 0.0 || bw <= 0.0 || bh <= 0.0 {
            return (0.0, 0.0);
        }
        let fit = (bw / iw).min(bh / ih).min(1.0);
        let z = pane.image_view_zoom().max(1.0);
        let raw_w = iw * fit * z;
        let raw_h = ih * fit * z;
        (((raw_w - bw) * 0.5).max(0.0), ((raw_h - bh) * 0.5).max(0.0))
    }
    /// 이미지 pane 의 화면 박스(원점+크기, logical px). `image_pan_bounds` 와 같은
    /// `effective_leaf_rects` 기반 — 커서기준 줌의 pane 중심 계산에 쓴다.
    pub(crate) fn image_pane_box(&self, pane_id: &str) -> Option<(f32, f32, f32, f32)> {
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        self.effective_leaf_rects(cols, rows)
            .into_iter()
            .find(|(id, ..)| id == pane_id)
            .map(|(_, cx, cy, cw, ch)| {
                (
                    pad + cx as f32 * self.cell.w,
                    TITLE_HEIGHT + cy as f32 * self.cell.h,
                    cw as f32 * self.cell.w,
                    ch as f32 * self.cell.h,
                )
            })
    }
    /// 이미지 pane 줌을 `factor` 배(버튼·키보드·핀치·휠 공용). 1.0~8.0 클램프,
    /// fit(≤1.0) 복귀 시 pan 리셋, 아니면 새 zoom 의 pan 한계로 재클램프. `anchor`
    /// 가 Some 이면 그 화면 좌표 아래 이미지 점이 고정되도록 pan 을 보정한다(커서
    /// 기준 줌). 대상이 이미지 pane 이 아니거나 변화가 없으면 false.
    pub(crate) fn image_zoom_by(
        &mut self,
        pane_id: &str,
        factor: f32,
        anchor: Option<(f32, f32)>,
    ) -> bool {
        let (z0, px0, py0) = {
            let ws = self.ws.lock().unwrap();
            match ws.panes.get(pane_id) {
                Some(p) if p.image().is_some() => {
                    (p.image_zoom.max(1.0), p.image_pan_x, p.image_pan_y)
                }
                _ => return false,
            }
        };
        let z1 = (z0 * factor).clamp(1.0, 8.0);
        if (z1 - z0).abs() < 1e-4 {
            return false;
        }
        let ratio = z1 / z0;
        // pan 기본은 비례 스케일. anchor 있으면 커서 밑 이미지 점을 고정:
        // 화면점 o(=pane 중심 대비 오프셋)에 대해 pan1 = o*(1-ratio) + pan0*ratio.
        let (mut px1, mut py1) = (px0 * ratio, py0 * ratio);
        if let Some((ax, ay)) = anchor {
            if let Some((bx, by, bw, bh)) = self.image_pane_box(pane_id) {
                let ox = ax - (bx + bw * 0.5);
                let oy = ay - (by + bh * 0.5);
                px1 = ox * (1.0 - ratio) + px0 * ratio;
                py1 = oy * (1.0 - ratio) + py0 * ratio;
            }
        }
        // zoom 을 먼저 반영해야 image_pan_bounds 가 z1 기준으로 한계를 낸다.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(p) = ws.panes.get_mut(pane_id) {
                p.image_zoom = z1;
                p.dirty = true;
            }
        }
        if z1 <= 1.0 {
            px1 = 0.0;
            py1 = 0.0;
        } else {
            let (mx, my) = self.image_pan_bounds(pane_id);
            px1 = px1.clamp(-mx, mx);
            py1 = py1.clamp(-my, my);
        }
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(p) = ws.panes.get_mut(pane_id) {
                p.image_pan_x = px1;
                p.image_pan_y = py1;
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }
    /// 트랙패드 핀치(magnification) → 커서 아래 이미지 pane 을 커서기준 줌. `delta`
    /// 는 winit 의 배율 증분(누적 아님, 양수=확대). 이미지 pane 이 아니면 no-op.
    pub(crate) fn handle_pinch(&mut self, delta: f64) {
        let pid = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        if let Some(pid) = pid {
            self.image_zoom_by(&pid, (1.0 + delta as f32).max(0.1), Some(self.cursor_px));
        }
    }
    /// Arm an image-pane pan drag from the current cursor, snapshotting the
    /// pane's current pan so CursorMoved can apply `base + delta`.
    pub(crate) fn begin_image_pan(&mut self, pane_id: &str) {
        let base = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(pane_id)
            .map(|p| (p.image_pan_x, p.image_pan_y))
            .unwrap_or((0.0, 0.0));
        self.image_pan_drag = Some((pane_id.to_string(), self.cursor_px, base));
    }
    /// Encode an SGR mouse event and ship it to the pane. `button` is
    /// the SGR button code (0 = left press/motion/release, +32 for
    /// motion-with-button-held). `press` toggles the final byte
    /// between `M` (press / motion) and `m` (release).
    pub(crate) fn send_mouse_sgr(
        &self,
        pane_id: &str,
        button: u8,
        col: u16,
        row: u16,
        press: bool,
    ) {
        let final_byte = if press { 'M' } else { 'm' };
        let payload = format!("\x1b[<{button};{};{}{final_byte}", col + 1, row + 1);
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = payload
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = tmux.send_keys_hex(Some(pane_id), &hex);
        } else if let Some(pty) = self.pty_for_pane(pane_id) {
            let _ = pty.send_bytes(payload.as_bytes());
        }
    }
    /// sticky 클릭 seek 한 틱: 다음 노치가 필요하면 wheel-up SGR 을 그 pane 에
    /// 쏘고 redraw 를 건다. 종료·대기 판정은 전부 render::sticky_seek_step 안에서.
    /// about_to_wait 가 매 틱 호출한다.
    pub(crate) fn run_pending_sticky_seek(&mut self) {
        if let Some((pane, col, row, down)) = crate::render::sticky_seek_step() {
            // 노치당 여러 줄 스크롤 — 1줄씩이라 너무 느렸다(거노). 여러 번 쏴
            // 체감 속도를 올린다. sticky_seek_step 의 reached 판정이 매 틱 화면을
            // 확인하므로 목표가 뷰에 들면 즉시 멈춘다(약간의 overshoot 는 허용 —
            // 한 화면 지나가도 프롬프트는 보인다).
            let button = if down { 65 } else { 64 };
            for _ in 0..4 {
                self.send_mouse_sgr(&pane, button, col, row, true);
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
    /// Resolve the user-visible label for a pane: OSC 0/2 title set
    /// by the shell or a TUI → foreground process comm → cwd. Used by
    /// both the per-pane header strip and the single-pane native
    /// window title so the two stay consistent.
    /// Scan the bottom of the active pane's grid for a Braille
    /// spinner glyph (U+2800..U+28FF). Tools like Claude Code, oh-my-
    /// zsh's pure-prompt, npm, etc. paint these one cell at a time
    /// to animate progress — picking the glyph straight from the
    /// grid lets us mirror their phase in the window title without
    /// any extra timing math. Returns None when no spinner is
    /// currently visible.
    /// Pull Claude Code's progress line ("✻ Brewed for 5s",
    /// "✶ Thinking…", etc.) straight out of the cell grid. We scan
    /// the bottom of the active pane for a row that starts with a
    /// star/asterisk glyph and trim that row to its text. The
    /// rendered grid is the only signal Claude Code gives us — it
    /// doesn't push these as OSC titles — so this is how we mirror
    /// the live status into the macOS titlebar.
    #[allow(dead_code)]
    pub(crate) fn active_claude_status(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let t = pane.term()?;
        let rows = t.cells.len();
        let start = rows.saturating_sub(10);
        for row in t.cells[start..].iter() {
            let mut text = String::new();
            let mut has_marker = false;
            for cell in row {
                if cell.ch == '\0' {
                    text.push(' ');
                } else {
                    text.push(cell.ch);
                    let cp = cell.ch as u32;
                    if (0x2731..=0x274F).contains(&cp) {
                        has_marker = true;
                    }
                }
            }
            if has_marker {
                let trimmed = text.trim();
                if trimmed.len() > 4 && trimmed.len() < 80 {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
    #[allow(dead_code)]
    pub(crate) fn active_spinner_glyph(&self) -> Option<char> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let t = pane.term()?;
        let rows = t.cells.len();
        let start = rows.saturating_sub(8);
        for row in &t.cells[start..] {
            for cell in row {
                let c = cell.ch;
                let cp = c as u32;
                // Braille spinners (npm, pure-prompt, etc.) +
                // Dingbats asterisks/stars (Claude Code uses
                // ✻/✶/✷/✸/✹/✺ as its "thinking" indicator).
                if (0x2800..=0x28FF).contains(&cp) || (0x2731..=0x274F).contains(&cp) {
                    return Some(c);
                }
            }
        }
        None
    }
    /// Refresh per-pane busy state by scanning each pane's grid for Claude
    /// Code's "working" marker, then drive the header working bar + completion
    /// toast off it. This is the local replacement for the removed daemon's
    /// transcript watcher: Claude doesn't push status over OSC, so the rendered
    /// grid is the only live signal. A `working → idle` flip fires the
    /// top-right completion toast + a header pulse, same as the old daemon path.
    /// Throttled (the scan walks every pane), so safe to call each frame.
    /// The claude spinner blanks/scrolls between frames, so the raw glyph scan
    /// flickers working↔idle — hold `busy` this long past the last sighting.
    /// Shared by the busy loop and the approval-prompt router below.
    const BUSY_GRACE: std::time::Duration = std::time::Duration::from_millis(1200);

    /// Enter 가 들어간 지 이 시간 안이면 괄호-없는 스피너 후보를 글리프-변화
    /// 확정 없이 신뢰한다 — 제출이라는 사건 자체가 「이건 진짜 스피너다」의
    /// 확정보다 강한 근거라, 타이핑 턴은 첫 프레임부터 학생 테마가 붙는다
    /// (거노 2026-08-20 「학생테마 치자마자 0.1초동안 적용안되는거」).
    /// 경과시간 괄호는 ~3초에야 붙으므로(unconfirmed_spinner_row 주석), 그
    /// 구간의 행 이동이 확정을 되돌리는 것까지 덮게 4초.
    pub(crate) const SUBMIT_TRUST: std::time::Duration = std::time::Duration::from_secs(4);

    /// ultracode 가 켜진 pane 을 훑는다 — 입력박스 테두리를 보라색으로 두르는 근거.
    ///
    /// claude 는 이 상태를 statusline payload 에 안 실어 준다(effort 는 low..max 뿐)
    /// → `ultracode-mark.py`(UserPromptSubmit)가 남기는 마커 파일이 유일한 신호다.
    /// **턴 단위**라 다음 프롬프트에 키워드가 없으면 훅이 지우고 테두리도 함께 꺼진다.
    /// 바깥주소(터널) 칩 상태 — 조회에 pgrep 서브프로세스가 들어가므로 5초
    /// 박자로만 본다(칩을 누른 손은 handler 가 낙관 반영하고 이 폴이 확정한다).
    fn refresh_tunnel_chip(&mut self) {
        let now = Instant::now();
        if self
            .statusbar
            .tunnel_checked
            .is_some_and(|t| now.duration_since(t).as_secs() < 5)
        {
            return;
        }
        self.statusbar.tunnel_checked = Some(now);
        self.statusbar.tunnel_on = Some(kasa_mcp::tunnel::is_on());
        self.statusbar.tunnel_host = kasa_mcp::tunnel::host();
        // 미니→맥북 크롬 다리 — 미니 상주 학생이 지금 어느 크롬을 쓰게 되는지.
        // 폴백은 실패 기반이라 사람이 상태를 볼 창이 따로 필요하다(2026-08-30
        // 지시 「하단에 맥북 열림 닫힘을 표시해둬서 나도 볼 수 있게」). 다리의
        // 실체 = 이 맥북의 크롬 브리지(8777)가 듣고 있고 + 역방향 터널
        // (-R 18800→8777)을 실은 ssh 가 살아 있는 것. 기계 명부가 비면 잴 이유가
        // 없다(칩도 안 그린다).
        self.statusbar.chrome_bridge = if kasa_mcp::machines::machines().is_empty() {
            None
        } else {
            let bridge = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], 8777)),
                std::time::Duration::from_millis(250),
            )
            .is_ok();
            let tunnel = crate::proc::command("pgrep")
                .args(["-f", "18800:127.0.0.1:8777"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            Some(bridge && tunnel)
        };
        // 같은 5초 박자에 얹는다 — 포트는 사실상 상수지만 파일이 bind 뒤에
        // 써지므로 폴로 읽어야 부팅 직후의 폴백(8765)이 굳지 않는다.
        self.statusbar.port = Some(crate::mcp_panel_port());
        if let Some(u) = sample_process_tree_usage(&mut self.statusbar.cpu_track) {
            self.statusbar.res = Some((u.cpu, u.rss));
            self.statusbar.usage_top = u.top;
            self.statusbar.usage_rows = u.rows;
            self.statusbar.usage_outside = u.outside;
            self.statusbar.usage_self = (u.self_cpu, u.self_hot);
        }
        // 같은 박자에 물리 메모리도 — 이쪽은 서브프로세스가 없어 사실상 공짜다.
        self.statusbar.mem = crate::sysmem::sample();
        // 계속 코어를 태우는 앱은 메모리와 **다른 축**이라 따로 말한다. 팬이
        // 도는 이유를 물었을 때 「메모리는 정상입니다」만 답하면 소용이 없다
        // (2026-08-27: 「안조용한데 위젯좀 잘만들어봐」).
        // 가장 세게 태우는 하나만 말한다. 목록 순서에 기대면 안 된다 — 그 자루엔
        // 메모리 잣대로 뽑힌 앱도 섞여 있다(2026-08-29).
        let hog = self
            .statusbar
            .usage_outside
            .iter()
            .filter(|a| a.is_hog())
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .cloned();
        if let Some(a) = &hog {
            let due = self
                .statusbar
                .hog_warned
                .is_none_or(|(p, t)| p != a.pid || t.elapsed() >= std::time::Duration::from_secs(3600));
            if due {
                self.statusbar.hog_warned = Some((a.pid, Instant::now()));
                self.set_toast(format!("{} 이(가) 계속 CPU 를 쓰는 중 · {:.0}%", a.name, a.cpu));
            }
        } else {
            self.statusbar.hog_warned = None;
        }
        if let Some(m) = self.statusbar.mem {
            let adv = m.advice();
            if adv.is_danger() {
                // 상태줄 표시만으로는 시야 끝이라 놓치고, 그 사이 무거운 채로
                // 계속 쓴다. 다만 임계는 한 번 넘으면 한동안 걸쳐 있으므로
                // 폴마다 띄우면 5초에 한 번 같은 말이 뜬다 — 한 번 말하고,
                // 무시하고 계속 쓰는 것도 자연스러운 사용이라 한 시간 뒤 다시.
                // 판정이 바뀌었으면 할 일도 바뀐 것이라 곧바로 다시 말한다.
                let due = self.statusbar.mem_warned.is_none_or(|(a, t)| {
                    a != adv || t.elapsed() >= std::time::Duration::from_secs(3600)
                });
                if due {
                    self.statusbar.mem_warned = Some((adv, Instant::now()));
                    // 「메모리 부족」에는 이유 대신 **범인**을 적는다. 토스트는
                    // 몇 초 뒤 사라져서 팝오버를 열 겨를이 없고, 그 몇 초 안에
                    // 쓸모가 있으려면 무엇을 닫으라는 말이어야 한다.
                    let detail = (adv == crate::sysmem::Advice::FreeUp)
                        .then(|| self.statusbar.usage_outside.first())
                        .flatten()
                        .map(|a| {
                            format!("{} {:.1}G", a.name, a.rss as f32 / (1024.0 * 1024.0 * 1024.0))
                        })
                        .unwrap_or_else(|| m.reason());
                    self.set_toast(format!("{} — {detail}", adv.headline()));
                }
            } else {
                // 내려왔으면 다음 진입에서 처음처럼 말한다.
                self.statusbar.mem_warned = None;
            }
        }
    }

    pub(crate) fn refresh_pane_ultracode(&mut self) {
        let dir = std::path::Path::new("/tmp/kasaterm-collab/ultracode");
        // 앱이 transcript 꼬리에서 직접 읽은 판정. 훅은 **프롬프트를 보내야** 도는데,
        // `/effort` 로 켜고 프롬프트 없이 앱을 끄는 것이 자연스러운 사용이라 그 구간엔
        // 표식이 아예 없었다 — 그러면 저장이 xhigh 로 굳어 다음 실행이 ultracode 를
        // 잃는다(거노 2026-08-15 두 번째 신고). 스캔이 답을 내면 그쪽이 최신이다:
        // 켰다면 표식이 없어도 켜고, 껐다면 표식이 남아 있어도 끈다.
        let scanned = self.scanned_ultracode();
        // 복원이 `--effort ultracode` 로 되살린 pane. 그 경로는 transcript 에 흔적을
        // 안 남기므로 스캔도 훅도 볼 것이 없다 — 되살리자마자 다시 끄면 그것만으로
        // ultracode 가 풀리던 자리다.
        let restored = self.restored_ultracode_panes();
        self.pane_ultracode = self
            .pane_claude_sid
            .iter()
            .filter(|(pane, sid)| {
                if let Some(on) = scanned.get(pane.as_str()) {
                    return *on;
                }
                // 훅과 **같은** 정제 규칙이어야 파일명이 어긋나지 않는다.
                !sid.is_empty()
                    && sid
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                    // 훅이 프롬프트마다 마커를 다시 쓰므로 살아 있는 세션은 mtime 이
                    // 계속 갱신된다. 죽은 세션의 .on 은 영구 잔존해, 같은 sid 를
                    // `--resume` 으로 새 pane 에 물리면 첫 프롬프트 전까지 거짓
                    // 글로우가 뜬다(2026-08-12 조사: 잔존 9개) — 반나절 지난 마커는
                    // 무시한다.
                    && std::fs::metadata(dir.join(format!("{sid}.on")))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() < 12 * 3600)
            })
            .map(|(pane, _)| pane.clone())
            .collect();
        // 복원 기준선은 **위 순회 밖**에서 얹는다. 저 순회는 `pane_claude_sid` 를
        // 도는데 그 표는 bind-transcript 훅이 채우고, 그 훅은 첫 프롬프트 뒤에나
        // 온다 — 복원 직후의 pane 은 아직 표에 없어 후보로도 안 잡힌다.
        // 스캔이 「껐다」고 말한 pane 만 빼고 나머지는 켠다.
        for pane in restored {
            if scanned.get(&pane) != Some(&false) {
                self.pane_ultracode.insert(pane);
            }
        }
        // 혜성 redraw 펌프(handler.rs)의 게이트 — 마커 스캔과 같은 손이 갱신해야
        // 켜짐/꺼짐이 글로우와 같은 박자로 움직인다.
        crate::render::ULTRA_COMET_ANIMATING.store(
            !self.pane_ultracode.is_empty(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub(crate) fn refresh_pane_activity(&mut self) {
        let now = Instant::now();
        if let Some(t) = self.pane_busy_check {
            // 미확정 스피너 후보가 살아 있는 동안은 박자를 100ms 로 좁힌다 —
            // 확정이 글리프 변화(≈스피너 주기)를 기다리는 동안 300ms 틱 두 번
            // (최악 0.6초)이 무테마로 새는 것을 줄인다. Enter 없는 턴 시작
            // (인박스 주입 등)이 이 경로의 수혜자다. 인용문 후보는 영영 확정이
            // 안 되므로 후보별 목격 시각 1.2초로 자른다.
            let hot = self.spinner_probe.values().any(|&(_, _, conf, seen)| {
                !conf && now.duration_since(seen).as_millis() < 1200
            });
            if now.duration_since(t).as_millis() < if hot { 100 } else { 300 } {
                return;
            }
        }
        self.pane_busy_check = Some(now);
        // 지난 프레임에 위로 스크롤돼 있던 pane 의 프롬프트 목록을 깊게 채운다.
        // **표시를 비우면서** 가져간다 — 아래로 내린 pane 이 목록에 남아 있으면
        // 틱마다 파일을 되짚게 된다. 계속 올려다보는 중이면 렌더가 다음 프레임에
        // 다시 남기므로(틱보다 훨씬 잦다) 끊기지 않는다.
        let want = std::mem::take(&mut *self.pane_deep_want.borrow_mut());
        for id in want {
            self.ensure_deep_prompts(&id);
        }
        // 이사(agent-stop)로 학생을 내준 pane 의 sid 주장 걷기 — HTTP 핸들러는 App
        // 상태를 못 만져 큐로 넘긴다. 걷자마자 저장까지 해 둔다: 몇 초 안에 앱이
        // 꺼져도 낡은 주장이 leaf 에 안 남아, 재시작 복원이 남의 기계로 이사 간
        // 대화를 다시 여는 일(2026-08-30 이중 열림 실측)이 없다.
        {
            let gone = kasa_mcp::remote::drain_migrated_away();
            let mut dropped = false;
            for pane in gone {
                dropped |= self.pane_claude_sid.remove(&pane).is_some();
            }
            if dropped {
                self.save_session_state();
            }
        }
        // 닫아 둔 pane 의 유휴도 같은 박자로 본다 — 판정 재료(`term_is_working`)가
        // 같으니, 화면에서 뗀 pane 만 따로 스캔할 이유가 없다.
        self.reap_idle_closed_panes();
        // 꼬리 스캔이 **먼저**다 — 아래 ultracode 판정이 그 결과를 읽는다. 뒤에 두면
        // 켜고 끈 것이 한 틱 늦게 화면에 온다.
        self.sync_session_titles();
        self.refresh_pane_ultracode();
        self.refresh_tunnel_chip();
        self.run_pending_autotitlesync();
        self.run_pending_autoultrascan();

        // Scan under the lock, then mutate `pane_activity` after dropping it —
        // the completion-toast path takes no further workspace lock. The same
        // pass also looks for a pending approval prompt (munder BLOCK_HINTS):
        // only meaningful when the spinner is gone, so busy panes skip it.
        // `bg_tab_busy` = **안 보이는 탭**에서 클로드가 도는 pane. 스캔이 활성 탭만
        // 보던 동안 뒤 탭 학생은 화면에 아무 흔적이 없었다 — busy 바도 완료 펄스도.
        // 그렇다고 스윕바를 띄우면 노는 화면 위에서 "이 화면이 일한다"는 거짓말이
        // 되므로, 있는 언어를 쓴다: 보이는 것은 busy, 안 보이는 것은 bg 펄스.
        let (busy_now, bg_tab_busy, compacting_now, stalled_now): (
            Vec<(String, bool, Option<ApprovalPrompt>)>,
            std::collections::HashSet<String>,
            std::collections::HashMap<String, Option<u8>>,
            std::collections::HashMap<String, &'static str>,
        ) = {
            // 프로브는 락 밖 필드라 ws 가드와 나란히 빌린다 — 메서드로 빼면 &mut self
            // 가 통째로 필요해 가드와 충돌한다.
            let probe = &mut self.spinner_probe;
            let ws = self.ws.lock().unwrap();
            let mut rows = Vec::with_capacity(ws.panes.len());
            let mut bg = std::collections::HashSet::new();
            // compact 중인 pane → 화면에서 읽은 진행률. 같은 스캔에서 뽑는다 — 따로
            // 한 바퀴 돌면 ws 락을 두 번 잡고, 그 사이 화면이 바뀌어 어긋난다.
            let mut compacting = std::collections::HashMap::new();
            // 연결이 끊겨 멈춘 pane → 화면에서 읽은 사연. `busy` 와 무관하게 본다 —
            // 멈추면 스피너가 사라져 idle 로 보이므로, busy 안에서만 재면 정작
            // 표시해야 할 상태를 통째로 놓친다.
            let mut stalled = std::collections::HashMap::new();
            for (id, pane) in ws.panes.iter() {
                match pane.term() {
                    Some(t) => {
                        // 턴 시작 첫 ~3초는 스피너가 `✢ Transmuting…` 뿐이라 본판정
                        // (경과시간 괄호 요구)이 거부한다. 글리프가 틱 사이에 바뀌면
                        // 진짜 스피너로 확정 — 인용문은 멈춰 있다. 규칙 본문은
                        // screenread::unconfirmed_spinner_row 주석.
                        let strict = term_is_working(t);
                        let boosted = if strict {
                            probe.remove(id);
                            false
                        } else {
                            match crate::render::unconfirmed_spinner_row(&t.cells) {
                                Some((r, _c, g)) => {
                                    // 방금 Enter 가 들어간 에이전트 pane 은 후보를 즉시
                                    // 신뢰한다(SUBMIT_TRUST 머리말). 에이전트 pane 한정 —
                                    // 셸 명령 출력이 우연히 이 모양이면 Enter 마다 4초씩
                                    // busy 로 오르는 것을 막는다.
                                    let submitted = self
                                        .pty
                                        .get(ws.active_tab_pid(id).as_str())
                                        .is_some_and(|p| {
                                            p.active_agent().is_some()
                                                && p.last_submit().is_some_and(|s| {
                                                    now.duration_since(s) < Self::SUBMIT_TRUST
                                                })
                                        });
                                    let (conf, seen) = match probe.get(id) {
                                        Some(&(pr, pg, pc, ps)) if pr == r => {
                                            (submitted || pc || pg != g, ps)
                                        }
                                        _ => (submitted, now),
                                    };
                                    probe.insert(id.clone(), (r, g, conf, seen));
                                    conf
                                }
                                None => {
                                    probe.remove(id);
                                    false
                                }
                            }
                        };
                        let busy = strict || boosted;
                        let prompt = if busy {
                            None
                        } else {
                            rows_show_approval_prompt(&t.cells)
                        };
                        // compact 중에도 스피너는 돌아서 busy 가 이미 참이다. 그 안에서만
                        // 좁히므로, 스크롤을 되짚다 옛 알림을 만나 바가 켜지는 일은 없다.
                        if busy {
                            if let Some(pct) = rows_show_compacting(&t.cells) {
                                compacting.insert(id.clone(), pct);
                            }
                        }
                        if let Some(why) = crate::screenread::find_connection_trouble(&t.cells) {
                            stalled.insert(id.clone(), why);
                        }
                        rows.push((id.clone(), busy, prompt));
                    }
                    None => rows.push((id.clone(), false, None)),
                }
                let active = pane.active_tab.min(pane.tabs.len().saturating_sub(1));
                if pane
                    .tabs
                    .iter()
                    .enumerate()
                    .any(|(i, t)| i != active && t.term().is_some_and(term_is_working))
                {
                    bg.insert(id.clone());
                }
            }
            (rows, bg, compacting, stalled)
        };

        // The claude spinner blanks/scrolls between frames, so the raw glyph
        // scan flickers working↔idle. Hold `busy` for BUSY_GRACE past the last
        // spinner sighting; only a real stop (grace elapsed with no spinner)
        // counts as completion — otherwise every blink fired a bogus toast.
        let mut completed: Vec<String> = Vec::new();
        for (id, raw_busy, _) in &busy_now {
            if *raw_busy {
                self.pane_last_busy.insert(id.clone(), now);
            }
            let busy = *raw_busy
                || self
                    .pane_last_busy
                    .get(id)
                    .map_or(false, |t| now.duration_since(*t) < Self::BUSY_GRACE);
            // Only a real "working" run counts toward the completion toast —
            // a pane leaving `blocked`/`waiting` (prompt answered) didn't
            // finish anything, it just got unstuck.
            // compact 도 「일하던 중」에 든다 — 여기서 빼면 compact 로 시작해 그대로 끝난
            // 턴이 완료로 안 잡혀 토스트가 사라진다.
            let was_busy = self
                .pane_activity
                .get(id)
                .map_or(false, |a| matches!(a.status.as_str(), "working" | "compacting"));
            if was_busy && !busy {
                completed.push(id.clone());
                self.pane_last_busy.remove(id);
            }
            // compact 중이면 「working」보다 좁게 적는다 — 헤더가 쓸림바 대신 차오르는
            // 바를 그리는 갈림길이 이 한 값이다. `busy` 가 grace 로 늘어나 있는 동안에도
            // 화면에 알림이 남아 있으면 compacting 으로 유지된다(끝나면 알림이 사라져
            // 자동으로 working→idle 로 떨어진다).
            let compact = compacting_now.get(id).copied();
            // **에이전트가 도는 pane 만** 빨갛게 둔다. 셸에서는 사람이 그 문구를
            // 직접 쳐 넣거나 남의 로그를 흘려보내는 일이 흔해서, 화면 글자만으로는
            // 진짜 끊김과 구분이 안 된다.
            let stalled = self
                .pty
                .get(id)
                .and_then(|p| p.active_agent())
                .and(stalled_now.get(id).copied())
                .map(str::to_string);
            let status = if busy {
                if compact.is_some() {
                    "compacting"
                } else {
                    "working"
                }
            } else {
                "idle"
            };
            let compact_pct = compact.flatten();
            // A visibly-working pane already shows the sweep, so skip the tail
            // read; only idle panes need the "background job running" check, and
            // their transcript rarely changes so the mtime cache keeps IO ~zero.
            let bg_active = if busy {
                false
            } else {
                // ⚠️ `||` 로 단축평가하지 마라 — bg 탭이 바쁘면 `pane_bg_active` 가
                // 통째로 안 불려 sticky 띠 글감이 안 채워진다(위 주석과 같은 사고).
                let from_pane = self.pane_bg_active(id);
                bg_tab_busy.contains(id) || from_pane
            };
            // 「도는 중」이 이어지는 동안 기준점을 유지하고, 멈추면 버린다. 직접
            // 일하는 것과 뒤에서 도는 것을 함께 센다 — 그림 굽는 배치는 claude 가
            // 「끝났나」로 되물으며 기다리는 사이 status 가 오가지만, 사람이 알고
            // 싶은 것은 그 일이 시작된 뒤로 흐른 시간이다.
            let running = bg_active || (status != "idle" && !crate::chrome::status_needs_you(status));
            let now = std::time::Instant::now();
            self.pane_activity
                .entry(id.clone())
                .and_modify(|a| {
                    a.status = status.to_string();
                    a.bg_active = bg_active;
                    a.compact_pct = compact_pct;
                    a.stalled = stalled.clone();
                    a.busy_since = running.then(|| a.busy_since.unwrap_or(now));
                })
                .or_insert_with(|| crate::stream::PaneStatusView {
                    status: status.to_string(),
                    bg_active,
                    compact_pct,
                    stalled,
                    busy_since: running.then_some(now),
                    ..Default::default()
                });
        }
        // Drop entries for panes that no longer exist (closed/undocked).
        self.pane_activity
            .retain(|k, _| busy_now.iter().any(|(id, _, _)| id == k));
        self.pane_last_busy
            .retain(|k, _| busy_now.iter().any(|(id, _, _)| id == k));
        self.route_approval_prompts(&busy_now, now);

        // 계정 전환 때 일하고 있어서 못 되띄운 pane — 방금 갱신한 활동 상태가
        // idle 로 떨어졌으면 여기서 따라 돌린다(같은 300ms 박자, 표시가 없으면 공짜).
        let switched = self.run_pending_account_restarts();
        if switched > 0 {
            self.set_toast(deferred_account_restart_toast(switched));
        }

        if completed.is_empty() {
            return;
        }
        // A sibling finished: 헤더 펄스 + 학생 cheer 만 남긴다. 완료 "토스트"는
        // glyph 스캔 기반이라 오탐(엉뚱한 pane·타이밍)이 잦아 제거(거노 요청).
        for id in completed {
            self.notify_flash.insert(id.clone(), now);
            // 턴 완료 → 학생 cheer 시작. 사용자가 이 pane 에 입력할 때까지 유지.
            self.turn_done_panes.insert(id);
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 이 pane 에 **화면 밖에서 도는 일**이 있나 — `run_in_background` 셸·`Monitor`,
    /// 그리고 아직 안 돌아온 **서브에이전트**.
    ///
    /// 서브에이전트를 같이 보는 이유: 세션 자체는 idle(스피너 없음)인데 Task 가 정보를
    /// 모아 오는 동안, 화면에서 그 pane 이 **정말 노는 pane 과 구별되지 않았다**(거노
    /// 2026-08-11 제안). 표시는 이미 갈려 있다 — 도는 중은 쓸어가는 sweep, 이쪽은
    /// 3초 숨쉬기 pulse(`render.rs` 의 `working_bar` vs `pulse_bar`).
    ///
    /// 답은 두 군데서 온다. **훅이 정본**이고(`kasaterm-agent-status.sh` 가 시작·종료를
    /// 그 순간 밀어 넣는다), transcript 꼬리는 폴백이다 — 앱이 나중에 떠서 시작을 못 본
    /// pane, Windows(python3 없어 훅이 안 도는 경우), 옛 세션이 그 폴백으로 산다.
    /// 순서가 이 방향인 이유: 꼬리는 64KB 라 세션이 커지면 런치 기록이 밀려나 **오래
    /// 기다리는 작업일수록 안 보였다**(실측: 3.8MB 7건 / 8.3MB·24MB 0건). 훅이 「있다」고
    /// 하면 그것으로 판정이 끝난다.
    ///
    /// ⚠️ 다만 훅이 「있다」고 **곧장 빠져나가지는 않는다**. 같은 꼬리에서 sticky 띠의
    /// 글감(마지막 프롬프트)도 함께 꺼내므로, 일찍 반환하면 **일하는 pane 일수록 그
    /// 글감이 영영 안 채워진다** — 훅 없는 임시창에서만 띠가 뜨고 실사용 창에서는 안
    /// 뜨는 어긋남이 그래서 났다(2026-08-30). 읽기는 mtime 게이트가 막고 있어 싸다.
    fn pane_bg_active(&mut self, pane_id: &str) -> bool {
        let hook_bg = self
            .collab
            .hook_activity
            .lock()
            .unwrap()
            .get(pane_id)
            .is_some_and(|a| !a.is_empty());
        let Some(sid) = self.pane_claude_sid.get(pane_id).cloned() else {
            return hook_bg;
        };
        let Some(path) = crate::socket::transcript_path_for_session(&sid) else {
            return hook_bg;
        };
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if let Some(mt) = mtime {
            if let Some((cached_mt, cached, _)) = self.pane_bg_mtime.get(pane_id) {
                if *cached_mt == mt {
                    return hook_bg || *cached;
                }
            }
        }
        // 512KB — `collab_board`(socket.rs)와 **같은 값이어야 한다**. board 는 64KB
        // 로는 런치가 대량 출력에 밀려 안 잡힌다는 걸 알고 진작 늘렸는데 이쪽만
        // 64KB 로 남아, 같은 pane 을 두고 board 는 「서브에이전트 돌는 중」이라 하고
        // 화면은 아무 표시도 안 하는 어긋남이 있었다(2026-08-11). 판정 재료가 다르면
        // 판정도 다르다 — 두 벌을 둘 거면 최소한 창 크기는 맞춰 둔다.
        let (tail, idle) = crate::socket::read_tail(&path, 512 * 1024);
        let snap = crate::transcript::snapshot_from_tail(&sid, &tail, idle);
        let bg = !snap.background.is_empty() || !snap.subagents.is_empty();
        if let Some(mt) = mtime {
            let prompts = crate::transcript::prompts_from_tail(&tail);
            self.pane_bg_mtime.insert(pane_id.to_string(), (mt, bg, prompts));
        }
        hook_bg || bg
    }

    /// 위로 스크롤한 pane 의 프롬프트 목록을 **깊은 꼬리**로 다시 채운다.
    ///
    /// `pane_bg_active` 가 채우는 목록은 512KB 꼬리에서 나온다. 그 크기는 헤더
    /// 펄스바(백그라운드 작업 감지)에는 넉넉하지만 프롬프트에는 턱없다 — 도구
    /// 출력 한 덩이가 1MB 를 넘는 일이 흔해서, 일하던 창은 그 창에 질문이 **하나**
    /// 밖에 안 들어간다(2026-08-31 실측: 24MB 기록에서 0.5MB→1개, 8MB→26개).
    /// 후보가 하나면 띠는 늘 그 하나를 그리고, 올려다보는 사람 눈에는 「엉뚱한
    /// 질문이 붙는다」로 보인다.
    ///
    /// 그래서 스크롤 게이트가 열린 pane 에서만 부른다. transcript mtime 으로 잠가
    /// 두므로 한 번 올려다보는 동안 8MB 읽기는 많아야 한 번이고, 스크롤을 안 하는
    /// 평상시에는 아예 일어나지 않는다.
    pub(crate) fn ensure_deep_prompts(&mut self, pane_id: &str) {
        // 8MB. 위 실측에서 26개를 얻은 크기다. 더 키워도 얻는 질문 수는 완만하게
        // 늘지만 읽기·파싱이 선형으로 는다.
        const DEEP_BYTES: u64 = 8 * 1024 * 1024;
        // 그 이상은 올려다봐도 못 닿는다. 캐시를 pane 마다 들고 있으므로 상한이
        // 없으면 창 여럿에서 메모리가 그대로 는다.
        const KEEP: usize = 24;
        let Some(sid) = self.pane_claude_sid.get(pane_id).cloned() else {
            return;
        };
        let Some(path) = crate::socket::transcript_path_for_session(&sid) else {
            return;
        };
        let Ok(mt) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            return;
        };
        if self.pane_deep_prompts.get(pane_id) == Some(&mt) {
            return;
        }
        // 넣을 칸이 없으면 그냥 돌아간다 — `pane_bg_active` 가 아직 이 pane 을 안
        // 훑었다는 뜻이고, mtime 만 적어 두면 다음부터 영영 건너뛰게 된다.
        if !self.pane_bg_mtime.contains_key(pane_id) {
            return;
        }
        let (tail, _) = crate::socket::read_tail(&path, DEEP_BYTES);
        let mut prompts = crate::transcript::prompts_from_tail(&tail);
        if prompts.len() > KEEP {
            prompts.drain(..prompts.len() - KEEP);
        }
        self.pane_deep_prompts.insert(pane_id.to_string(), mt);
        if let Some(slot) = self.pane_bg_mtime.get_mut(pane_id) {
            slot.2 = prompts;
        }
    }

    /// 그 pane 에서 사람이 친 프롬프트들(오래된 것부터). 스크롤 sticky 띠의 글감 —
    /// claude 가 그 행을 더 이상 그려 주지 않으므로 kasaterm 이 직접 채운다.
    /// `pane_bg_active` 가 같은 tail 에서 채워 둔 캐시를 읽기만 한다(무IO).
    pub(crate) fn pane_prompts(&self, pane_id: &str) -> &[(String, Vec<String>)] {
        self.pane_bg_mtime.get(pane_id).map_or(&[], |(_, _, p)| p.as_slice())
    }

    /// 커서가 멎은 `[Image #N]` 참조를 받아 툴팁 상태를 옮기고, 이번 프레임에
    /// 그릴 썸네일을 돌려준다. `hit` 이 `None`(참조 밖) 이면 툴팁을 접는다.
    ///
    /// 캐시를 두지 않는다 — 번호는 claude 를 다시 켤 때마다 1부터 다시 매겨져
    /// 같은 `(pane, 번호)` 가 나중에 다른 그림을 가리킨다. 커서가 옮겨갈 때마다
    /// 한 번 읽는 편이 무효화 규칙을 세우는 것보다 단순하고 항상 최신이다.
    pub(crate) fn pump_image_tip(
        &mut self,
        hit: Option<(String, u32)>,
    ) -> Option<(Arc<Vec<u8>>, u32, u32)> {
        let Some(at) = hit else {
            let had = self.image_tip.take().is_some_and(|t| t.thumb.is_some());
            if had {
                self.chrome_dirty = true;
            }
            return None;
        };
        if self.image_tip.as_ref().is_none_or(|t| t.at != at) {
            self.image_tip = Some(crate::ImageTip {
                at: at.clone(),
                since: std::time::Instant::now(),
                thumb: None,
                looked: false,
            });
        }
        let tip = self.image_tip.as_ref()?;
        if tip.thumb.is_none() {
            if tip.looked || tip.since.elapsed() < crate::IMAGE_TIP_DELAY {
                return None;
            }
            let thumb = self.image_tip_thumb(&at.0, at.1);
            let tip = self.image_tip.as_mut()?;
            tip.looked = true;
            tip.thumb = thumb;
            self.chrome_dirty = true;
        }
        self.image_tip.as_ref()?.thumb.clone()
    }

    /// 그 pane 의 transcript 에서 `#n` 그림을 찾아 툴팁 크기로 줄인 RGBA.
    fn image_tip_thumb(&self, pane_id: &str, n: u32) -> Option<(Arc<Vec<u8>>, u32, u32)> {
        let sid = self.pane_claude_sid.get(pane_id)?;
        let path = crate::socket::transcript_path_for_session(sid)?;
        // 실측(2026-08-15, 56MB 세션): 가장 최근 그림이 파일 끝에서 0.7MB, 그
        // 앞의 것들이 5~8MB. 화면에 떠 있는 참조는 거의 최근 프롬프트라 4MB 로
        // 대개 잡힌다. 못 잡으면 한 번만 넓혀 본다 — 두 번째도 실패면 그 번호는
        // 아직 제출 안 된 프롬프트라 어디에도 없다.
        let mut bytes = None;
        for cap in [4u64, 24] {
            let (tail, _) = crate::socket::read_tail(&path, cap * 1024 * 1024);
            bytes = crate::transcript::image_paste_bytes(&tail, n);
            // 꼬리가 상한보다 짧으면 파일을 통째로 본 것이라 더 넓혀도 같다.
            if bytes.is_some() || (tail.len() as u64) < cap * 1024 * 1024 {
                break;
            }
        }
        let img = image::load_from_memory(&bytes?).ok()?;
        // 툴팁은 로지컬 320px 인데 Retina 는 2배로 그린다 — 640 아래로 줄이면
        // 화면에서 흐려진다. 원본이 더 작으면 확대하지 않는다.
        const MAX_PX: u32 = 640;
        let img = if img.width().max(img.height()) > MAX_PX {
            img.thumbnail(MAX_PX, MAX_PX)
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Some((Arc::new(rgba.into_raw()), w, h))
    }

    /// 라이트↔다크 플립을 감지해 **떠 있는** claude 세션까지 갈아입힌다.
    ///
    /// 새 세션은 이미 맞다 — `sync_claude_theme`(theme.rs 훅)이 settings.json
    /// 을 갱신하고, `theme: auto` 면 부팅 때 OSC 11 질의에 우리가 현재 팔레트로
    /// 답한다(kasa-pty ColorRequest). 문제는 실행 중 세션인데, claude 2.1.232
    /// 실측(2026-08-14)으로 그쪽 지렛대가 전부 죽어 있음을 확인했다:
    ///   - settings.json 변경: 파일워치 없음(플립 후 출력 0B)
    ///   - `CSI ?997;N n` 리포트: `?2031h` 를 켜 두고도 무반응(auto 에서도)
    ///   - 리사이즈 재그리기: 부팅 때 캐시한 테마로 다시 그림
    ///   - `/theme dark` 인자: 무시하고 피커만 연다
    /// 살아 있는 지렛대는 `/config theme=<값>`(v2.1.181+, 실측: 즉시 적용 +
    /// settings 기록 + "Set Theme to dark" 확인 출력)뿐이다. 그래서:
    ///   ① 2031 구독 pane 에는 표준 리포트를 쏜다(무해 + claude 가 언젠가
    ///      반응하는 순간 이 경로가 공짜로 살아난다)
    ///   ② claude 가 도는 pane 을 큐에 넣고, idle + 빈 입력줄(❯ 단독)일 때만
    ///      `/config theme=<값>` 을 쳐 준다. 입력줄에 글이 있으면 미룬다 —
    ///      주입이 초안 뒤에 붙는 사고(tell 유형)를 막는다.
    pub(crate) fn poll_claude_retheme(&mut self) {
        let light = crate::theme::current_is_light();
        let prev = self.theme_light_last.replace(light);
        let Some(prev) = prev else {
            return; // 첫 틱 — 기준만 세운다
        };
        if prev != light {
            // ① 표준 경로: 컬러스킴 변경 리포트(구독한 앱에만 — 셸에 보내면
            //    입력줄에 이스케이프 쓰레기가 박힌다).
            let report: &[u8] = if light { b"\x1b[?997;2n" } else { b"\x1b[?997;1n" };
            for p in self.pty.values() {
                if p.wants_scheme_reports() {
                    let _ = p.send_bytes(report);
                }
            }
            // ② `/config theme=` 주입 큐. 값은 플립 시점에 한 번 읽는다 —
            //    sync_claude_theme 이 방금 갱신한 settings 값이라 새 세션이
            //    읽을 값과 항상 같다. auto 는 재선택이 OSC 11 재질의를 부르니
            //    그대로 보내면 된다.
            //
            //    **`custom:` 은 보내지 않는다.** 그 커맨드는 표준 7종만 받고
            //    커스텀은 거부한다("For custom themes, use /theme." — 2026-08-15
            //    실측). 보내 봐야 세션마다 그 빨간 안내만 남는다. 커스텀을 쓰는
            //    동안은 팔레트가 갈릴 때 `~/.claude/themes/kasaterm.json` 이 다시
            //    구워지는 것으로 충분하다 — 그 파일은 claude 가 지켜본다.
            if let Some(value) = crate::socket::claude_theme_value()
                .as_deref()
                .filter(|v| !v.starts_with("custom:"))
                .and_then(claude_theme_token)
            {
                let now = Instant::now();
                let ids: Vec<String> =
                    self.ws.lock().unwrap().panes.keys().cloned().collect();
                for id in ids {
                    let is_claude = self.pty_for_pane(&id).is_some_and(|p| {
                        matches!(p.active_agent(), Some(kasa_pty::AgentKind::Claude))
                    });
                    if is_claude {
                        self.retheme_queue.insert(
                            id,
                            crate::RethemeState {
                                expires: now + std::time::Duration::from_secs(60),
                                value: value.to_string(),
                            },
                        );
                    }
                }
            }
        }
        self.drive_retheme_queue();
    }

    /// `retheme_queue` 를 한 틱 진행시킨다 — 틈이 난 pane 에 `/config
    /// theme=<값>` 을 쳐 주고 큐에서 뺀다. 못 친 pane 은 만료까지 미루다 조용히
    /// 포기한다: 테마는 다음 플립에 또 기회가 있고, 사용자 입력줄을 어지럽히는
    /// 쪽이 더 나쁘다.
    fn drive_retheme_queue(&mut self) {
        if self.retheme_queue.is_empty() {
            return;
        }
        let now = Instant::now();
        let ids: Vec<String> = self.retheme_queue.keys().cloned().collect();
        for id in ids {
            let Some(p) = self.pty_for_pane(&id).cloned() else {
                self.retheme_queue.remove(&id);
                continue;
            };
            let Some(st) = self.retheme_queue.get(&id) else {
                continue;
            };
            if now >= st.expires {
                self.retheme_queue.remove(&id);
                continue;
            }
            // claude 가 내렸으면(셸로 복귀 등) 주입할 곳이 없다.
            if !matches!(p.active_agent(), Some(kasa_pty::AgentKind::Claude)) {
                self.retheme_queue.remove(&id);
                continue;
            }
            let idle = self
                .pane_activity
                .get(&id)
                .map_or(false, |a| a.status == "idle");
            if !idle {
                continue;
            }
            let bare = {
                let ws = self.ws.lock().unwrap();
                ws.panes
                    .get(&id)
                    .and_then(|pn| pn.term())
                    .map_or(false, |t| input_line_bare(&t.cells))
            };
            if !bare {
                continue;
            }
            // 커맨드 뒤 CR 은 **지연 별도 write** — 본문에 붙은 CR 은 Ink 가
            // 제출이 아니라 개행으로 읽는다(SocketBytes 의 실측 교훈,
            // handler.rs). 같은 140ms 패턴을 그대로 쓴다. `/config theme=` 는
            // 실측(2.1.232)으로 즉시 적용 + settings 기록 + 확인 한 줄 출력.
            let cmd = format!("/config theme={}", st.value);
            let _ = p.send_bytes(cmd.as_bytes());
            let p2 = std::sync::Arc::clone(&p);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(140));
                let _ = p2.send_bytes(b"\r");
            });
            self.retheme_queue.remove(&id);
        }
    }

    /// munder식 승인 프롬프트 라우팅: 오케스트레이터(또는 협업방 없는 단독 pane)의 프롬프트만
    /// 사용자를 부르고 — sticky 토스트 + 승인/거부 칩 + 데스크탑 알림 — 워커의
    /// 프롬프트는 board `waiting` 으로만 흘려 오케스트레이터가 처리하게 둔다. 프롬프트가
    /// 사라지거나(답함) pane 이 다시 일하면 플래그·토스트를 걷는다.
    fn route_approval_prompts(
        &mut self,
        scan: &[(String, bool, Option<ApprovalPrompt>)],
        now: Instant,
    ) {
        let mut changed = false;
        for (id, raw_busy, prompt) in scan {
            let busy = *raw_busy
                || self
                    .pane_last_busy
                    .get(id)
                    .map_or(false, |t| now.duration_since(*t) < Self::BUSY_GRACE);
            let flagged = self.pane_prompt_wait.contains_key(id);
            if !busy && prompt.is_some() {
                if !flagged {
                    // 솔로(자동통솔 폐기 06-18) — 모든 pane 이 사용자 직행.
                    let faces_user = true;
                    self.pane_prompt_wait.insert(id.clone(), faces_user);
                    // ⚠️ 여기에 `notify_flash` 를 넣지 마라. 그 맵은 **턴 완료** 전용
                    // 채널이라(초록 펄스 + 학생 만세), 승인 대기에 진입하는 순간
                    // 막혀 선 학생이 「끝난 학생」으로 보였다 — 없는 신호보다 나쁜
                    // 틀린 신호다(2026-08-11). 대기는 attention 색이 말한다.
                    // board 에 waiting 으로 노출 — 오케스트레이터가 board 로 본다.
                    self.collab
                        .attention
                        .lock()
                        .unwrap()
                        .insert(id.clone(), "승인 대기 (화면 감지)".to_string());
                    // 화면 승인 토스트/칩은 화면 감지 오탐이 있어 제거(거노 요청).
                    // 승인 신호는 board attention(위) + (pane 안 볼 때) 데스크탑 알림
                    // 으로만 남긴다 — toast_action 은 Sparkle 업데이트 토스트가 공유해
                    // 건드리지 않는다.
                    if faces_user {
                        // 보고 있는 pane 이라고 삼키지 않는다 — 거노 2026-08-11
                        // "pane별로 그냥 다오게하자". 프사가 붙어 누구 건지 갈린다.
                        let ch = self.pane_character_if_known(id);
                        let who = ch.clone().unwrap_or_else(|| "pane".to_string());
                        // 훅 경로(`chrome.rs` 의 `⚠ 권한 필요`)와 같은 열쇠 — 같은
                        // 프롬프트에 배너가 둘 나가는 걸 발사구에서 막는다.
                        let sid = self.pane_claude_sid.get(id).cloned();
                        crate::chrome::notify_desktop(
                            "⚠ 승인 필요",
                            &who,
                            ch.as_deref(),
                            Some(&format!("approval:{id}")),
                            Some((id, sid.as_deref())),
                        );
                    }
                    changed = true;
                }
                let st = if self.pane_prompt_wait.get(id).copied().unwrap_or(false) {
                    "blocked"
                } else {
                    "waiting"
                };
                if let Some(a) = self.pane_activity.get_mut(id) {
                    if a.status != st {
                        a.status = st.to_string();
                        changed = true;
                    }
                }
            } else if flagged {
                self.pane_prompt_wait.remove(id);
                self.collab.attention.lock().unwrap().remove(id);
                if self.collab.toast_action.as_deref() == Some(id.as_str()) {
                    self.clear_approval_toast();
                }
                changed = true;
            }
        }
        self.pane_prompt_wait
            .retain(|k, _| scan.iter().any(|(id, _, _)| id == k));
        if changed {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Drop the sticky approval toast and its chip hit-rects in one place —
    /// called when the prompt resolves, a chip is clicked, or it's dismissed.
    pub(crate) fn clear_approval_toast(&mut self) {
        self.collab.toast = None;
        self.collab.toast_action = None;
        self.collab.toast_rect = None;
        self.collab.toast_approve_rect = None;
        self.collab.toast_deny_rect = None;
    }

    /// 승인 토스트 칩 클릭 → 대상 pane PTY 에 응답 키 직주입 (forward_key 의
    /// IME/모디파이어 경로를 타지 않는다). 클릭 시점에 그리드를 재스캔해 종류를
    /// 확정한다 — 메뉴에 'n' 을 보내면 글자는 무시되고 \r 만 남아 Yes 를 골라버리는
    /// 오발이 있어서, munder처럼 y\r 맹발사하지 않는다.
    pub(crate) fn respond_approval(&mut self, pane_id: &str, approve: bool) {
        let kind = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .get(pane_id)
                .and_then(|p| p.term())
                .and_then(|t| rows_show_approval_prompt(&t.cells))
        };
        let bytes: &[u8] = match (kind, approve) {
            (Some(ApprovalPrompt::YesNo), true) => b"y\r",
            (Some(ApprovalPrompt::YesNo), false) => b"n\r",
            // 메뉴(또는 프롬프트가 막 사라진 경우): Enter=하이라이트(기본 Yes) / Esc=거부.
            (_, true) => b"\r",
            (_, false) => b"\x1b",
        };
        if let Some(pty) = self.pty_for_pane(pane_id) {
            let _ = pty.send_bytes(bytes);
        }
    }

    /// Sync the macOS window title (Dock label, ⌘-Tab switcher) with
    /// the active pane's resolved label when only one pane is open.
    /// Skipped for multi-pane workspaces — there the per-pane header
    /// strip carries the same information and the OS title gets
    /// noisy as the user shuffles focus between splits.
    pub(crate) fn maybe_update_window_title(&mut self) {
        // Throttle: scroll bursts can fire RedrawRequested 60+ times
        // per second, and every call here takes a workspace lock and
        // may shell out to `ps` for the process name. 200ms is fast
        // enough for "title follows focus" but cheap enough that a
        // wheel sweep stays smooth.
        let now = Instant::now();
        if let Some(t) = self.last_window_title_check {
            if now.duration_since(t).as_millis() < 200 {
                return;
            }
        }
        self.last_window_title_check = Some(now);
        // Native window title always tracks the focused pane. In a
        // split workspace this means macOS's Dock / ⌘-Tab label
        // updates when the user clicks a different split — matching
        // iTerm / Terminal.app.
        let active = {
            let ws = self.ws.lock().unwrap();
            let id = ws
                .active_pane
                .clone()
                .or_else(|| ws.panes.keys().next().cloned());
            let osc = id
                .as_ref()
                .and_then(|i| ws.panes.get(i))
                .and_then(|p| p.title.clone());
            id.map(|i| (i, osc))
        };
        let Some((id, osc)) = active else { return };
        let _ = osc;
        let label = self
            .pty
            .get(&id)
            .and_then(|p| p.shell_pid())
            .and_then(socket::pid_cwd)
            .map(|p| crate::session::tilde_home(&p.to_string_lossy()))
            .unwrap_or_else(|| Self::resolve_pane_label(&self.pty, &id, None));
        // Claude Code response indicator. Priority:
        //   1. Lift Claude Code's own status line straight from the
        //      cell grid ("✻ Brewed for 5s") so the user sees the
        //      same words they would on the prompt — including the
        //      live elapsed time.
        //   2. Fallback when only a spinner glyph is detected but no
        //      status text: cycle our own asterisk sequence
        //      ✶ ✳ ✶ · ✽ next to the "claude" label.
        // Note: we intentionally do NOT scrape the grid for the
        // "✻ Brewed for Ns" status line here. iTerm-style behavior is
        // to let the inner program drive the title via OSC 0/2 only
        // — Claude Code sends the conversation summary that way, and
        // the per-question status is meant to stay inside the pane.
        // Scraping the grid would clobber the conversation title
        // every few hundred ms with whatever Claude was rendering.
        if self.last_window_title.as_deref() == Some(&label) {
            return;
        }
        if let Some(w) = self.window.as_ref() {
            w.set_title(&label);
        }
        self.last_window_title = Some(label);
    }
    /// 사이드바 pane 줄이 쓰는 이름 — **붙인 이름 > OSC > 프로세스 이름** 순.
    ///
    /// 붙인 이름이 먼저인 이유: `title_pinned` 은 사람이나 에이전트가 이 pane 을
    /// 뭐라고 부를지 **직접 정했다**는 표시다(`surface.rename`·`kasaspace_rename`·
    /// 파일 탭). OSC 를 적용하는 쪽(`session.rs`)이 이미 같은 핀을 보고 비켜서므로,
    /// 여기서도 같은 핀을 봐야 두 곳이 같은 이름을 말한다.
    ///
    /// ⚠️ 핀과 OSC 는 **저장소가 다르다.** 핀이 막는 건 GUI 쪽 사본
    /// (`ws.panes[id].title`)뿐이고, PTY 는 `title_handle` 에 OSC 를 계속 받아 둔다.
    /// 그래서 예전처럼 `osc_title()` 만 읽으면 이름을 붙여도 줄은 안 바뀐다 — 붙인
    /// 이름이 사이드바에 닿는 길이 아예 없었다(2026-08-13 실측). GUI 사본을 먼저
    /// 보는 것이 그 길이다.
    ///
    /// 핀이 없으면 예전 규칙 그대로다: OSC 가 첫 번째 진실이고(claude 는 뜨자마자
    /// 「✳ Claude Code」를 보낸다) 프로세스 이름이 폴백이다. 즉 **이름을 안 붙인
    /// pane 의 그림은 하나도 안 바뀐다.**
    pub(crate) fn pane_row_label(&self, id: &str) -> String {
        // 고정 제목은 pane(leaf) 것이고 OSC 제목·프로세스 이름은 **PTY(=탭 pid)**
        // 것이다. 같은 락 통행에서 탭 pid 도 꺼내 아래 두 조회에 쓴다 — 접지 않으면
        // 탭으로 띄운 pane 의 줄이 자기 탭 대신 폴백 이름으로 떨어진다(얼굴이
        // 사라지던 `pane_claude_ready` 와 같은 병).
        let (pinned, tab) = {
            let ws = self.ws.lock().unwrap();
            let pinned = ws
                .panes
                .get(id)
                .filter(|p| p.title_pinned)
                .and_then(|p| p.title.clone())
                .filter(|s| !s.trim().is_empty());
            (pinned, ws.active_tab_pid(id))
        };
        if let Some(name) = pinned {
            return name;
        }
        self.pty
            .get(tab.as_str())
            .and_then(|p| p.osc_title())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Self::resolve_pane_label(&self.pty, &tab, None))
    }
    pub(crate) fn resolve_pane_label(
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        pane_id: &str,
        osc_title: Option<&str>,
    ) -> String {
        if let Some(t) = osc_title.filter(|s| !s.is_empty()) {
            return t.to_string();
        }
        if let Some(name) = pty.get(pane_id).and_then(|p| p.active_process_name()) {
            return decorate_process_name(&name);
        }
        std::env::current_dir()
            .ok()
            .map(|p| crate::session::tilde_home(&p.to_string_lossy()))
            .unwrap_or_else(|| "shell".to_string())
    }
    /// pane 의 현재 스크롤(logical px). 없는 pane 이면 0.
    pub(crate) fn md_scroll_of(&self, id: &str) -> f32 {
        self.ws
            .lock()
            .ok()
            .and_then(|ws| {
                ws.panes
                    .get(id)
                    .and_then(|p| p.markdown())
                    .map(|m| m.scroll)
            })
            .unwrap_or(0.0)
    }

    /// 노치 스크롤 한 프레임 보간. 목표에 지수로 붙고, 0.5px 안에 들면 목표에
    /// 붙이고 애니를 끝낸다. 반환값 = 아직 도는 애니가 있는지(프레임 펌프 조건).
    ///
    /// 감쇠를 프레임이 아니라 **경과 시간**으로 계산하는 이유: 디버그 빌드는 한
    /// 프레임이 수십 ms 씩 튀는데, 프레임당 고정 비율로 붙이면 같은 스크롤이
    /// 빌드마다 다른 속도로 보인다.
    pub(crate) fn tick_md_scroll(&mut self) -> bool {
        if self.md_scroll_anim.is_empty() {
            return false;
        }
        let now = Instant::now();
        let mut done: Vec<String> = Vec::new();
        if let Ok(mut ws) = self.ws.lock() {
            for (id, (target, last)) in self.md_scroll_anim.iter_mut() {
                // 첫 프레임이 창 리사이즈 등으로 늦게 오면 dt 가 커져 한 번에
                // 목표까지 튄다 — 관성이 사라지므로 상한을 둔다.
                let raw_dt = now.duration_since(*last).as_secs_f32();
                let dt = raw_dt.min(0.05);
                *last = now;
                let Some(pane) = ws.panes.get_mut(id) else {
                    done.push(id.clone());
                    continue;
                };
                let Some(m) = pane.markdown_mut() else {
                    done.push(id.clone());
                    continue;
                };
                let d = *target - m.scroll;
                if d.abs() < 0.5 {
                    m.scroll = *target;
                    done.push(id.clone());
                } else {
                    m.scroll += d * (1.0 - (-dt * 18.0).exp());
                }
                if std::env::var_os("KASATERM_WHEEL_DEBUG").is_some() {
                    eprintln!(
                        "[wheel]   tick 간격={:.1}ms scroll={:.1} → {:.1}",
                        raw_dt * 1000.0,
                        m.scroll,
                        target
                    );
                }
                pane.dirty = true;
            }
        }
        for id in done {
            self.md_scroll_anim.remove(&id);
        }
        !self.md_scroll_anim.is_empty()
    }

    /// 렌더 뷰 선택 텍스트. 낱말 사각형(마지막 프레임)에서 범위에 든 것을 읽는
    /// 순서로 이어 붙인다 — 줄이 바뀌면 개행. 사각형은 화면에 그려진 것만 있으므로
    /// 보이는 범위가 곧 복사 범위다(화면 밖 블록은 애초에 레이아웃을 건너뛴다).
    ///
    /// 클립보드 쓰기와 갈라 둔 이유는 검증 때문이다 — 헤드리스 하네스가 이걸 불러
    /// 결과를 로그로 찍으면 거노 클립보드를 건드리지 않고 정확성을 확인할 수 있다.
    pub(crate) fn md_render_selection_text(&self) -> Option<String> {
        let sel = self.md_render_sel.as_ref()?;
        let words = self.md_word_rects.get(&sel.pane)?;
        let scroll = self
            .ws
            .lock()
            .ok()
            .and_then(|ws| {
                ws.panes
                    .get(&sel.pane)
                    .and_then(|p| p.markdown())
                    .map(|m| m.scroll)
            })
            .unwrap_or(0.0);
        let screen = (
            sel.anchor.0,
            sel.anchor.1 - scroll,
            sel.end.0,
            sel.end.1 - scroll,
        );
        let mut hits: Vec<(f32, f32, &str)> = words
            .iter()
            .filter(|(x, y, w, h, _)| {
                crate::gpu::word_in_sel(Some(screen), x + w * 0.5, y + h * 0.5)
            })
            .map(|(x, y, _, _, t)| (*y, *x, t.as_str()))
            .collect();
        if hits.is_empty() {
            return None;
        }
        hits.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut out = String::new();
        let mut line_y = hits[0].0;
        for (y, _, t) in &hits {
            // 낱말 사이에 공백을 끼워 넣지 않는다 — 기록된 조각이 원문의 트레일링
            // 공백을 그대로 품고 있어서, 넣으면 `**굵게**,` 가 `굵게 ,` 로 벌어진다.
            if *y > line_y {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
            }
            line_y = *y;
            out.push_str(t);
        }
        Some(out.trim_end().to_string())
    }
    /// 렌더 뷰 선택을 클립보드로. true 면 이 호출이 복사를 처리했다.
    pub(crate) fn copy_md_render_selection(&self) -> bool {
        let Some(text) = self.md_render_selection_text() else {
            return false;
        };
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[kasaterm] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[kasaterm] clipboard open failed: {e}"),
        }
        true
    }
    pub(crate) fn copy_selection(&self) {
        if self.copy_md_render_selection() {
            return;
        }
        let Some(sel) = self.selection else {
            return;
        };
        let rows = {
            let ws = self.ws.lock().unwrap();
            match ws.active().and_then(|p| p.term()) {
                Some(t) => t.cells.clone(),
                None => return,
            }
        };
        let text = extract_selection(&rows, sel);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[kasaterm] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[kasaterm] clipboard open failed: {e}"),
        }
    }
    pub(crate) fn paste_clipboard(&self) {
        let mut cb = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                eprintln!("[kasaterm] clipboard open failed: {e}");
                return;
            }
        };
        // 텍스트가 있으면 무조건 텍스트 우선(bracketed paste). 일부 앱은 텍스트를
        // 복사해도 TIFF 표현을 같이 올려 get_image()가 Ok를 뱉는데, 이미지를 먼저
        // 검사하면 멀쩡한 텍스트 paste가 0x16으로 새버린다(거노: 붙여넣기 먹통).
        // 텍스트가 *없고* 이미지만 있을 때만 0x16을 흘려 claude code가 osascript로
        // 클립보드 PNG를 [Image] 칩으로 읽게 한다.
        if let Ok(text) = cb.get_text() {
            if !text.is_empty() {
                // 감싸개(`ESC[200~ … ESC[201~`)는 앱이 DECSET 2004 로 **켰을 때만**
                // 보낸다. 안 켠 앱은 저 바이트를 입력의 일부로 받는다 — `claude auth
                // login` 의 코드 프롬프트가 그래서 "Invalid code" 로 튕겼다(거노:
                // "붙여넣기가 안되는거같은데"). zsh·claude TUI 는 켜므로 평소 붙여넣기
                // 경험은 그대로다.
                let bracketed = self
                    .ws
                    .lock()
                    .unwrap()
                    .active()
                    .and_then(|p| p.term())
                    .map(|t| t.bracketed_paste)
                    .unwrap_or(false);
                if bracketed {
                    let mut payload = Vec::with_capacity(text.len() + 12);
                    payload.extend_from_slice(b"\x1b[200~");
                    payload.extend_from_slice(text.as_bytes());
                    payload.extend_from_slice(b"\x1b[201~");
                    self.send_bytes(&payload);
                } else {
                    // 감싸개가 없으면 줄바꿈이 곧 실행이다 — 실제 터미널과 같이
                    // CR 로 보낸다(LF 는 대부분의 라인 편집기가 안 먹는다).
                    let raw = text.replace("\r\n", "\r").replace('\n', "\r");
                    self.send_bytes(raw.as_bytes());
                }
                return;
            }
        }
        if cb.get_image().is_ok() {
            self.send_bytes(&[0x16]);
            return;
        }
        eprintln!("[kasaterm] paste: clipboard has neither text nor image");
    }
    pub(crate) fn handle_wheel(&mut self, delta: MouseScrollDelta) {
        let wdbg = std::env::var_os("KASATERM_WHEEL_DEBUG").is_some();
        let dy_cells = match delta {
            // Mouse wheel: winit normalises one notch to y=±1.0. At 0.3 cells a
            // notch it took ~4 notches to move a single row (wheel_step only
            // emits once |accum| ≥ 1). 3 cells/notch matches the 3-line default
            // most GUI terminals use, so a notch scrolls immediately.
            MouseScrollDelta::LineDelta(_, y) => y * 3.0,
            // Trackpad: pixel-precise, already smooth. 고해상도 마우스휠도 같은
            // PixelDelta 로 와서 **둘을 구분할 수가 없다** — 그래서 한때 마우스휠을
            // 살리려고 배율을 올리고 "2px 이상은 최소 1셀" 을 얹었더니 트랙패드가
            // 세 배 넘게 민감해졌다. 승격은 걷어내고 배율 하나로만 두되, 그 배율을
            // 설정에서 조절하게 해 마우스를 쓰는 날엔 올릴 수 있게 한다.
            MouseScrollDelta::PixelDelta(p) => {
                (p.y as f32) / self.cell.h.max(1.0) * wheel_pixel_gain()
            }
        };
        if wdbg {
            eprintln!(
                "[wheel] delta={delta:?} dy_cells={dy_cells:.4} accum_before={:.4} cursor_px=({:.1},{:.1})",
                self.wheel_accum_y, self.cursor_px.0, self.cursor_px.1
            );
        }
        // 상태줄 팝오버 위 — 목록이 길면 스크롤한다. 셀 단위 양자화 전에 가로채는
        // 건 이미지 pane 과 같은 이유로, 픽셀 스크롤이 끊기지 않게 하려는 것이다.
        if let Some((px, py, pw, ph)) = self.statusbar.popover_rect {
            let (cx, cy) = self.cursor_px;
            if cx >= px && cx <= px + pw && cy >= py && cy <= py + ph {
                let px_dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 40.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.statusbar.popover_scroll = (self.statusbar.popover_scroll - px_dy).max(0.0);
                self.chrome_dirty = true;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
                return;
            }
        }
        // Image pane: 트랙패드 두손가락(PixelDelta)=pan, 마우스 휠(LineDelta)=
        // Preview 식 zoom. wheel_step 양자화 *전* raw delta 로 처리해야 pan 이 안
        // 끊긴다. 커서가 이미지 pane 위일 때만 가로채고, 아니면 아래로 흘려보낸다.
        let hover_image = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .filter(|id| {
                self.ws
                    .lock()
                    .ok()
                    .and_then(|w| w.panes.get(id).map(|p| p.image().is_some()))
                    .unwrap_or(false)
            });
        if let Some(pid) = hover_image {
            match delta {
                MouseScrollDelta::PixelDelta(p) => {
                    // 두손가락 grab-pan — zoom>1 일 때만 의미(bounds 0 이면 no-op).
                    // 자연 스크롤 부호가 grab 과 반대라 빼서 이미지가 손가락을 따라온다.
                    let (mx, my) = self.image_pan_bounds(&pid);
                    if mx > 0.0 || my > 0.0 {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.panes.get_mut(&pid) {
                                pane.image_pan_x = (pane.image_pan_x - p.x as f32).clamp(-mx, mx);
                                pane.image_pan_y = (pane.image_pan_y - p.y as f32).clamp(-my, my);
                                pane.dirty = true;
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
                MouseScrollDelta::LineDelta(_, y) => {
                    // 마우스 휠 = Preview 식 줌(위로 굴리면 확대), 커서 기준.
                    let factor = if y > 0.0 { 1.25 } else { 1.0 / 1.25 };
                    self.image_zoom_by(&pid, factor, Some(self.cursor_px));
                }
            }
            return;
        }
        // Window-tab strip overflow: the wheel steps the windowed run of
        // whole tabs (win_tab_first). Top mode scrolls in the title strip,
        // side mode over the sidebar tab column. Raw deltas (pre wheel_step)
        // so trackpads stay smooth; consumed only while tabs actually
        // overflow, so a fitting strip changes nothing.
        {
            let n = self.windows.len();
            let vis = self.win_tab_vis.max(1);
            let (cx, cy) = self.cursor_px;
            let in_status_menu = self.statusbar.menu_rect.is_some_and(|(mx, my, mw, mh)| {
                cx >= mx && cx <= mx + mw && cy >= my && cy <= my + mh
            });
            let over_strip = if self.tabs_on_top {
                cy < TITLE_HEIGHT
            } else {
                self.tab_strip_w() > 0.0 && cx < self.tab_strip_w() && cy > TITLE_HEIGHT
            };
            // 세로 사이드바의 한계는 실제 카드 높이로 잰다 — `n - vis` 는 카드가
            // 다 같은 높이일 때만 맞는 근사라, 펼친 방이 섞이면 목록 끝에 못 닿는다.
            // 띠 위에 있을 때만 센다: 방마다 트리를 훑는 계산이라, 커서가 딴 데
            // 있는 굴림까지 매번 재면 그냥 버리는 일이 된다.
            // 세로 사이드바는 **픽셀**로 흐르고 가로 탭은 알약 한 칸씩 넘어간다.
            // 세는 단위가 달라 여기서 갈래를 나눈다. 카드 높이를 재는 계산은 방마다
            // 트리를 훑으므로 커서가 띠 위일 때만 한다 — 딴 데서 굴린 것까지 매번
            // 재면 그냥 버리는 일이 된다.
            let live = over_strip && !in_status_menu;
            let max_first = if live && self.tabs_on_top { n.saturating_sub(vis) } else { 0 };
            let sb_max_px = if live && !self.tabs_on_top {
                let win_h = self
                    .window
                    .as_ref()
                    .map(|w| w.inner_size().height as f32 / self.effective_scale())
                    .unwrap_or(800.0);
                self.sidebar_max_scroll(win_h)
            } else {
                0.0
            };
            if max_first > 0 || sb_max_px > 0.0 {
                // 가로 축은 상단 탭 모드에서 옆으로 확실히 그은 스와이프일 때만 쓴다.
                let d = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        if self.tabs_on_top && x.abs() > y.abs() {
                            x * 48.0
                        } else {
                            y * 48.0
                        }
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        let (px, py) = (p.x as f32, p.y as f32);
                        if self.tabs_on_top && px.abs() > py.abs() {
                            px
                        } else {
                            py
                        }
                    }
                };
                let moved = if sb_max_px > 0.0 {
                    // 굴린 만큼 그대로 흐른다. 카드 단위로 끊으면 손보다 화면이
                    // 앞서고, 카드 하나가 통째로 사라졌다 나타나 끊겨 보인다.
                    // d>0 = 위로 = 목록 앞쪽 = 스크롤 감소.
                    let next = (self.sidebar_scroll_px - d).clamp(0.0, sb_max_px);
                    let moved = next != self.sidebar_scroll_px;
                    self.sidebar_scroll_px = next;
                    moved
                } else {
                    // 가로 탭은 알약이 다 같은 크기라 48px 한 칸이 그대로 맞는다.
                    self.win_tab_wheel_accum += d;
                    let steps = (self.win_tab_wheel_accum / 48.0).trunc() as i64;
                    self.win_tab_wheel_accum -= steps as f32 * 48.0;
                    // steps>0 = wheel up/left = toward the first tab.
                    let next =
                        (self.win_tab_first as i64 - steps).clamp(0, max_first as i64) as usize;
                    let moved = next != self.win_tab_first;
                    self.win_tab_first = next;
                    moved
                };
                if moved {
                    self.chrome_dirty = true;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                return;
            }
        }
        // In-pane tab strip: the wheel over a pane's tab band steps that
        // pane's windowed run — only when its tabs overflow, so a fitting
        // strip falls through to the normal per-pane scroll.
        {
            let (cx, cy) = self.cursor_px;
            let target = self
                .pane_tab_rects
                .iter()
                .find(|(_, _, r)| {
                    cx >= r.0 - 6.0 && cx <= r.0 + r.2 + 6.0 && cy >= r.1 && cy <= r.1 + r.3
                })
                .map(|(pid, _, _)| pid.clone());
            if let Some(pid) = target {
                let overflow = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes
                        .get(&pid)
                        .map(|p| (p.tabs.len(), p.tab_first, p.tab_vis))
                        .filter(|(n, _, vis)| *n > *vis)
                };
                if let Some((n, first, vis)) = overflow {
                    let d = match delta {
                        MouseScrollDelta::LineDelta(x, y) => {
                            if x.abs() > y.abs() {
                                x * 48.0
                            } else {
                                y * 48.0
                            }
                        }
                        MouseScrollDelta::PixelDelta(p) => {
                            let (px, py) = (p.x as f32, p.y as f32);
                            if px.abs() > py.abs() {
                                px
                            } else {
                                py
                            }
                        }
                    };
                    // Same 48px-per-tab accumulator as the window strip — a
                    // gesture only ever drives one strip at a time.
                    self.win_tab_wheel_accum += d;
                    let steps = (self.win_tab_wheel_accum / 48.0).trunc() as i64;
                    if steps != 0 {
                        self.win_tab_wheel_accum -= steps as f32 * 48.0;
                        let next = (first as i64 - steps).clamp(0, (n - vis) as i64) as usize;
                        if next != first {
                            if let Ok(mut ws) = self.ws.lock() {
                                if let Some(p) = ws.panes.get_mut(&pid) {
                                    p.tab_first = next;
                                    p.dirty = true;
                                }
                            }
                            self.chrome_dirty = true;
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                    return;
                }
            }
        }
        // File-tree column scroll — handled BEFORE wheel_step so it uses the raw
        // delta: trackpad gets pixel-precise smooth scroll, the mouse wheel reacts
        // instantly per notch (no accumulate-to-a-whole-line quantise lag). Only
        // intercepts while the pointer is over the tree column; body_rect (the real
        // geometry the renderer fills each paint) drives max_scroll.
        if self.file_tree.visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.file_tree_col_x()
            && self.cursor_px.0 < self.file_tree_col_x() + self.file_tree_col_w()
        {
            let (_, start_y, _, visible_h) = self.file_tree.body_rect;
            // 첫 paint 전이면 body_rect 가 (0,0,0,0) — 스크롤 대신 redraw 로 geometry 를
            // 채우고 다음 휠부터 정상 동작.
            if (start_y, visible_h) == (0.0, 0.0) {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            let item_h = 26.0_f32;
            let content_h = self.file_tree.nodes.len() as f32 * item_h;
            let max_scroll = (content_h - visible_h).max(0.0);
            // 픽셀 스크롤량: 마우스휠은 노치당 ≈3행, 트랙패드는 픽셀 1:1(관성 그대로).
            // 아래로 굴리면(자연 스크롤) scroll 증가.
            let delta_px = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * item_h * 3.0,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            let next = (self.file_tree.scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.file_tree.scroll).abs() > 0.01 {
                self.file_tree.scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // 마크다운·편집기 pane 도 파일트리와 같은 이유로 wheel_step **앞에서** 잡는다.
        // 정수 셀로 양자화하면 트랙패드 픽셀 델타가 한 행 높이씩 튀고, 관성 꼬리는
        // 1셀 임계를 못 넘어 통째로 버려진다 — 문서 뷰는 셀 격자에 맞출 이유가 없다.
        // 크롬 오버레이(상태바 드롭다운·사이드 열)가 커서 아래 있으면 그쪽이 먼저
        // 먹어야 하므로 그 두 경우만 비켜 준다(target 이 활성 pane 으로 폴백하므로
        // 커서가 pane 밖에 있어도 여기 걸린다).
        let (cx, cy) = self.cursor_px;
        let over_menu = self
            .statusbar
            .menu_rect
            .is_some_and(|(mx, my, mw, mh)| cx >= mx && cx <= mx + mw && cy >= my && cy <= my + mh);
        let over_side_col = self.git.col_visible && cy > TITLE_HEIGHT && cx >= self.git_col_x();
        // Decide which pane handles this wheel: the pane the pointer is
        // hovering over. Falls back to the active pane if the pointer
        // is in a gutter. Multi-pane lets the user scroll inside any
        // pane regardless of which one currently has keyboard focus.
        let target_pane_id = self
            .px_to_pane_cell(cx, cy)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        // Markdown pane: scroll the laid-out document by pixels (it has no PTY
        // history to delegate to). Clamp to the content height the renderer
        // last published so it can't scroll past the end.
        let (is_md, is_raw) = {
            let ws = self.ws.lock().unwrap();
            target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id))
                .and_then(|p| p.markdown())
                .map_or((false, false), |m| (true, m.raw_mode))
        };
        if is_md && !over_menu && !over_side_col {
            // Raw editor: a horizontal wheel/trackpad component pans long code
            // lines under the fixed gutter. Clamp to the longest line so it
            // can't scroll into empty space past the end of the text.
            if is_raw {
                let (dx_px, dy_cmp) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * self.cell.w * 3.0, y),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                // Trackpad vertical swipes carry a stray horizontal component
                // (e.g. x:36, y:-98). Pan only when the gesture is decisively
                // horizontal (dx > 2×dy) — otherwise a plain up/down scroll
                // yanks long code lines sideways and the short ones vanish.
                if dx_px.abs() > dy_cmp.abs() * 2.0 && dx_px.abs() > 1.0 {
                    if let Some(id) = target_pane_id.as_deref() {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.panes.get_mut(id) {
                                let cw = self.cell.w;
                                if let Some(m) = pane.markdown_mut().filter(|m| !m.wrap) {
                                    // 캐시된 칸 수 — 제스처 프레임마다 버퍼를
                                    // 다시 훑지 않는다. 글자 수가 아니라 칸 수라야
                                    // 한글이 섞인 줄의 상한이 실제 폭과 맞는다.
                                    let longest = m.longest_cols();
                                    let max_h = (longest as f32 * cw - cw * 4.0).max(0.0);
                                    m.h_scroll = (m.h_scroll - dx_px).clamp(0.0, max_h);
                                }
                                pane.dirty = true;
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            if let Some(id) = target_pane_id.as_deref() {
                let visible_h = self.window.as_ref().map_or(400.0, |w| {
                    w.inner_size().height as f32 / (w.scale_factor() as f32 * self.ui_zoom)
                }) - TITLE_HEIGHT
                    - PANE_HEADER_HEIGHT
                    - 2.0 * PANE_INNER_Y;
                let content_h = self.md_content_h.get(id).copied().unwrap_or(0.0);
                let max_scroll = (content_h - visible_h).max(0.0);
                // 픽셀 스크롤량: 마우스휠은 노치당 3행, 트랙패드는 픽셀 1:1.
                // 아래로 굴리면(자연 스크롤) scroll 증가.
                let (dy_px, notch) = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * self.cell.h * 3.0, true),
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32, false),
                };
                if wdbg {
                    eprintln!("[wheel]   md pane={id} dy_px={dy_px:.2} max={max_scroll:.1}");
                }
                // 노치는 목표만 옮기고 실제 위치는 `tick_md_scroll` 이 따라간다 —
                // 한 번에 세 줄을 순간이동하면 계단으로 읽힌다. 이미 애니가 도는
                // 중이면 표시 위치가 아니라 **목표**에 누적해야 빠른 연타가 밀리지
                // 않는다. 트랙패드는 손가락이 곧 위치라 즉시 반영(보간하면 늦게
                // 미끄러진다).
                let mut moved = false;
                if notch {
                    let base = self
                        .md_scroll_anim
                        .get(id)
                        .map(|(t, _)| *t)
                        .unwrap_or_else(|| self.md_scroll_of(id));
                    let target = (base - dy_px).clamp(0.0, max_scroll);
                    if (target - self.md_scroll_of(id)).abs() > 0.5 {
                        self.md_scroll_anim
                            .insert(id.to_string(), (target, Instant::now()));
                        moved = true;
                    }
                } else {
                    self.md_scroll_anim.remove(id);
                    if let Ok(mut ws) = self.ws.lock() {
                        if let Some(pane) = ws.panes.get_mut(id) {
                            if let Some(m) = pane.markdown_mut() {
                                let next = (m.scroll - dy_px).clamp(0.0, max_scroll);
                                if (next - m.scroll).abs() > 0.01 {
                                    m.scroll = next;
                                    moved = true;
                                }
                            }
                            pane.dirty |= moved;
                        }
                    }
                }
                if moved {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            return;
        }
        let lines = match wheel_step(
            &mut self.wheel_accum_y,
            dy_cells,
            &mut self.last_wheel_emit,
            Instant::now(),
        ) {
            Some(l) => l,
            None => {
                if wdbg {
                    eprintln!(
                        "[wheel]   -> None (accum_after={:.4}, no emit)",
                        self.wheel_accum_y
                    );
                }
                return;
            }
        };
        // Open status-bar dropdown overlays everything, so the wheel scrolls
        // its list first when the pointer is inside it (a cwd with many subdirs
        // overflows the capped viewport). Whole-row steps to match the render.
        if let Some((mx, my, mw, mh)) = self.statusbar.menu_rect {
            let (cx, cy) = self.cursor_px;
            if cx >= mx && cx <= mx + mw && cy >= my && cy <= my + mh {
                let item_h = 24.0_f32;
                // lines>0 = wheel up = toward the top = less scroll.
                self.statusbar.menu_scroll =
                    (self.statusbar.menu_scroll - lines as f32 * item_h).max(0.0);
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }
        // Info 탭: 프로세스 목록 스크롤. clamp 는 렌더가 실제 행 수로 다시 하니
        // (draw_info_col) 여기선 하한만 잡고 넘긴다 — 목록 길이가 워커 스레드에서
        // 바뀌므로 입력 시점의 최대치는 이미 낡았을 수 있다.
        if self.git.col_visible
            && self.info.tab == state::SideTab::Info
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let next = (self.info.scroll - lines as f32 * 22.0).max(0.0);
            if (next - self.info.scroll).abs() > 0.01 {
                self.info.scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // 세션 기록 탭: 목록 스크롤. Info 와 같은 이유로 상한은 렌더가 잡는다
        // (행 수가 워커 스레드에서 바뀌어 입력 시점의 최대치는 낡았을 수 있다).
        if self.git.col_visible
            && self.info.tab == state::SideTab::Mcp
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let next = (self.mcp_col.scroll - lines as f32 * 22.0).max(0.0);
            if (next - self.mcp_col.scroll).abs() > 0.01 {
                self.mcp_col.scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        if self.git.col_visible
            && self.info.tab == state::SideTab::Sessions
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let next = (self.sessions_col.scroll - lines as f32 * 22.0).max(0.0);
            if (next - self.sessions_col.scroll).abs() > 0.01 {
                self.sessions_col.scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Git column: scroll the change list when the pointer is over it. Same
        // clamp idea as the file tree; the visible height is the band between
        // the header and the bottom button zone.
        if self.git.col_visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let item_h = 22.0_f32;
            // 기하는 그리기 쪽이 직전 프레임에 써 둔 것을 쓴다. 여기서 다시 세면
            // 반드시 어긋난다 — 예전엔 `헤더 68px · 버튼 44px` 로 자리를 어림하고
            // 내용은 `파일 수 × 22` 로 셌는데, 그 셈에는 섹션 머리 둘과 **펼친 diff
            // 줄이 통째로 빠져 있었다.** 그래서 diff 를 펼치면 목록은 화면 몇 배로
            // 길어지는데 상한은 파일 몇 개 몫 그대로라 끝까지 스크롤이 안 됐다.
            let (visible_h, content_h) = self.git.col_list_extent;
            let max_scroll = (content_h - visible_h).max(0.0);
            let delta_px = lines as f32 * item_h;
            let next = (self.git.col_scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.git.col_scroll).abs() > 0.01 {
                self.git.col_scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        if wdbg {
            eprintln!(
                "[wheel]   lines={lines} target_pane={:?} active={:?}",
                target_pane_id,
                self.ws.lock().unwrap().active_pane
            );
        }
        let (alt, hist_len, mouse_on, mouse_sgr, launch_screen) = {
            let ws = self.ws.lock().unwrap();
            let pane = target_pane_id.as_deref().and_then(|id| ws.panes.get(id));
            match pane.and_then(|p| p.term()) {
                Some(t) => (
                    t.alt_screen,
                    t.history.len(),
                    t.mouse_enabled,
                    t.mouse_sgr,
                    pane.is_some_and(|p| claude_launch_screen(&p.visible_text(0))),
                ),
                None => return,
            }
        };
        // Claude builds its welcome screen through several intermediate
        // redraws. Those frames are terminal history, not user-visible
        // conversation, and exposing them tears narrow launch cards apart.
        // Downward wheel input remains available to leave an older offset.
        if lines > 0 && launch_screen {
            return;
        }
        // 마우스를 받는 앱이라도 **본화면(non-alt)에 그리는 앱이면 휠을 넘기지
        // 않는다.** 넘기면 그 앱이 자기 버퍼를 굴리고 터미널 스크롤백(alacritty
        // display_offset)은 0 에 머무는데, 우리 대화 턴 헤더는 그 값이 0 이 아닐
        // 때만 뜬다(`TurnJump::header`) — 즉 넘기는 순간 헤더가 통째로 죽는다.
        //
        // claude 가 정확히 그 경우다. 본화면 렌더러(`tui: "default"`)로 돌면서
        // 마우스를 켜므로 휠이 전부 그쪽으로 갔고, 그래서 「올려다볼 때 위에 붙는
        // 질문 띠」가 안 떴다(2026-08-29 지적: "스크롤조금올리면 위에 고정돼서
        // 클릭하면 올라가고내려가고 그거 왜 안되지"). 스크롤백에 같은 내용이 그대로
        // 쌓여 있으니 터미널이 굴리는 편이 사용자에게 손해가 없고, 헤더는 절대 줄
        // 번호를 알아 한 번에 그 질문 자리로 간다.
        //
        // alt-screen 앱(vim·htop)은 그대로 넘긴다 — 그쪽은 터미널 스크롤백이 아예
        // 없어서 우리가 굴릴 것이 없다.
        if std::env::var_os("KASATERM_TURN_DEBUG").is_some() {
            eprintln!("[wheel] alt={alt} mouse_on={mouse_on} sgr={mouse_sgr} lines={lines}");
        }
        if mouse_on && mouse_sgr && alt {
            let (col, row) = self
                .px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                .unwrap_or((1, 1));
            let button = if lines > 0 { 64 } else { 65 };
            let count = lines.unsigned_abs().min(8) as usize;
            let single = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
            let payload: Vec<u8> = single.as_bytes().repeat(count.max(1));
            // For the tmux backend we name the pane explicitly so an
            // inactive-but-hovered pane scrolls instead of the focused
            // one. The pty backend is single-pane: the pane id is
            // already implicit.
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = payload
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(pty) = self.pty_for_pane(id) {
                    let _ = pty.send_bytes(&payload);
                }
            }
            return;
        }
        if alt {
            let esc: &[u8] = if lines > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = esc
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(pty) = self.pty_for_pane(id) {
                    let _ = pty.send_bytes(esc);
                }
            }
            return;
        }
        // Normal screen scrollback. PTY backend delegates to
        // alacritty's own scrollback (display_offset) — it tracks
        // scroll-region TUIs (claude code's pinned input) correctly,
        // unlike the old frame-diff shift heuristic. tmux backend
        // keeps the local history composition.
        let step = lines.unsigned_abs().min(8) as i32;
        let _ = hist_len;
        if let Some(id) = target_pane_id.as_deref() {
            if self.tmux.is_some() {
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(id) {
                        pane.dirty = true;
                        if let Some(t) = pane.term_mut() {
                            let s = step as usize;
                            if lines > 0 {
                                t.scroll_offset = (t.scroll_offset + s).min(hist_len);
                            } else {
                                t.scroll_offset = t.scroll_offset.saturating_sub(s);
                            }
                        }
                    }
                }
            } else if let Some(pty) = self.pty_for_pane(id) {
                // Positive `lines` = scroll up = toward older history.
                let off = pty.scroll(if lines > 0 { step } else { -step });
                if wdbg {
                    eprintln!("[wheel]   pty.scroll step={step} -> display_offset={off}");
                }
            } else if wdbg {
                eprintln!("[wheel]   no pty_for_pane({id}) -> NO-OP");
            }
        } else if wdbg {
            eprintln!("[wheel]   target_pane_id=None -> NO-OP");
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Insert text at the commit-message cursor (committed Hangul or a typed
    /// char). Char-indexed; advances the cursor by the inserted char count.
    pub(crate) fn git_commit_insert(&mut self, text: &str) {
        crate::lineedit::insert(&mut self.git.commit_msg, &mut self.git.commit_cursor, text);
    }
    /// 조합기의 주인을 `next` 로 옮긴다. 주인이 실제로 바뀔 때만 일한다.
    ///
    /// 조합기(`self.hangul`)와 preedit 은 App 에 하나뿐인데 이걸 쓰는 입구는
    /// 아홉 곳(터미널·편집기·별도창 셋·git 커밋·경로검색·트리검색·새이름·설정)
    /// 이다. 문맥이 바뀌어도 조합 상태가 그대로 남아 있어서, 터미널에서 "한" 을
    /// 치다 편집기를 클릭하면 그 "한" 이 **편집기에** 떨어지고(이상하게 쳐짐),
    /// Backspace 는 편집기 글자 대신 그 잔재를 갉아 아무것도 안 지워진다
    /// (이상하게 지워짐). 거노 실사고 — 편집기가 "못 쓸 정도" 였던 정체다.
    ///
    /// 남은 음절은 **떠나는 쪽에** 확정시킨다(macOS 가 포커스 이동 때 하는 것과
    /// 같다). 떠나는 쪽이 이미 사라졌으면(pane 닫힘·드롭다운 닫힘) 조용히 버린다
    /// — 갈 곳 없는 음절을 아무 데나 떨구는 게 잃는 것보다 나쁘다.
    pub(crate) fn ime_retarget(&mut self, next: crate::ImeFocus) {
        if self.ime_focus.as_ref() == Some(&next) {
            return;
        }
        let prev = self.ime_focus.replace(next);
        let pending = self.hangul.flush();
        self.preedit.clear();
        self.in_preedit = false;
        // 별도창(편집기·터미널 양쪽)은 프리에딧을 self 가 아니라 자기 창에
        // 스탬프한다. 주인이 바뀌었다는 건 그 조합이 끝났다는 뜻이니 전부
        // 비운다 — 안 그러면 조합 중이던 글자가 떠난 창에 유령으로 남는다.
        // (새 주인 창은 다음 키에서 다시 스탬프한다.)
        for a in self.aux_windows.iter_mut() {
            a.preedit.clear();
        }
        let (Some(text), Some(prev)) = (pending, prev) else {
            return;
        };
        match prev {
            crate::ImeFocus::Pane(id) => {
                // 탭이 있는 pane 은 leaf id 와 실제 pid 가 갈린다 —
                // `self.pty.get(id)` 로는 보조 탭을 못 찾는다.
                if let Some(s) = self.pty_for_pane(&id) {
                    let _ = s.send_bytes(text.as_bytes());
                }
            }
            crate::ImeFocus::Editor(id) => self.md_insert_into(&id, &text),
            crate::ImeFocus::AuxEditor(i) => self.aux_insert(i, &text),
            crate::ImeFocus::GitCommit => self.git_commit_insert(&text),
            crate::ImeFocus::McpAdd => self.mcp_add_insert(&text),
            crate::ImeFocus::RoomRename(_) => self.room_rename_insert(&text),
            crate::ImeFocus::PathSearch => self.statusbar_search_insert(&text),
            crate::ImeFocus::TreeSearch => self.file_tree_search_insert(&text),
            crate::ImeFocus::TreeNew => self.ft_edit_insert(&text),
            crate::ImeFocus::Settings => self.settings_insert_text(&text),
            crate::ImeFocus::WebAddr => self.web_addr_insert(&text),
            crate::ImeFocus::WebFind => self.web_find_insert(&text),
        }
    }
    /// Commit-input key entry with Hangul composition, mirroring
    /// `md_editor_input` for the single-line git commit field. macOS hands jamo
    /// through `event.text`; feed the shared composer, insert committed
    /// syllables, keep the preedit in `self.preedit` for the overlay. Non-jamo
    /// flushes the pending syllable first, then edits.
    pub(crate) fn git_commit_input(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        if is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::GitCommit);
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.git_commit_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.git_commit_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        self.git_commit_key(event);
    }
    /// Single-line editing for the commit field. 조작은 `lineedit` 한 벌을 쓴다 —
    /// 칸마다 커서 산수를 다시 짜면 한글 경계에서 하나씩 어긋난다. Enter 는 커밋을
    /// 실행하고 Esc 는 포커스를 뗀다(그 둘만 이 칸의 몫). 한글 조합은 이 함수 앞의
    /// `git_commit_input` 이 이미 흘려보냈다.
    pub(crate) fn git_commit_key(&mut self, event: &KeyEvent) {
        let act = crate::lineedit::key(
            &mut self.git.commit_msg,
            &mut self.git.commit_cursor,
            &event.logical_key,
        );
        match act {
            crate::lineedit::LineEditAction::Submit => {
                self.run_git_col_action(GitColBtn::Commit);
                return;
            }
            crate::lineedit::LineEditAction::Cancel => {
                self.git.commit_focused = false;
                self.preedit.clear();
                self.in_preedit = false;
                let _ = self.hangul.flush();
            }
            _ => {}
        }
        self.chrome_dirty = true;
    }
    /// 커서 자리에 글자를 넣는다 — 조합이 끝난 한글도, 포커스가 떠나며 확정된
    /// 음절(`ime_retarget`)도 여기로 온다.
    pub(crate) fn statusbar_search_insert(&mut self, text: &str) {
        crate::lineedit::insert(
            &mut self.statusbar.menu_search,
            &mut self.statusbar.menu_search_cursor,
            text,
        );
        self.statusbar.menu_scroll = 0.0;
    }
    /// Type-to-search for the open path dropdown, with the shared Hangul
    /// composer so Korean filters compose. 조작은 `lineedit` 한 벌 — 검색칸도
    /// 가운데를 고칠 수 있어야 한다. Esc 는 드롭다운을 닫고 Enter 는 첫 후보를
    /// 연다(그 둘만 이 칸의 몫). 목록이 실제로 달라졌을 때만 스크롤을 되감는다 —
    /// 커서만 옮겼는데 스크롤이 튀면 보던 자리를 잃는다.
    pub(crate) fn statusbar_menu_search_key(&mut self, event: &KeyEvent) {
        if is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::PathSearch);
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.statusbar_search_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        // 조합 중이던 자모를 지우는 백스페이스가 먼저다 — 완성 글자를 지우기 전에
        // 조합기 안의 것부터 물린다.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.statusbar_search_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let act = crate::lineedit::key(
            &mut self.statusbar.menu_search,
            &mut self.statusbar.menu_search_cursor,
            &event.logical_key,
        );
        match act {
            crate::lineedit::LineEditAction::Submit => {
                self.statusbar_menu_activate_first();
                self.chrome_dirty = true;
                return;
            }
            crate::lineedit::LineEditAction::Cancel => {
                self.statusbar.menu = None;
            }
            // 걸러지는 목록이 바뀐 키에만 맨 위로. 커서 이동에 되돌리면 ←→ 마다
            // 스크롤이 튄다.
            crate::lineedit::LineEditAction::Edited => self.statusbar.menu_scroll = 0.0,
            _ => {}
        }
        self.chrome_dirty = true;
    }
    /// 트리 검색칸의 커서 자리에 글자를 넣고 걸린 노드를 다시 모은다.
    pub(crate) fn file_tree_search_insert(&mut self, text: &str) {
        crate::lineedit::insert(
            &mut self.file_tree.search_query,
            &mut self.file_tree.search_cursor,
            text,
        );
        self.file_tree_search_collect();
        self.file_tree.scroll = 0.0;
    }
    /// Same search entry for the file-tree column's search box, on the shared
    /// `lineedit` 조작. Esc closes the box; the filtered node list is recomputed
    /// by `file_tree_search_collect` on each edit — 쿼리가 그대로인 커서 이동엔
    /// 안 돌린다(트리를 다시 훑을 이유가 없고 스크롤도 안 튄다).
    pub(crate) fn file_tree_search_key(&mut self, event: &KeyEvent) {
        if is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::TreeSearch);
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.file_tree_search_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.file_tree_search_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let act = crate::lineedit::key(
            &mut self.file_tree.search_query,
            &mut self.file_tree.search_cursor,
            &event.logical_key,
        );
        let cancelled = act == crate::lineedit::LineEditAction::Cancel;
        if cancelled {
            self.file_tree.search_active = false;
            self.file_tree.search_query.clear();
            self.file_tree.search_cursor = 0;
        }
        // 쿼리가 실제로 바뀐 키에만 다시 훑는다 — 커서 이동에도 돌리면 ←→ 를
        // 누를 때마다 트리를 재수집하고 스크롤이 맨 위로 튄다.
        if cancelled || act == crate::lineedit::LineEditAction::Edited {
            self.file_tree_search_collect(); // 빈 쿼리 → 트리 복원
            self.file_tree.scroll = 0.0;
        }
        self.chrome_dirty = true;
    }
    /// Name entry for the inline new-file/folder row. Enter creates the entry,
    /// Esc cancels; Hangul composes like the search box.
    /// 인라인 입력행(새 파일/폴더 또는 이름변경) 한 키. 두 모드의 편집 버퍼는
    /// `ft_edit_buf` 로 통일 — rename 이 있으면 그쪽, 없으면 new. Enter 는 모드에
    /// 맞는 commit, Esc 는 둘 다 취소. 한글 조합은 search 행과 동일 경로.
    pub(crate) fn file_tree_new_key(&mut self, event: &KeyEvent) {
        if is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::TreeNew);
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.ft_edit_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.ft_edit_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let act = match self.ft_edit() {
            Some((buf, cursor)) => crate::lineedit::key(buf, cursor, &event.logical_key),
            None => crate::lineedit::LineEditAction::Ignored,
        };
        match act {
            crate::lineedit::LineEditAction::Submit => {
                if self.file_tree.rename.is_some() {
                    self.commit_rename();
                } else {
                    self.commit_new_entry();
                }
            }
            crate::lineedit::LineEditAction::Cancel => {
                self.file_tree.new = None;
                self.file_tree.new_parent = None;
                self.file_tree.rename = None;
                self.file_tree.edit_cursor = 0;
            }
            _ => {}
        }
        self.chrome_dirty = true;
    }
    /// 인라인 입력행의 편집 버퍼와 그 커서 — rename 우선, 없으면 new. 둘은 서로
    /// 배타라 커서(`edit_cursor`)를 한 벌만 두고 열린 쪽에 붙인다.
    fn ft_edit(&mut self) -> Option<(&mut String, &mut usize)> {
        let ft = &mut self.file_tree;
        let buf = match (ft.rename.as_mut(), ft.new.as_mut()) {
            (Some((_, b)), _) => b,
            (None, Some((_, b))) => b,
            (None, None) => return None,
        };
        Some((buf, &mut ft.edit_cursor))
    }
    /// 커서 자리에 글자를 넣는다 — 조합이 끝난 한글도, 포커스가 떠나며 확정된
    /// 음절(`ime_retarget`)도 여기로 온다.
    pub(crate) fn ft_edit_insert(&mut self, text: &str) {
        if let Some((buf, cursor)) = self.ft_edit() {
            crate::lineedit::insert(buf, cursor, text);
        }
    }
    /// 지금 pane 이 스크롤백을 보고 있으면 살아 있는 끝으로 되돌린다.
    /// 이미 끝에 있으면 아무 일도 안 한다(공짜 질의라 매번 물어도 된다).
    fn follow_live_tail_now(&mut self) {
        let Some(id) = self.target_surface() else { return };
        let Some(sess) = self.pty_for_pane(&id) else { return };
        if sess.view_state().0 > 0 {
            sess.scroll_to_bottom();
        }
    }

    pub(crate) fn forward_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // 방 이름 편집 중이면 키는 전부 그쪽 것이다 — 여기서 안 가로채면 타이핑이
        // pane 의 셸로 새 나간다(이름을 고치다 셸에 명령이 찍힌다).
        if self.room_rename_key(event) {
            return;
        }
        // 웹 pane 주소창 편집도 같은 규칙 — 주소를 치다 셸에 새면 안 된다.
        if self.web_addr_key(event) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // 웹 pane 페이지 내 찾기 칸도 같은 규칙.
        if self.web_find_key(event) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Touch the input timer so the cursor stays solid for a beat and
        // the blink phase re-starts from "on" once it kicks in.
        self.last_input_at = Instant::now();
        // 사용자가 이 pane 에 키를 치면 turn-done cheer 를 걷는다(만세 → idle) —
        // 완료 직후~다음 입력 전까지만 학생이 양팔 만세로 서 있는다.
        if let Some(id) = self.target_surface() {
            self.turn_done_panes.remove(&id);
        }
        // Git commit field has focus: keystrokes edit the message, not the PTY.
        // (Click elsewhere blurs it — see the column's mouse handler.)
        if self.git.commit_focused {
            self.git_commit_input(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // MCP 탭의 추가 칸도 같은 자리에서 가른다 — 열려 있으면 키가 PTY 로 안 간다.
        if self.mcp_col.add.is_some() {
            self.mcp_add_input(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Open path dropdown is a modal search: keystrokes filter it, not the
        // PTY. (Branch menu has no search — its lists are short — so it falls
        // through.)
        if self
            .statusbar
            .menu
            .as_ref()
            .map(|(_, k)| matches!(k, StatusbarMenu::Path))
            .unwrap_or(false)
        {
            self.statusbar_menu_search_key(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Cmd+Delete (= Cmd+Backspace) on a selected tree row → trash it.
        // Gated on no text input having focus so it never eats shell editing
        // keys; the selection clears when a terminal pane is clicked.
        if !self.git.commit_focused
            && self.file_tree.new.is_none()
            && self.file_tree.rename.is_none()
            && !self.file_tree.search_active
        {
            let has_sel =
                self.file_tree.selected.is_some() || !self.file_tree.selected_more.is_empty();
            if has_sel {
                use winit::keyboard::{Key, NamedKey};
                let del = matches!(&event.logical_key, Key::Named(NamedKey::Delete))
                    || (self.modifiers.super_key()
                        && matches!(&event.logical_key, Key::Named(NamedKey::Backspace)));
                if std::env::var_os("KASATERM_KEY_DEBUG").is_some() {
                    eprintln!(
                        "[ftdel] has_sel={} del={} super={} key={:?}",
                        has_sel,
                        del,
                        self.modifiers.super_key(),
                        event.logical_key
                    );
                }
                if del {
                    self.delete_tree_selection();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
            }
        }
        // Inline new-file/folder or rename row is open: keystrokes name it.
        if self.file_tree.new.is_some() || self.file_tree.rename.is_some() {
            self.file_tree_new_key(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // File-tree search box has focus: keystrokes filter the tree.
        if self.file_tree.search_active {
            self.file_tree_search_key(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Past here the key is bound for the shell (or an image/raw pane), so
        // drop any tree selection — a later Cmd+Backspace then edits the shell
        // line, not the previously-clicked file.
        self.file_tree.selected = None;
        // Image panes have no PTY — repurpose keys for view control.
        //   +/=      zoom in     -    zoom out
        //   0        reset       r/R  rotate 90° CW
        // Every other key is swallowed (no shell to receive them).
        let is_image = {
            let ws = self.ws.lock().unwrap();
            ws.active().map(|p| p.image().is_some()).unwrap_or(false)
        };
        if is_image {
            let mut changed = false;
            // 키보드 줌은 active pane 중심(anchor 없음). id 를 먼저 떠서 헬퍼에 넘긴다.
            let active_id = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
            if let Key::Character(s) = &event.logical_key {
                match s.as_str() {
                    "+" | "=" => {
                        if let Some(id) = &active_id {
                            changed = self.image_zoom_by(id, 1.25, None);
                        }
                    }
                    "-" | "_" => {
                        if let Some(id) = &active_id {
                            changed = self.image_zoom_by(id, 1.0 / 1.25, None);
                        }
                    }
                    "0" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_zoom = 1.0;
                                pane.image_rot = 0;
                                pane.image_pan_x = 0.0;
                                pane.image_pan_y = 0.0;
                                changed = true;
                            }
                        }
                    }
                    "r" | "R" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_rot = (pane.image_rot + 1) % 4;
                                pane.image_pan_x = 0.0;
                                pane.image_pan_y = 0.0;
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if changed {
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Markdown panes have no PTY. In Raw mode plain keys edit the buffer;
        // in Render mode they're swallowed (scrolling is wheel-driven).
        // Cmd/Ctrl combos first get an editor-shortcut shot (save/paste), then
        // FALL THROUGH to the global block below — the old early-return made
        // Cmd+W type a 'w' into the buffer instead of closing the tab.
        let (is_md, is_raw) = {
            let ws = self.ws.lock().unwrap();
            ws.active().map_or((false, false), |p| match p.markdown() {
                Some(m) => (true, m.raw_mode),
                None => (false, false),
            })
        };
        if is_md {
            if self.host_mod() || self.modifiers.control_key() {
                if is_raw && self.md_editor_shortcut(event) {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // 터미널용 잔재 제안이 아래 Ctrl+E 수락 경로에 낚이지 않게 비운다.
                self.current_suggestion = None;
            } else {
                // Rendered 모드에서 글자를 치면 삼키는 대신 raw 로 넘어가 그
                // 글자를 살린다. 편집하려는 의도가 분명한데 아무 일도 안 일어나면
                // 왜 안 써지는지 화면이 설명해 주지 못한다. 방향키·PageUp 같은
                // 이동 키는 그대로 두고(스크롤은 휠), 버퍼를 바꾸는 키만 태운다.
                let mut raw_now = is_raw;
                if !raw_now && crate::markdown::md_mutating_key(event) {
                    let id = self.ws.lock().unwrap().active_pane.clone();
                    if let Some(id) = id {
                        self.set_md_mode(&id, true);
                        // 실제로 바뀌었는지 다시 읽는다 — 못 바꿨는데 편집 경로로
                        // 넘기면 씨딩 안 된 버퍼를 건드린다.
                        let ws = self.ws.lock().unwrap();
                        raw_now = ws
                            .panes
                            .get(&id)
                            .and_then(|p| p.markdown())
                            .is_some_and(|m| m.raw_mode);
                    }
                }
                if raw_now {
                    self.md_editor_input(event);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                return;
            }
        }
        // 여기부터는 터미널 pane 의 키다 — 조합기 주인을 이 pane 으로 옮긴다.
        // md pane 도 Cmd 조합이면 위 블록을 통과해 여기까지 흘러오므로 `is_md`
        // 로 걸러낸다. 안 그러면 편집기가 쥐고 있던 소유권을 PTY 없는 pane 이
        // 뺏어, 확정될 음절이 갈 곳을 잃는다.
        if !is_md {
            // id 를 별도 문으로 꺼내 락을 확실히 놓는다 — `if let` 조건식의
            // 임시 MutexGuard 는 2021 에디션에서 body 끝까지 살아, 안에서
            // ws 를 다시 잠그는 `ime_retarget` 과 자기 락에 물린다.
            let active = self.ws.lock().ok().and_then(|ws| ws.active_pane.clone());
            if let Some(id) = active {
                self.ime_retarget(crate::ImeFocus::Pane(id));
            }
        }
        // Typing snaps the active pane back to live tail. Other panes'
        // scroll offsets are left alone — switching focus by clicking
        // doesn't disturb where the user was reading.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(t) = ws.active_mut().and_then(|p| p.term_mut()) {
                if t.scroll_offset != 0 {
                    t.scroll_offset = 0;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
        // Inline-autosuggestion accept. When a ghost suggestion is on
        // screen the cursor is necessarily at the end of the typed line
        // (we clear the suggestion on any left/up/down motion), so it's
        // safe to repurpose → / End / Ctrl-E to accept it and Alt-F to
        // accept one word — matching zsh-autosuggestions / fish. Tab is
        // deliberately left to the shell's own completion. We send the
        // remainder to the PTY and grow input_buf so the next frame keeps
        // suggesting from the extended prefix.
        if let Some(sugg) = self.current_suggestion.clone() {
            if !sugg.is_empty() {
                use winit::keyboard::{KeyCode, PhysicalKey};
                let plain = !self.modifiers.alt_key() && !self.modifiers.super_key();
                let phys = match event.physical_key {
                    PhysicalKey::Code(c) => Some(c),
                    _ => None,
                };
                let accept_full = (matches!(event.logical_key, Key::Named(NamedKey::ArrowRight))
                    && plain)
                    || (matches!(event.logical_key, Key::Named(NamedKey::End)) && plain)
                    || (self.modifiers.control_key() && phys == Some(KeyCode::KeyE));
                let accept_word =
                    self.modifiers.alt_key() && phys == Some(KeyCode::KeyF) && !sugg.is_empty();
                if accept_full {
                    self.send_bytes(sugg.as_bytes());
                    self.input_buf.push_str(&sugg);
                    self.current_suggestion = None;
                    return;
                }
                if accept_word {
                    // One word = leading spaces + the run up to the next
                    // space boundary, so repeated Alt-F walks the line.
                    let mut end = 0usize;
                    let bytes = sugg.as_bytes();
                    while end < bytes.len() && bytes[end] == b' ' {
                        end += 1;
                    }
                    while end < bytes.len() && bytes[end] != b' ' {
                        end += 1;
                    }
                    let word = &sugg[..end];
                    self.send_bytes(word.as_bytes());
                    self.input_buf.push_str(word);
                    self.current_suggestion = None;
                    return;
                }
            }
        }
        // Modifier-bearing keys must NEVER reach the Hangul composer.
        // In Korean keyboard layout the C key still produces 'ㅊ' as
        // text, but Ctrl+C is meant for SIGINT / "copy" and Cmd+V for
        // paste — we look at the *physical* key for these, not the
        // IME-resolved logical key. While we're here, also forward the
        // standard control-letter byte for any Ctrl+<letter> combo
        // (Ctrl+L clears, Ctrl+D = EOF, etc), so shells and TUI apps
        // behave as users expect regardless of which keyboard layout
        // happens to be active.
        let host = self.host_mod();
        let ctrl = self.modifiers.control_key();
        if host || ctrl {
            use winit::keyboard::{KeyCode, PhysicalKey};
            if let PhysicalKey::Code(code) = event.physical_key {
                // Host-modifier shortcuts. macOS uses Cmd, Windows/Linux
                // use Ctrl+Shift — see `host_mod()`.
                if host {
                    // OS 키 자동반복(키를 누르고 있을 때 반복 발사되는 Pressed
                    // 이벤트, repeat=true)은 무시한다. Cmd 단축키는 전부 단발성
                    // 동작이라, 안 거르면 Cmd+D를 살짝 길게 누르는 것만으로
                    // split이 우르르 나가 pane이 증식하고 Cmd+W는 여러 pane을
                    // 한꺼번에 닫는다. 글자 타이핑 반복은 이 블록 밖이라 무관.
                    if event.repeat {
                        return;
                    }
                    if code == KeyCode::KeyC && self.selection.is_some() {
                        self.copy_selection();
                        return;
                    }
                    // Cmd+A → 활성 pane 화면 전체 선택. 터미널 입력줄엔 「전체
                    // 선택」이 없으니 macOS 관례가 기대하는 대상은 화면 텍스트다
                    // — 이어지는 Cmd+C 가 위의 copy_selection 으로 떨어진다.
                    if code == KeyCode::KeyA && !self.modifiers.shift_key() {
                        let extent = self
                            .ws
                            .lock()
                            .unwrap()
                            .active()
                            .and_then(|p| p.term())
                            .map(|t| (t.cols, t.rows));
                        if let Some((cols, rows)) = extent {
                            if cols > 0 && rows > 0 {
                                self.selection = Some(Selection {
                                    anchor: (0, 0),
                                    end: (cols - 1, rows - 1),
                                });
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        return;
                    }
                    // Cmd+Z → 셸 undo. ^_ 는 zsh/bash emacs 모드 기본
                    // 바인딩(undo)이고 TUI 에선 사실상 무동작이라 안전하다.
                    // Shift(redo)는 셸에 기본 위젯이 없어 흘려보낸다.
                    if code == KeyCode::KeyZ && !self.modifiers.shift_key() {
                        self.send_bytes(b"\x1f");
                        return;
                    }
                    if code == KeyCode::KeyV {
                        // Pasted text bypasses our key path, so we can't
                        // mirror it into input_buf — drop the suggestion
                        // prefix so we never suggest off a stale line.
                        self.input_buf.clear();
                        self.current_suggestion = None;
                        self.paste_clipboard();
                        return;
                    }
                    // Terminal.app-style split shortcuts. PTY mode only
                    // — tmux mode lets the daemon handle its own keys.
                    //   D       → horizontal (stacked, default)
                    //   Shift+D → vertical (side-by-side, macOS chord)
                    //   E       → vertical (Windows-friendly chord that
                    //              avoids the Shift-on-Shift conflict)
                    // On macOS host_mod_alt resolves to Shift so
                    // Cmd+Shift+D still flips to vertical. On
                    // Windows/Linux host_mod already owns Shift, so the
                    // dedicated KeyE binding is the practical one.
                    if code == KeyCode::KeyD {
                        let dir = if self.host_mod_alt() {
                            kasa_pty::SplitDir::Vertical
                        } else {
                            kasa_pty::SplitDir::Horizontal
                        };
                        if let Err(e) = self.split_active_pane(dir) {
                            eprintln!("[kasaterm] split failed: {e}");
                        }
                        return;
                    }
                    if code == KeyCode::KeyE {
                        if let Err(e) = self.split_active_pane(kasa_pty::SplitDir::Vertical) {
                            eprintln!("[kasaterm] split failed: {e}");
                        }
                        return;
                    }
                    // Cmd+Shift+R (Ctrl+Shift+R elsewhere) → rebuild the
                    // renderer's display state: swapchain, DPI scale, font
                    // metrics, glyph atlas. Browser hard-refresh parity, and
                    // the one recovery a terminal can offer for a window that
                    // came back wrong from another monitor — quitting would
                    // take every live pane with it. Plain Cmd+R is left to the
                    // shell (^R reverse-search).
                    if code == KeyCode::KeyR && self.modifiers.shift_key() {
                        self.refresh_renderer();
                        return;
                    }
                    // Close the focused tab (a multi-tab pane keeps its other
                    // tabs). Last-tab/last-pane close is left to the OS close
                    // button.
                    if code == KeyCode::KeyW {
                        self.close_active_tab();
                        return;
                    }
                    // Cmd+L → 활성 웹 pane 의 주소창(브라우저 관례). 웹 pane 이
                    // 아니면 흘려보낸다 — 터미널의 Cmd+L 은 원래 무동작이라
                    // 뺏는 것이 없다. 웹뷰가 key 일 때의 Cmd+L 은 WEB_CHORD_JS
                    // 가 같은 곳으로 보낸다.
                    if code == KeyCode::KeyL && !self.modifiers.shift_key() {
                        if let Some(pid) = self.active_web_pane() {
                            self.begin_web_addr_edit(&pid);
                            return;
                        }
                    }
                    // Cmd+F → 활성 웹 pane 의 페이지 내 찾기. 같은 게이트.
                    if code == KeyCode::KeyF && !self.modifiers.shift_key() {
                        if let Some(pid) = self.active_web_pane() {
                            self.begin_web_find(&pid);
                            return;
                        }
                    }
                    // Cmd+T → new window in the current session (PTY backend
                    // only; tmux owns its own windows). Cmd+1..9 switch to
                    // that window. Digit0 is font-reset above, so windows
                    // start at 1.
                    if code == KeyCode::KeyT && self.tmux.is_none() {
                        if self.modifiers.shift_key() {
                            // Cmd+Shift+T → 마지막에 닫은 pane 되살리기(ghostty).
                            // 닫을 때 적어 둔 레코드로 세션 복원과 같은 경로를 타므로
                            // claude 였던 pane 은 대화까지 --resume 으로 돌아온다.
                            self.reopen_closed_pane();
                        } else {
                            self.new_window();
                        }
                        return;
                    }
                    let win_digit = match code {
                        KeyCode::Digit1 | KeyCode::Numpad1 => Some(0),
                        KeyCode::Digit2 | KeyCode::Numpad2 => Some(1),
                        KeyCode::Digit3 | KeyCode::Numpad3 => Some(2),
                        KeyCode::Digit4 | KeyCode::Numpad4 => Some(3),
                        KeyCode::Digit5 | KeyCode::Numpad5 => Some(4),
                        KeyCode::Digit6 | KeyCode::Numpad6 => Some(5),
                        KeyCode::Digit7 | KeyCode::Numpad7 => Some(6),
                        KeyCode::Digit8 | KeyCode::Numpad8 => Some(7),
                        KeyCode::Digit9 | KeyCode::Numpad9 => Some(8),
                        _ => None,
                    };
                    if let Some(idx) = win_digit {
                        if self.tmux.is_none() {
                            self.switch_window(idx);
                            return;
                        }
                    }
                    // `[` / `]` cycle focus through panes in document
                    // order.
                    if code == KeyCode::BracketLeft {
                        self.cycle_focus(-1);
                        return;
                    }
                    if code == KeyCode::BracketRight {
                        self.cycle_focus(1);
                        return;
                    }
                    // Cmd+Option+Arrow → move focus to the spatially
                    // adjacent pane; add Shift to swap the two panes.
                    // iTerm uses the same chord. Gated on Option so plain
                    // Cmd+Arrow still reaches the shell (line start/end).
                    if self.modifiers.alt_key() {
                        let fdir = match code {
                            KeyCode::ArrowLeft => Some(FocusDir::Left),
                            KeyCode::ArrowRight => Some(FocusDir::Right),
                            KeyCode::ArrowUp => Some(FocusDir::Up),
                            KeyCode::ArrowDown => Some(FocusDir::Down),
                            _ => None,
                        };
                        if let Some(d) = fdir {
                            if self.modifiers.shift_key() {
                                self.swap_dir(d);
                            } else {
                                self.focus_dir(d);
                            }
                            return;
                        }
                    }
                }
                // Font zoom. macOS gates on Cmd (= host_mod); Windows/Linux
                // on plain Ctrl (Ctrl+= / Ctrl+- / Ctrl+0, matching Windows
                // Terminal, VS Code, and browsers). Ctrl+Shift+= also lands
                // here since `+` is Shift+`=`. macOS deliberately stays on
                // Cmd so plain Ctrl+letter still reaches the shell as a
                // control byte. Match BOTH the physical key (US layout
                // assumption) AND the logical key text — Korean / European
                // layouts may emit the same character from a different
                // physical position.
                let zoom_mod = if cfg!(target_os = "macos") {
                    host
                } else {
                    ctrl
                };
                if zoom_mod {
                    use winit::keyboard::Key;
                    let logical_str = match &event.logical_key {
                        Key::Character(s) => Some(s.as_str()),
                        _ => None,
                    };
                    // host_mod_alt (Win: Alt, mac: Shift) narrows the zoom to
                    // just the focused pane; without it, the whole UI zooms.
                    let pane_only = self.host_mod_alt();
                    match zoom_key(Some(code), logical_str) {
                        Some(ZoomKey::In) => {
                            if pane_only {
                                self.change_pane_font(0.1);
                            } else {
                                self.change_ui_zoom(0.1);
                            }
                            return;
                        }
                        Some(ZoomKey::Out) => {
                            if pane_only {
                                self.change_pane_font(-0.1);
                            } else {
                                self.change_ui_zoom(-0.1);
                            }
                            return;
                        }
                        Some(ZoomKey::Reset) => {
                            if pane_only {
                                self.reset_pane_font();
                            } else {
                                self.reset_ui_zoom();
                            }
                            return;
                        }
                        None => {}
                    }
                }
                // Ctrl+letter → the corresponding ASCII control byte.
                // This covers Ctrl+C → 0x03 (SIGINT), Ctrl+D → 0x04 (EOF),
                // Ctrl+L → 0x0c (clear), Ctrl+R → 0x12 (reverse search), etc.
                // Suppressed when host is engaged so Ctrl+Shift+letter
                // shortcuts on Windows/Linux don't double-fire as both a
                // shortcut and a control byte.
                if ctrl && !host {
                    // Ctrl+1..9 → 그 번호 pane 으로 점프. 번호는 ⌘[·⌘] 순환과 같은
                    // 문서 순서다. Ctrl+숫자를 골라 잡은 이유: 터미널 세계에 이 조합의
                    // 인코딩이 아예 없어(죽은 키) 셸·TUI 가 잃는 것이 없다 — 옵션+숫자는
                    // 특수문자·zsh 반복 인자 입력을 뺏는다(2026-08-17 「옵션키 번호로
                    // 할까 컨트롤키 번호로 할까」에 대한 답). ⌘+숫자는 방(윈도우) 전환에
                    // 이미 쓰고 있다. Shift·Alt 가 섞이면 우리 것이 아니다 — Windows 의
                    // host chord(Ctrl+Shift)와도 그 조건으로 갈린다.
                    if !self.modifiers.shift_key() && !self.modifiers.alt_key() && !event.repeat {
                        let pane_digit = match code {
                            KeyCode::Digit1 | KeyCode::Numpad1 => Some(0),
                            KeyCode::Digit2 | KeyCode::Numpad2 => Some(1),
                            KeyCode::Digit3 | KeyCode::Numpad3 => Some(2),
                            KeyCode::Digit4 | KeyCode::Numpad4 => Some(3),
                            KeyCode::Digit5 | KeyCode::Numpad5 => Some(4),
                            KeyCode::Digit6 | KeyCode::Numpad6 => Some(5),
                            KeyCode::Digit7 | KeyCode::Numpad7 => Some(6),
                            KeyCode::Digit8 | KeyCode::Numpad8 => Some(7),
                            KeyCode::Digit9 | KeyCode::Numpad9 => Some(8),
                            _ => None,
                        };
                        if let Some(idx) = pane_digit {
                            self.focus_pane_at(idx);
                            return;
                        }
                    }
                    let letter = match code {
                        KeyCode::KeyA => Some(b'\x01'),
                        KeyCode::KeyB => Some(b'\x02'),
                        KeyCode::KeyC => Some(b'\x03'),
                        KeyCode::KeyD => Some(b'\x04'),
                        KeyCode::KeyE => Some(b'\x05'),
                        KeyCode::KeyF => Some(b'\x06'),
                        KeyCode::KeyG => Some(b'\x07'),
                        KeyCode::KeyH => Some(b'\x08'),
                        KeyCode::KeyI => Some(b'\x09'),
                        KeyCode::KeyJ => Some(b'\x0a'),
                        KeyCode::KeyK => Some(b'\x0b'),
                        KeyCode::KeyL => Some(b'\x0c'),
                        KeyCode::KeyM => Some(b'\x0d'),
                        KeyCode::KeyN => Some(b'\x0e'),
                        KeyCode::KeyO => Some(b'\x0f'),
                        KeyCode::KeyP => Some(b'\x10'),
                        KeyCode::KeyQ => Some(b'\x11'),
                        KeyCode::KeyR => Some(b'\x12'),
                        KeyCode::KeyS => Some(b'\x13'),
                        KeyCode::KeyT => Some(b'\x14'),
                        KeyCode::KeyU => Some(b'\x15'),
                        KeyCode::KeyV => Some(b'\x16'),
                        KeyCode::KeyW => Some(b'\x17'),
                        KeyCode::KeyX => Some(b'\x18'),
                        KeyCode::KeyY => Some(b'\x19'),
                        KeyCode::KeyZ => Some(b'\x1a'),
                        _ => None,
                    };
                    if let Some(b) = letter {
                        // Flush any pending Hangul syllable before
                        // sending the control byte — typing Enter
                        // mid-syllable already does this; control
                        // letters should too.
                        if let Some(flushed) = self.hangul.flush() {
                            self.send_bytes(flushed.as_bytes());
                            self.preedit.clear();
                            self.in_preedit = false;
                        }
                        // Keep the autosuggest line buffer in sync with
                        // the control byte the shell is about to act on.
                        match b {
                            0x15 | 0x03 | 0x01 => self.input_buf.clear(), // Ctrl-U / Ctrl-C / Ctrl-A
                            0x17 => self.buf_pop_word(),                  // Ctrl-W
                            _ => {}
                        }
                        self.send_bytes(&[b]);
                        return;
                    }
                }
            }
        }
        // Backspace special: when the in-process Hangul composer is
        // mid-syllable, eat the backspace to chip a jamo off the
        // preedit rather than forwarding `\x7f` to the shell (which
        // would erase already-committed text instead).
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.hangul.backspace() {
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.in_preedit = !self.preedit.is_empty();
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }
        // Any non-character control flushes the composer first so a
        // pending 자모 doesn't get stranded when the user hits Enter /
        // arrow / escape mid-syllable.
        let is_control_key = matches!(
            event.logical_key,
            Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::Tab)
                | Key::Named(NamedKey::Escape)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::ArrowDown)
                | Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowRight)
        );
        // Stash the committed syllable instead of writing it now, so it
        // ships in the SAME write as the key's own bytes below. Two
        // separate writes let an async TUI (claude code / Ink) submit
        // on the trailing \r before it has applied the multibyte
        // syllable — the last char gets dropped. One atomic write keeps
        // "녕" and "\r" in the same stdin chunk.
        let mut commit_prefix: Vec<u8> = Vec::new();
        if is_control_key {
            if let Some(flushed) = self.hangul.flush() {
                commit_prefix.extend_from_slice(flushed.as_bytes());
            }
            self.preedit.clear();
            self.in_preedit = false;
        }
        // Readline-style delete shortcuts. Defaults match iTerm2 /
        // Terminal.app on macOS and Windows Terminal on Windows:
        //   Option/Alt+Backspace → `\e\x7f`  (backward-kill-word)
        //   host_mod+Backspace   → `\x15`    (unix-line-discard, Ctrl+U)
        // host_mod resolves to Cmd on macOS, Ctrl+Shift on Windows/Linux.
        // We match physical key so the Korean layout's mapped char
        // ('ㅣ' etc.) doesn't interfere.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.host_mod() {
                self.input_buf.clear();
                self.send_bytes(b"\x15");
                return;
            }
            // Ctrl+Backspace → backward-kill-word (Ctrl+W, 0x17). Windows'
            // standard word-erase chord; on macOS host_mod is Cmd so plain
            // Ctrl still lands here. Alt+Backspace does the same via `\e\x7f`.
            if self.modifiers.control_key() {
                self.buf_pop_word();
                self.send_bytes(b"\x17");
                return;
            }
            if self.modifiers.alt_key() {
                self.buf_pop_word();
                self.send_bytes(b"\x1b\x7f");
                return;
            }
        }
        let bytes: Vec<u8> = match &event.logical_key {
            // Shift+Enter / Option(Alt)+Enter insert a newline instead of
            // submitting. claude code reads a bare LF (0x0a, the byte
            // Ctrl+J sends) as a newline; plain Enter stays CR (0x0d),
            // which submits. We used to send ESC+CR here, but current
            // claude code / Ink doesn't treat that as a newline — so
            // multiline never engaged and the up-arrow fell through to
            // command history instead of moving between lines. claude
            // never negotiates the kitty keyboard protocol (no `CSI ? u`
            // in its startup modes), so CSI 13;2u wouldn't reach it
            // either; a raw LF is the portable answer.
            Key::Named(NamedKey::Enter) => {
                if self.modifiers.shift_key() || self.modifiers.alt_key() {
                    b"\n".to_vec()
                } else {
                    // Plain Enter submits the line: remember it for
                    // instant suggestions and reset the buffer.
                    if !self.input_buf.is_empty() {
                        self.autosuggest.record(&self.input_buf);
                    }
                    self.input_buf.clear();
                    self.current_suggestion = None;
                    // 스크롤을 올려 둔 채로 보냈으면 살아 있는 끝으로 따라 내려간다.
                    // 입력창은 렌더러가 맨 아래에 붙잡아 두므로 스크롤 위에서도 칠
                    // 수 있는데, 그대로 두면 정작 **대답이 화면 밖에서** 흐른다.
                    //
                    // 출력이 시야를 끌어내리지 않는 규칙과 축이 다르다 — 그쪽은
                    // 남이 떠드는 것(읽던 자리를 뺏기면 안 된다), 이쪽은 내가 방금
                    // 보낸 것(답을 보려고 보냈다).
                    self.follow_live_tail_now();
                    b"\r".to_vec()
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.input_buf.pop();
                b"\x7f".to_vec()
            }
            Key::Named(NamedKey::Tab) => {
                // Shift+Tab = CSI Z (backtab). 표준 시퀀스라 claude code 의 permission
                // 모드 순환·역방향 포커스 등이 이걸 기대한다. Shift 무시하고 \t 만
                // 보내면 backtab 이 영영 안 닿음.
                if self.modifiers.shift_key() {
                    b"\x1b[Z".to_vec()
                } else {
                    b"\t".to_vec()
                }
            }
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
            // Editing / navigation keys that carry no `event.text`, so without
            // an explicit arm they fall through and send nothing (the "Delete /
            // Home / End don't work" report). Delete carries a Ctrl modifier
            // (CSI mod 5) for forward-word-delete; Home/End use the CSI form
            // every shell understands.
            Key::Named(NamedKey::Delete) => {
                if self.modifiers.control_key() {
                    b"\x1b[3;5~".to_vec()
                } else {
                    b"\x1b[3~".to_vec()
                }
            }
            Key::Named(NamedKey::Home) => b"\x1b[H".to_vec(),
            Key::Named(NamedKey::End) => b"\x1b[F".to_vec(),
            Key::Named(NamedKey::PageUp) => b"\x1b[5~".to_vec(),
            Key::Named(NamedKey::PageDown) => b"\x1b[6~".to_vec(),
            Key::Named(
                nk @ (NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowRight
                | NamedKey::ArrowLeft),
            ) => {
                // Any cursor motion ends a suggestion: it only makes
                // sense while the cursor sits at the end of the typed
                // line. (→ at end-of-line is intercepted earlier as
                // accept when a suggestion is showing.)
                self.input_buf.clear();
                self.current_suggestion = None;
                let letter = match nk {
                    NamedKey::ArrowUp => 'A',
                    NamedKey::ArrowDown => 'B',
                    NamedKey::ArrowRight => 'C',
                    _ => 'D', // ArrowLeft
                };
                // Carry modifiers so claude code (Ink) / zsh see word-wise
                // and line-wise motion instead of a bare one-cell arrow.
                //   Option(Alt)+←/→ → ESC b / ESC f = backward/forward-word
                //   Cmd(super)+←/→  → ^A / ^E      = line start/end
                // Cmd+Option+arrow never reaches here — it's consumed above
                // as the pane-focus shortcut.
                //
                // 셸에는 readline 제어문자로 보내야 한다 — 전에 쓰던 CSI
                // 인코딩(`\x1b[H`/`\x1b[F`·`\x1b[1;3C`)은 zsh 기본 bindkey 에
                // 아예 없어서(실측: `^A`/`^E`·`^[b`/`^[f`·`^[OH` 만 있다)
                // 셸 프롬프트에서 조용히 무시됐다(거노: "터미널에서 커맨드
                // 이동·옵션 단어 이동이 안 돼"). alt-screen TUI(vim·less)는
                // 반대로 CSI 를 이해하고 ^A 가 딴 뜻(숫자 증가)이라, 화면
                // 상태로 인코딩을 가른다. DECCKM 도 같은 스냅샷에서 읽는다.
                let (app_cursor, alt_screen) = self
                    .ws
                    .lock()
                    .unwrap()
                    .active()
                    .and_then(|p| p.term())
                    .map(|t| (t.app_cursor, t.alt_screen))
                    .unwrap_or((false, false));
                if self.modifiers.super_key() {
                    match letter {
                        'D' if !alt_screen => b"\x01".to_vec(),
                        'C' if !alt_screen => b"\x05".to_vec(),
                        'D' => b"\x1b[H".to_vec(),
                        'C' => b"\x1b[F".to_vec(),
                        _ => format!("\x1b[{letter}").into_bytes(),
                    }
                } else if self.modifiers.control_key() {
                    // Windows / readline word-wise motion: Ctrl+←/→.
                    // CSI modifier 5 = Ctrl (1 + ctrl-bit 4). zsh, bash
                    // readline, and Ink all read this as forward/backward-word.
                    format!("\x1b[1;5{letter}").into_bytes()
                } else if self.modifiers.alt_key() {
                    // 위아래(Option+↑/↓)는 셸에 대응 위젯이 없어 CSI 그대로.
                    match letter {
                        'D' if !alt_screen => b"\x1bb".to_vec(),
                        'C' if !alt_screen => b"\x1bf".to_vec(),
                        _ => format!("\x1b[1;3{letter}").into_bytes(),
                    }
                } else {
                    // Plain arrow: honor the active pane's DECCKM. When the
                    // inner app (claude code / vim / readline) set
                    // application-cursor mode it expects SS3 (`ESC O A`);
                    // sending CSI (`ESC [ A`) there silently fails, which
                    // is why up/down line-navigation in the prompt did
                    // nothing while modified arrows still worked.
                    if app_cursor {
                        format!("\x1bO{letter}").into_bytes()
                    } else {
                        format!("\x1b[{letter}").into_bytes()
                    }
                }
            }
            _ => match event.text.as_ref() {
                Some(t) => {
                    if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                        eprintln!(
                            "[key] text={t:?} logical_key={:?} ime_active={} in_preedit={}",
                            event.logical_key, self.ime_active, self.in_preedit
                        );
                    }
                    // Hangul branch (macOS only). On macOS our
                    // set_ime_allowed(false) means the Korean keyboard
                    // layout hands jamo codepoints straight through on
                    // KeyboardInput.text; we feed them into the local
                    // Composer to dodge the NSTextInputContext first-
                    // key drop. Windows / Linux let the OS IME handle
                    // the whole composition (Ime::Preedit/Commit), so
                    // we skip this branch and forward whatever text the
                    // keyboard layer produced as-is.
                    #[cfg(target_os = "macos")]
                    if t.chars().count() == 1 {
                        if let Some(c) = t.chars().next() {
                            if (0x3130..=0x318F).contains(&(c as u32)) {
                                if let Some(commit) = self.hangul.feed(c) {
                                    // Remember the committed text + cursor
                                    // so the overlay can show it until the
                                    // shell echo catches up (cursor moves).
                                    let before = self.ws.lock().ok().and_then(|ws| {
                                        ws.active_pane.clone().and_then(|id| {
                                            ws.panes
                                                .get(&id)
                                                .and_then(|p| p.term())
                                                .map(|t| (t.cursor_row, t.cursor_col))
                                        })
                                    });
                                    self.commit_overlay = before.map(|b| (commit.clone(), b));
                                    self.input_buf.push_str(&commit);
                                    self.send_bytes(commit.as_bytes());
                                }
                                self.preedit = self.hangul.preedit().unwrap_or_default();
                                self.in_preedit = !self.preedit.is_empty();
                                // Preedit lives in the chrome overlay, not the
                                // PTY grid — without flagging chrome_dirty the
                                // damage gate skips the frame and the composing
                                // syllable only flickers in on blink ticks.
                                self.chrome_dirty = true;
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    // Non-Hangul ASCII / control characters: flush any
                    // pending Hangul syllable to the shell first, then
                    // forward the new character verbatim.
                    if !t.chars().all(|c| c.is_ascii() && !c.is_control()) {
                        if (self.ime_active || self.in_preedit)
                            && t.chars().any(is_hangul_codepoint)
                        {
                            return;
                        }
                    }
                    if let Some(flushed) = self.hangul.flush() {
                        commit_prefix.extend_from_slice(flushed.as_bytes());
                        self.input_buf.push_str(&flushed);
                        self.preedit.clear();
                        self.in_preedit = false;
                    }
                    // Mirror printable text into the autosuggest buffer.
                    // Control chars (e.g. a lone ESC sequence) don't grow
                    // the visible line, so they don't belong in the prefix.
                    if t.chars().all(|c| !c.is_control()) {
                        self.input_buf.push_str(t);
                    }
                    t.as_bytes().to_vec()
                }
                None => return,
            },
        };
        if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
            eprintln!(
                "[send] prefix={:?} bytes={:?} preedit={:?} in_preedit={} ime_active={}",
                String::from_utf8_lossy(&commit_prefix),
                String::from_utf8_lossy(&bytes),
                self.preedit,
                self.in_preedit,
                self.ime_active
            );
        }
        if commit_prefix.is_empty() {
            self.send_bytes(&bytes);
        } else if is_control_key {
            // claude code (Ink) reads stdin asynchronously and can act on
            // a trailing control byte (\r submit, arrow nav) before it has
            // applied the multibyte syllable in front of it — even when
            // both arrive in one write. Send the committed syllable, let
            // it land, then the control byte. The gap is below human
            // perception and only happens on the (rare) control keypress.
            self.send_bytes(&commit_prefix);
            std::thread::sleep(std::time::Duration::from_millis(12));
            self.send_bytes(&bytes);
        } else {
            // Plain text after a flush has no submit race — ship it in one
            // write so the syllable and the next char stay together.
            commit_prefix.extend_from_slice(&bytes);
            self.send_bytes(&commit_prefix);
        }
    }
}

/// True when a pane's grid shows a live "working" animation in its bottom rows:
/// Braille spinners (U+2800..U+28FF — npm, pure-prompt) or Dingbat stars
/// (U+2731..U+274F — Claude Code's ✻/✶/✷ thinking indicator). Claude clears
/// this line the moment it goes idle, so the absence of a marker is a reliable
/// idle signal. Only the last ~10 rows are scanned (the live status sits at the
/// bottom); scrollback above is ignored.
pub(crate) fn term_is_working(t: &TerminalPane) -> bool {
    rows_show_working(&t.cells)
}

/// 수식키 **단독** 입력인가.
///
/// 조합기를 쓰는 입구들은 하나같이 "자모도 Backspace 도 아니면 조합을 확정한다"로
/// 짜여 있는데, 그 규칙에 Shift 가 걸린다. "계"의 ㅖ 는 **Shift+ㅔ** 라, ㄱ 을 친
/// 뒤 Shift 를 누르는 순간 ㄱ 이 확정돼 "ㄱㅖ"가 된다(거노 실측 2026-08-04).
/// 수식키를 누르는 것은 조합을 끝내겠다는 뜻이 아니므로 조합기에 닿으면 안 된다.
///
/// 수식키가 **조합된** 단축키(Cmd+W 등)는 여기 안 걸린다 — 그때 logical_key 는
/// 글자 쪽이다. 걸리는 건 수식키만 눌린 프레임뿐이라 단축키 경로는 그대로다.
pub(crate) fn is_modifier_key(event: &KeyEvent) -> bool {
    is_modifier_logical(&event.logical_key)
}

/// `is_modifier_key` 의 판정부. winit `KeyEvent` 는 비공개 필드가 있어 테스트에서
/// 만들 수 없어 logical_key 만 따로 받는다.
fn is_modifier_logical(key: &winit::keyboard::Key) -> bool {
    use winit::keyboard::{Key, NamedKey};
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}

/// 줌 단축키(확대/축소/원래대로) 판정.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ZoomKey {
    In,
    Out,
    Reset,
}

/// `code`(물리 위치) 와 `logical`(그 키가 실제로 내놓은 문자)을 **둘 다** 본다.
/// 한글·유럽 배열은 같은 문자를 다른 물리 위치에서 내놓기 때문이다 — 물리키만
/// 보면 그 배열에서 Cmd+- 가 안 먹고, 문자만 보면 US 배열의 NumpadSubtract 를
/// 놓친다. 메인 창(`forward_key`)과 별도창(`aux_terminal_key`)이 같은 판정을
/// 쓰라고 한 벌만 둔다: 두 벌이면 한쪽만 고쳐진다(별도창이 실제로 그랬다).
///
/// winit `KeyEvent` 는 비공개 필드가 있어 테스트에서 만들 수 없으니
/// `is_modifier_logical` 과 같은 이유로 두 조각만 따로 받는다.
pub(crate) fn zoom_key(
    code: Option<winit::keyboard::KeyCode>,
    logical: Option<&str>,
) -> Option<ZoomKey> {
    use winit::keyboard::KeyCode;
    // `+` 는 Shift+`=` 라 같은 팔에 든다.
    if code == Some(KeyCode::Equal)
        || code == Some(KeyCode::NumpadAdd)
        || logical == Some("=")
        || logical == Some("+")
    {
        return Some(ZoomKey::In);
    }
    if code == Some(KeyCode::Minus)
        || code == Some(KeyCode::NumpadSubtract)
        || logical == Some("-")
        || logical == Some("_")
    {
        return Some(ZoomKey::Out);
    }
    if code == Some(KeyCode::Digit0) || code == Some(KeyCode::Numpad0) || logical == Some("0") {
        return Some(ZoomKey::Reset);
    }
    None
}

/// Whether the bottom of `cells` shows a live "agent working" indicator.
///
/// Scans the last 10 *non-blank* rows, not the last 10 physical rows. Claude
/// (and other TUIs) leave blank padding rows at the bottom of the grid, so
/// `cells[rows-10..]` can be all whitespace with the live status line
/// ("✢ Gitifying…") sitting just above the window — the bar then never shows
/// even while the spinner is clearly on screen. Anchor on the last non-blank
/// row instead (the same content range `visible_text`/peek report), which is
/// why peek saw the spinner but the working bar didn't.
///
/// Claude's live footer reads "✳ Verbing… (12s · esc to interrupt)". Three
/// signals, in order of reliability:
///   - "esc to interrupt" — exact, but TRUNCATED on narrow panes, so it can't
///     be the only cue.
///   - star dingbat (✢✳✻… U+2720–274F) + "…" ellipsis on the SAME line —
///     present at any width. The completion summary ("✻ Churned for 42s") keeps
///     the star but DROPS the ellipsis, so requiring both rejects it. (The old
///     bare-star check pinned the bar on forever after a turn.)
///   - braille spinner (other CLIs) — animation-only, safe on its own.
pub(crate) fn rows_show_working(cells: &[Vec<GridCell>]) -> bool {
    // 판정은 render 쪽과 한 곳에서 — 도트 위치와 busy 판정이 갈리면 학생만
    // 걸어다니고 헤더는 멈춰 있는(혹은 그 반대) 꼴이 된다. 오탐 이력은 거기 주석에.
    //
    // 전에는 행 판정(`spinner_row_col`)만 공유하고 스캔 창은 각자 가졌다(여기 10행,
    // 저기 30행). 그래서 todo 트리가 끼어 스피너가 10행 밖으로 밀리면 학생은 걷는데
    // 헤더 바는 멈추는 어긋남이 남았고, 「스피너 아래에 대화 마커가 없어야 한다」는
    // 문맥 조건도 행 하나만 봐서는 못 건다. 창까지 통째로 넘긴다.
    crate::render::find_claude_spinner(cells).is_some()
}

/// claude 가 대화를 compact 하는 중인가 — 화면 하단에 그 알림이 떠 있는지 본다.
///
/// 문구는 claude 번들에서 실측한 것이다(`Compacting conversation`·`compacting
/// history`). 추측한 패턴을 넣으면 감지가 조용히 실패하고 바가 영원히 안 뜨므로,
/// 실제 문자열에서 **가장 짧고 변하지 않을 조각**(`ompacting`)만 본다 — 대문자
/// 여부와 뒤에 붙는 말("conversation"·"history"·"…")이 버전마다 흔들려도 남는 부분이다.
///
/// compact 중에도 스피너는 돌기 때문에 `rows_show_working` 은 이미 true 다. 그래서
/// 이 판정은 working 을 **대체하지 않고 덧붙는다** — 상태를 「working」에서
/// 「compacting」으로 좁히는 데만 쓴다.
///
/// 스캔 창(하단 N행)을 쓰지 않고 `find_claude_spinner` 가 짚은 **행 하나**만 보는
/// 이유: 그 창은 todo 트리(~7행)와 입력박스(~4행)가 사이에 끼는 순간 통째로
/// 어긋난다. 실제로 하단 10행으로 뒀더니 알림이 맨 아랫줄에서 12행 위에 있어 한
/// 번도 안 걸렸다(2026-08-13 지적: "compacting 프로세스바도 안되네"). 바로 위
/// `rows_show_working` 이 같은 함정을 밟고 이미 창을 버렸는데, 여기에 그 창을 다시
/// 만들어 둔 것이었다.
///
/// 행 하나로 좁혀도 되는 건 알림이 스피너와 **같은 줄**에 뜨기 때문이고
/// (`✻ Compacting conversation… (3m 31s · ↓ 8.7k tokens)`), 덤으로 스크롤백에 굳은
/// 옛 알림 오탐이 사라진다 — `spinner_is_live` 가 이미 그 둘을 가른다.
///
/// 반환: `None` = compact 아님, `Some(pct)` = compact 중이고 `pct` 는 알림 바로
/// 아래 진행률 행(`▰▰▱ 45%`)에서 읽은 값. claude 는 진행률을 화면에만 내놓으므로
/// 글자에서 읽는 수밖에 없고, 그 행이 안 보이는 프레임은 `Some(None)` — 바는
/// 시간 루프로 폴백한다(2026-08-13 지시: 퍼센트 파싱해서 진짜 진행률로).
pub(crate) fn rows_show_compacting(cells: &[Vec<GridCell>]) -> Option<Option<u8>> {
    let (r, _) = crate::render::find_claude_spinner(cells)?;
    let text: String = cells[r].iter().map(|c| c.ch).collect();
    if !text.contains("ompacting") {
        return None;
    }
    // 진행률 행은 알림 바로 아래 붙는다(두 스샷 실측 공통). 2행까지만 보는 이유:
    // 더 내려가면 본문·statusline 의 우연한 %(디스크 사용률 등)를 주울 수 있는데,
    // 알림 직하 2행이라는 위치가 그 오염을 막는 담이다.
    let pct = cells.iter().skip(r + 1).take(2).find_map(|row| {
        let t: String = row
            .iter()
            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
            .collect();
        let p = t.find('%')?;
        let digits: String = t[..p]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            return None;
        }
        let n: u32 = digits.chars().rev().collect::<String>().parse().ok()?;
        Some(n.min(100) as u8)
    });
    Some(pct)
}

/// settings.json 의 `theme` 값이 `/config theme=` 뒤에 **그대로 타이핑해도
/// 안전한 토큰**인지. 값은 입력줄로 들어가므로 공백·제어문자가 섞이면 엉뚱한
/// 텍스트가 제출될 수 있다 — 표준 값(dark, light-daltonized, auto)과
/// custom:<slug>[:<slug>] 가 전부 이 문자군 안이다.
fn claude_theme_token(v: &str) -> Option<&str> {
    let ok = !v.is_empty()
        && v.len() <= 64
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_'));
    ok.then_some(v)
}

/// kasaterm 자신 + 자식 트리(PTY 셸·claude 들)의 (CPU %, RSS bytes) 합 —
/// 하단바 리소스 표시(2026-08-15 지시). 터미널의 체감 무게는 앱 하나가 아니라
/// 그 아래 도는 학생들까지라 트리로 합산한다. ps 한 번이라 5초 폴에 충분히 싸다.
/// 반환: (cpu 합, rss 합(bytes), 상위 목록, **트리 전체 프로세스 수**). 마지막 값이
/// 있어야 팝오버가 「그 외 N개」를 셀 수 있다 — 목록은 상위 몇 개뿐이라, 합계와
/// 목록 합이 어긋나 보이는 것을 그 한 줄이 설명한다(2026-08-16 「3.1G가 다 더하면
/// 아니지않나」).
/// 이 앱 밖의 앱 하나.
#[derive(Clone, Debug)]
pub(crate) struct AppUsage {
    /// 앱 본체(대표) pid — 종료 신호를 여기로 보내면 자식도 함께 정리된다.
    pub(crate) pid: u32,
    pub(crate) rss: u64,
    pub(crate) name: String,
    /// 이 앱이 낳은 프로세스 수. 이 줄이 **합**이라는 표시다.
    pub(crate) procs: usize,
    /// 직전 폴 이후 **구간** 사용률(%). 코어 하나를 다 쓰면 100.
    pub(crate) cpu: f32,
    /// 그 사용률이 이어진 폴 수. 잠깐 바쁜 것과 계속 태우는 것을 가르는 값이다.
    pub(crate) hot: u16,
}

/// 폴 수가 「계속 태우는 중」에 이르렀는가. 목록에 안 담기는 우리 자신도 같은
/// 잣대를 써야 해서 자유함수로 둔다.
pub(crate) fn is_hot(hot: u16) -> bool {
    hot >= hog_polls()
}

impl AppUsage {
    /// 코어를 **계속** 태우는 중인가 — 팬이 도는 이유는 대개 이것이다. 잠깐
    /// 튀는 것과 가르려고 폴 수까지 본다.
    pub(crate) fn is_hog(&self) -> bool {
        is_hot(self.hot)
    }
}

/// 앱별 CPU 시간을 폴 사이에 이어 두는 자리.
///
/// `ps` 의 `%CPU` 를 그대로 믿으면 안 된다 — 커널이 주는 최근 추정치라 튀고,
/// 잠깐 바쁜 것과 몇 시간째 코어를 태우는 것을 구별하지 못한다. 팬을 돌리는
/// 것은 후자뿐이다. 누적 CPU 시간의 **증분**을 폴 간격으로 나누면 그 구간의
/// 실제 사용률이 나오고, 그게 몇 번 이어졌는지까지 세면 둘이 갈린다.
#[derive(Default)]
pub(crate) struct CpuTrack {
    /// 대표 pid → (그 앱의 누적 CPU 시간 ms, 잰 시각).
    last: std::collections::HashMap<u32, (u64, Instant)>,
    /// 대표 pid → 임계를 넘긴 채 이어진 폴 수.
    streak: std::collections::HashMap<u32, u16>,
}

impl CpuTrack {
    /// 한 「앱」의 누적 CPU 시간을 받아 구간 사용률과 이어진 폴 수를 낸다.
    /// 바깥 앱과 이 앱 자신이 같은 잣대를 쓰도록 한 곳에 모은다.
    fn tick(&mut self, root: u32, cpu_time: u64, now: Instant) -> (f32, u16) {
        // 첫 표본에는 견줄 데가 없어 0 이다. 5초 뒤 두 번째 폴부터 값이 선다.
        let cpu = match self.last.get(&root) {
            Some((prev_ms, prev_at)) => {
                let dt = now.duration_since(*prev_at).as_millis() as f32;
                // 너무 짧은 간격은 나눗셈이 튄다. 폴은 5초라 정상 경로에서는
                // 걸리지 않고, 창을 다시 그리며 두 번 불릴 때만 걸린다.
                (dt >= 1000.0)
                    // 프로세스가 죽어 합이 줄면 음수가 되므로 포화 뺄셈.
                    .then(|| cpu_time.saturating_sub(*prev_ms) as f32 / dt * 100.0)
                    .unwrap_or(0.0)
            }
            None => 0.0,
        };
        let n = self.streak.entry(root).or_default();
        // 한 번 내려갔다고 0 으로 되돌리지 않는다. 실측에서 스핀 루프가 임계를
        // 넘나들었는데, 리셋하면 그런 앱은 아무리 오래 돌아도 카운터가 못 쌓여
        // 영영 안 잡힌다. 대신 한 칸씩 물러나게 해서, 정말 멈춘 앱은 같은 시간
        // 안에 목록에서 빠진다.
        *n = if cpu >= hog_pct() { n.saturating_add(1) } else { n.saturating_sub(1) };
        let hot = *n;
        // 다음 폴이 견줄 자리. 이 한 줄이 빠지면 `last` 가 영영 비어 있어 모든
        // 사용률이 0 으로 나온다 — 화면에는 「아무도 CPU 를 안 쓴다」로 보인다.
        self.last.insert(root, (cpu_time, now));
        (cpu, hot)
    }
}

/// 코어 하나를 사실상 다 쓰는 선. 90 이 아니라 85 인 것은 실측 때문이다 —
/// 코어를 꽉 채워 도는 스핀 루프조차 기계가 바쁘면 88~96% 를 오르내려서
/// (2026-08-28), 90 으로 잡으면 명백한 폭주가 임계 아래로 새는 폴이 생긴다.
const HOG_PCT: f32 = 85.0;
/// 이만큼 이어져야 「계속 태우는 중」으로 본다. 5초 폴이라 약 1분 —
/// 빌드·영상 인코딩 한 판은 이 안에 끝나거나 오르내리므로 걸리지 않는다.
const HOG_POLLS: u16 = 12;

/// 두 임계 모두 env 로 덮을 수 있다. 위 값은 이 기계 한 대의 표본에서 나온
/// 추정이라 코어 수나 쓰는 앱이 다르면 어긋나고, 무엇보다 검증에 필요하다 —
/// 기본값대로면 화면 한 장을 보려고 1분을 태워야 한다.
fn hog_pct() -> f32 {
    std::env::var("KASATERM_CPU_HOG_PCT").ok().and_then(|v| v.parse().ok()).unwrap_or(HOG_PCT)
}

fn hog_polls() -> u16 {
    std::env::var("KASATERM_CPU_HOG_POLLS").ok().and_then(|v| v.parse().ok()).unwrap_or(HOG_POLLS)
}

/// 그 프로세스가 **실제로** 쥔 물리 메모리(byte).
///
/// `ps` 의 RSS 를 그대로 더하면 안 된다. 공유 라이브러리가 매핑된 프로세스마다
/// 다시 세어져서, 같은 앱을 여럿 띄우면 합이 크게 부푼다 — 실측 2026-08-29 에
/// claude 여섯의 ps 합이 2377MB 인데 실제는 1564MB(66%) 였다. 학생이 스무 명
/// 도는 자리에서는 그 허수가 몇 기가가 되고, 그러면 「13.7기가인데 안에는
/// 그만큼 없다」가 된다(같은 날 지적). 커널이 그 몫을 뺀 값을 따로 갖고 있으니
/// 프로세스마다 그걸 묻는다 — 시스템 콜 하나라 `ps` 한 번보다 훨씬 싸다.
#[cfg(target_os = "macos")]
fn phys_footprint(pid: u32) -> Option<u64> {
    // SAFETY: 읽기 전용 조회다. 우리가 잡은 버퍼를 넘기고, 커널이 성공을
    // 돌려줬을 때만 그 안을 읽는다. flavor 와 구조체 판이 짝을 이룬다(V2).
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            std::ptr::addr_of_mut!(info).cast(),
        );
        // 남의 프로세스는 권한이 없어 실패한다. 그때는 부르는 쪽이 RSS 로 돌아간다.
        (rc == 0).then_some(info.ri_phys_footprint)
    }
}

#[cfg(not(target_os = "macos"))]
fn phys_footprint(_pid: u32) -> Option<u64> {
    None
}

/// 칩·토스트에 넣을 만큼 짧은 앱 이름.
///
/// 앱 이름은 길다(`Google Chrome Helper (Renderer)`). 자르지 않으면 하단바에서
/// 왼쪽 이웃(포트 칩)을 밀어낸다.
pub(crate) fn short_app_name(name: &str) -> String {
    let short: String = name.chars().take(14).collect();
    if short.chars().count() < name.chars().count() {
        format!("{short}…")
    } else {
        short
    }
}

/// `ps` 의 누적 CPU 시간(`[[DD-]HH:]MM:SS[.ss]`)을 밀리초로.
fn cpu_ms(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let mut secs = 0.0f64;
    for part in rest.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((secs * 1000.0) as u64 + days * 86_400_000)
}

/// 한 번 잰 프로세스 사용량.
pub(crate) struct UsageSample {
    /// 이 앱 트리의 cpu 합(%)·rss 합(byte).
    pub(crate) cpu: f32,
    pub(crate) rss: u64,
    /// 트리 상위 몇 개 — (pid, cpu%, rss byte, 이름).
    pub(crate) top: Vec<(u32, f32, u64, String)>,
    /// 트리에 든 프로세스 수 전체.
    pub(crate) rows: usize,
    /// 이 앱 트리의 구간 사용률(%)과 그게 이어진 폴 수. 바깥 앱과 같은 잣대로
    /// 재되 목록에는 안 넣는다 — 자기 자신에게 끄기 버튼을 붙일 수는 없으니
    /// 말만 하고 손은 사람이 쓴다(2026-08-28 지시: 「카사텀 자신도 짚게」).
    pub(crate) self_cpu: f32,
    pub(crate) self_hot: u16,
    /// 트리 **밖**의 앱 상위 몇. 「메모리 부족」일 때 무엇을 닫아야 하는지가
    /// 이 목록이다(2026-08-27 지시: 「위험! 종료할까요?(뭔지)」). 계속 코어를
    /// 태우는 앱이 있으면 그쪽이 먼저 온다 — 팬이 도는 이유가 그것이라서다.
    pub(crate) outside: Vec<AppUsage>,
}

fn sample_process_tree_usage(track: &mut CpuTrack) -> Option<UsageSample> {
    // `spawn` + `wait_with_output` 인 것은 **재는 도구가 결과에 끼기 때문**이다.
    // `ps` 는 우리 자식이라 트리에 들고, `output()` 은 그 pid 를 안 알려줘서 뺄
    // 수가 없다. 실제로 목록 맨 아래에 `ps 0.0% 1MB` 가 늘 앉아 있었다.
    let child = std::process::Command::new("ps")
        // uid 를 함께 읽는 것은 바깥 앱 목록 때문이다 — 거기엔 끄기 버튼이 붙는데,
        // 남(root 데몬)의 프로세스는 눌러도 권한이 없어 아무 일도 안 일어난다.
        // `time` 은 누적 CPU 시간 — 폴 사이의 증분이 그 구간의 실제 사용률이다.
        .args(["-axo", "uid=,pid=,ppid=,pcpu=,rss=,time=,comm="])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let ps_pid = child.id();
    let out = child.wait_with_output().ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<Row> = txt
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let uid = it.next()?.parse().ok()?;
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let cpu = it.next()?.parse().ok()?;
            let rss = it.next()?.parse().ok()?;
            let cpu_time = cpu_ms(it.next()?)?;
            // comm 은 경로이고 공백을 품을 수 있다(`.../Google Chrome`). 남은 걸
            // 통째로 이어 붙인 뒤 마지막 조각만 쓴다 — 열 단위로 자르면 공백 있는
            // 이름에서 파싱이 통째로 어긋난다.
            let comm: String = it.collect::<Vec<_>>().join(" ");
            let name = comm.rsplit('/').next().unwrap_or(&comm).to_string();
            // 커널이 아는 실제 값으로 갈아 끼운다. 못 읽으면(남의 프로세스)
            // `ps` 의 RSS 를 그대로 둔다 — 부풀려진 값이라도 없는 것보다 낫다.
            let rss = phys_footprint(pid).map_or(rss, |b| b / 1024);
            Some(Row { uid, pid, ppid, cpu, rss, cpu_time, name })
        })
        .collect();
    let me = std::process::id();
    let mut tree: std::collections::HashSet<u32> = std::collections::HashSet::from([me]);
    // ppid 순서가 임의라 고정점까지 돈다 — 트리 깊이만큼(셸→claude→도구, 얕다).
    loop {
        let before = tree.len();
        for r in &rows {
            if tree.contains(&r.ppid) {
                tree.insert(r.pid);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    let (mut cpu, mut rss_kb) = (0.0f32, 0u64);
    // 행을 버리지 않고 남긴다 — 합계만 알면 "많이 쓴다" 까지만 알고 "무엇이" 는
    // 모른다(2026-08-15 지시 「사용량도 펼쳐져서 보이게 뭐가 잡아먹는지」). 목록과
    // 합계가 **같은 표본**에서 나와야 둘이 어긋나 보이지 않는다.
    let mut top: Vec<(u32, f32, u64, String)> = Vec::new();
    for r in &rows {
        if tree.contains(&r.pid) && r.pid != ps_pid {
            cpu += r.cpu;
            rss_kb += r.rss;
            top.push((r.pid, r.cpu, r.rss, r.name.clone()));
        }
    }
    // CPU 가 먼저다 — 지금 느린 이유를 찾는 것이 이 목록의 쓸모고, 메모리는 0.1%
    // 짜리들 사이의 순서를 정하는 데만 쓴다.
    top.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(b.2.cmp(&a.2))
    });
    let total_rows = top.len();
    // 상위 몇 개까지 담을지. 실측(2026-08-27, 이 기계 트리 231개 8.4G)에서 상위
    // 8개는 39%, 20개는 80%, 30개는 88% 였다 — 30 부터는 한 개당 20MB 아래로
    // 떨어져 더 담아도 답이 안 바뀐다. 화면에는 팝오버가 스크롤로 보여 준다.
    top.truncate(30);
    // 트리 전체를 한 「앱」으로 놓고 바깥과 같은 잣대로 잰다. 대표 pid 는 우리
    // 자신이라 바깥 목록의 키와 겹치지 않는다(우리는 거기서 빠져 있다).
    let self_time: u64 =
        rows.iter().filter(|r| tree.contains(&r.pid)).map(|r| r.cpu_time).sum();
    let (self_cpu, self_hot) = track.tick(me, self_time, Instant::now());
    Some(UsageSample {
        cpu,
        rss: rss_kb * 1024,
        rows: total_rows,
        outside: outside_apps(&rows, &tree, track),
        top,
        self_cpu,
        self_hot,
    })
}

/// `ps` 한 줄.
struct Row {
    uid: u32,
    pid: u32,
    ppid: u32,
    cpu: f32,
    rss: u64,
    /// 누적 CPU 시간(ms).
    cpu_time: u64,
    name: String,
}

/// 이 앱 트리 **밖**에서 메모리를 많이 쥔 것들을 앱 단위로 묶어 상위 몇 개.
///
/// 프로세스 하나씩 세면 안 된다 — Chrome·Electron 앱은 Helper 를 수십 개 낳아서,
/// 12G 를 쥔 Chrome 이 300MB 짜리 마흔 개로 흩어져 목록 어디에도 안 보인다.
/// launchd(1) 바로 아래 조상을 그 앱의 대표로 삼으면 Helper 가 전부 본체로
/// 접히고, 대표에게 종료 신호를 보내면 자식도 함께 정리된다.
fn outside_apps(
    rows: &[Row],
    tree: &std::collections::HashSet<u32>,
    track: &mut CpuTrack,
) -> Vec<AppUsage> {
    use std::collections::HashMap;
    let me_uid = unsafe { libc::getuid() };
    let parent: HashMap<u32, u32> = rows.iter().map(|r| (r.pid, r.ppid)).collect();
    let app_root = |mut pid: u32| -> u32 {
        // 상한은 순환 방어다. ppid 는 커널이 주지만 표본이 찍히는 사이 부모가
        // 죽으면 자식이 1 로 재부모되므로 사슬이 늘 온전하지는 않다.
        for _ in 0..32 {
            match parent.get(&pid) {
                Some(&pp) if pp > 1 => pid = pp,
                _ => break,
            }
        }
        pid
    };
    // (rss 합, 프로세스 수, 누적 CPU 시간 합)
    let mut apps: HashMap<u32, (u64, usize, u64)> = HashMap::new();
    for r in rows {
        // 내 것이 아니면 눌러도 권한이 없다. 보여 주고 안 되는 것보다 안 보이는
        // 편이 낫다 — 시스템이 쥔 몫은 어차피 wired 쪽에서 잡힌다.
        if r.uid != me_uid || tree.contains(&r.pid) {
            continue;
        }
        let e = apps.entry(app_root(r.pid)).or_default();
        e.0 += r.rss;
        e.1 += 1;
        e.2 += r.cpu_time;
    }
    let name_of: HashMap<u32, &str> = rows.iter().map(|r| (r.pid, r.name.as_str())).collect();
    let now = Instant::now();
    let out: Vec<AppUsage> = apps
        .iter()
        // 대표가 표본에 없으면(권한·경합) 이름도 없고 끌 수도 없다.
        .filter_map(|(root, (rss, procs, cpu_time))| {
            let name = (*name_of.get(root)?).to_string();
            let (cpu, hot) = track.tick(*root, *cpu_time, now);
            Some(AppUsage { pid: *root, rss: rss * 1024, name, procs: *procs, cpu, hot })
        })
        .collect();
    // 사라진 앱은 여기서 잊는다 — 안 지우면 pid 가 재사용될 때 엉뚱한 앱의
    // 이력을 물려받는다. 우리 자신(트리)은 `apps` 에 없으므로 함께 남긴다.
    let me = std::process::id();
    track.last.retain(|k, _| *k == me || apps.contains_key(k));
    track.streak.retain(|k, _| *k == me || apps.contains_key(k));
    // **두 잣대로 각각 뽑아 합친다.** 하나로 정렬해 자르면 다른 잣대의 범인이
    // 통째로 사라진다 — CPU 만 태우고 메모리는 거의 안 쓰는 것이 있고(스핀 루프가
    // 그렇다), 그게 정확히 팬을 돌리는 부류다. 실측 2026-08-28 에 코어를 통째로
    // 태우던 프로세스가 1MB 라는 이유로 rss 정렬 밖으로 잘렸다. 반대도 같아서,
    // 팝오버가 잣대별 탭으로 갈린 뒤로는(2026-08-29) 양쪽 후보가 다 있어야 한다.
    // 고르는 것과 정렬은 팝오버가 자기 탭 기준으로 다시 한다.
    let mut by_cpu = out.clone();
    by_cpu.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    let mut by_rss = out;
    by_rss.sort_by(|a, b| b.rss.cmp(&a.rss));
    let mut picked: Vec<AppUsage> = Vec::with_capacity(TOP_APPS * 2);
    for a in by_cpu.into_iter().take(TOP_APPS).chain(by_rss.into_iter().take(TOP_APPS)) {
        if !picked.iter().any(|p| p.pid == a.pid) {
            picked.push(a);
        }
    }
    picked
}

/// 잣대 하나당 남기는 바깥 앱 수. 팝오버는 이 중 셋만 펴지만, 탭을 옮겼을 때
/// 그 잣대의 상위가 비어 있으면 안 되므로 수집은 넉넉히 한다.
const TOP_APPS: usize = 5;

/// 입력줄이 비어 있는가 — 하단 14행 안에 「❯」 단독 행이 있으면 참.
/// 재테마 주입은 이게 참일 때만 연다: 반쯤 친 초안이 있으면 주입 글자가 그
/// 뒤에 붙는다(tell 의 알려진 사고 유형). 승인 메뉴·피커가 떠 있을 때도 ❯ 는
/// 항목에 붙어 단독 행이 아니므로 자연히 걸러진다.
fn input_line_bare(cells: &[Vec<GridCell>]) -> bool {
    let Some(last) = cells
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))
    else {
        return false;
    };
    let start = (last + 1).saturating_sub(14);
    cells[start..=last].iter().any(|row| {
        let text: String = row
            .iter()
            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
            .collect();
        text.trim() == "❯"
    })
}

/// 승인/질문 프롬프트의 종류 — 응답 키 주입이 다르다 (munder-difflin BLOCK_HINTS 이식).
///   Menu:  claude 의 "❯ 1. Yes" 번호 메뉴(permission/AskUserQuestion). Enter=하이라이트
///          선택(기본 Yes), Esc=거부. 'y'/'n' 글자는 메뉴에서 무시되므로 못 쓴다.
///   YesNo: 셸 스크립트의 인라인 "(y/n)" 질문. y\r / n\r.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ApprovalPrompt {
    Menu,
    YesNo,
}

/// Bottom-anchored scan for a *pending* approval/question prompt. Mirrors
/// munder's BLOCK_HINTS with three hard-won exclusions baked in:
///   - never match the bare word "permission" — claude's footer permanently
///     shows "bypass permissions on", which would flag every busy pane;
///   - "(y/n)" counts only on the LAST non-blank row (the cursor line). An
///     answered shell prompt scrolls up but stays on screen ("proceed? (y/n) y"),
///     so matching it anywhere re-flags long after it was answered;
///   - a Menu match is REJECTED if a bare chevron row ("❯" with nothing after
///     it) sits BELOW it. That bare "❯ " is claude's idle input line; a *live*
///     approval menu replaces the input line with its options, so a real menu
///     never has a bare chevron under it. When one does, the "menu" text is
///     just quoted history in the transcript (e.g. a `peek` dump of another
///     pane's prompt) — matching it made an idle orchestrator pane toast itself and,
///     worse, a chip click injected Enter into its own input line. (거노
///     실클릭으로 확인된 false-positive.)
/// Callers must check `rows_show_working` first — a spinner means the prompt
/// text still on screen is history, not a question.
pub(crate) fn rows_show_approval_prompt(cells: &[Vec<GridCell>]) -> Option<ApprovalPrompt> {
    let last = cells
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    // 메뉴는 옵션 + 안내문으로 working 스캔(10행)보다 길 수 있어 14행까지 본다.
    let start = (last + 1).saturating_sub(14);
    let mut menu_found = false;
    let mut bare_chevron_below_menu = false;
    for (i, row) in cells[start..=last].iter().enumerate() {
        let line: String = row
            .iter()
            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
            .collect();
        if let Some(pos) = line.find('❯') {
            let rest = line[pos + '❯'.len_utf8()..].trim();
            if rest.is_empty() {
                // bare "❯ " = claude idle 입력행. 이미 찾은 메뉴 후보 아래에
                // 있으면 그 메뉴는 인용된 가짜 → 뒤에서 reject.
                if menu_found {
                    bare_chevron_below_menu = true;
                }
                continue;
            }
            // "❯ 12. …" — 커서가 올라간 번호 옵션.
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && rest[digits..].starts_with('.') {
                menu_found = true;
                continue;
            }
        }
        let lower = line.to_lowercase();
        // claude 승인 메뉴의 헤더 — 메뉴 행이 잘려도(좁은 pane) 이 문구로 Menu 판정.
        if lower.contains("do you want to proceed") {
            menu_found = true;
            continue;
        }
        // YesNo 는 last 행에서만(인라인 셸 질문). 메뉴와 독립이라 즉시 반환.
        if start + i == last && (lower.contains("(y/n)") || lower.contains("[y/n]")) {
            return Some(ApprovalPrompt::YesNo);
        }
    }
    if menu_found && !bare_chevron_below_menu {
        return Some(ApprovalPrompt::Menu);
    }
    None
}

#[cfg(test)]
mod working_scan_tests {
    use super::*;
    use kasa_bridge::screen::Cell;

    fn row(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|ch| Cell {
                ch,
                ..Cell::blank()
            })
            .collect()
    }
    fn blank() -> Vec<GridCell> {
        vec![Cell::blank(); 8]
    }

    #[test]
    fn deferred_account_restart_toast_does_not_claim_a_provider_or_account() {
        let toast = deferred_account_restart_toast(2);
        assert_eq!(toast, "계정 전환을 적용해 대기 중이던 pane 2개를 다시 띄웠어요");
        assert!(!toast.to_ascii_lowercase().contains("claude"));
        assert!(!toast.to_ascii_lowercase().contains("codex"));
    }

    #[test]
    fn claude_launch_screens_do_not_expose_redraw_history() {
        assert!(claude_launch_screen(
            "╭─ Claude Code ─╮\nWelcome back 양건호!\nUsing Opus 5"
        ));
        assert!(claude_launch_screen(
            "Accessing workspace:\nQuick safety check: Is this a project you trust?"
        ));
        assert!(!claude_launch_screen(
            "Claude Code\n작업 결과입니다.\n❯ 다음 요청"
        ));
    }

    #[test]
    fn spinner_above_trailing_blank_padding_is_working() {
        // The exact 거노 case: claude's status line, then a wall of blank
        // padding rows filling out the grid. The old `cells[rows-10..]` scan
        // saw only blanks here and missed it.
        let mut cells = vec![row("✢ Gitifying… (1m 35s · ↑ 4.9k tokens)")];
        cells.extend(std::iter::repeat_with(blank).take(12));
        assert!(rows_show_working(&cells));
    }

    #[test]
    fn all_blank_is_idle() {
        assert!(!rows_show_working(&vec![blank(); 20]));
    }

    #[test]
    fn dot_frame_without_esc_hint_is_working() {
        // 라이브 실측(claude code 2.1.207): 점 프레임 스피너 행에 "esc to
        // interrupt" 힌트가 없다 — 점+… 문맥만으로 working 이어야 한다.
        assert!(rows_show_working(&[row(
            "· Caramelizing… (3m 39s · ↓ 9.7k tokens)"
        )]));
    }

    #[test]
    fn completion_summary_without_ellipsis_is_idle() {
        // "✻ Churned for 42s" keeps the star but drops the ellipsis.
        assert!(!rows_show_working(&[row("✻ Churned for 42s")]));
    }

    #[test]
    fn esc_to_interrupt_is_working() {
        assert!(rows_show_working(&[row("Working (esc to interrupt)")]));
    }

    #[test]
    fn braille_spinner_is_working() {
        assert!(rows_show_working(&[row("⠋ installing")]));
    }

    /// codex 도 같은 판정에 걸린다 — 실측 문구가 claude 와 글자 그대로 겹친다.
    /// 2026-08-05 codex 0.146.0 화면에서 그대로 떠온 줄이다:
    ///   `• Working (3s • esc to interrupt)`
    /// 앞머리 글리프는 `•`(U+2022)라 claude 의 Dingbat 별(U+2720–274F)에도
    /// 점자에도 안 걸리지만, "esc to interrupt" 가 먼저 잡아 준다. 그래서 codex
    /// busy 표시는 **코드를 안 고치고** 동작한다 — 이 테스트는 codex 가 나중에
    /// 문구를 바꾸면 조용히 죽는 대신 여기서 터지라고 있는 것이다.
    #[test]
    fn codex_working_line_is_working() {
        assert!(rows_show_working(&[row(
            "• Working (3s • esc to interrupt)"
        )]));
        // 입력줄·상태줄이 아래에 깔려도 하단 10행 창 안이라 잡힌다(실측 배치).
        assert!(rows_show_working(&[
            row("• Working (12s • esc to interrupt)"),
            row("› Run /review on my current changes"),
            row("  gpt-5.5 medium · tmuxify · main · Ask for approval · Context 3% used"),
        ]));
        // 답이 끝나면 그 줄이 사라진다 → idle. 상태줄만 남은 화면은 working 이 아니다.
        assert!(!rows_show_working(&[
            row("› Run /review on my current changes"),
            row("  gpt-5.5 medium · tmuxify · main · Ask for approval · Context 3% used"),
        ]));
    }

    /// 답변 본문이 working 으로 잡히면 헤더 busy 바와 pane 펄스가 답이 끝난 뒤로도
    /// 계속 돈다. 한국어 문장부호(·, …)가 그대로 스피너 시그니처와 겹쳐서 났다
    /// (2026-08-12 지적 — 학생이 본문 위를 걸어다녔다).
    #[test]
    fn korean_prose_is_idle() {
        assert!(!rows_show_working(&[row(
            "간·창별 막대) → Usage details & history / Manage Accounts…"
        )]));
        assert!(!rows_show_working(&[row("· 임계 60/80%, 문구까지 Orca 그대로…")]));
    }

    #[test]
    fn numbered_menu_is_menu_prompt() {
        let cells = vec![
            row("Do you want to proceed?"),
            row("❯ 1. Yes"),
            row("  2. Yes, and don't ask again"),
            row("  3. No, and tell Claude what to do differently"),
        ];
        assert_eq!(
            rows_show_approval_prompt(&cells),
            Some(ApprovalPrompt::Menu)
        );
    }

    #[test]
    fn ask_user_question_menu_without_yes_is_menu_prompt() {
        // AskUserQuestion 옵션은 Yes/No 가 아닐 수 있다 — 번호+점이면 메뉴.
        let cells = vec![row("❯ 1. worktree로 격리"), row("  2. 그냥 main에서")];
        assert_eq!(
            rows_show_approval_prompt(&cells),
            Some(ApprovalPrompt::Menu)
        );
    }

    #[test]
    fn bare_input_chevron_is_not_a_prompt() {
        // claude 입력창의 맨 "❯ " — 번호가 없으면 메뉴가 아니다.
        assert_eq!(rows_show_approval_prompt(&[row("❯ ")]), None);
    }

    #[test]
    fn permission_footer_alone_is_not_a_prompt() {
        // 푸터의 "bypass permissions on" 은 항상 떠 있다 — 매칭 금지 (munder 함정).
        let cells = vec![
            row("❯ "),
            row("  bypass permissions on (shift+tab to cycle)"),
        ];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn yn_on_last_row_is_yesno_prompt() {
        let cells = vec![row("Overwrite existing file? (y/n)")];
        assert_eq!(
            rows_show_approval_prompt(&cells),
            Some(ApprovalPrompt::YesNo)
        );
    }

    #[test]
    fn answered_yn_above_prompt_line_is_ignored() {
        // 이미 답한 (y/n) 가 위로 스크롤돼 남아 있어도 마지막 행이 아니면 무시.
        let cells = vec![row("Overwrite? (y/n) y"), row("done."), row("$ ")];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn quoted_menu_with_bare_chevron_below_is_rejected() {
        // 오케스트레이터 pane 이 `peek %2` 결과를 자기 대화창에 인용 → transcript 에 박제된
        // 가짜 메뉴. 그 아래에 claude idle 입력행(bare "❯ ")이 있으면 reject.
        // (거노 실클릭으로 확인: 안 잡으면 idle pane 이 자기한테 토스트 쏘고
        //  칩 클릭 시 자기 입력행에 Enter 가 주입됨.)
        let cells = vec![
            row("> peek %2 결과:"),
            row("  Do you want to proceed?"),
            row("  ❯ 1. Yes"),
            row("    2. No"),
            row("알겠어, 확인했어."),
            row("❯ "),
        ];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn live_menu_without_bare_chevron_below_is_menu() {
        // 진짜 활성 메뉴: 입력행이 메뉴 옵션으로 대체돼 아래에 bare "❯ " 가 없다.
        let cells = vec![
            row("Do you want to proceed?"),
            row("❯ 1. Yes"),
            row("  2. No"),
        ];
        assert_eq!(
            rows_show_approval_prompt(&cells),
            Some(ApprovalPrompt::Menu)
        );
    }

    #[test]
    fn menu_then_blank_padding_then_bare_chevron_is_rejected() {
        // 인용 메뉴와 입력행 사이에 빈 줄이 끼어도 reject (아래 어디든 bare-❯면).
        let cells = vec![row("❯ 1. Yes"), row("  2. No"), blank(), row("❯ ")];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn multi_digit_menu_option_is_menu_prompt() {
        assert_eq!(
            rows_show_approval_prompt(&[row("❯ 12. 마지막 옵션")]),
            Some(ApprovalPrompt::Menu)
        );
    }

    // compact 알림을 잡는가. 문구는 claude 번들 실측(`Compacting conversation`·
    // `compacting history`)이고, 판정은 그 둘에 공통인 `ompacting` 만 본다 — 대문자
    // 여부와 뒤에 붙는 말이 버전마다 흔들려도 남는 조각이다.
    #[test]
    fn compacting_notice_is_detected_in_either_wording() {
        assert!(rows_show_compacting(&[row(
            "✻ Compacting conversation… (3m 31s · ↓ 8.7k tokens)"
        )])
        .is_some());
        assert!(rows_show_compacting(&[row(
            "✻ compacting history (esc to interrupt)"
        )])
        .is_some());
    }

    // 평범한 working 화면을 compact 로 오인하면 모든 바쁜 pane 이 채워지는 바를 단다.
    #[test]
    fn ordinary_working_screen_is_not_compacting() {
        assert!(rows_show_compacting(&[row("✻ Pondering… (esc to interrupt)")]).is_none());
        assert!(rows_show_compacting(&[row("")]).is_none());
        assert!(rows_show_compacting(&[]).is_none());
    }

    // ★회귀: 알림과 맨 아랫줄 사이에 todo 트리와 입력박스가 끼어도 잡아야 한다.
    // 하단 10행 창으로 뒀을 때 거노 화면에서 그 거리가 12행이라 한 번도 안 걸렸다
    // (2026-08-13). 판정을 스피너 행에 앵커하면 그 거리는 무의미해진다.
    // 진행률 행(45%)도 알림 바로 아래에서 함께 읽혀야 한다.
    #[test]
    fn compacting_notice_far_above_the_bottom_is_still_detected() {
        let mut cells = vec![row("✻ Compacting conversation… (3m 31s · ↓ 8.7k tokens)")];
        cells.push(row("▰▰▰▱▱▱ 45%"));
        cells.push(row("└ □ 딜 검토에 출구를 만든다"));
        cells.push(row("   ✓ 딜 등록의 담당자를 여러 명으로"));
        for _ in 0..8 {
            cells.push(row("│ 입력박스와 statusline"));
        }
        assert_eq!(rows_show_compacting(&cells), Some(Some(45)));
    }

    // ★회귀: 경과시간 괄호가 아예 없는 변형(2026-08-13 스샷 실측). 이 행이 그
    // 화면의 유일한 스피너 행이라, spinner_row_col 의 괄호 요구에 걸리면 compact
    // 중인 pane 전체가 busy 도 아니게 읽혀 바·완료 판정이 전부 죽었다.
    // 진행률 행이 없으면 Some(None) — 바는 시간 루프로 폴백한다.
    #[test]
    fn compacting_notice_without_elapsed_suffix_is_detected_and_busy() {
        let cells = vec![row("· Compacting conversation…")];
        assert!(rows_show_working(&cells));
        assert_eq!(rows_show_compacting(&cells), Some(None));
    }

    // ★회귀: 실제 compact 화면 그대로 — 알림 아래 진행률 행과 `⎿ Tip:` 행이 깔린다
    // (2026-08-13 스샷 실측). Tip 의 `⎿` 를 대화 마커로 세면 spinner_is_live 가
    // 알림을 스크롤백으로 오판해 스스로를 죽인다. 퍼센트(7%)는 진짜 진행률로 읽힌다.
    #[test]
    fn compacting_screen_with_progress_and_tip_rows_is_detected() {
        let cells = vec![
            row("· Compacting conversation…"),
            row("▰▰▰▱▱▱▱▱▱▱ 7%"),
            row("⎿  Tip: Did you know you can drag and drop image files into your terminal?"),
        ];
        assert!(rows_show_working(&cells));
        assert_eq!(rows_show_compacting(&cells), Some(Some(7)));
    }

    // Tip 행의 우연한 %(예: "100% faster")를 진행률로 줍지 않는가 — 스캔은 알림
    // 직하 2행까지지만, 숫자+% 형태만 인정하므로 진행률 행이 그 자리에 있으면
    // 그것이 이긴다. 여기선 진행률 행이 없고 Tip 에만 % 가 있는 경우를 본다.
    #[test]
    fn compacting_pct_is_not_taken_from_prose_percent() {
        let cells = vec![
            row("· Compacting conversation…"),
            row("⎿  Tip: kasaterm renders 100% of your panes"),
        ];
        // Tip 행도 스캔 창(직하 2행) 안이라 숫자+% 는 읽힌다 — 형태만으로는 못
        // 가른다. 대신 실제 화면에선 진행률 행이 항상 알림 바로 아래라 Tip 이
        // 먼저 잡힐 일이 없다. 이 테스트는 그 순서 의존을 문서화한다.
        assert_eq!(rows_show_compacting(&cells), Some(Some(100)));
    }

    // 스크롤백에 굳은 옛 알림은 무시돼야 한다 — 안 그러면 compact 가 끝난 뒤에도
    // 바가 영원히 남는다. 가르는 축은 거리가 아니라 아래에 쌓인 대화 마커(`⎿`)다.
    #[test]
    fn compacting_notice_scrolled_into_the_backlog_is_ignored() {
        let cells = vec![
            row("✻ Compacting conversation… (3m 31s · ↓ 8.7k tokens)"),
            row("⎿ 그 뒤에 이어진 도구 출력"),
        ];
        assert!(rows_show_compacting(&cells).is_none());
    }

    #[test]
    fn bare_input_line_gates_retheme_injection() {
        // 빈 입력줄(❯ 단독) = 주입해도 안전.
        assert!(input_line_bare(&[row("본문 끝"), row("❯"), row("statusline")]));
        // 반쯤 친 초안이 있으면 미룬다 — 주입이 초안 뒤에 붙는 사고 방지.
        assert!(!input_line_bare(&[row("❯ 반쯤 친 메시지")]));
        // 승인 메뉴의 ❯ 는 항목에 붙어 있어 단독 행이 아니다.
        assert!(!input_line_bare(&[row("❯ 1. Yes"), row("  2. No")]));
        // 새 세션의 placeholder(「❯ Try "…"」)도 단독 행이 아니라 미뤄진다 —
        // 새 세션은 어차피 부팅 때 맞는 테마로 뜨므로 놓쳐도 된다.
        assert!(!input_line_bare(&[row("❯ Try \"fix lint errors\"")]));
    }

    // 값은 `/config theme=` 뒤에 그대로 타이핑되므로, 토큰 문자군을 벗어나면
    // 주입 자체를 접는다 — 엉뚱한 텍스트가 제출되는 쪽이 훨씬 나쁘다.
    #[test]
    fn claude_theme_token_rejects_untypable_values() {
        assert_eq!(claude_theme_token("dark"), Some("dark"));
        assert_eq!(claude_theme_token("light-daltonized"), Some("light-daltonized"));
        assert_eq!(claude_theme_token("auto"), Some("auto"));
        assert_eq!(claude_theme_token("custom:mine"), Some("custom:mine"));
        assert_eq!(claude_theme_token(""), None);
        assert_eq!(claude_theme_token("dark mode"), None);
        assert_eq!(claude_theme_token("dark\rrm -rf ~"), None);
        assert_eq!(claude_theme_token(&"x".repeat(65)), None);
    }

    #[test]
    fn modifier_alone_must_not_end_a_composition() {
        use winit::keyboard::{Key, NamedKey};
        // "계"의 ㅖ 는 Shift+ㅔ — 조합 중 Shift 는 조합의 일부지 끝내라는 뜻이 아니다.
        assert!(super::is_modifier_logical(&Key::Named(NamedKey::Shift)));
        assert!(super::is_modifier_logical(&Key::Named(NamedKey::Control)));
        assert!(super::is_modifier_logical(&Key::Named(NamedKey::Alt)));
        assert!(super::is_modifier_logical(&Key::Named(NamedKey::Super)));
        assert!(super::is_modifier_logical(&Key::Named(NamedKey::CapsLock)));
        // 글자·편집키는 그대로 조합을 확정시켜야 한다.
        assert!(!super::is_modifier_logical(&Key::Character("r".into())));
        assert!(!super::is_modifier_logical(&Key::Named(NamedKey::Enter)));
        assert!(!super::is_modifier_logical(&Key::Named(
            NamedKey::Backspace
        )));
        assert!(!super::is_modifier_logical(&Key::Named(NamedKey::Escape)));
        assert!(!super::is_modifier_logical(&Key::Named(NamedKey::Space)));
        // 단축키는 수식키가 눌린 채로 와도 logical_key 가 글자라 안 걸린다.
        assert!(!super::is_modifier_logical(&Key::Character("w".into())));
    }
}

#[cfg(test)]
mod zoom_key_tests {
    use super::{cpu_ms, hog_polls, outside_apps, zoom_key, AppUsage, CpuTrack, Row, ZoomKey};
    use winit::keyboard::KeyCode;

    #[test]
    fn us_layout_physical_keys() {
        assert_eq!(zoom_key(Some(KeyCode::Equal), Some("=")), Some(ZoomKey::In));
        assert_eq!(
            zoom_key(Some(KeyCode::Minus), Some("-")),
            Some(ZoomKey::Out)
        );
        assert_eq!(
            zoom_key(Some(KeyCode::Digit0), Some("0")),
            Some(ZoomKey::Reset)
        );
        assert_eq!(zoom_key(Some(KeyCode::NumpadAdd), None), Some(ZoomKey::In));
        assert_eq!(
            zoom_key(Some(KeyCode::NumpadSubtract), None),
            Some(ZoomKey::Out)
        );
    }

    /// Shift 를 낀 `+` / `_` 도 같은 팔이어야 한다 — `+` 는 Shift+`=` 다.
    #[test]
    fn shifted_variants() {
        assert_eq!(zoom_key(Some(KeyCode::Equal), Some("+")), Some(ZoomKey::In));
        assert_eq!(
            zoom_key(Some(KeyCode::Minus), Some("_")),
            Some(ZoomKey::Out)
        );
    }

    /// 이게 이 함수의 존재 이유다: 한글(두벌식) 배열에선 Cmd 를 낀 키의 물리 위치가
    /// US 와 어긋날 수 있어, 물리키만 보면 별도창에서 Cmd+- 가 안 먹고 '-' 가 셸에
    /// 박혔다. 문자가 맞으면 물리 위치를 몰라도 잡아야 한다.
    #[test]
    fn logical_char_alone_is_enough() {
        assert_eq!(zoom_key(None, Some("-")), Some(ZoomKey::Out));
        assert_eq!(zoom_key(None, Some("=")), Some(ZoomKey::In));
        assert_eq!(zoom_key(None, Some("0")), Some(ZoomKey::Reset));
    }

    /// 조합 중인 자모나 평범한 글자는 절대 줌으로 새면 안 된다 — 새면 그 키가
    /// 셸에 안 가고 조용히 사라진다.
    #[test]
    fn ordinary_keys_are_not_zoom() {
        assert_eq!(zoom_key(Some(KeyCode::KeyA), Some("a")), None);
        assert_eq!(zoom_key(Some(KeyCode::KeyR), Some("ㄱ")), None);
        assert_eq!(zoom_key(Some(KeyCode::Digit1), Some("1")), None);
        assert_eq!(zoom_key(None, None), None);
    }

    /// `ps` 의 시간 형식은 길이에 따라 자리 수가 바뀐다. 틀려도 컴파일은 되고
    /// 화면에는 그럴듯한 퍼센트가 뜨므로, 값으로 못 박아 둔다.
    #[test]
    fn 누적_cpu_시간을_형식대로_읽는다() {
        assert_eq!(cpu_ms("4:50.82"), Some(290_820)); // MM:SS.ss
        assert_eq!(cpu_ms("14:44:55"), Some(53_095_000)); // HH:MM:SS
        // DD-HH:MM:SS — 하루를 안 더하면 오래 산 프로세스의 사용률이 통째로
        // 어긋난다(증분이 음수가 되어 0 으로 눌린다).
        assert_eq!(cpu_ms("01-17:19:49"), Some(86_400_000 + 62_389_000));
        assert_eq!(cpu_ms("-"), None);
    }

    /// 갱신 한 줄을 빠뜨리면 모든 사용률이 0 이 되고, 화면에는 「아무도 CPU 를
    /// 안 쓴다」로 조용히 나온다 — 실제로 리팩터링 중에 그 회귀를 냈다.
    #[test]
    fn 폴_사이의_증분이_사용률이_된다() {
        use std::time::Duration;
        let mut t = CpuTrack::default();
        let t0 = std::time::Instant::now();
        // 첫 표본은 견줄 데가 없다.
        assert_eq!(t.tick(1, 1_000, t0).0, 0.0);
        // 5초 사이에 CPU 시간이 5초 늘었으면 코어 하나를 통째로 쓴 것이다.
        let (cpu, hot) = t.tick(1, 6_000, t0 + Duration::from_secs(5));
        assert!((cpu - 100.0).abs() < 0.1, "cpu={cpu}");
        assert_eq!(hot, 1);
        // 멈추면 사용률이 0 이 되고 카운터도 물러난다.
        let (cpu, hot) = t.tick(1, 6_000, t0 + Duration::from_secs(10));
        assert_eq!(cpu, 0.0);
        assert_eq!(hot, 0);
    }

    /// 한 잣대로 정렬해 자르면 다른 잣대의 범인이 통째로 사라진다 — 2026-08-28 에
    /// 코어를 통째로 태우던 프로세스가 1MB 라는 이유로 rss 정렬 밖으로 잘렸다.
    /// 팝오버가 잣대별 탭으로 갈린 뒤로는 양쪽 상위가 다 있어야 두 탭이 선다.
    #[test]
    fn 두_잣대의_범인이_둘_다_살아남는다() {
        let me = unsafe { libc::getuid() };
        let row = |pid: u32, rss: u64, cpu_time: u64, name: &str| Row {
            uid: me,
            pid,
            // 1 이면 자기 자신이 대표가 된다 — 앱 묶기를 타지 않는 가장 단순한 꼴.
            ppid: 1,
            cpu: 0.0,
            rss,
            cpu_time,
            name: name.to_string(),
        };
        // 메모리만 큰 것 여섯 + CPU 만 태우는 작은 것 하나. 한 잣대로 다섯을
        // 자르면 마지막 하나가 반드시 잘려 나가는 형태다.
        let mut rows: Vec<Row> =
            (0..6).map(|i| row(10 + i, 8_000_000 - i as u64 * 1000, 0, "big")).collect();
        rows.push(row(99, 1024, 0, "spinner"));
        let tree = std::collections::HashSet::new();
        let mut track = CpuTrack::default();
        // 첫 표본은 견줄 데가 없어 사용률이 0 이다 — 두 번 재야 증분이 선다.
        outside_apps(&rows, &tree, &mut track);
        if let Some(r) = rows.iter_mut().find(|r| r.pid == 99) {
            r.cpu_time = 60_000;
        }
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let out = outside_apps(&rows, &tree, &mut track);
        assert!(out.iter().any(|a| a.pid == 99), "CPU 범인이 잘렸다: {out:?}");
        assert!(out.iter().any(|a| a.pid == 10), "메모리 1등이 잘렸다: {out:?}");
    }

    #[test]
    fn 잠깐_바쁜_것과_계속_태우는_것을_가른다() {
        let a = |cpu: f32, hot: u16| AppUsage {
            pid: 1,
            rss: 0,
            name: "x".into(),
            procs: 1,
            cpu,
            hot,
        };
        // 한 번 튄 것은 빌드·인코딩과 구별되지 않는다.
        assert!(!a(99.0, 1).is_hog());
        assert!(!a(99.0, hog_polls() - 1).is_hog());
        // 1분 내내 코어 하나를 태우면 그때부터 팬 이유로 본다.
        assert!(a(99.0, hog_polls()).is_hog());
    }
}
