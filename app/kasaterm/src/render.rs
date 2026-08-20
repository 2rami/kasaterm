//! GPU 렌더 패스 — App 렌더 메서드(cell-renderer 파이프라인 + chrome 오버레이).
//! main.rs 의 impl App 에서 분리. struct App·자유함수·타입은 crate root 그대로 참조.
use super::*;
pub(crate) use crate::screenread::*;
pub(crate) use crate::sprites::*;

/// 펼친 방의 pane 줄 하나를 그리는 데 필요한 것 전부 — 페인트 루프가 `g`
/// (=&mut self.gpu) 를 잡고 있어 `self` 를 다시 읽을 수 없으므로 미리 뜬 스냅샷이다.
/// 튜플로 두다 필드가 여섯이 되면서 `.3`/`.4` 가 무엇인지 호출부에서 안 읽혀 이름을 달았다.
struct SidebarRowInfo {
    /// 배정 학생명(얼굴용). claude 가 안 붙은 pane 은 빈 문자열.
    who: String,
    /// 줄에 적는 것 — 그 pane 이 지금 무엇인가(claude · zsh · 편집기…).
    label: String,
    /// 오른쪽 끝 상태 점 색(`pane_state_color`).
    color: [u8; 4],
    /// 지금 보고 있는 pane.
    is_cur: bool,
    /// 못 본 완료 — 줄 끝 점이 accent 로 깜빡인다.
    alert: bool,
    /// 승인·입력을 기다리는 중 — 줄 끝 점이 주황으로, 두 배 빠르게 깜빡인다.
    waiting: bool,
    /// 지금 도는 중 — 학생이 걷는다. 기다리는 중은 여기 안 든다(그건 멈춘 것이다).
    busy: bool,
    /// 사이드바에서 숨긴 pane — 화면엔 없지만 PTY 는 돈다. 흐리게 + 아이콘으로
    /// 그려 「없는 것」이 아니라 「치워 둔 것」임을 말한다.
    stashed: bool,
    /// 학생 얼굴이 없을 때 칸이 무엇인지 말하는 아이콘 — 웹 pane 은 globe,
    /// 이미지는 image, md 는 file-text, 그 외 terminal. 이게 없으면 미니맵이
    /// 웹 pane 도 터미널이라고 거짓말한다.
    icon: &'static str,
}

impl App {
    /// Phase 2a path. Collects every pane's live cell grid and hands
    /// it to the cell-renderer pipeline. Chrome (sidebar, tabs,
    /// headers, cursor block, selection, preedit) is intentionally
    /// not drawn yet — Phase 2b+ will reattach those via the same
    /// pipeline / atlas.
    /// Self-only snapshot used by `paint_gpu_overlays`. Built before
    /// we borrow `self.gpu` mutably so the renderer pass can run
    /// without a re-entrant `&self` read. All coordinates here are
    /// already cell-space — the renderer-side helper applies cell
    /// metric multiplication.
    fn gpu_overlay_snapshot(&self) -> GpuOverlay {
        // 터미널 오버레이는 조합기 주인이 터미널일 때만 그린다. 편집기가 조합 중인데
        // 포커스만 터미널로 옮겨진 순간(클릭 직후, 아직 아무 키도 안 친 상태)
        // 남의 조합 글자를 터미널 커서에 그리게 된다.
        //
        // **화이트리스트로 둔다.** 크롬 쪽 입력칸(git 커밋·파일트리·방 이름·경로 검색)은
        // 전부 자기 자리에 프리에딧을 그리므로, 빠뜨린 갈래가 하나라도 있으면 같은 글자가
        // 그 칸과 터미널 커서에 이중으로 뜬다. 목록에 없는 새 필드가 생겨도 안 새게.
        let preedit_text = match &self.ime_focus {
            None | Some(crate::ImeFocus::Pane(_)) => self.preedit.clone(),
            _ => String::new(),
        };
        let commit_overlay = self.commit_overlay.clone();
        // Active pane's font multiplier — the overlay anchors to this same
        // pane (see pane_origin below), so its cell size must match the
        // pane's zoomed glyphs, not the base grid.
        let pane_font_scale = self
            .target_pane()
            .and_then(|id| self.pane_font_scales.get(&id).copied())
            .unwrap_or(1.0);
        let snap = {
            let ws = self.ws.lock().unwrap();
            // Active pane's top-left in cell units. When the workspace is
            // split the cursor/preedit overlay must anchor to THIS pane,
            // not the global origin (which is the left/top pane).
            //
            // 원점은 **실제로 그린 rect**(effective_leaf_rects)에서 온다 —
            // ws.layout 의 split 좌표는 줌을 모르기 때문이다. 줌한 pane 은 원래
            // 자리가 아니라 작업영역 한가운데 들여 그려지므로, split 좌표를 쓰면
            // 커서·조합 오버레이만 옛 자리에 남아 화면 어딘가로 사라진다.
            // (아래쪽 pane 을 줌하면 예전에도 어긋났고, inset 이 생기며 항상
            // 드러났다.) leaf_rects 의 id 는 이미 `%n` 꼴이라 변환이 없다.
            let (gcols, grows) = self.window_cells();
            let pane_origin = ws
                .active_pane
                .as_ref()
                .and_then(|aid| {
                    self.effective_leaf_rects(gcols, grows)
                        .into_iter()
                        .find(|(id, ..)| id == aid)
                        .map(|(_, x, y, _, _)| (x, y))
                })
                .unwrap_or((0u16, 0u16));
            ws.active_pane.clone().and_then(|id| {
                ws.panes.get(&id).map(|pane| {
                    // Preedit sits exactly on the reported PTY cursor —
                    // that's where the next char lands. We used to bump
                    // the column to the row's last filled cell to dodge
                    // tail padding, but a TUI's grey placeholder ("Type
                    // something") counts as filled, so that dragged the
                    // composing syllable past it to the line's end. The
                    // cursor column is already correct (incl. trailing
                    // spaces the PTY echoes), so trust it directly.
                    // Image/markdown panes have no PTY cursor — their terminal
                    // block cursor stays hidden (the Raw editor draws its own).
                    let (cur_row, cur_col, cur_vis, cols, cur_w) = match pane.term() {
                        Some(t) => (
                            t.cursor_row,
                            t.cursor_col,
                            t.cursor_visible,
                            t.cells.first().map(|r| r.len()).unwrap_or(80) as u16,
                            cursor_cell_width(&t.cells, t.cursor_row, t.cursor_col),
                        ),
                        None => (0, 0, false, 80, 1),
                    };
                    let (base_row, base_col) = (cur_row, cur_col);
                    // Until the committed syllable's echo lands (cursor
                    // still where it was at commit time), draw the
                    // committed text in front of the preedit at that spot.
                    //
                    // PTY 가 없는 pane(편집기·이미지)은 위에서 (0,0) 을 기본으로
                    // 받는다. 거기에 조합 문자열을 그리면 **줄번호 거터 위에
                    // 유령**이 뜨고, raw 편집기는 자기 문서 캐럿에 preedit 을
                    // 따로 그리므로 같은 글자가 두 군데 보인다(거노: "입력이
                    // 동시에 되고"). 터미널 오버레이는 터미널일 때만 그린다.
                    let (display, prow, pcol) = match &commit_overlay {
                        _ if pane.term().is_none() => (String::new(), base_row, base_col),
                        Some((ctext, before)) if *before == (cur_row, cur_col) => {
                            (format!("{ctext}{preedit_text}"), before.0, before.1)
                        }
                        _ => (preedit_text.clone(), base_row, base_col),
                    };
                    (
                        cur_row,
                        cur_col,
                        cur_vis,
                        cols,
                        cur_w,
                        prow,
                        pcol,
                        display,
                        pane_origin.0,
                        pane_origin.1,
                        pane.header_px(),
                    )
                })
            })
        };
        let (
            cursor_row,
            cursor_col,
            cursor_visible,
            cols,
            cursor_w,
            preedit_row,
            preedit_col,
            preedit,
            pane_x,
            pane_y,
            header_shift,
        ) = snap.unwrap_or((0, 0, false, 80, 1, 0, 0, preedit_text.clone(), 0, 0, 0.0));
        // When split OR any pane is multi-tab, every pane body is pushed
        // down by its header band. The cursor / preedit / selection
        // overlays anchor off the same origin as the cells, so they must
        // apply the identical shift — otherwise the cursor floats up into
        // the header row (which is exactly what made it appear one line
        // above the actual prompt after a cross-pane tab drop).
        // header_shift = active pane 의 header_px(snap 에서 가져옴). 헤더 있는
        // pane 은 커서/조합(IME)/선택 오버레이도 셀과 똑같이 헤더만큼 내려간다.
        GpuOverlay {
            cell_w: self.cell.w,
            cell_h: self.cell.h,
            pad_x: WINDOW_PADDING + self.effective_sidebar_w() + pane_x as f32 * self.cell.w + PANE_INNER_X,
            pad_y: TITLE_HEIGHT + pane_y as f32 * self.cell.h + header_shift + PANE_INNER_Y,
            cursor_row,
            cursor_col,
            cursor_w,
            cursor_shape: self.cursor_shape.clone(),
            cursor_thickness: self.cursor_thickness,
            cursor_visible,
            cols,
            blink_on: self.cursor_blink_on(Instant::now()),
            preedit,
            preedit_row,
            preedit_col,
            font_size: self.font_size,
            font_scale: pane_font_scale,
            selection: self.selection,
            suggestion: self.current_suggestion.clone().unwrap_or_default(),
        }
    }

    /// OS IME 후보창을 커서 칸에 붙인다. macOS 는 플랫폼 IME 자체를 끄고 우리가
    /// 조합하므로 해당 없고(`set_ime_allowed(false)`), 켜 두는 Windows·Linux 만
    /// 대상이다. 한 번도 안 부르면 좌표가 클라이언트 영역 원점이라 한자 변환·
    /// 후보 목록이 창 왼쪽 위 구석에 뜬다 — 조합 중인 글자와 딴 데.
    ///
    /// 좌표는 물리 픽셀(`self.cell` 이 이미 scale 반영). 프레임마다 Win32 를
    /// 때리지 않도록 칸이 바뀔 때만 보낸다.
    #[cfg(target_os = "macos")]
    fn sync_ime_cursor_area(&mut self, _ov: &GpuOverlay) {}

    #[cfg(not(target_os = "macos"))]
    fn sync_ime_cursor_area(&mut self, ov: &GpuOverlay) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let cw = ov.cell_w * ov.font_scale;
        let ch = ov.cell_h * ov.font_scale;
        // 조합 중이면 조합 시작 칸, 아니면 커서 칸.
        let (row, col) = if ov.preedit.is_empty() {
            (ov.cursor_row, ov.cursor_col)
        } else {
            (ov.preedit_row, ov.preedit_col)
        };
        let x = (ov.pad_x + col as f32 * cw).round() as i32;
        let y = (ov.pad_y + row as f32 * ch).round() as i32;
        if self.ime_cursor_px == Some((x, y)) {
            return;
        }
        self.ime_cursor_px = Some((x, y));
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(x, y),
            winit::dpi::PhysicalSize::new(cw.round() as u32, ch.round() as u32),
        );
    }

    /// Phase 2d overlays — pure free function on the snapshot so it
    /// doesn't fight a mutable borrow on `self.gpu`.
    fn paint_gpu_overlays(g: &mut gpu::GpuRenderer, ov: &GpuOverlay) {
        // Effective cell size for THIS pane: base metric × pane zoom. The
        // anchor (pad_x/pad_y) stays on the base grid because the pane's
        // top-left lives there, but every per-column/row step must use the
        // zoomed size or the cursor/preedit/selection drift right & down
        // as the pane is shrunk.
        let cw = ov.cell_w * ov.font_scale;
        let ch = ov.cell_h * ov.font_scale;
        if ov.cursor_visible && ov.blink_on && ov.preedit.is_empty() {
            let cx = ov.pad_x + ov.cursor_col as f32 * cw;
            let cy = ov.pad_y + ov.cursor_row as f32 * ch;
            let mut c = cells::iterm_cursor();
            c[3] = 140; // ~0.55 alpha
            // 모양은 사각형 하나의 폭·높이·y 로 전부 표현된다. bar 는 셀 왼쪽에 붙는
            // 세로선(Ghostty 식), underline 은 셀 바닥에 붙는 가로선이다. 굵기가 셀보다
            // 크면 block 과 구분이 안 되므로 셀 안으로 조인다.
            let full_w = cw * ov.cursor_w as f32;
            let (rx, ry, rw, rh) = match ov.cursor_shape.as_str() {
                "bar" => (cx, cy, ov.cursor_thickness.min(full_w), ch),
                "underline" => {
                    let t = ov.cursor_thickness.min(ch);
                    (cx, cy + ch - t, full_w, t)
                }
                _ => (cx, cy, full_w, ch),
            };
            g.rect(rx, ry, rw, rh, c);
        }
        // Inline autosuggestion ghost text — dim, on the same baseline as
        // committed cells, starting at the cursor and clipped to the row's
        // right edge so it never wraps. Drawn only when not composing.
        if ov.preedit.is_empty() && !ov.suggestion.is_empty() {
            let gx = ov.pad_x + ov.cursor_col as f32 * cw;
            let gy = ov.pad_y + ov.cursor_row as f32 * ch;
            let max_cells = ov.cols.saturating_sub(ov.cursor_col) as u32;
            if max_cells > 0 {
                g.draw_ghost(gx, gy, &ov.suggestion, max_cells, ov.font_scale);
            }
        }
        if !ov.preedit.is_empty() {
            let px = ov.pad_x + ov.preedit_col as f32 * cw;
            let py = ov.pad_y + ov.preedit_row as f32 * ch;
            // Route preedit through the cell-grid path so the composing
            // syllable sits on the same baseline as committed text
            // instead of floating above the row.
            g.draw_preedit(px, py, &ov.preedit, cells::iterm_cursor(), ov.font_scale);
        }
        if let Some(sel) = ov.selection {
            let (start, stop) = if (sel.anchor.1, sel.anchor.0) <= (sel.end.1, sel.end.0) {
                (sel.anchor, sel.end)
            } else {
                (sel.end, sel.anchor)
            };
            let color = cells::ITERM_SELECTION;
            if start.1 == stop.1 {
                let x = ov.pad_x + start.0 as f32 * cw;
                let y = ov.pad_y + start.1 as f32 * ch;
                let w = (stop.0 - start.0 + 1) as f32 * cw;
                g.rect(x, y, w, ch, color);
            } else {
                let x = ov.pad_x + start.0 as f32 * cw;
                let y = ov.pad_y + start.1 as f32 * ch;
                let row_w = (ov.cols - start.0) as f32 * cw;
                g.rect(x, y, row_w, ch, color);
                for r in (start.1 + 1)..stop.1 {
                    let yy = ov.pad_y + r as f32 * ch;
                    g.rect(ov.pad_x, yy, ov.cols as f32 * cw, ch, color);
                }
                let yy = ov.pad_y + stop.1 as f32 * ch;
                let last_w = (stop.0 + 1) as f32 * cw;
                g.rect(ov.pad_x, yy, last_w, ch, color);
            }
        }
    }

    /// `surface.capture` — pane 한 칸만 잘라 다음 프레임에 PNG 로 굽도록 무장한다.
    ///
    /// 좌표는 렌더가 셀을 놓는 식 그대로다(`render_frame_gpu` 의 pad_x/pad_y 와 같은
    /// 원점, 안쪽 여백 PANE_INNER 만 빼고 pane 상자 전체). 헤더 띠는 폐기돼(ghostty식,
    /// `layout.rs` 히트테스트 참조) 세로 보정이 없다. 프레임버퍼는 물리 픽셀이라
    /// 마지막에 scale 을 곱한다 — 안 곱하면 HiDPI 에서 좌상단 1/4 만 잘린다.
    ///
    /// ★ `pane` 이 **빈 문자열이면 창 전체**다. 프레임버퍼에는 사이드바·탭바·우측 칼럼이
    /// 이미 다 그려져 있고 pane 캡처가 위 오프셋으로 **일부러 잘라내던 것**이라, 크롭을
    /// 안 세우면(`capture_crop = None`) 그대로 창 한 장이 나온다. 에이전트가 제 UI 를
    /// 보려면 이 길이 필요하다 — pane 만 찍혀서는 사이드바가 어떻게 보이는지 알 수 없다.
    ///
    /// ⚠️ **메인 창만이다.** 별도 창으로 꺼낸 방(`auxwin`)은 자기 `GpuRenderer` 를 따로
    /// 들고 있어 이 서피스에 없다.
    pub(crate) fn arm_pane_capture(
        &mut self,
        pane: &str,
        path: Option<String>,
        max_w: u32,
        reply: std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ) {
        if self.gpu.is_none() {
            let _ = reply.send(Err("capture needs the gpu renderer".into()));
            return;
        }
        // 프레임당 한 장만 읽는다. 헤드리스 autocapture 와 겹치면 그쪽이 먼저다 —
        // 덮어쓰면 그 검증이 엉뚱한 pane 을 찍고 조용히 통과한다.
        if self.gpu.as_ref().is_some_and(|g| g.capture_next.is_some()) {
            let _ = reply.send(Err("another capture is already armed; retry".into()));
            return;
        }
        // 창 전체는 크롭을 안 세운다 — 아래 pane 갈래가 잘라내던 것을 안 자르는 것뿐이다.
        let crop = if pane.is_empty() {
            None
        } else {
            let (gcols, grows) = self.window_cells();
            let Some((_, cx, cy, cw, ch)) = self
                .effective_leaf_rects(gcols, grows)
                .into_iter()
                .find(|(id, ..)| id == pane)
            else {
                // 줌 중이면 가려진 pane 은 rect 가 아예 없다. 「없는 pane」과 구분해
                // 답해야 부른 쪽이 줌을 풀 생각을 한다.
                let zoomed = self.zoomed_pane.is_some();
                let _ = reply.send(Err(if zoomed {
                    format!("{pane} is hidden behind a zoomed pane")
                } else {
                    format!("no such pane: {pane}")
                }));
                return;
            };
            let s = self.effective_scale();
            let px = (WINDOW_PADDING + self.effective_sidebar_w() + cx as f32 * self.cell.w) * s;
            let py = (TITLE_HEIGHT + cy as f32 * self.cell.h) * s;
            let pw = (cw as f32 * self.cell.w * s).round().max(1.0) as u32;
            let ph = (ch as f32 * self.cell.h * s).round().max(1.0) as u32;
            Some((px.max(0.0) as u32, py.max(0.0) as u32, pw, ph))
        };
        let path = path.unwrap_or_else(|| {
            let name = if pane.is_empty() {
                "kasaterm-window.png".to_string()
            } else {
                format!("kasaterm-capture-{}.png", pane.trim_start_matches('%'))
            };
            std::env::temp_dir().join(name).to_string_lossy().into_owned()
        });
        if let Some(g) = self.gpu.as_mut() {
            g.capture_crop = crop;
            g.capture_max_w = max_w;
            g.capture_next = Some(path.clone());
        }
        self.pending_capture_reply.push((path, reply));
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 무장한 캡처가 실제로 파일로 떨어졌는지 확인하고 회신한다(렌더 직후 호출).
    pub(crate) fn settle_pane_captures(&mut self) {
        if self.pending_capture_reply.is_empty() {
            return;
        }
        // 아직 안 그려진 요청이 남아 있으면 이번엔 건너뛴다 — 무장이 그대로면
        // 렌더가 소비하지 않은 것이다.
        if self.gpu.as_ref().is_some_and(|g| g.capture_next.is_some()) {
            return;
        }
        for (path, reply) in std::mem::take(&mut self.pending_capture_reply) {
            let msg = match std::fs::metadata(&path) {
                Ok(m) if m.len() > 0 => Ok(serde_json::json!({
                    "path": path,
                    "bytes": m.len(),
                })),
                Ok(_) => Err(format!("capture wrote an empty file: {path}")),
                Err(e) => Err(format!("capture produced no file ({path}): {e}")),
            };
            let _ = reply.send(msg);
        }
    }

    fn render_frame_gpu(&mut self, scale: f32, time_secs: f32) {
        // 두 하단바 높이는 설정값이라 프레임 내내 여러 번 읽힌다. 여기서 한 번
        // 뽑아 두는 건 값이 도중에 바뀔 일이 없어서이기도 하지만, 아래쪽 대부분이
        // `self.gpu` 를 빌린 안쪽이라 거기서 `&self` 메서드를 다시 못 부르기 때문이다.
        let status_h = self.status_h();
        let pane_footer_h = self.pane_footer_h();
        // Glyph-atlas repack, if one is pending. This is the only safe point
        // for it: from here on the frame emits quads whose UVs index the
        // current packing, so a repack mid-frame would show them whatever
        // texels land in those slots instead. Everything this frame needs
        // re-bakes below.
        if let Some(g) = self.gpu.as_mut() {
            g.maintain_atlas();
        }
        // Keep the header breadcrumb's cwd cache fresh (self-rate-limited).
        self.refresh_pane_cwds();
        // File-tree column follows the active pane's cwd (rebuild on change).
        if self.file_tree.visible {
            self.refresh_file_tree();
        }
        // Git column follows the same active-pane cwd; publish it so the
        // off-thread poller refreshes the right repo.
        self.publish_git_col_cwd();
        // Info 탭이 열려 있으면 프로세스·포트 스냅샷을 갱신(스로틀은 내부에서).
        self.pump_info();
        self.pump_sessions_col();
        self.pump_mcp_col();
        // Every pane's status bar wants its own repo badge — feed all pane cwds
        // to the same poller.
        self.publish_pane_git_cwds();
        // Mirror each pane's cwd+badge for the BA GUI's `/layout` (Warp bar on
        // plain terminal tiles). Reads the caches above; no extra git/lsof.
        self.publish_pane_status();
        let Some(window) = self.window.as_ref() else { return };
        // Snapshot for the launch banner before the &mut self.gpu borrow
        // below (which rules out re-borrowing &self inside that block).
        let win_size = window.inner_size();
        let win_px = (win_size.width as f32, win_size.height as f32);
        let version_alpha = self.version_alpha();
        // URL under the mouse right now (pane id + cell range). Hovering it
        // draws a blue underline; computed before the workspace lock below so
        // it doesn't re-enter it. None when the cursor isn't over a link.
        let hovered_link = self.link_hit(self.cursor_px.0, self.cursor_px.1);
        let cell_w_px = self.cell.w * scale;
        let cell_h_px = self.cell.h * scale;
        // Snapshot per-pane cell grids while we hold the workspace
        // lock so the render call below can run without re-locking
        // (matches the sugarloaf path's design).
        struct PaneSlot {
            rows: Vec<Vec<GridCell>>,
            origin_px: (f32, f32),
            dim: bool,
            font_scale: f32,
            /// The single URL range under the mouse, if it's in this pane —
            /// drawn as a blue hover underline. Empty otherwise (links only
            /// show on hover, not always-on).
            links: Vec<crate::links::LinkSpan>,
            /// pane 기본 전경색(tmux window-style fg 등가) — 학생 pane 은 accent
            /// 틴트, 무배정은 테마 default fg. slot 빌드 시 pane 당 1회 결정.
            default_fg: [u8; 4],
        }
        // Header chrome carried in LOGICAL px — gpu.rect/draw_text
        // promote to physical internally, matching the cell pass.
        #[allow(dead_code)]
        struct HeaderInfo {
            id: String,
            x: f32,
            y: f32,
            w: f32,
            /// Full pane box height (header + body) in logical px, used
            /// to draw the divider / active-focus ring around the pane.
            box_h: f32,
            label: String,
            is_active: bool,
            color: Option<[u8; 4]>,
            /// Markdown panes get Render/Raw toggle pills in the header.
            is_markdown: bool,
            /// Current markdown mode (true = Raw editor) for pill highlighting.
            md_raw_mode: bool,
            /// Image panes get zoom/rotate buttons instead of the terminal-action cluster.
            is_image: bool,
            /// Web panes get back/forward/reload/open-external instead of the
            /// terminal cluster — split/statusbar buttons don't fit a browser.
            is_web: bool,
            /// 웹 pane 의 현재 주소(활성 탭) — 헤더 주소 pill 표시용. 페이지
            /// 이동을 따라간다(webpane 의 500ms 주소 폴링이 WebPane.url 갱신).
            web_url: Option<String>,
            /// 웹 pane 페이지 로딩 중 — 헤더 작업 바를 켜고 리로드 버튼을
            /// ×(정지)로 바꾼다.
            web_loading: bool,
            /// In-pane tab labels (empty = single-tab; header shows `label`).
            tabs: Vec<String>,
            /// Per-tab "is this a file tab (markdown/text editor, not a shell)"
            /// — drives the hover pop-out icon. Same length/order as `tabs`;
            /// empty when `tabs` is (single-tab fallback uses `single_is_file`).
            tab_is_file: Vec<bool>,
            /// Single-tab fallback: whether the lone tab is a file editor.
            single_is_file: bool,
            /// Active tab index into `tabs`.
            active_tab: usize,
            /// Overflow windowing: first tab drawn (pane.tab_first snapshot).
            tab_first: usize,
            /// `active_tab` at the previous frame — a mismatch means a tab
            /// switch happened and the strip must reveal the new active tab.
            tab_last_active: usize,
            /// True while this pane is working (daemon transcript watcher sees a
            /// running tool, cross-window). Draws the flowing bar along the
            /// header bottom; idle panes draw nothing.
            busy: bool,
            /// True when a background shell / Monitor is in-flight but no spinner
            /// shows (from the transcript tail, not the glyph scan). When `busy`
            /// is false, this draws the slower pulse bar instead.
            bg_active: bool,
            /// True while claude compacts its conversation. `busy` 도 같이 참이지만
            /// 이쪽이 이기고 채워지는 바를 그린다 — compact 는 끝이 있는 작업이라
            /// 쓸림바로는 「얼마나 남았나」가 안 읽히고, 화면에 뜨는 알림은 teammate
            /// 메시지 오버레이에 가려질 수 있어 헤더가 그 신호를 들어야 한다.
            compacting: bool,
            /// 화면의 `▰▰▱ N%` 에서 읽은 compact 진행률. Some 이면 바를 이 값으로
            /// 채우고(진짜 진행률), None 이면 시간 루프로 폴백한다.
            compact_pct: Option<u8>,
        }
        // Captured once so the &mut self.gpu block below (which can't
        // re-borrow &self) can still see the collapsed/expanded width.
        // `sidebar_w` = full left chrome (tabs + tree) for the cell-grid
        // origin; the tab strip and tree column have their own widths so
        // each paints into its own band.
        let sidebar_w = self.effective_sidebar_w();
        let tab_strip_w = self.tab_strip_w();
        let tree_col_x = self.file_tree_col_x();
        let tree_col_w = self.file_tree_col_w();
        // Right-hand git column geometry (logical px) + this frame's status
        // snapshot, all captured before the &mut self.gpu block (which can't
        // re-borrow &self). `git_reserve` is what the rightmost pane's stretch
        // must leave free on the right: the column plus one window padding, or
        // 0 when the column is hidden (so the pane keeps hugging the edge).
        let git_col_w = self.git_col_w();
        let git_col_x = (win_px.0 / scale - git_col_w).max(0.0);
        let git_reserve = if git_col_w > 0.0 {
            git_col_w + WINDOW_PADDING
        } else {
            0.0
        };
        let git_view = self
            .git.col_data
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        // Distinct repos to offer in the path dropdown. Union of every cwd we
        // can see — the badge cache (`window_git` keys), the pane-cwd cache,
        // and the column's current repo — so the list isn't empty when one
        // source is sparse (the daemon-mode pane-cwd cache often is). Deduped
        // + sorted for a stable order.
        let git_repo_list: Vec<std::path::PathBuf> = {
            let mut set: std::collections::BTreeSet<std::path::PathBuf> = self
                .window_git
                .lock()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            set.extend(self.pane_cwd_cache.values().cloned());
            if let Some(cur) = git_view.cwd.clone() {
                set.insert(cur);
            }
            set.into_iter().collect()
        };
        let pad_px = (WINDOW_PADDING + sidebar_w) * scale;
        let title_px = TITLE_HEIGHT * scale;
        // Per-pane font multipliers (keyed by pty/leaf id), so each pane's
        // glyphs can be sized independently of the shared base cell.
        let pane_scales = self.pane_font_scales.clone();
        // Code-block copy buttons (text + logical rect), filled per pane in
        // the loop below and handed to both the mouse handler and overlay.
        // Image panes collected here (id, pixels, body box in LOGICAL px) so
        // the gpu block below can upload + queue them after the cell pass.
        // (pid, image_data, body_box, zoom, rotation_quarters, pan_xy)
        let mut image_slots: Vec<(String, Arc<ImagePane>, (f32, f32, f32, f32), f32, u8, (f32, f32))> =
            Vec::new();
        // Claude Code 시작 배너의 Clawd 아트 자리에 그릴 학생 도트:
        // (에셋 슬러그, 배너 박스 LOGICAL px). 셀 스냅샷에서 감지·수집.
        // (slug, 배너 박스 rect, pane 세로 클립(y0, y1)) — 박스는 스크롤로
        // pane 밖까지 이어질 수 있고, 그리기는 클립 범위 안만.
        let mut banner_slots: Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))> =
            Vec::new();
        // agents 뷰 SCHALE 로고 자리(Clawd 마스코트 위치 / 헤더 왼쪽 여백) — 위치만.
        let mut schale_logo_slots: Vec<(f32, f32, f32, f32)> = Vec::new();
        // agents 목록·resume 피커 화면의 교실 배경(셀 뒤 cover-fit). pane 본문 rect.
        let mut classroom_slots: Vec<(f32, f32, f32, f32)> = Vec::new();
        // /rename 세션명 아웃라인 (x,y,w,h,color) — 입력박스 위 구분선 이름을 사각 테두리로.
        let mut title_outline_slots: Vec<(f32, f32, f32, f32, [u8; 4])> = Vec::new();
        // Claude Code 스크롤 sticky prompt → 웹뷰풍 pill: (px, py, pw, ph, text,
        // pane_id). logical px. 스캔 루프에서 감지·수집, chrome 패스에서 그린다.
        // (px, py, pw, ph, 텍스트, pane_id, ↑ rect, ↓ rect). 화살표 자리는 셀 폭을
        // 아는 스캔 루프에서 미리 재 둔다 — chrome 패스에서 되재면 어긋난다.
        type StickySlot = (
            f32, f32, f32, f32, String, String,
            Option<(f32, f32, f32, f32)>, Option<(f32, f32, f32, f32)>,
        );
        let mut sticky_pill_slots: Vec<StickySlot> = Vec::new();
        // 대화 턴 헤더 — (pane_id, 바 rect, ↑ rect, ↓ rect, 헤더 내용). logical px.
        // 화살표 rect 는 갈 곳이 있을 때만 담긴다(흐린 화살표는 눌러도 무반응).
        type TurnSlot = (
            String,
            (f32, f32, f32, f32),
            Option<(f32, f32, f32, f32)>,
            Option<(f32, f32, f32, f32)>,
            crate::turnjump::TurnHeader,
        );
        let mut turn_header_slots: Vec<TurnSlot> = Vec::new();
        // pane 별 헤더는 **여기서** 지어 둔다. 짓는 데 `&mut self`(앵커 캐시)가 필요한데
        // 아래 pane 루프는 ws 락을 쥔 채 돌아, 그 안에서 `pty_for_pane`(자체 락)을
        // 부르면 같은 뮤텍스를 두 번 잡는다. 락을 잡기 전에 끝내는 것이 그 함정을
        // 구조적으로 없앤다.
        let turn_headers: std::collections::HashMap<String, crate::turnjump::TurnHeader> = {
            let ids: Vec<String> = self
                .ws
                .lock()
                .ok()
                .map(|ws| ws.panes.keys().cloned().collect())
                .unwrap_or_default();
            self.turn.retain_panes(|id| ids.iter().any(|k| k == id));
            let mut out = std::collections::HashMap::new();
            for id in ids {
                // Arc 를 복제해 self 빌림을 끊는다 — 참조를 든 채로는 캐시를 못 고친다.
                let Some(sess) = self.pty_for_pane(&id).cloned() else { continue };
                if let Some(h) = self.turn.header(&id, &sess) {
                    out.insert(id, h);
                }
            }
            out
        };
        // 인라인 이미지 이번 프레임 배치 — (텍스처 키, 파일, x, y, w, h, clip_y0,
        // clip_y1, hug). 좌표는 LOGICAL px(queue_image 관례). hug=박스를 그림 비율로
        // 좁혀 왼쪽에 붙인다(글 흐름 그림용, OSC 1337 은 박스가 이미 맞아 false).
        let mut inline_slots: Vec<(String, String, f32, f32, f32, f32, f32, f32, bool)> =
            Vec::new();
        // 커서가 멎은 `[Image #N]` — (pane, 번호, 그 글자의 화면 박스). 박스는
        // 툴팁을 글자 바로 옆에 붙이는 데 쓴다.
        let mut tip_hit: Option<(String, u32, (f32, f32, f32, f32))> = None;
        // working 스피너(✻/braille) 자리 학생 도트(제자리 걸음): 같은 형태.
        let mut spinner_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // 승인 대기(approval prompt) 학생 도트(폴짝 바운스): 같은 형태.
        let mut waiting_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // statusline 자리표시자(U+FFFC) → 학생 프사(bust, 정적 1프레임).
        let mut profile_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // statusline 프사의 hover 확대·클릭용 (학생이름, slug, rect). profile_slots
        // 와 달리 학생 이름을 들고 있어(hover 큰 bust·클릭→학생설정 딥링크) — /resume
        // 피커 프사는 이름을 모르므로 여기 안 담고 statusline 프사만.
        // 입력박스 위 스페이서 행(effort 칩 자리)에 서 있는 학생(전신 애니).
        // 두 번째 필드 = 모션: "cheer"(턴 완료 직후) 또는 "idle"(대기).
        let mut standing_slots: Vec<(&'static str, &'static str, (f32, f32, f32, f32))> =
            Vec::new();
        // Markdown panes: (id, doc, body box, scroll px, raw_mode, edit lines,
        // cursor, selection, h_scroll px, syntax lang). Render mode draws
        // blocks; Raw mode draws the editor buffer.
        #[allow(clippy::type_complexity)]
        let mut md_slots: Vec<(
            String,
            Arc<MarkdownDoc>,
            (f32, f32, f32, f32),
            f32,
            bool,
            Option<Arc<Vec<String>>>,
            (usize, usize),
            Option<((usize, usize), (usize, usize))>,
            f32,
            &'static str,
            Option<FindState>,
            Option<(Vec<String>, usize, usize)>,
            Vec<crate::lsp::Diag>,
            crate::markdown::Folds,
            bool,
            Vec<crate::markdown::Caret>,
        )> = Vec::new();
        // Per-pane body rect (header-excluded) in logical px, collected for
        // every pane so in-pane WebViews and other overlays can be snapped
        // to their pane after the borrow scope ends.
        let mut body_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let (slots, headers, footer_slots, agents_view_panes): (
            Vec<PaneSlot>,
            Vec<HeaderInfo>,
            Vec<(String, f32, f32, f32, f32)>,
            std::collections::HashSet<String>,
        ) = {
            let ws = self.ws.lock().unwrap();
            let active_id = ws.active_pane.clone();
            // Total grid rows/cols — used to detect the bottom-row / right-col
            // pane so it can stretch to the window's true edge (window_cells
            // floors both, leaving a sub-cell remainder otherwise).
            let (grid_cols, grid_rows) = self.window_cells();
            // tmux-style zoom: render only the zoomed pane, filling the grid and
            // hiding the rest. Skips when the zoomed pane isn't in this window's
            // map (closed / moved out) so no phantom paints.
            let zoom_leaves: Option<Vec<(String, u16, u16, u16, u16)>> =
                match self.zoomed_pane.as_deref() {
                    Some(z) if ws.panes.contains_key(z) => {
                        // 사방을 들여 「떠 있는 카드」로 — inset 규칙은
                        // effective_leaf_rects(PTY resize·히트테스트)와 같은
                        // 함수를 써야 그린 칸과 PTY 가 어긋나지 않는다.
                        let (ix, iy) = self.zoom_inset_cells(grid_cols, grid_rows);
                        Some(vec![(
                            z.to_string(),
                            ix,
                            iy,
                            grid_cols - ix * 2,
                            grid_rows - iy * 2,
                        )])
                    }
                    _ => None,
                };
            let leaves: Vec<(String, u16, u16, u16, u16)> = if let Some(z) = zoom_leaves {
                z
            } else if let Some(layout) = ws.layout.as_ref() {
                layout
                    .leaves()
                    .into_iter()
                    .filter_map(|n| match n {
                        Layout::Pane { id, x, y, w, h } => {
                            Some((format!("%{id}"), *x, *y, *w, *h))
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                // Single-pane fallback (no split tree). `ws.panes` holds EVERY
                // window's pane (a session shares one pane map across its
                // windows), so an arbitrary entry would draw another
                // window's/session's pane here — the dead-pane "resurrection"
                // in an emptied window. Honor ONLY the active pane; if it's
                // unset/gone, draw nothing and let the next State broadcast set
                // the right one. Never fall back to an arbitrary HashMap entry.
                let active = active_id
                    .as_ref()
                    .filter(|id| ws.panes.contains_key(*id))
                    .cloned();
                match active {
                    Some(id) => vec![(id, 0, 0, 0, 0)],
                    None => Vec::new(),
                }
            };
            // Header bar when split OR when any pane carries multiple tabs.
            // A lone pane with a single tab stays header-less so the first
            // session reads as a plain terminal; but a lone pane with two or
            // more tabs (after a cross-pane drag, or a +button add) MUST
            // keep its strip so the tabs stay reachable.
            // ghostty식: 상시 헤더 띠 폐기 → 셀 시프트 0, 헤더 paint 없음.
            // 비활성 pane dim(흐림)만 split 여부로 유지. pane 컨트롤은
            // hover ⋮ 핸들로 이관(Phase 2~4).
            let is_split = leaves.len() > 1;
            let mut slots = Vec::new();
            let mut headers = Vec::new();
            // Box geometry per leaf (id, x, y, w, h) in logical px — collected
            // for EVERY pane, headered or not, so the per-pane status bar can
            // anchor to the box bottom even on a lone unsplit pane.
            let mut footer_slots: Vec<(String, f32, f32, f32, f32)> = Vec::new();
            // claude agents(에이전트 목록 뷰)로 판정된 pane 집합 — 개별 학생 대신
            // SCHALE 조직 정체성(타이틀·테두리)으로 표시한다. 판정은 루프 안에서
            // argv(is_claude_agents) + statusline 프사 슬롯(U+FFFC) 부재로 하고,
            // 루프 뒤 타이틀바·테두리 패스가 이 집합을 읽는다.
            let mut agents_view_panes: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for (id, x_cells, y_cells, w_cells, h_cells) in leaves {
                let Some(pane) = ws.panes.get(&id) else { continue };
                // pane.cells already holds the correct view: the PTY
                // backend snapshots through alacritty's display_offset,
                // so a scrolled-up frame arrives here pre-composed with
                // real scrollback (scroll-region TUIs included). Just
                // normalise each row to the current width so the GPU
                // pipeline emits exactly `cols` cells per row.
                // During a divider drag we DEFER the PTY reshape (SIGWINCH +
                // shell repaint is what causes the flicker), so the PTY's
                // reported cols/rows are stale. Clip the rendered cells to
                // the layout's CURRENT pane rect — overflow gets dropped at
                // the new edge instead of bleeding into the neighbouring
                // pane. After release, the final resize_backend lets the
                // shell catch up and the clip is a no-op.
                //
                // Single-pane fallback path (no layout tree yet) passes
                // (0,0,0,0) as a placeholder — that would clip everything
                // to nothing, so skip the layout clip entirely when w_cells
                // or h_cells is 0 and just trust the PTY dims.
                let pty_cols = pane.term().map_or(1, |t| t.cols).max(1) as usize;
                let pty_rows = pane.term().map_or(0, |t| t.cells.len());
                let (cols_now, rows_now) = if w_cells == 0 || h_cells == 0 {
                    (pty_cols, pty_rows)
                } else {
                    // Mirror resize_backend EXACTLY: pane box in base-grid px,
                    // minus real insets/header, divided by the ZOOMED cell.
                    // The clip has to land on the same count the PTY was sized
                    // to, or a zoomed-out pane (more cols/rows in the PTY) gets
                    // truncated back to the base-grid count and the TUI's
                    // layout tears.
                    let fs = pane_scales
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(1.0)
                        .max(0.1);
                    let cw = self.cell.w.max(1.0);
                    let ch = self.cell.h.max(1.0);
                    let scaled_cw = cw * fs;
                    let scaled_ch = ch * fs;
                    let header_px_now = pane.header_px();
                    let footer_px_now = self.statusbar_px(id.as_str());
                    let usable_w = (w_cells as f32 * cw - 2.0 * PANE_INNER_X).max(scaled_cw);
                    let usable_h = (h_cells as f32 * ch
                        - header_px_now
                        - footer_px_now
                        - 2.0 * PANE_INNER_Y)
                        .max(scaled_ch);
                    let layout_cols = (usable_w / scaled_cw).floor() as usize;
                    let layout_rows = (usable_h / scaled_ch).floor() as usize;
                    (layout_cols.min(pty_cols).max(1), layout_rows.min(pty_rows))
                };
                let normalise = |row: &Vec<GridCell>| -> Vec<GridCell> {
                    let mut r = row.clone();
                    if r.len() < cols_now {
                        r.resize(cols_now, GridCell::blank());
                    } else if r.len() > cols_now {
                        r.truncate(cols_now);
                    }
                    r
                };
                // Image/markdown panes carry no PTY grid; an empty rows vec
                // makes draw_cells a no-op and the content (texture or laid-out
                // document) is painted into the pane box instead (queued below).
                let img = pane.image().cloned();
                let img_zoom = pane.image_view_zoom();
                let img_rot = pane.image_rot % 4;
                let img_pan = (pane.image_pan_x, pane.image_pan_y);
                // Snapshot markdown render data: (doc, raw_mode, edit lines if
                // raw, cursor, normalized selection, scroll px, h_scroll px,
                // syntax lang).
                let md: Option<(
                    Arc<MarkdownDoc>,
                    bool,
                    Option<Arc<Vec<String>>>,
                    (usize, usize),
                    Option<((usize, usize), (usize, usize))>,
                    f32,
                    f32,
                    &'static str,
                    Option<FindState>,
                    Option<(Vec<String>, usize, usize)>,
                    crate::markdown::Folds,
                    bool,
                    Vec<crate::markdown::Caret>,
                )> = pane.markdown().map(|m| {
                    (
                        m.doc.clone(),
                        m.raw_mode,
                        // 프레임마다 도는 자리다 — Arc 라 포인터 하나 복사.
                        m.raw_mode.then(|| Arc::clone(&m.edit_lines)),
                        (m.cur_line, m.cur_col),
                        m.sel_range(),
                        m.scroll,
                        m.h_scroll,
                        code_lang_for_path(std::path::Path::new(&m.doc.path)),
                        m.find.clone(),
                        // 팝업이 열렸을 때만 후보를 복사한다(최대 8개).
                        m.complete
                            .as_ref()
                            .map(|c| (c.items.clone(), c.sel, c.from_col)),
                        m.folds.clone(),
                        m.wrap,
                        m.extra.clone(),
                    )
                });
                let mut composed: Vec<Vec<GridCell>> = match pane.term() {
                    Some(t) => t.cells.iter().take(rows_now).map(normalise).collect(),
                    None => Vec::new(),
                };
                // 학생 도트·배너·스피너 같은 claude 화면 해석은 **claude 가 실제로
                // 도는 pane** 에서만 한다. 화면 모양만 보고 판정하면 남의 TUI 를
                // claude 로 오인한다 — helix 의 LSP 진행 스피너가 브라유라, 파일을
                // 편집기 pane 으로 열면 학생 도트가 편집기 상태줄 위에 올라앉았다
                // (실측). alt screen 여부로는 못 가른다: claude code 2.1.220 도
                // alt screen 을 쓴다(tmux `#{alternate_on}` 으로 helix·claude 양쪽
                // 실측 — 둘 다 1).
                // active_process_name 은 셸의 **직속** 자식이라, claude 가 안에서
                // cargo·vim 을 띄워도 여전히 claude 다(그것들의 부모는 claude).
                // 500ms 캐시가 이미 붙어 있어 매 프레임 불러도 싸다.
                // 학생 상태는 **탭 pid** 로 기록되고 이 루프가 든 `id` 는 BSP leaf 다.
                // 접지 않으면 탭에서 도는 클로드가 안 잡혀, 프사·전신·배너 도트가
                // 통째로 안 뜬다(거노 2026-08-07). 아래 ordinal 도 같은 키를 쓴다.
                let tab_pid = ws.active_tab_pid(&id);
                let agent_kind =
                    self.pty.get(tab_pid.as_str()).and_then(|p| p.active_agent());
                let runs_claude = agent_kind.is_some();
                // Cells start below the header band when split, and are
                // inset inside the pane box so text never jams the divider
                // or window edge.
                let header_shift_px = pane.header_px() * scale;
                let origin_px = (
                    pad_px + x_cells as f32 * cell_w_px + PANE_INNER_X * scale,
                    title_px
                        + y_cells as f32 * cell_h_px
                        + header_shift_px
                        + PANE_INNER_Y * scale,
                );
                // Code-block copy buttons: scan this pane's grid for bg
                // boxes (Claude Code code/command blocks) and stash a copy
                // button at each block's top-right. Logical px so the mouse
                // handler and the overlay pass agree on the hit area.
                let header_shift_logical = pane.header_px();
                let body_left = WINDOW_PADDING
                    + sidebar_w
                    + x_cells as f32 * self.cell.w
                    + PANE_INNER_X;
                let body_top = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
                // 인라인 이미지(OSC 1337) — PTY 가 절대 줄 앵커를 뷰포트 좌표로
                // 환산해 준 그대로 그린다. GUI 는 스크롤 상태를 모르므로 여기서
                // 계산을 더하면 반드시 어긋난다(정본은 alacritty Term). 클립은
                // pane 셀 영역 — 스크롤로 반쯤 나간 그림이 셀과 함께 잘린다.
                if let Some(t) = pane.term().filter(|t| !t.inline_images.is_empty()) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let (icw, ich) = (self.cell.w * fs, self.cell.h * fs);
                    let clip_y0 = body_top;
                    let clip_y1 = body_top + rows_now as f32 * ich;
                    for v in &t.inline_images {
                        inline_slots.push((
                            format!("inline:{}:{}:{}", tab_pid, v.id, v.path),
                            v.path.clone(),
                            body_left + v.col as f32 * icw,
                            body_top + v.row as f32 * ich,
                            v.cols as f32 * icw,
                            v.rows as f32 * ich,
                            clip_y0,
                            clip_y1,
                            false,
                        ));
                    }
                }
                // 글 흐름 안 그림 — `[[img:<경로>:<행수>]]` 표식이 잡은 자리에 얹는다.
                // OSC 1337 을 못 쓰는 claude pane 을 위한 길이라(그쪽 함수 주석) 셸
                // pane 에도 그대로 열어 둔다: `echo '[[img:a.png:12]]'` 로도 뜬다.
                {
                    let blocks = find_image_blocks(&composed);
                    if !blocks.is_empty() {
                        let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                        let (icw, ich) = (self.cell.w * fs, self.cell.h * fs);
                        let clip_y0 = body_top;
                        let clip_y1 = body_top + rows_now as f32 * ich;
                        for b in &blocks {
                            blank_image_block(&mut composed, b);
                            let h = b.rows as f32 * ich;
                            // 가로는 3:1 까지만 벌린다. 박스를 pane 폭으로 두면
                            // 넓은 창에서 그림이 한가운데로 밀려(contain-fit 은 중앙
                            // 정렬) 글 흐름에서 떨어져 보인다. 스크린샷 대부분이
                            // 16:9(1.78) 라 이 안에 들어 왼쪽에서 시작한다.
                            let w = (h * 3.0).min(cols_now as f32 * icw);
                            inline_slots.push((
                                format!("mdimg:{tab_pid}:{}", b.path),
                                b.path.clone(),
                                body_left,
                                body_top + b.row as f32 * ich,
                                w,
                                h,
                                clip_y0,
                                clip_y1,
                                true,
                            ));
                        }
                    }
                }
                // `[Image #N]` 위에 멎은 커서. 셀 역산은 이 pane 의 원점·폰트배율로
                // 하고 행·열 범위로 잘라 낸다 — 옆 pane 위의 커서는 이 pane 의 셀
                // 범위를 넘어서므로 여기서 걸러진다. 참조 탐색은 커서가 이 pane 의
                // 셀 안에 있을 때만 돈다(그리드 전수 스캔이라 매 프레임 모든 pane
                // 에 돌릴 일이 아니다).
                // 게이트가 claude 여부가 아니라 **세션이 묶였나**인 이유: 그림은
                // 그 세션의 transcript 에만 있어서, sid 가 없으면 찾아 봐야 없다.
                if tip_hit.is_none() && self.pane_claude_sid.contains_key(id.as_str()) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let (icw, ich) = (self.cell.w * fs, self.cell.h * fs);
                    let (rx, ry) = (self.cursor_px.0 - body_left, self.cursor_px.1 - body_top);
                    if rx >= 0.0 && ry >= 0.0 && icw > 0.0 && ich > 0.0 {
                        let (cc, cr) = ((rx / icw) as usize, (ry / ich) as usize);
                        if composed.get(cr).is_some_and(|row| cc < row.len()) {
                            if let Some(r) = find_image_refs(&composed)
                                .into_iter()
                                .find(|r| r.row == cr && (r.col0..=r.col1).contains(&cc))
                            {
                                tip_hit = Some((
                                    id.clone(),
                                    r.n,
                                    (
                                        body_left + r.col0 as f32 * icw,
                                        body_top + r.row as f32 * ich,
                                        (r.col1 - r.col0 + 1) as f32 * icw,
                                        ich,
                                    ),
                                ));
                            }
                        }
                    }
                }
                // Claude Code 스크롤 sticky prompt → 웹뷰풍 pill. mouse-tracking
                // 중이라 뷰포트 스크롤 여부를 직접 못 안다 — "Jump to bottom" 힌트로
                // 게이트한다(find_sticky_prompt). 감지 행 셀은 스냅샷에서 blank 처리해
                // 원본 흐릿한 텍스트를 지우고, 그 자리에 pill 을 얹는다. 클릭 rect 는
                // 아래 chrome 패스에서 STICKY_PILLS 로 mouse handler 에 넘긴다.
                //
                // pill 은 **프롬프트 띠 재도색과 같은 테마 스타일**로 칠한다 — 흰
                // pill 은 없애기로 했다(거노 2026-08-19: "흰색없애기로했었는데 클릭은
                // 되게하면서"). 08-15 재도색이 흰 pill 을 덮으면서 pill 이 박아 둔
                // 검은 글자만 남아 줄이 통째로 안 보였는데, 그 답은 흰색 복원이
                // 아니라 pill 자체를 테마 띠로 그리는 것이다. 클릭 rect·↑↓·seek 는
                // 색과 무관하게 그대로 산다.
                //
                // 재도색 스캔은 이 행을 건너뛴다 — pill 이 여기서 fg 까지 완성하므로
                // (재도색은 fg 를 ❯ 만 만진다) 다시 칠하면 이 선명화가 무너진다.
                let mut sticky_pill_row: Option<usize> = None;
                if let Some(sticky) = find_sticky_prompt(&composed) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let ncols = composed.get(sticky.row).map_or(0, |r| r.len());
                    let end = sticky.col_end.min(ncols);
                    sticky_pill_row = Some(sticky.row);
                    // 흰 배경 pill 을 pane 양끝(col 0..ncols)까지 채운다(거노: "흰색
                    // 바탕 pane 양끝으로 다 채워"). 클릭 rect 도 행 전체 폭 — 흰 바탕
                    // 어디를 눌러도 seek(begin_sticky_seek)가 걸린다.
                    let px = body_left;
                    let py = body_top + sticky.row as f32 * sch;
                    let arrow_rect = |c: usize| {
                        // 한 칸은 손가락으로 누르기 좁다 — 좌우 반 칸씩 넓혀 잡는다.
                        (px + c as f32 * scw - scw * 0.5, py, scw * 2.0, sch)
                    };
                    let (a_up, a_down) = crate::turnjump::sticky_arrow_cols(ncols);
                    sticky_pill_slots.push((
                        px,
                        py,
                        ncols as f32 * scw,
                        sch,
                        sticky.text.clone(),
                        id.clone(),
                        a_up.map(arrow_rect),
                        a_down.map(arrow_rect),
                    ));
                    if let Some(row) = composed.get_mut(sticky.row) {
                        // 원본 셀(등폭 그리드)을 지우지 않고 그 자리에서 선명화만
                        // 한다 — draw_text(proportional)로 다시 그리던 옛 방식은
                        // 한글 wide glyph 를 ink 폭으로 tighten 해 자간이 어긋났다
                        // (거노: "딱 안 맞아 자간 이상"). 그리드 셀은 등폭이라
                        // 폭·자간이 원본과 정확히 일치한다.
                        //
                        // 색은 프롬프트 띠 재도색과 **같은 공식**(테마 배경에 학생
                        // accent 를 살짝 섞은 fill + accent ❯) — sticky 는 「내가
                        // 친 프롬프트 줄」의 대리이니 같은 시각 언어여야 하고, 흰
                        // pill 은 없애기로 했다(위 주석). 본문 글자만 테마 텍스트색
                        // 으로 밝힌다 — claude 원본은 흐릿한 회색이라 띠 위에서
                        // 안 읽힌다. ↑↓(앞뒤 질문 건너뛰기)는 accent 로 세워 이
                        // 줄이 일반 띠가 아니라 조작 가능한 pill 임을 말한다.
                        let accent = self
                            .display_pane_char(&ws, &id)
                            .as_deref()
                            .or(pane.character.as_deref())
                            .and_then(|n| {
                                theme::character_accent_n(
                                    n,
                                    theme::character_ordinal(&ws.pane_character, &tab_pid),
                                )
                            })
                            .unwrap_or_else(|| theme::accent_color(theme::accent_name()));
                        let base = theme::bg();
                        let light =
                            base[0] as u16 + base[1] as u16 + base[2] as u16 > 380;
                        let amount = if light { 0.10 } else { 0.18 };
                        let fill =
                            tint_toward([base[0], base[1], base[2]], accent, amount);
                        let text = theme::text();
                        let (up_col, down_col) = crate::turnjump::sticky_arrow_cols(row.len());
                        for (i, cell) in row.iter_mut().enumerate() {
                            cell.dim = false;
                            cell.inverse = false;
                            cell.bg = fill.clone();
                            cell.fg = if cell.ch == '❯' || Some(i) == up_col || Some(i) == down_col {
                                kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2])
                            } else {
                                kasa_bridge::screen::Color::Rgb(text[0], text[1], text[2])
                            };
                            if Some(i) == up_col {
                                cell.ch = '↑';
                                cell.bold = true;
                                continue;
                            }
                            if Some(i) == down_col {
                                cell.ch = '↓';
                                cell.bold = true;
                                continue;
                            }
                            if i < sticky.col_start || i >= end {
                                cell.ch = ' ';
                            }
                        }
                    }
                }
                // 대화 턴 헤더 — 터미널 스크롤백을 올려다볼 때만 첫 행을 덮어쓴다.
                // 라이브 바닥이면 `turn_headers` 에 항목 자체가 없어서 평소 화면은
                // 손대지 않는다. 바로 위 sticky pill 과 자리를 다툴 일은 없다 —
                // 저쪽은 claude 가 **자기 버퍼를** 스크롤할 때뿐이고, 그때 터미널
                // 쪽 offset 은 0 이라 이 헤더가 아예 안 뜬다.
                if let Some(h) = turn_headers.get(id.as_str()) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let (hcw, hch) = (self.cell.w * fs, self.cell.h * fs);
                    if let Some(row) = composed.get_mut(0) {
                        let cols = crate::turnjump::paint_header_row(row, h);
                        let rect_at = |c: usize| {
                            // 화살표 한 칸은 손가락으로 누르기엔 좁다 — 좌우로 반 칸씩
                            // 넓혀 잡는다. 그래도 서로 두 칸 떨어져 있어 안 겹친다.
                            (body_left + c as f32 * hcw - hcw * 0.5, body_top, hcw * 2.0, hch)
                        };
                        turn_header_slots.push((
                            id.clone(),
                            (body_left, body_top, cols_now as f32 * hcw, hch),
                            cols.up.map(rect_at),
                            cols.down.map(rect_at),
                            h.clone(),
                        ));
                    }
                }
                // agents 목록 뷰 판정 — statusline 프사 슬롯(U+FFFC)이 있으면 실제
                // 대화 세션, 없고 argv 가 claude agents 면 관리 화면(목록 뷰). 세션에
                // 진입하면 statusline 이 붙어 자동으로 학생 표시로 넘어간다(argv 는
                // 진입해도 그대로 agents 라 단독으론 못 가름). 목록 뷰면 아래 학생
                // 스프라이트(배너·스피너·standing)·본문 틴트를 모두 건너뛰고 SCHALE
                // 조직 정체성(타이틀·테두리)만 준다.
                let has_profile_slot = composed
                    .iter()
                    .any(|row| row.iter().any(|c| c.ch == '\u{fffc}'));
                let agents_view = !has_profile_slot
                    && (self
                        .pty
                        .get(id.as_str())
                        .map(|p| p.is_claude_agents())
                        .unwrap_or(false)
                        || screen_is_agents_list(&composed));
                if agents_view {
                    agents_view_panes.insert(id.clone());
                    // 관리 화면 = SCHALE 조직 정체성. claude 캐릭터(Clawd) 자리에 SCHALE
                    // 로고를 얹는다(거노: 그 자리가 비어 보임). Clawd 블록아트가 있으면 그
                    // 자리를 지우고 동일 위치에, 없으면(agents 목록) "Claude Code" 헤더
                    // 왼쪽 여백에 앵커한다. 로고는 정사각이라 폭을 셀 비율로 맞춘다.
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let logo_rows = CLAWD_ROWS;
                    let logo_cols =
                        ((logo_rows as f32 * sch / scw).round() as usize).max(3);
                    // SCHALE 로고는 클립 경로가 없어 완전 노출 배너만 쓴다 —
                    // 스크롤로 잘린 배너는 헤더 앵커 폴백(원본 글리프 유지).
                    let clawd = find_clawd_banners(&composed);
                    let anchor = clawd
                        .iter()
                        .find(|&&(br, _)| {
                            br >= 0 && br as usize + CLAWD_ROWS <= composed.len()
                        })
                        .map(|&(br, bc)| (br as usize, bc));
                    let anchor = if let Some((br, bc)) = anchor {
                        for row in composed[br..br + CLAWD_ROWS].iter_mut() {
                            for cell in row.iter_mut().skip(bc).take(CLAWD_COLS) {
                                *cell = GridCell::blank();
                            }
                        }
                        Some((br, bc))
                    } else {
                        find_agents_header_anchor(&composed, logo_cols)
                    };
                    if let Some((br, bc)) = anchor {
                        schale_logo_slots.push((
                            body_left + bc as f32 * scw,
                            body_top + br as f32 * sch,
                            logo_cols as f32 * scw,
                            logo_rows as f32 * sch,
                        ));
                    }
                }
                // Claude Code 시작 배너의 Clawd 아트 → 이 pane 학생의 도트로.
                // 학생 배정 pane(=claude 용도로 spawn된 pane)만 스캔한다.
                // 감지된 셀은 스냅샷에서 blank 처리해 자리를 비우고, 그
                // 자리에 도트 이미지를 queue한다 — 이미지 패스는 셀/chrome
                // 보다 먼저 그려지므로 비워진 셀 밑으로 도트가 보인다.
                // "터미널은 파싱만"(거노): claude sessionId 바인딩 우선, 뷰 pane 은
                // 파싱 전 스폰 랜덤 미표시 — display_pane_char(chrome.rs)가 규칙 정본.
                let true_char = self.display_pane_char(&ws, &id);
                if let Some((name, slug)) = true_char
                    .as_deref()
                    .filter(|_| !agents_view && runs_claude)
                    .and_then(|n| theme::character_slug(n).map(|s| (n, s)))
                {
                    // 같은 학생 pane 이 여럿이면(지정 스폰 중복 허용) 순번 변주색.
                    let accent = theme::character_accent_n(
                        name,
                        theme::character_ordinal(&ws.pane_character, &id),
                    );
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    // 그림이 없는 학생은 배너를 건드리지 않는다 — 지운 뒤 못
                    // 그리면 원래 있던 Clawd 배너까지 사라진다.
                    // claude 의 Clawd 아트와 agy 의 Antigravity 로고는 모양도 크기도
                    // 다르다. 어느 하네스로 떴는지 따지지 않고 둘 다 훑는다 — 화면에
                    // 실제로 그려진 로고가 정본이고, 한 pane 에 둘이 함께 뜰 일은 없다.
                    let logos: Vec<(isize, usize, usize, usize, &[char])> =
                        find_clawd_banners(&composed)
                            .into_iter()
                            .map(|(br, bc)| (br, bc, CLAWD_COLS, CLAWD_ROWS, CLAWD_TITLE))
                            .chain(find_agy_banners(&composed).into_iter().map(
                                |(br, bc)| (br, bc, AGY_COLS, AGY_ROWS, AGY_TITLE),
                            ))
                            .filter(|_| student_has_sprite(slug, "idle"))
                            .collect();
                    for (br, bc, lcols, lrows, title) in logos {
                        // br 은 스크롤로 위가 잘리면 음수, 아래가 잘리면 박스가
                        // 그리드 밖까지 이어진다 — 스프라이트는 pane 세로 범위로
                        // 클립해 셀 스크롤과 함께 자연스럽게 잘려 나가게 한다.
                        // 로고 칸이 Clawd 보다 크면 도트를 늘리지 말고 비율을 지켜
                        // 안에 맞춘 뒤 바닥에 세운다(발이 로고 밑선에 닿는다).
                        let (bw, bh) = fit_sprite_box(lcols, lrows, scw, sch);
                        banner_slots.push((
                            slug,
                            (
                                body_left
                                    + bc as f32 * scw
                                    + (lcols as f32 * scw - bw) * 0.5,
                                body_top + br as f32 * sch + (lrows as f32 * sch - bh),
                                bw,
                                bh,
                            ),
                            (body_top, body_top + composed.len() as f32 * sch),
                        ));
                        let r0 = br.max(0) as usize;
                        let r1 = (br + lrows as isize)
                            .clamp(0, composed.len() as isize)
                            as usize;
                        for row in composed[r0..r1].iter_mut() {
                            for cell in row.iter_mut().skip(bc).take(lcols) {
                                *cell = GridCell::blank();
                            }
                        }
                        // 배너 타이틀("Claude Code"·"Antigravity CLI")도 학생 이름으로 —
                        // 도트만 바뀌면 학생이 남의 이름표를 달고 서 있는 꼴(거노).
                        replace_banner_title(
                            &mut composed, br, bc, lcols, lrows, title, name, accent,
                        );
                        // 웰컴 배너("Welcome back <user>!")면 도트 위 인사말 행을
                        // 배정 학생 페르소나 인사말로 — launcher 화면에선 no-op.
                        replace_welcome_greeting(&mut composed, br, name, accent);
                    }
                    // codex 시작 패널: 세울 아트가 없어 이름표만 바꾼다. 도트 유무와
                    // 무관하니 위 로고 루프 밖이고, codex pane 일 때만 훑는다 —
                    // 배너와 달리 화면 전체를 봐야 해서 공짜가 아니다.
                    if agent_kind == Some(kasa_pty::AgentKind::Codex) {
                        let n = composed.len();
                        replace_banner_title(
                            &mut composed, 0, 0, 0, n, CODEX_TITLE, name, accent,
                        );
                    }
                    // working 스피너 자리 → 학생이 제자리 걸음으로 "작업 중".
                    // 스피너 글리프 셀은 스냅샷에서 비우고, 그 자리(스피너 행
                    // 바닥 정렬, 2행 높이)에 walk 도트를 icon 패스로 얹는다.
                    // 스피너가 없고 승인 프롬프트가 떠 있으면 → 질문 행 텍스트
                    // 끝 옆에서 폴짝 바운스("선생님, 승인 기다려요!"). pane
                    // 우상단은 collab 승인 토스트(윈도우 우상단)와 겹친다.
                    // 스피너 walk·승인대기 바운스가 뜨는 동안은 standing 도트를
                    // 숨긴다 — 같은 학생이 화면에 두 명 서 있으면 버그로 보인다.
                    let mut pet_busy = false;
                    // 본판정 + 프로브 확정 후보(턴 시작 첫 ~3초의 괄호 없는
                    // `✢ Transmuting…`). 확정은 refresh_pane_activity 가 글리프
                    // 변화로 세운다 — 여기서는 읽기만.
                    // 프로브 확정 전이라도 이 pane 에 방금 제출(Enter)이 있었으면
                    // 후보를 그 프레임부터 신뢰한다 — refresh 틱(100~300ms)을
                    // 기다리는 동안 claude 원색 스피너가 그대로 보이던 마지막
                    // 깜빡임 조각(거노 2026-08-20 「치자마자 0.1초동안
                    // 적용안되는거」). runs_claude 게이트 안이라 셸 출력 오탐
                    // 걱정은 없다.
                    let spinner_hit = find_claude_spinner(&composed).or_else(|| {
                        let trusted = self
                            .spinner_probe
                            .get(tab_pid.as_str())
                            .is_some_and(|&(_, _, confirmed, _)| confirmed)
                            || self
                                .pty
                                .get(tab_pid.as_str())
                                .and_then(|p| p.last_submit())
                                .is_some_and(|s| s.elapsed() < Self::SUBMIT_TRUST);
                        trusted
                            .then(|| unconfirmed_spinner_row(&composed))
                            .flatten()
                            .map(|(r, c, _)| (r, c))
                    });
                    if let Some((sr, sc)) = spinner_hit {
                        pet_busy = true;
                        // 스피너 행 텍스트("Cerebrating… · esc to interrupt")를
                        // 학생 accent 색으로 — walk 도트 + 텍스트색이 함께
                        // "이 학생이 작업 중"임을 말한다. 여기에 glow shimmer:
                        // accent 위로 밝은 밴드가 좌→우로 흐른다(claude code 의
                        // 반짝이는 텍스트). 밴드 중심은 시간에 따라 이동하고 각
                        // 셀은 중심과의 거리(가우시안)만큼 흰색에 lerp 된다.
                        // working 중엔 walk 애니 33ms 펌프가 재렌더를 이미 돌려
                        // 애니 비용이 추가로 들지 않는다.
                        if let Some(a) = accent {
                            use kasa_bridge::screen::Color;
                            let t = self.version_anim_start.elapsed().as_secs_f32();
                            let row = &composed[sr];
                            // glow/색은 동사 문구("Cerebrating…")까지만 — 뒤의
                            // "(esc to interrupt · N tokens)" 는 원래 dim 색을 둔다
                            // (거노: 문구만 glow). 줄임표(…) 다음을 경계로, 없으면
                            // "(" 앞, 그것도 없으면 행 끝.
                            let end = row
                                .iter()
                                .position(|c| c.ch == '…')
                                .map(|p| p + 1)
                                .or_else(|| row.iter().position(|c| c.ch == '('))
                                .unwrap_or(row.len());
                            let first = row
                                .iter()
                                .take(end)
                                .position(|c| !matches!(c.ch, ' ' | '\0'))
                                .unwrap_or(0);
                            let lastc = row
                                .iter()
                                .take(end)
                                .rposition(|c| !matches!(c.ch, ' ' | '\0'))
                                .unwrap_or(first);
                            let span = lastc.saturating_sub(first).max(1) as f32;
                            const PERIOD: f32 = 2.0; // 한 번 스윕(초)
                            const SIGMA: f32 = 2.0; // 밴드 폭(셀)
                            const GLOW: f32 = 0.9; // 밴드 중심 밝기(흰색 비율)
                            // 밴드가 문구 왼쪽 밖에서 오른쪽 밖으로 완전히 지나가게.
                            let sweep = (t / PERIOD).fract();
                            let center =
                                first as f32 - SIGMA * 2.0 + sweep * (span + SIGMA * 4.0);
                            for (idx, cell) in composed[sr].iter_mut().enumerate().take(end) {
                                if matches!(cell.ch, ' ' | '\0') {
                                    continue;
                                }
                                let d = idx as f32 - center;
                                let g = (-(d * d) / (2.0 * SIGMA * SIGMA)).exp() * GLOW;
                                let mix = |b: u8| (b as f32 + (255.0 - b as f32) * g).round() as u8;
                                cell.fg = Color::Rgb(mix(a[0]), mix(a[1]), mix(a[2]));
                            }
                            // 꼬리("(49s · thinking some more…)")도 학생 색 언어로 —
                            // glow 는 여전히 문구까지만(거노: 문구만 glow)이고, 꼬리는
                            // accent 를 테마 배경에 눕힌 차분한 톤. claude 가 제 주황을
                            // 남겨 두면 학생색 줄 한가운데 남의 색이 선다(2026-08-16
                            // 「almost done thinking 같은 거도 색 바꿔줘」).
                            let bg = theme::bg();
                            let tail = crate::screenread::tint_toward(
                                [bg[0], bg[1], bg[2]],
                                [a[0], a[1], a[2], 255],
                                0.6,
                            );
                            for cell in composed[sr].iter_mut().skip(end) {
                                if matches!(cell.ch, ' ' | '\0') {
                                    continue;
                                }
                                cell.fg = tail.clone();
                            }
                        }
                        // 스피너 글리프를 지우는 건 그 자리에 학생을 세울 수
                        // 있을 때만. 못 세우면 도는 표시가 통째로 없어진다.
                        if student_has_sprite(slug, "walk") {
                            composed[sr][sc] = GridCell::blank();
                            let top_r = sr.saturating_sub(1);
                            spinner_slots.push((
                                slug,
                                (
                                    body_left + sc as f32 * scw,
                                    body_top + top_r as f32 * sch,
                                    2.0 * scw,
                                    (sr - top_r + 1) as f32 * sch,
                                ),
                            ));
                        }
                    } else if !crate::input::rows_show_working(&composed)
                        && crate::input::rows_show_approval_prompt(&composed).is_some()
                    {
                        if let Some((ar, ac)) = approval_anchor(&composed) {
                            pet_busy = true;
                            const DOT: f32 = 40.0;
                            if student_has_sprite(slug, "wave") {
                                let pane_w = cols_now as f32 * scw;
                                let pane_h = rows_now as f32 * sch;
                                let dot = DOT.min(pane_w).min(pane_h);
                                if dot >= scw.min(sch) {
                                    let x = (body_left + (ac + 2) as f32 * scw)
                                        .clamp(body_left, body_left + pane_w - dot);
                                    let y = (body_top + (ar + 1) as f32 * sch - dot)
                                        .clamp(body_top, body_top + pane_h - dot);
                                    waiting_slots.push((slug, (x, y, dot, dot)));
                                }
                            }
                        }
                    }
                    // 입력창 위 standing 앵커. claude 는 statusline 표식(U+FFFC)에서
                    // 출발하지만 codex 는 그게 없어(위 `find_filled_standing_anchor`
                    // 주석) 입력행에서 바로 잡는다. 둘 다 못 잡으면 안 세운다.
                    let mut stand_anchor: Option<(usize, f32)> = None;
                    if let Some((sr, sc, len)) = find_statusline_face(&composed) {
                        for cell in composed[sr].iter_mut().skip(sc).take(len) {
                            *cell = GridCell::blank();
                        }
                        // 프사는 여기 안 그린다(거노 2026-08-11: "클로드코드 상태줄
                        // 학생프사는 없애자"). statusline 은 이제 `● 이름` 을 직접
                        // 찍고, 남은 U+FFFC 한 칸은 **신호**다 — 위 blank 로 지우고
                        // `sr` 만 standing 앵커로 쓴다. 자리표시자를 아예 없애면
                        // agents 뷰 판정(`has_profile_slot`)·stale statusline 복구
                        // (socket.rs)·이 앵커가 한꺼번에 죽는다.
                        let _ = (sc, len);
                        // 입력박스 위에 서 있는 학생(전신 idle) — 프롬프트 위
                        // 스페이서 행(effort 칩·context 경고가 뜨는 자리) 우측.
                        // statusline 바로 위 행이 아래 테두리(전폭 '─')면 그
                        // 위로 첫 '─' 행이 입력박스 윗 테두리다 — ❯ 영역이
                        // 여러 줄로 자라도 스캔이라 따라간다. 발은 윗 테두리
                        // 줄에 닿고, 칩이 떠 있으면 그 왼쪽으로 비켜 선다.
                        // working/승인대기 중엔 스피너 walk·바운스 도트가 이미
                        // 학생을 그리므로(pet_busy) 세우지 않는다. 앵커 규칙은
                        // `find_standing_anchor` 에 — 별도창도 같은 자리에 세운다.
                        // `KASATERM_STUDENT_DEBUG=1` — 왜 학생이 안 서는지 앱이
                        // 직접 말한다. 이 자리는 조건 셋(스피너 감지·pet_busy·앵커)이
                        // 겹쳐 있고 실패하면 **아무것도 안 그려** 밖에서 원인을 가릴
                        // 수 없다. 정적 스프라이트(프사)만 뜨고 애니가 안 뜬다는
                        // 신고를 받고도 코드 읽기로는 못 좁혔다(2026-08-05).
                        if std::env::var_os("KASATERM_STUDENT_DEBUG").is_some() {
                            use std::sync::{Mutex, OnceLock};
                            static LAST: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
                            let last = LAST.get_or_init(|| Mutex::new(std::time::Instant::now()));
                            let mut g = last.lock().unwrap();
                            if g.elapsed() >= std::time::Duration::from_millis(1000) {
                                *g = std::time::Instant::now();
                                let a = find_standing_anchor(&composed, sr, cols_now as usize);
                                eprintln!(
                                    "[student-debug] pane={id} slug={slug} face_row={sr} cols={cols_now} pet_busy={pet_busy} spinner={} anchor={a:?} rows={}",
                                    find_claude_spinner(&composed).is_some(),
                                    composed.len(),
                                );
                                if sr >= 4 {
                                    let rule = |r: usize| {
                                        let row = &composed[r];
                                        let d = row.iter().filter(|c| c.ch == '─').count();
                                        let l = row
                                            .iter()
                                            .filter(|c| !matches!(c.ch, '─' | ' ' | '\0'))
                                            .count();
                                        format!("dash={d}/{} label={l}", row.len())
                                    };
                                    eprintln!(
                                        "[student-debug]   아래테두리 rows[{}] {}",
                                        sr - 1,
                                        rule(sr - 1)
                                    );
                                }
                            }
                        }
                        stand_anchor = find_standing_anchor(&composed, sr, cols_now as usize);
                    }
                    // statusline 자리표시자가 없는 하네스(codex) — 입력행에서 바로.
                    if stand_anchor.is_none() {
                        stand_anchor = find_filled_standing_anchor(&composed, cols_now as usize);
                    }
                    // agy — 자리표시자도 채운 입력행도 없다. 모양은 claude 와 같은
                    // 대시 보더 두 줄인데 마커가 ASCII `>` 라 공용 판정에서 빠져 있다
                    // (인용문·diff 오인 방지, 2026-07-22). 앵커 규칙은 그대로 쓰고
                    // statusline 자리만 「맨 아래 보더 다음 행」으로 잡아 준다.
                    if stand_anchor.is_none() {
                        stand_anchor = find_agy_standing_anchor(&composed, cols_now as usize);
                    }
                    {
                        if !pet_busy {
                            if let Some((anchor, left_c)) = stand_anchor {
                                let h = (INPUT_STANDING_ROWS as f32 * sch)
                                    .min(rows_now as f32 * sch);
                                {
                                    // 턴 완료 직후 ~1.8s(notify_flash)는 양팔 만세
                                    // cheer, 그 뒤로 계속 대기하면 손 흔들며 기다리는
                                    // wave("선생님, 다음 지시 기다려요"). 학생 pane 은
                                    // bypass 모드라 승인 프롬프트가 안 떠 우상단 wave
                                    // 트리거가 사실상 죽어 있다 — wave 를 standing 순환에
                                    // 넣어야 "입력 기다림"이 보인다. 사용자가 이 pane 에
                                    // 타이핑하면 idle 로.
                                    let motion = if self.turn_done_panes.contains(&id) {
                                        if self.notify_flash_factor(&id).is_some() {
                                            "cheer"
                                        } else {
                                            "wave"
                                        }
                                    } else {
                                        "idle"
                                    };
                                    if student_has_sprite(slug, motion) {
                                        standing_slots.push((
                                            slug,
                                            motion,
                                            (
                                                body_left + left_c * scw,
                                                (body_top + (anchor + 1) as f32 * sch - h)
                                                    .max(body_top),
                                                STAND_CELLS * scw,
                                                h,
                                            ),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                // ` ultracode ` 배지는 지운다 — 모드는 입력박스 글로우가 이미
                // 말하므로 글자는 중복이고, 그 자리는 /rename 세션명 자리라 이름이
                // 바뀐 것처럼 읽힌다(2026-08-12 지적 「/rename 그자리에 ultracode
                // 써진다」). find_titled_rule 보다 먼저 — 지운 뒤엔 순수 rule 이다.
                erase_ultracode_badge(&mut composed);
                // /rename 세션명 아웃라인 — claude 입력박스 위 "── 세션명 ──" 구분선의
                // 이름 텍스트 섬을 찾아 그 셀 범위를 rename/학생 색 사각 테두리로 두른다
                // (거노). 순수 '─' rule·statusline·입력행은 걸러진다. 테두리 패스에서 소비.
                if let Some((tr, c0, c1)) = find_titled_rule(&composed) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    // 가로는 셀 경계 딱 맞게(대시와 안 겹침, 이름 양옆 공백이 패딩 역할),
                    // 세로만 살짝 여백.
                    let pad_x = 0.0;
                    let pad_y = 2.0;
                    let col = pane
                        .color
                        .or_else(|| {
                            // `pane.character` 는 pane 단위 필드라 탭이 둘이면 **마지막에
                            // 출력한 탭**이 이긴다. 접힌 `true_char` 를 앞에 세워 지금
                            // 보이는 탭의 학생을 쓴다.
                            true_char
                                .as_deref()
                                .or(pane.character.as_deref())
                                .and_then(|n| {
                                    theme::character_accent_n(
                                        n,
                                        theme::character_ordinal(&ws.pane_character, &tab_pid),
                                    )
                                })
                        })
                        .unwrap_or_else(theme::border);
                    // ultracode pane 은 이 테두리도 입력박스 보더와 같은 위상으로
                    // 숨쉰다(2026-08-17 「리네임되는 부분 테두리도 같이 숨쉬기되게」).
                    let col = if self.pane_ultracode.contains(&tab_pid) {
                        ultracode_breath(Some(col), self.version_anim_start.elapsed().as_secs_f32())
                    } else {
                        col
                    };
                    title_outline_slots.push((
                        body_left + c0 as f32 * scw - pad_x,
                        body_top + tr as f32 * sch - pad_y,
                        (c1 - c0 + 1) as f32 * scw + pad_x * 2.0,
                        sch + pad_y * 2.0,
                        col,
                    ));
                }
                // /resume 피커 학생 프사 — 스위퍼(resume_visibility)가 세션 행
                // 설명줄 끝에 스탬프한 ` · #학생이름` 태그를 지우고 그 자리에
                // 프사(bust)를 얹는다(거노: 이름 말고 프사). 세션 행 아래는
                // 구분 빈 줄이라 2행 키로 아래로 내려 그린다. pane 학생과
                // 무관하게 행마다 태그된 학생의 얼굴 — profile_slots(statusline
                // 프사와 같은 이미지 패스)로 소비된다.
                {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let rows_n = composed.len();
                    let mut faces = 0usize;
                    for r in 0..rows_n {
                        if faces >= 40 {
                            break; // 폭주 방어 — 화면에 이보다 많을 수 없다
                        }
                        let Some((c0, end, tag_slug)) = picker_student_tag(&composed[r])
                        else {
                            continue;
                        };
                        for cell in composed[r][c0..=end].iter_mut() {
                            *cell = GridCell::blank();
                        }
                        let row_w = composed[r].len() as f32 * scw;
                        let face_w = 4.0 * scw;
                        let face_h = 2.0 * sch;
                        let x = (body_left + c0 as f32 * scw)
                            .min(body_left + row_w - face_w)
                            .max(body_left);
                        // 바닥 정렬(statusline 프사 공식) — 얼굴 발을 설명줄
                        // 바닥에 붙이고 위(제목행 끝자락)로 서게. 아래로 내리면
                        // 구분 빈 줄에 매달려 다음 세션 것처럼 보인다(거노).
                        let y = (body_top + (r + 1) as f32 * sch - face_h).max(body_top);
                        profile_slots.push((tag_slug, (x, y, face_w, face_h)));
                        faces += 1;
                    }
                }
                // agents 목록 뷰(claude agents 피커)의 세션 행에 캐릭터 칩 — resume
                // 피커의 `· #학생` 태그가 없어, 캐시된 세션 name→sid→캐릭터로 역추적
                // 한다(호시노 청사진). 각 행의 실제 텍스트에서 캐시 name 을 substring
                // 검색해 세션 행을 식별(그룹 헤더·빈 줄은 매칭 안 됨), 동명세션은 캐시
                // 에서 이미 드롭돼 스킵된다. 얼굴은 name 시작 셀 왼쪽(마커 자리)에 얹어
                // 세션명은 가리지 않는다. 긴 이름이 …로 잘린 행은 매칭 실패로 스킵.
                if agents_view {
                    let name_sids = crate::socket::agents_name_sids_cached();
                    if !name_sids.is_empty() {
                        let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                        let scw = self.cell.w * fs;
                        let sch = self.cell.h * fs;
                        let mut faces = 0usize;
                        'agents_rows: for r in 0..composed.len() {
                            if faces >= 40 {
                                break;
                            }
                            let (text, cols) = row_text_cells(&composed[r]);
                            let text_chars: Vec<char> = text.chars().collect();
                            for (name, sid) in &name_sids {
                                let name_chars: Vec<char> = name.chars().collect();
                                if name_chars.len() < 2 || name_chars.len() > text_chars.len() {
                                    continue;
                                }
                                let Some(cpos) = text_chars
                                    .windows(name_chars.len())
                                    .position(|w| w == name_chars.as_slice())
                                else {
                                    continue;
                                };
                                let Some(slug) = kasa_mcp::character::session_character(sid)
                                    .and_then(|c| theme::character_slug(&c))
                                else {
                                    continue;
                                };
                                let cell_col = cols[cpos];
                                // 이름 왼쪽 여백(불릿·공백)을 지우고 그 자리에 얼굴 —
                                // 세션명 첫 글자와 겹치지 않게 얼굴 오른끝을 이름 시작에
                                // 맞춘다(여백이 좁으면 body_left 로 클램프).
                                let row_len = composed[r].len();
                                for cell in composed[r][..cell_col.min(row_len)].iter_mut() {
                                    *cell = GridCell::blank();
                                }
                                // 정사각(누끼 bust 96×96 왜곡 방지). 얼굴은 이름 왼쪽
                                // 여백(불릿·공백)에만 앉혀 세션명을 안 가린다 — 변은
                                // 여백폭에 맞추되(최대 2행) 이름 시작을 넘지 않는다.
                                // 밀집 단행이라 세로는 행 중앙 정렬(투명 여백만 이웃 행에).
                                let name_x = body_left + cell_col as f32 * scw;
                                let side = (name_x - body_left).min(2.0 * sch).max(sch);
                                let x = body_left;
                                let y = (body_top + r as f32 * sch + (sch - side) / 2.0)
                                    .max(body_top);
                                profile_slots.push((slug, (x, y, side, side)));
                                faces += 1;
                                continue 'agents_rows;
                            }
                        }
                    }
                }
                // 접힌 팀메시지("› Message from @이름", verbose OFF) — 보낸 학생
                // 색으로 "@ 이름❯ 본문…" 인라인 전개(거노: verbose 안 켜고도
                // 읽고 싶다. 클로드코드에 팀메시지만 펼치는 설정은 없음 —
                // verbosity 카테고리는 bash/agent/todo 뿐이라 그리드 재작성으로).
                // 본문은 이 pane transcript tail 의 <teammate-message> 태그에서.
                // 그리드는 reflow 불가라 접힌 줄 아래 빈 여백(blank_run)에만 다줄
                // 전개 — 최신 메시지(하단, 빈 공간 큼)일수록 더 많이 보인다.
                {
                    let msg_path = self.pane_claude_sid.get(id.as_str()).and_then(|sid| {
                        let cwd = self
                            .pane_view_cwd
                            .get(id.as_str())
                            .or_else(|| self.pane_cwd_cache.get(id.as_str()))?;
                        crate::socket::project_jsonl(cwd, sid)
                    });
                    for r in 0..composed.len() {
                        let Some((c0, _count, sender)) = teammate_collapsed_line(&composed[r])
                        else {
                            continue;
                        };
                        let msg = msg_path
                            .as_deref()
                            .and_then(|p| latest_teammate_msg(p, &sender));
                        // 화면에 `@peer` 로 떴어도 태그에서 진짜 발신자를 찾았으면 그것으로
                        // 그린다 — 이름이 바뀌어야 아래 색·프사·전개가 전부 걸린다.
                        let sender = msg
                            .as_ref()
                            .and_then(|m| m.sender.clone())
                            .unwrap_or(sender);
                        // 그 이름으로도 학생을 못 찾으면 **세션 id 로 pane 을 되짚는다.**
                        // 명부의 이름은 세션 제목이라 자동 요약에 덮인다 — 실측으로
                        // 모모이 pane 은 `mcp, skill사이드바` 였고, 로스터가 아는 글자가
                        // 하나도 없어 색도 프사도 안 걸렸다(거노 2026-08-11: "sm테마는 왜
                        // 안됐어"). 앞서 이름 파싱을 고친 것은 이름에 슬러그가 들어 있을
                        // 때만 듣는 반쪽이었다. pane 을 되짚으면 제목이 뭐로 바뀌든 맞는다.
                        //
                        // ⚠️ `pane_character_if_known` 을 부르면 안 된다 — 그 안에서
                        // `ws` 를 다시 잠그는데 여기는 이미 그 락 안(557~1788)이라
                        // 재진입 데드락이다. 들고 있는 `ws` 를 그대로 쓴다.
                        let sender = if teammate_sender_slug(&sender).is_some() {
                            sender
                        } else {
                            msg.as_ref()
                                .and_then(|m| m.peer_sid.as_deref())
                                .and_then(|sid| {
                                    self.pane_claude_sid
                                        .iter()
                                        .find(|(_, s)| s.as_str() == sid)
                                        .map(|(p, _)| ws.active_tab_pid(p))
                                })
                                .and_then(|key| ws.pane_character.get(&key).cloned())
                                .unwrap_or(sender)
                        };
                        let accent = teammate_sender_accent(
                            &sender,
                            msg.as_ref().and_then(|m| m.color.as_deref()),
                        );
                        let face_col = expand_teammate_message(
                            &mut composed,
                            r,
                            c0,
                            &sender,
                            msg.as_ref().map(|m| m.body.as_str()),
                            accent,
                        );
                        // 발신 학생 프사 — tell 과 같은 이미지 패스·같은 자리(첫 줄
                        // 왼쪽 여백 2칸). 셀이 세로 2:1 이라 2칸×1행이 정사각.
                        if let (Some(fc), Some(slug)) =
                            (face_col, teammate_sender_slug(&sender))
                        {
                            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                            let scw = self.cell.w * fs;
                            let sch = self.cell.h * fs;
                            let face_w = TELL_FACE_COLS as f32 * scw;
                            let face_h = sch;
                            let x = body_left + fc as f32 * scw;
                            let y = body_top + r as f32 * sch;
                            profile_slots.push((slug, (x, y, face_w, face_h)));
                        }
                    }
                    // claude v2.1.228 은 처리 끝난 팀메시지를 접힌 줄이 아니라
                    // `@ <발신 라벨>❯` + 들여쓴 본문으로 **펼쳐서** 그린다 — 위
                    // 접힌 줄 탐지가 영영 안 걸리는 형태다(2026-08-12, 거노 스샷).
                    // 화면 라벨이 transcript 태그의 from_label 과 일치할 때만
                    // 남의 메시지로 인정한다 — 사용자가 직접 친 `@ …❯` 보호.
                    for r in 0..composed.len() {
                        let Some((c0, qcol, label)) =
                            peer_native_header_line(&composed[r])
                        else {
                            continue;
                        };
                        let msg = msg_path
                            .as_deref()
                            .and_then(|p| latest_teammate_msg(p, PEER_LABEL));
                        // 와이드 글리프 스페이서가 공백으로 섞이므로 공백 무시 대조.
                        let norm = |s: &str| -> String {
                            s.chars().filter(|c| !c.is_whitespace()).collect()
                        };
                        // from-name 은 발신 세션에 제목이 없으면 통째로 빠진다(신생
                        // pane 첫 메시지, 2026-08-12 실측) — 그때 claude 는 소켓
                        // pid 를 라벨로 그리므로(`@ 12889❯`) pid 대조를 함께 받는다.
                        let label_hit = msg.as_ref().is_some_and(|m| {
                            m.from_label.as_deref().map(&norm) == Some(norm(&label))
                                || m.from_pid.as_deref().map(&norm) == Some(norm(&label))
                        });
                        // 대조되는 것은 **최신 메시지 하나**뿐이라, 스크롤백의 옛
                        // 메시지·tail(256KB) 밖 메시지는 대조가 영영 안 된다 — 라벨이
                        // 로스터 학생의 agent 이름꼴이면 그것만으로 남의 메시지로
                        // 인정한다(2026-08-20 거노 스샷: dismiss 된 미도리의 메시지가
                        // 무테마로 남았다).
                        if !label_hit && !label_is_roster_agent(&label) {
                            continue;
                        }
                        let sender = label_hit
                            .then(|| msg.as_ref().and_then(|m| m.sender.clone()))
                            .flatten()
                            .unwrap_or_else(|| PEER_LABEL.to_string());
                        // 이름이 학생을 안 알려 주면 세션 id 로 pane 을 되짚는다 —
                        // 접힌 경로와 같은 규칙(⚠️pane_character_if_known 금지, 위 주석).
                        let sender = if teammate_sender_slug(&sender).is_some() {
                            sender
                        } else {
                            msg.as_ref()
                                .filter(|_| label_hit)
                                .and_then(|m| m.peer_sid.as_deref())
                                .and_then(|sid| {
                                    self.pane_claude_sid
                                        .iter()
                                        .find(|(_, s)| s.as_str() == sid)
                                        .map(|(p, _)| ws.active_tab_pid(p))
                                })
                                .and_then(|key| ws.pane_character.get(&key).cloned())
                                .unwrap_or(sender)
                        };
                        // 명부까지 죽었으면(발신 pane dismiss) 여기 와서도 `peer` 다 —
                        // 라벨의 로마자 머리가 로스터를 안다(midori-p4-v32 → 미도리).
                        let sender = if teammate_sender_slug(&sender).is_none()
                            && label_is_roster_agent(&label)
                        {
                            label.clone()
                        } else {
                            sender
                        };
                        let accent = teammate_sender_accent(
                            &sender,
                            msg.as_ref()
                                .filter(|_| label_hit)
                                .and_then(|m| m.color.as_deref()),
                        );
                        let slug = teammate_sender_slug(&sender);
                        // 학생을 알면 긴 발신 라벨을 이름으로 갈아끼운다 — 라벨은
                        // 발신 세션의 자동 제목이라 「sendmessage로 7유저에게…」 같은
                        // 소음이다. 표시는 한글 이름으로(agent 명 "kanna-p1-qpo" 를
                        // 그대로 쓰면 로마자 꼬리표가 남는다). 못 찾으면 원문 유지(색만).
                        if let Some(slug) = slug {
                            let display = theme::slug_character(slug).unwrap_or(&sender);
                            restyle_peer_native_header(
                                &mut composed[r], c0, qcol, display, accent,
                            );
                        }
                        tint_row(&mut composed[r], accent);
                        let mut rr = r + 1;
                        let mut face_row: Option<usize> = None;
                        while rr < composed.len() {
                            if tell_wrap_continuation(&composed[rr]) {
                                face_row.get_or_insert(rr);
                                tint_row(&mut composed[rr], accent);
                                rr += 1;
                            } else if msg_paragraph_gap(&composed, rr) {
                                rr += 1;
                            } else {
                                break;
                            }
                        }
                        // 프사 — tell 과 같은 관례: 본문 행 왼쪽 여백(들여쓰기 2칸)에
                        // 본문과 같은 행으로. 본문이 없으면(헤더뿐) 포기하고 색만.
                        if let (Some(fr), Some(slug)) = (face_row, slug) {
                            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                            let scw = self.cell.w * fs;
                            let sch = self.cell.h * fs;
                            profile_slots.push((
                                slug,
                                (
                                    body_left,
                                    body_top + fr as f32 * sch,
                                    TELL_FACE_COLS as f32 * scw,
                                    sch,
                                ),
                            ));
                        }
                    }
                }
                // 크로스-방 tell(⟦캐릭터⟧ 본문)을 발신 학생 테마색으로 — 팀 경계를
                // 넘는 tell 은 네이티브 teammate 가 아니라 raw user 입력이라 거노 발신
                // 처럼 보인다. 마커가 유효 캐릭터면 그 행과 wrap 연속 행을 발신자
                // accent 로 칠하고, 마커 자리에 발신 학생 프사(bust)를 얹는다 —
                // profile_slots(statusline·resume 피커와 같은 이미지 패스)로 소비.
                {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let mut r = 0;
                    while r < composed.len() {
                        if let Some((marker_start, marker_end, name)) =
                            tell_marker_line(&composed[r])
                        {
                            if let Some(accent) = theme::character_accent(&name) {
                                let face_col = restyle_tell_line(
                                    &mut composed[r],
                                    marker_start,
                                    marker_end,
                                    &name,
                                    accent,
                                );
                                if let (Some(c0), Some(slug)) =
                                    (face_col, theme::character_slug(&name))
                                {
                                    // 프사는 본문 왼쪽 여백(`❯` 자리, 2칸)에 같은
                                    // 행으로 — 셀은 세로 2:1 이라 2칸×1행이 정사각.
                                    // 본문은 첫 줄부터 wrap 연속 행과 같은 col.
                                    let face_w = TELL_FACE_COLS as f32 * scw;
                                    let face_h = sch;
                                    let x = body_left + c0 as f32 * scw;
                                    let y = body_top + r as f32 * sch;
                                    profile_slots.push((slug, (x, y, face_w, face_h)));
                                }
                                r += 1;
                                while r < composed.len() && tell_wrap_continuation(&composed[r]) {
                                    tint_row(&mut composed[r], accent);
                                    r += 1;
                                }
                                continue;
                            }
                        }
                        r += 1;
                    }
                }
                // 학생 완료 보고 줄(`[완료] 미도리(%4) — …`, socket.rs pane_done
                // 주입)도 보고한 학생색으로 — 어느 학생의 보고인지 색으로 읽힌다
                // (거노 2026-08-20 「이왕하는거면 학생테마에 맞게 색상 다 해」).
                // 캐릭터를 모르는 옛 형식(`[완료] %4(%4)`)은 원색 유지 — 엉뚱한
                // 색보다 낫다.
                {
                    let mut r = 0;
                    while r < composed.len() {
                        let accent = done_report_line(&composed[r])
                            .and_then(|n| theme::character_accent(&n));
                        let Some(accent) = accent else {
                            r += 1;
                            continue;
                        };
                        tint_row(&mut composed[r], accent);
                        r += 1;
                        while r < composed.len() && tell_wrap_continuation(&composed[r]) {
                            tint_row(&mut composed[r], accent);
                            r += 1;
                        }
                    }
                }
                let pane_font_scale = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                let hover_links = hovered_link
                    .as_ref()
                    .filter(|(pid, _, _)| pid.as_str() == id.as_str())
                    .map(|(_, span, _)| vec![span.clone()])
                    .unwrap_or_default();
                // 학생 accent 는 입력박스 보더·@배지 도색에만(거노 2026-07-18:
                // 응답 본문·"Reading 1 file" 상태줄까지 학생색이면 헷갈린다 —
                // 출력 글자는 테마 기본 fg. 옛 본문 틴트 폐기). 게이트는 pane
                // 테두리와 동일: 배정 캐릭터 + claude 가 foreground 일 때만
                // (active_process_name=="claude", 500ms 캐시 — 순정 셸 오염
                // 방지, 거노 실사고). agents 목록 뷰는 중립.
                // resume 피커(claude 시스템 UI)는 `╭─╮ Search ╰─╯` 박스가 pane
                // 입력박스로 오인돼 학생 accent 후처리가 오발동한다(거노: 빈 초록
                // 사각형). agents 목록 뷰처럼 학생 accent·세션 제목 인레이를 끈다.
                let resume_picker = screen_is_resume_picker(&composed);
                // AskUserQuestion picker 도 `❯ 1. …` 옵션줄 + 하단 힌트 박스가
                // 입력박스로 오인돼 accent 사각형이 남는다(거노: "question 이나
                // resume" 둘 다). team member/bg 세션 입력박스는 @칩 대신 세션
                // 제목이 상단보더에 와서 @칩 게이트론 못 가른다 → 화면 시그니처
                // ("Chat about this" 등)로 감지해 resume 와 동일하게 accent 를 끈다.
                let ask_picker = screen_is_ask_picker(&composed);
                let prompt_accent = if agents_view || resume_picker || ask_picker {
                    None
                } else {
                    true_char
                        .as_deref()
                        .or(pane.character.as_deref())
                        .and_then(|n| {
                            theme::character_accent_n(
                                n,
                                theme::character_ordinal(&ws.pane_character, &tab_pid),
                            )
                        })
                        .filter(|_| {
                            self.pty
                                .get(tab_pid.as_str())
                                .and_then(|p| p.active_agent())
                                .is_some()
                        })
                };
                // ultracode 는 학생 배정과 무관한 pane 상태다 — 학생 accent 게이트
                // (Some 일 때만 칠함) 안쪽에 두면 미배정 pane 은 마커가 있어도 영영
                // 안 칠해진다(2026-08-12 조사). 피커 게이트는 prompt_accent 와 같은
                // 조건을 그대로 쓴다 — resume/ask 피커 오탐 방지 유지.
                let ultra = self.pane_ultracode.contains(&tab_pid)
                    && !(agents_view || resume_picker || ask_picker)
                    && self
                        .pty
                        .get(tab_pid.as_str())
                        .and_then(|p| p.active_agent())
                        .is_some();
                if ultra {
                    let t = self.version_anim_start.elapsed().as_secs_f32();
                    // 학생색 ↔ 보라 **순환** 숨쉬기(2026-08-17 「학생색 유지되면서
                    // 순환하는 형식으로」 — 통보라 숨쉬기는 누구 pane 인지 잃어서
                    // 이상했다). 골에서는 학생색 그대로, 마루에서 보라로 씻겼다가
                    // 돌아온다. 「ultracode」 라벨은 상시고(깜빡임 반려, 2026-08-17)
                    // 색만 보더와 함께 숨쉰다. 라벨 심기가 페인트보다 먼저다 —
                    // 글자를 먼저 놓아야 style_prompt_box 가 같은 색을 입혀 준다.
                    overlay_ultracode_label(&mut composed);
                    style_prompt_box(&mut composed, ultracode_breath(prompt_accent, t));
                } else if let Some(accent) = prompt_accent {
                    style_prompt_box(&mut composed, accent);
                    // 칩 제거는 위 `runs_claude` 블록에서 이미 끝났다 — 여기서 한 번
                }
                // 내가 친 프롬프트 띠 재도색 — claude 테마의 전폭 띠(라이트=씻긴
                // 회백, 다크=흰 띠)를 kasaterm 테마·학생색으로(2026-08-15 지시
                // 「색상이랑 디자인 바꾸자」). 디자인: 띠는 본문 폭까지만(전폭
                // 꼬리는 기본 배경으로), 바탕은 학생 accent 를 테마 배경에 살짝
                // 섞은 톤, `❯` 는 accent 원색. 픽커/목록 화면은 선택 강조가
                // (`❯`+배경) 오탐되므로 통째로 건너뛴다.
                if !(agents_view || resume_picker || ask_picker) {
                    let accent = prompt_accent
                        .unwrap_or_else(|| theme::accent_color(theme::accent_name()));
                    let base = theme::bg();
                    let light = base[0] as u16 + base[1] as u16 + base[2] as u16 > 380;
                    let amount = if light { 0.10 } else { 0.18 };
                    let fill = tint_toward([base[0], base[1], base[2]], accent, amount);
                    let mut r = 0;
                    while r < composed.len() {
                        // sticky pill 행은 재도색 금지 — 위 sticky 블록 주석 참고.
                        if Some(r) == sticky_pill_row {
                            r += 1;
                            continue;
                        }
                        let Some(band) = user_prompt_band(&composed[r]) else {
                            r += 1;
                            continue;
                        };
                        loop {
                            restyle_user_prompt_row(&mut composed[r], &fill, accent);
                            r += 1;
                            if r >= composed.len()
                                || band_bg(&composed[r]).as_ref() != Some(&band)
                            {
                                break;
                            }
                        }
                    }
                }
                slots.push(PaneSlot {
                    rows: composed,
                    origin_px,
                    // Unfocused panes dim their text only (no box veil). Single
                    // un-split pane is never dimmed.
                    dim: is_split && active_id.as_deref() != Some(id.as_str()),
                    font_scale: pane_font_scale,
                    links: hover_links,
                    default_fg: cells::default_fg(),
                });
                // Body box (header band excluded, inset by the same
                // PANE_INNER margins the cell grid uses) in logical px.
                // Bottom-row stretch mirrors the header's box_h so the
                // content fills to the window edge with no seam.
                // Computed for EVERY pane (not just image/md) — in-pane
                // WebViews need it too.
                let bx = WINDOW_PADDING
                    + sidebar_w
                    + x_cells as f32 * self.cell.w
                    + PANE_INNER_X;
                let by = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
                // A single un-split pane reports 0 cells (the layout tree has no
                // split to divide by). The cell-grid clip above already falls
                // back to the full window in that case; mirror it here, or the
                // body box — and the Alt pane-number overlay drawn on it —
                // collapses to 1px (overlay then skipped by the rw<24 guard).
                let eff_w_cells = if w_cells == 0 { grid_cols } else { w_cells };
                let eff_h_cells = if h_cells == 0 { grid_rows } else { h_cells };
                let base_w = eff_w_cells as f32 * self.cell.w;
                let full_w = if x_cells + eff_w_cells >= grid_cols {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lw = w.inner_size().width as f32 / s;
                        // Stop at the git column's left padding when it's shown,
                        // else hug the true window edge (git_reserve == 0).
                        (raw_lw
                            - git_reserve
                            - (WINDOW_PADDING + sidebar_w + grid_cols as f32 * self.cell.w))
                            .max(0.0)
                    });
                    base_w + extra
                } else {
                    base_w
                };
                // An edge pane meets the window border, not a divider, so it
                // gets no inner inset on that side — otherwise the right/bottom
                // edge keeps an inner-pad-width empty strip (the "우측하단 빈칸"
                // a drag leaves when it puts a pane against the window edge).
                let right_inset = if x_cells + eff_w_cells >= grid_cols { 0.0 } else { PANE_INNER_X };
                let bw = (full_w - PANE_INNER_X - right_inset).max(1.0);
                let base_h = eff_h_cells as f32 * self.cell.h;
                let full_h = if y_cells + eff_h_cells >= grid_rows {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lh = w.inner_size().height as f32 / s;
                        (raw_lh
                            - self.bottom_reserve_h()
                            - (TITLE_HEIGHT + grid_rows as f32 * self.cell.h))
                            .max(0.0)
                    });
                    base_h + extra
                } else {
                    base_h
                };
                let bottom_inset = if y_cells + eff_h_cells >= grid_rows { 0.0 } else { PANE_INNER_Y };
                // 상태바 띠를 빼야 한다. PTY 그리드는 `resize_backend` 가 푸터만큼
                // 행을 줄여 바 위에서 멈추는데, 편집기·이미지처럼 PTY 없는 pane 은
                // 이 박스가 곧 본문 클립이라 여기서 빼지 않으면 마지막 줄이 바
                // 아래로 새어 창 끝까지 그려졌다(실측).
                let bh = (full_h
                    - header_shift_logical
                    - PANE_INNER_Y
                    - bottom_inset
                    - self.statusbar_px(&id))
                .max(1.0);
                body_rects.push((id.clone(), (bx, by, bw, bh)));
                if let Some(image) = img {
                    image_slots.push((id.clone(), image, (bx, by, bw, bh), img_zoom, img_rot, img_pan));
                }
                if let Some((
                    doc,
                    raw_mode,
                    lines,
                    cursor,
                    sel,
                    scroll,
                    h_scroll,
                    lang,
                    find,
                    complete,
                    folds,
                    wrap,
                    extra,
                )) = md
                {
                    // 편집기 모드에서만 — 렌더 뷰엔 밑줄을 그릴 자리가 없다.
                    // 여기서 뽑는 이유는 아래 그리는 루프가 `&mut self.gpu` 를
                    // 잡고 있어 self 를 다시 빌릴 수 없기 때문이다.
                    let diags = if raw_mode {
                        self.lsp_diags(&doc.path)
                    } else {
                        Vec::new()
                    };
                    md_slots.push((
                        id.clone(),
                        doc,
                        (bx, by, bw, bh),
                        scroll,
                        raw_mode,
                        lines,
                        cursor,
                        sel,
                        h_scroll,
                        lang,
                        find,
                        complete,
                        diags,
                        folds,
                        wrap,
                        extra,
                    ));
                }
                // Box geometry (logical px). Right/bottom-edge panes stretch to
                // the window's true edge so the floored sub-cell remainder
                // doesn't read as a seam. Computed unconditionally — the status
                // bar anchors off box_y + box_h whether or not a header is drawn.
                let box_x = WINDOW_PADDING + sidebar_w + x_cells as f32 * self.cell.w;
                let box_y = TITLE_HEIGHT + y_cells as f32 * self.cell.h;
                // A lone unsplit pane arrives as a (0,0,0,0) placeholder (see the
                // clip note above), which would leave the box 0×0 and starve the
                // footer (`fbox_h < pane_footer_h()` → skipped). Treat a 0 span
                // as "fills the grid" so the box — and its status bar — spans the
                // whole pane area just like a real right/bottom-edge leaf.
                let box_w = {
                    let base = w_cells as f32 * self.cell.w;
                    if w_cells == 0 || x_cells + w_cells >= grid_cols {
                        let right_edge = WINDOW_PADDING + sidebar_w + (x_cells + w_cells) as f32 * self.cell.w;
                        let extra = self.window.as_ref().map_or(0.0, |w| {
                            let s = w.scale_factor() as f32 * self.ui_zoom;
                            let raw_lw = w.inner_size().width as f32 / s;
                            (raw_lw - git_reserve - right_edge).max(0.0)
                        });
                        base + extra
                    } else {
                        base
                    }
                };
                let box_h = {
                    let base = h_cells as f32 * self.cell.h;
                    if h_cells == 0 || y_cells + h_cells >= grid_rows {
                        let bottom_edge = TITLE_HEIGHT + (y_cells + h_cells) as f32 * self.cell.h;
                        let extra = self.window.as_ref().map_or(0.0, |w| {
                            let s = w.scale_factor() as f32 * self.ui_zoom;
                            let raw_lh = w.inner_size().height as f32 / s;
                            // 상태줄·dock 예약을 빼야 한다 — body_rects 의 stretch 는
                            // 빼는데 여기만 안 빼서, 하단행 pane 의 박스가 창 끝까지
                            // 내려가 포커스 테두리 아랫변이 나중에 그려지는 전역
                            // 상태줄 뒤에 통째로 깔렸다(거노 2026-08-15 「하단바때문에
                            // 포커스 테두리 밑에가 안보여」, 창 캡처 실측).
                            (raw_lh - self.bottom_reserve_h() - bottom_edge).max(0.0)
                        });
                        base + extra
                    } else {
                        base
                    }
                };
                footer_slots.push((id.clone(), box_x, box_y, box_w, box_h));
                // claude agents 목록·resume 피커 화면에만 샬레 교실 배경을 셀 뒤에
                // 깐다(거노: 세션 선택 화면만). default-bg 셀은 fill 을 안 뿜어
                // (gpu.draw_cells) 이미지가 그 자리로 비치고, 메뉴 글리프는 위 패스에
                // 또렷이 얹힌다. 로더가 이미지를 어둡게 낮춰 텍스트 대비를 확보한다.
                // 거노 2026-07-26: /resume 은 일반 배경으로 — 백그라운드 세션
                // 목록(agents)과 같은 교실 배경을 쓰니 두 화면이 겹쳐 보였다.
                // 교실 배경은 agents 목록의 시각 정체성으로만 남긴다.
                if agents_view {
                    classroom_slots.push((box_x, box_y, box_w, box_h));
                }
                // image/md pane만 헤더 띠 데이터 생성(전용 컨트롤 자리). 일반
                // 터미널은 hover ⋮ 핸들로 — has_header()가 그 경계를 가른다.
                if pane.has_header() {
                    // 캐릭터 배정 pane(학생)은 헤더에도 이름을 — "미도리 · 작업명"(작업명
                    // =OSC title). BA GUI board 라벨과 통일(거노: 터미널 탭도 학생 이름).
                    // 비배정 pane 만 기존 "%N · 프로세스" 폴백.
                    let label = if agents_view {
                        // 관리 화면 — 개별 학생 대신 SCHALE. 작업명(OSC title)은 유지.
                        match pane
                            .title
                            .clone()
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|t| !t.is_empty())
                        {
                            Some(t) => format!("샬레 · {t}"),
                            None => "샬레".to_string(),
                        }
                    } else if let Some(c) = true_char.as_ref() {
                        // 헤더 학생명은 `display_pane_char`(=true_char) 정본을 쓴다 —
                        // raw pane.character 만 보면 claude agents 로 이어받은 백그라운드
                        // 세션은 ws.pane_character 가 비어(attach 스폰이 캐릭터 미배정)
                        // 이 분기를 못 타고 아래 폴백으로 흘러 "미도리 · 작업명" 대신
                        // 세션제목이 칩자리에 박혔다(거노 Q1). session_character(bound sid)
                        // 로 해석하면 스프라이트·프사(둘 다 true_char)와 헤더가 일치한다.
                        match pane
                            .title
                            .clone()
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|t| !t.is_empty())
                        {
                            // pane 아이디를 캐릭터 뒤에 붙인다(거노 2026-08-05: "칩 위치를
                            // 바꿔 pane아이디 이런데에, 거기는 /rename 들어갈 자리니까").
                            // 입력박스 보더 우측은 `/rename` 이름 자리로 비워 뒀으니
                            // (`inlay_prompt_box_right`) **이 pane 이 누구인가**는 헤더가
                            // 든다. 학생 pane 은 여태 캐릭터만 실어 아이디가 어디에도
                            // 없었다 — `tell %N` 을 쓰려면 그걸 알아야 한다.
                            //
                            // agent 이름(`midori-p1`)을 그대로 싣지 않는 이유: 그건 캐릭터
                            // 슬러그 + pane 번호라 `미도리 %1` 과 같은 정보인데, 로마자
                            // 슬러그는 스프라이트·board 의 한글 이름과 안 맞아 두 이름을
                            // 오가게 만든다. 정체 표시는 한 벌로 둔다.
                            Some(t) => format!("{c} {id} · {t}"),
                            None => format!("{c} {id}"),
                        }
                    } else {
                        // Custom title (rename / OSC) wins; otherwise the live
                        // foreground process (vim, claude, zsh …); fall back to
                        // the raw "%N" id only if both are empty.
                        let smart = self.pty.get(&id).and_then(|p| Self::smart_pane_label(p));
                        let base = pane
                            .title
                            .clone()
                            .filter(|t| !t.is_empty())
                            .or(smart)
                            .unwrap_or_else(|| id.clone());
                        // Prefix the pane id (for `tell %N`, etc.); skip when the
                        // label already fell back to the id — no "%18 · %18".
                        if base == id { base } else { format!("{id} · {base}") }
                    };
                    // Append the pane's real OS tty (ghostty-style) — daemon
                    // cache first (the daemon owns the PTY), else local pty.
                    let tty = self
                        .pane_tty_cache
                        .get(&id)
                        .cloned()
                        .or_else(|| self.pty.get(&id).and_then(|p| p.tty().map(str::to_string)));
                    let label = match tty {
                        Some(t) => format!("{label}  ·  {t}"),
                        None => label,
                    };
                    // 웹 pane 로딩 상태 — host 실물(web_hosts)이 쥔다. 헤더 작업
                    // 바(busy)와 리로드↔정지 아이콘이 읽는다.
                    let web_loading = pane
                        .web()
                        .and_then(|w| self.web_hosts.get(&w.host_id))
                        .map(|h| h.loading)
                        .unwrap_or(false);
                    headers.push(HeaderInfo {
                        id: id.clone(),
                        x: box_x,
                        y: box_y,
                        w: box_w,
                        box_h,
                        // ● = 미저장 편집(raw 편집기). 단일탭 폴백 라벨에도 붙어야
                        // 헤더 어디로 그려지든 저장 안 된 게 보인다.
                        label: if pane.markdown().map_or(false, |m| m.modified) {
                            format!("● {label}")
                        } else {
                            label
                        },
                        is_active: active_id.as_deref() == Some(id.as_str()),
                        // Busy = the daemon's transcript watcher sees this pane
                        // working (cross-window). Drives the header working bar.
                        // 웹 pane 은 페이지 로딩이 곧 「작업 중」이다.
                        busy: self.pane_is_busy(&id) || web_loading,
                        // A background shell / Monitor is running with no spinner —
                        // drives the slower header pulse bar when not busy.
                        bg_active: self
                            .pane_activity
                            .get(&id)
                            .map(|a| a.bg_active)
                            .unwrap_or(false),
                        compacting: self
                            .pane_activity
                            .get(&id)
                            .is_some_and(|a| a.status == "compacting"),
                        compact_pct: self
                            .pane_activity
                            .get(&id)
                            .and_then(|a| a.compact_pct),
                        color: pane.color,
                        is_markdown: pane.markdown().map_or(false, |m| m.is_md_doc),
                        md_raw_mode: pane.markdown().map_or(false, |m| m.raw_mode),
                        is_image: pane.image().is_some(),
                        is_web: pane.web().is_some(),
                        web_url: pane.web().map(|w| w.url.clone()),
                        web_loading,
                        // 단일 탭 + 배정된 학생이면 탭 제목을 비운다 — render 의 tab_list
                        // 폴백(h.tabs.is_empty → h.label)이 character label("미도리 · 작업명")
                        // 을 헤더에 그리게(거노: 탭 제목이 학생 이름을 덮어쓰던 버그). 멀티탭/
                        // 비배정 pane 은 기존대로 탭별 제목.
                        tabs: if pane.tabs.len() <= 1
                            && pane.character.as_deref().is_some_and(|c| !c.is_empty())
                        {
                            Vec::new()
                        } else {
                            pane.tabs
                                .iter()
                                .enumerate()
                                .map(|(i, t)| {
                                    let name = t
                                        .title
                                        .clone()
                                        .filter(|s| !s.is_empty())
                                        .or_else(|| {
                                            // 각 탭의 pid로 스마트 라벨(셸=cwd, 명령=프로세스).
                                            t.pid
                                                .as_deref()
                                                .and_then(|p| self.pty.get(p))
                                                .and_then(|s| Self::smart_pane_label(s))
                                        })
                                        .unwrap_or_else(|| {
                                            if i == 0 { id.clone() } else { format!("탭 {}", i + 1) }
                                        });
                                    // 탭별 ● 미저장 도트 — 멀티탭 pane 에서 어느
                                    // 파일이 저장 안 됐는지 탭 단위로 보이게.
                                    if t.markdown().map_or(false, |m| m.modified) {
                                        format!("● {name}")
                                    } else {
                                        name
                                    }
                                })
                                .collect()
                        },
                        // 팝아웃 아이콘 대상 판정: 마크다운/텍스트 편집 탭만(터미널 제외).
                        // tabs 라벨을 비운 학생 pane 폴백과 길이를 맞추려 같은 조건으로 계산.
                        tab_is_file: if pane.tabs.len() <= 1
                            && pane.character.as_deref().is_some_and(|c| !c.is_empty())
                        {
                            Vec::new()
                        } else {
                            pane.tabs
                                .iter()
                                .map(|t| matches!(t.content, PaneContent::Markdown(_)))
                                .collect()
                        },
                        single_is_file: pane
                            .tabs
                            .first()
                            .map(|t| matches!(t.content, PaneContent::Markdown(_)))
                            .unwrap_or(false),
                        active_tab: pane.active_tab,
                        tab_first: pane.tab_first,
                        tab_last_active: pane.tab_last_active,
                    });
                }
            }
            // Fallback: if nothing is marked active (e.g. active_pane not yet
            // set right after a split), make the first header active so the
            // focused-tab box/accent always shows on exactly one pane.
            if !headers.is_empty() && !headers.iter().any(|h| h.is_active) {
                headers[0].is_active = true;
            }
            (slots, headers, footer_slots, agents_view_panes)
        };
        let toast_alpha = self.copy_toast_alpha();
        // Collab completion toast (top-right). Pre-read here like toast_alpha so
        // the render block below never re-borrows self while g is held.
        // 한도 배지 — g 생성 전에 읽어 borrow 충돌을 피한다(다른 pre-read 와 동일).
        // 쓰는 곳은 Info 탭 머리의 계정 행(info::draw_info_actions).
        let claude_usage_pct = self.claude_usage.lock().ok().and_then(|v| v.clone());
        let collab_toast_alpha = self.collab_toast_alpha();
        let collab_toast_msg = self.collab.toast.as_ref().map(|(m, _)| m.clone());
        let collab_toast_action_on = self.collab.toast_action.is_some();
        // 업데이트 토스트(win_sparkle 센티널)면 칩 라벨이 승인/거부 대신 설치/나중에.
        let update_toast_on = self.collab.toast_action.as_deref()
            == Some(crate::win_sparkle::UPDATE_TOAST_ACTION);
        let slot_views: Vec<gpu::PaneSlot<'_>> = slots
            .iter()
            .map(|s| gpu::PaneSlot {
                rows: &s.rows,
                origin_px: s.origin_px,
                dim: s.dim,
                font_scale: s.font_scale,
                links: s.links.clone(),
                default_fg: s.default_fg,
            })
            .collect();
        // Recompute the inline suggestion against the freshly-applied
        // grid before snapshotting it into the overlay.
        self.update_suggestion();
        let overlay = self.gpu_overlay_snapshot();
        self.sync_ime_cursor_area(&overlay);
        let chrome_font = 14.0_f32;
        // Markdown Render/Raw toggle lives in the pane action buttons
        // (drawn in the header loop), not a separate pill.
        // Session tabs live in a wry webview panel (like the git panel), not
        // the native title bar — drawing them here collided with the OSC title.
        // Drop-zone overlay: while a header drag is active, highlight the
        // half of the target pane the dragged pane would land in. Computed
        // here (immutable self borrow) so the gpu block below only touches
        // the cached rect.
        // Drop zone shows for BOTH header drags (whole pane → quadrant)
        // and tab drags whose cursor is over a pane BODY (split + place
        // moved tab as new pane). Tab drag over a strip is handled by
        // tab_drag_info's insertion bar instead.
        let header_drag_active = self
            .header_drag
            .as_ref()
            .map(|hd| hd.active)
            .unwrap_or(false);
        let tab_drag_active = self
            .tab_drag
            .as_ref()
            .map(|d| d.active)
            .unwrap_or(false);
        // The strip-only insertion bar gets replaced by the zone overlay
        // — without it the user sees no preview when hovering the header,
        // which is exactly the spot most people aim for when intending
        // "merge into this pane".
        // 라이브 드래그(실제 레이아웃이 재배치되는 케이스): header/handle 드래그는
        // 항상, tab 드래그는 단일탭 pane 일 때. 진짜 reflow 가 곧 피드백이므로 파란
        // drop-zone 박스를 띄우지 않는다 — 박스는 라이브가 아닌 tab 드래그(멀티탭
        // 탭 추출)에만 남긴다.
        let live_drag = header_drag_active
            || self
                .tab_drag
                .as_ref()
                .map(|t| {
                    t.active
                        && self
                            .ws
                            .lock()
                            .ok()
                            .and_then(|w| w.panes.get(&t.pane).map(|p| p.tabs.len() <= 1))
                            .unwrap_or(true)
                })
                .unwrap_or(false);
        // 중앙("안에 넣기") 프리뷰는 라이브 드래그에서도 박스를 띄운다 — 소스가
        // 그리드에서 빠지는 것만으론 *어느* pane 안으로 들어가는지 안 보인다.
        let live_center = self
            .drag_live_applied
            .as_ref()
            .is_some_and(|(_, z)| *z == DropZone::Center);
        let show_drop_zone = (tab_drag_active && !live_drag) || live_center;
        // Indicator policy:
        //   - header band (cursor_on_header) → strip insertion bar only
        //                                       (overlay 안 그림)
        //   - body Center / split            → rectangle overlay
        // 두 인디케이터가 동시에 뜨지 않게 mutually exclusive.
        let current_zone = self.drop_target_at(self.cursor_px.0, self.cursor_px.1);
        let cursor_on_header = matches!(current_zone, Some((_, DropZone::Center))) && {
            // 헤더 = pane_top ~ pane_top + header_band. body_top
            // 10px 위까지 관대 (좁은 헤더에서 마우스 못 맞추는 거 방지).
            let cur_y = self.cursor_px.1;
            let leaves = self.pty_layout.as_ref().map(|t| t.leaves().len()).unwrap_or(1);
            let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
            current_zone
                .as_ref()
                .and_then(|(id, _)| {
                    let tree = self.pty_layout.as_ref()?;
                    let (cols, rows) = self.window_cells();
                    tree.leaf_rects(cols, rows)
                        .into_iter()
                        .find(|(i, ..)| i == id)
                        .map(|(_, _, cy, _, _)| TITLE_HEIGHT + cy as f32 * self.cell.h)
                })
                .map(|pane_top| cur_y < pane_top + header_band + 10.0)
                .unwrap_or(false)
        };
        // Overlay shows when cursor is over a pane BODY (split zone or
        // body-Center). Header-Center routes to the strip insertion bar.
        let zone_overlay_active = tab_drag_active && current_zone.is_some() && !cursor_on_header;
        let drop_zone_rect: Option<(f32, f32, f32, f32)> = show_drop_zone
            .then_some(current_zone)
            .flatten()
            .filter(|_| live_center || !cursor_on_header)
            .and_then(|(target, zone)| {
                let tree = self.pty_layout.as_ref()?;
                let leaves = tree.leaves().len();
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                let (_, cx, cy, cw, ch) = tree
                    .leaf_rects(cols, rows)
                    .into_iter()
                    .find(|(id, ..)| *id == target)?;
                let bx = pad + cx as f32 * self.cell.w;
                let pane_top = TITLE_HEIGHT + cy as f32 * self.cell.h;
                let bw = cw as f32 * self.cell.w;
                let bh = ch as f32 * self.cell.h;
                let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
                // Split overlay는 body 영역만 색칠 (헤더 띠 침범 X).
                let body_top = pane_top + header_band;
                let body_h = (bh - header_band).max(1.0);
                Some(match zone {
                    DropZone::Left => (bx, body_top, bw / 2.0, body_h),
                    DropZone::Right => (bx + bw / 2.0, body_top, bw / 2.0, body_h),
                    DropZone::Up => (bx, body_top, bw, body_h / 2.0),
                    DropZone::Down => (bx, body_top + body_h / 2.0, bw, body_h / 2.0),
                    // 반쪽이 아니라 body 통째 — "이 pane 안으로 들어간다"는 뜻이고,
                    // 어느 쪽으로도 갈리지 않는다는 것도 같이 읽힌다.
                    DropZone::Center => (bx, body_top, bw, body_h),
                })
            });
        // Ghostty-style split seams: one 1px hairline per interior split
        // boundary instead of a 4-side border around every pane (which
        // doubled up into a thick seam between abutting panes). Coords match
        // divider_at_px so drag hit-testing lines up with the drawn line.
        let pane_seams: Vec<(f32, f32, f32, f32)> = if self.zoomed_pane.is_some() {
            // Zoom 최대화 시 형제 pane이 숨겨지므로 분할선도 생략한다 — 안 그러면
            // 가려진 split 경계선이 최대화 화면 위에 1px 선으로 남는다(C 버그).
            Vec::new()
        } else {
            self.pty_layout
            .as_ref()
            .map(|tree| {
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                // True window edges (logical). window_cells floors the grid,
                // so a seam spanning the last row/col must reach past the grid
                // to the real edge — otherwise it stops short like box_h did.
                // ⚠️ 오른쪽 끝은 **창 끝이 아니라 우측 컬럼(Git·Info) 앞**이다.
                // 격자는 `window_cells` 가 그 폭을 이미 접어 두는데 이 선만 창 끝을
                // 써서, 마지막 열까지 걸친 가로선이 열려 있는 패널을 관통했다
                // (거노: "73·27 사이 선이 우측 패널까지 뚫어버려").
                let (win_right, win_bottom) = self.window.as_ref().map_or(
                    (
                        pad + cols as f32 * self.cell.w,
                        TITLE_HEIGHT + rows as f32 * self.cell.h,
                    ),
                    |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        (
                            w.inner_size().width as f32 / s - self.effective_right_chrome_w(),
                            w.inner_size().height as f32 / s,
                        )
                    },
                );
                tree.dividers(cols, rows)
                    .into_iter()
                    .map(|d| match d.dir {
                        kasa_pty::SplitDir::Horizontal => {
                            let x = pad + d.edge as f32 * self.cell.w;
                            let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                            let y1 = if d.span_start + d.span_len >= rows {
                                win_bottom
                            } else {
                                TITLE_HEIGHT
                                    + (d.span_start + d.span_len) as f32 * self.cell.h
                            };
                            (x, y0, 1.0, (y1 - y0).max(0.0))
                        }
                        kasa_pty::SplitDir::Vertical => {
                            let y = TITLE_HEIGHT + d.edge as f32 * self.cell.h;
                            let x0 = pad + d.span_start as f32 * self.cell.w;
                            let x1 = if d.span_start + d.span_len >= cols {
                                win_right
                            } else {
                                pad + (d.span_start + d.span_len) as f32 * self.cell.w
                            };
                            (x0, y, (x1 - x0).max(0.0), 1.0)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
        };
        // Left window-tab sidebar geometry. Cache the hit rects for the
        // mouse handler; the gpu block below paints from the same numbers so
        // a click always lands on what the user sees.
        let sb_win_h = win_px.1 / scale;
        self.refresh_window_labels();
        let sb_labels = self.window_labels.clone();
        let (sb_tabs, sb_closes, sb_plus, sb_rows, sb_mini) = self.sidebar_layout(sb_win_h);
        // Windowed strip: publish the effective first/visible-count for the
        // wheel handler's clamp, and note per-side overflow for the chevron
        // hints painted with the tabs below.
        self.win_tab_first = sb_tabs.first().map_or(0, |(i, _)| *i);
        self.win_tab_vis = sb_tabs.len().max(1);
        let sb_over_before = self.win_tab_first > 0;
        let sb_over_after = sb_tabs
            .last()
            .is_some_and(|(i, _)| i + 1 < self.windows.len());
        // Only register hit-rects when tabs are actually painted (side strip
        // open, or top-tabs mode where they always live in the title bar). A
        // hidden sidebar (file-tree-only / collapsed) must not leave stale tab
        // rects that a header-drag would false-hit as a cross-window drop.
        let sidebar_shown = self.tabs_on_top || self.tab_strip_w() > 0.0;
        self.window_tab_rects = if sidebar_shown { sb_tabs.clone() } else { Vec::new() };
        // 배치도 칸도 같은 히트 벡터에 넣는다 — 칸을 눌러도 목록 행을 누른 것과 똑같이
        // 포커스가 가고 드래그·우클릭까지 그대로 따라온다. 한 pane 이 rect 둘(칸·행)을
        // 갖지만 히트는 `find`(첫 매치)라 무해하다. **행이 먼저** 와야 좁은 칸보다
        // 누르기 쉬운 쪽이 이긴다.
        self.sidebar_row_rects = if sidebar_shown {
            sb_rows.iter().chain(sb_mini.iter()).cloned().collect()
        } else {
            Vec::new()
        };
        self.window_tab_close_rects = if sidebar_shown { sb_closes.clone() } else { Vec::new() };
        self.new_window_btn_rect = Some(sb_plus);
        // Shell picker popup layout, computed here (no GPU borrow) so the
        // click hit-list and the painted boxes share one source of truth.
        // Top tabs have room below the button, while the sidebar button lives
        // in the bottom tray and must open upward to stay inside the window.
        let menu_open = self.shell_menu_open;
        let shell_items: Vec<(&'static str, &'static str, String)> =
            if menu_open { available_shells() } else { Vec::new() };
        const SHELL_ITEM_H: f32 = 34.0;
        let menu_w_for_paint = sb_plus.2.max(210.0);
        let shell_menu_layout: Vec<(String, &'static str, &'static str, (f32, f32, f32, f32))> = {
            let (px, py, _, ph) = sb_plus;
            let menu_h = shell_items.len() as f32 * SHELL_ITEM_H;
            let mut iy = if self.tabs_on_top {
                py + ph + 4.0
            } else {
                py - menu_h - 4.0
            };
            shell_items
                .iter()
                .map(|(label, icon, cmd)| {
                    let r = (px, iy, menu_w_for_paint, SHELL_ITEM_H);
                    iy += SHELL_ITEM_H;
                    (cmd.clone(), *label, *icon, r)
                })
                .collect()
        };
        self.shell_menu_hits = shell_menu_layout
            .iter()
            .map(|(cmd, _, _, r)| (cmd.clone(), *r))
            .collect();
        let sb_active = self.active_window;
        // 방 단위 "작업 중"·"방금 끝남" 플래그는 걷어냈다. 그 둘이 칩 모서리의 점
        // 쌍을 켜던 유일한 자리였는데, 상태가 바뀔 때마다 점이 두 모서리를 오가는
        // 게 정보보다 먼저 읽혔다(2026-08-11 지시). 지금은 방을 펴면 그 방의 줄이
        // 학생을 걷게 해서 도는 중임을 말하고, 카드 머리에 남은 건 손이 필요할 때만
        // 깜빡이는 동그라미 하나다.
        //
        // 방마다 pane 하나당 점 하나 — 방을 열지 않고도 "누가 나를 기다리는지"가
        // 보이게 한다(거노). 색이 곧 상태다: 대기=danger(내가 엔터를 쳐야 풀린다) ·
        // 작업 중=accent · 방금 끝남=success · 쉬는 중=흐린 회색. 순서는 leaves
        // 순서라 pane 이 늘거나 줄기 전까진 점의 자리가 고정된다.
        //
        // `sb_busy` 하나로 뭉뚱그리던 것을 여기서 가른다 — 그건 `status != "idle"`
        // 이라 **엔터를 기다리는 pane 도 작업 중과 같은 파란 점**이었다. 정작 손이
        // 필요한 쪽이 바쁜 쪽과 구별되지 않던 게 이 화면의 가장 큰 거짓말이었다.
        let sb_dots: Vec<Vec<[u8; 4]>> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i).iter().map(|id| self.pane_state_color(id)).collect()
            })
            .collect();
        let sb_expand_t: Vec<f32> =
            (0..sb_labels.len()).map(|i| self.expand_progress(i)).collect();
        let sb_row_drop: Option<(String, bool, String)> = self
            .sidebar_row_drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| d.target.as_ref().map(|(t, b)| (t.clone(), *b, d.pane.clone())));
        // 펼치기 버튼의 사각은 클릭 판정과 같은 것을 쓴다 — 페인트 루프는 `&self`
        // 를 다시 못 빌리므로(GPU 를 이미 빌렸다) 방 인덱스로 늘어놓고 들어간다.
        let sb_expand: Vec<Option<(f32, f32, f32, f32)>> = (0..sb_labels.len())
            .map(|i| {
                sb_tabs
                    .iter()
                    .find(|(ti, _)| *ti == i)
                    .and_then(|(_, r)| self.window_expand_rect(i, *r))
            })
            .collect();
        // 뷰 전환 배지도 같은 이유로 미리 늘어놓는다. `(사각, 목록모드인가)`.
        let sb_view: Vec<Option<((f32, f32, f32, f32), bool)>> = (0..sb_labels.len())
            .map(|i| {
                sb_tabs
                    .iter()
                    .find(|(ti, _)| *ti == i)
                    .and_then(|(_, r)| self.window_view_rect(i, *r))
                    .map(|r| (r, self.list_view_windows.contains(&i)))
            })
            .collect();
        // 펼친 방의 pane 한 줄씩 — 이름·색을 여기서 뽑아 둔다. `pane_character_if_known`
        // 이 `ws` 를 잠그므로 GPU 를 빌린 페인트 루프 안에서 부르면 그 자리에서 멈춘다.
        // 줄에 적는 건 **그 pane 이 무엇을 하고 있나**(claude · zsh · 편집기…)다.
        // 학생 이름은 얼굴이 이미 말하고 있어, 글자로 한 번 더 쓰면 같은 말이 두 번
        // 나오고 정작 pane 을 가르는 정보가 자리를 잃는다(거노: "학생이름은 빼고").
        // 배치도 칸에 쓸 활성 pane — 칸마다 락을 잡지 않게 여기서 한 번만 뜬다
        // (페인트 루프는 gpu 를 빌린 상태라 `&self` 메서드도 못 부른다).
        let sb_active_pane = self.ws.lock().unwrap().active_pane.clone();
        // 배치도 칸과 꼬리 줄이 **같은 것**을 말한다 — 한쪽만 고치면 같은 pane 이
        // 자리마다 다른 얼굴을 갖는다. 그래서 계산은 한 벌이다.
        let pane_info = |id: &String| -> SidebarRowInfo {
            {
                // 얼굴은 claude 가 붙은 pane 에만 — 셸만 도는 자리에 학생이 먼저 앉아
                // 있으면 목록이 "이미 일하는 중"이라고 거짓말한다.
                let who = self
                    .pane_claude_ready(id)
                    .then(|| self.pane_character_if_known(id))
                    .flatten()
                    .unwrap_or_default();
                let (is_cur, icon) = {
                    let ws = self.ws.lock().unwrap();
                    let is_cur = ws.active_pane.as_deref() == Some(id.as_str());
                    // 활성 탭 기준(Deref) — 칸/줄은 pane 하나를 대표하므로
                    // 보이는 탭이 말하는 게 맞다.
                    let icon = ws
                        .panes
                        .get(id)
                        .map(|p| match &p.content {
                            PaneContent::Web(_) => "globe",
                            PaneContent::Image(_) => "image",
                            PaneContent::Markdown(_) => "file-text",
                            _ => "terminal",
                        })
                        .unwrap_or("terminal");
                    (is_cur, icon)
                };
                let label = self.pane_row_label(id);
                let waiting = self.pane_needs_you(id);
                // 걷게 할 조건은 헤더 진행 바와 **같은 한 벌**을 쓴다. 기다리는 중은
                // 빠진다 — 그건 도는 게 아니라 멈춘 것이고, 걸으면서 동시에 나를
                // 부르면 두 신호가 서로를 부정한다.
                let busy = self.pane_is_busy(id);
                SidebarRowInfo {
                    who,
                    label,
                    color: self.pane_state_color(id),
                    is_cur,
                    // 못 본 완료 — 방이 아니라 **이 줄** 이 숨쉰다(거노: "숨쉬기효과
                    // 윈도우전체가 아니라 완료된세션하나만"). window_alert 가 아니라
                    // unread_panes 를 보는 이유: 전자는 배경 방에만 서고, 지금 보고
                    // 있는 방에서 옆 pane 이 끝난 것도 알려야 한다. 내가 그 pane 을
                    // 보는 순간 sync_dock_badge 가 지운다.
                    //
                    // 대기 중이면 양보한다 — `handle_attention` 이 unread 에도 넣기
                    // 때문에 둘이 같이 서고, 그러면 한 줄에 느린 숨과 빠른 깜빡임이
                    // 겹쳐 어느 쪽도 안 읽힌다. 급한 쪽이 이긴다.
                    alert: !waiting && self.unread_panes.contains(id),
                    waiting,
                    busy,
                    stashed: self
                        .closed_panes
                        .iter()
                        .any(|c| c.stashed && c.alive && c.pane_id == *id),
                    icon,
                }
            }
        };
        let sb_row_info: Vec<SidebarRowInfo> =
            sb_rows.iter().map(|(_, id, _)| pane_info(id)).collect();
        let sb_mini_info: Vec<SidebarRowInfo> =
            sb_mini.iter().map(|(_, id, _)| pane_info(id)).collect();
        // 펼친 방에서 그 방의 알림·대기를 **줄이 이미 말하고 있는가**. 말하고 있으면
        // 카드 머리는 조용히 둔다 — 같은 뜻을 두 겹으로 칠하면 결국 방 전체가 빛나
        // 고치기 전으로 돌아간다. 접힌 방은 줄이 없으니 여기에 안 들고, 머리가 계속
        // 말한다(그때는 그게 유일한 자리다).
        let mut sb_row_alert_win: std::collections::HashSet<usize> = Default::default();
        let mut sb_row_wait_win: std::collections::HashSet<usize> = Default::default();
        // 배치도 칸도 같은 말을 한다 — 칸이 통째로 숨쉬게 된 뒤로는 목록과 똑같은
        // 자격이다. 여기 안 넣으면 배치도 모드에서 칸과 머리 점이 같이 떠, 목록을
        // 고치며 없앴던 두 겹 칠하기가 그대로 되살아난다(실측: 캡처에 둘 다 떴다).
        let signalled = sb_rows
            .iter()
            .zip(sb_row_info.iter())
            .chain(sb_mini.iter().zip(sb_mini_info.iter()));
        for ((wi, _, _), info) in signalled {
            if info.alert {
                sb_row_alert_win.insert(*wi);
            }
            if info.waiting {
                sb_row_wait_win.insert(*wi);
            }
        }
        // 아이콘 칩 모서리의 작업 점도 같은 구분을 따른다 — 사이드바를 좁혀 두면
        // 그 점이 그 방의 유일한 표시라, 여기서만 뭉뚱그리면 좁은 모드에서 다시
        // 거짓말이 된다.
        let sb_wait: Vec<bool> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i).iter().any(|id| {
                    self.pane_needs_you(id)
                })
            })
            .collect();
        // Per-window "unseen notification" flag: a pane finished / needs
        // attention while this window sat in the background. The tab pulses
        // (synced to the cursor blink) until the user switches to it. Unlike
        // sb_done's brief flash, this persists across the whole alert.
        let sb_alert: Vec<bool> = (0..sb_labels.len())
            .map(|i| self.window_alert.contains(&i))
            .collect();
        // 별도 창으로 나가 있는 방 — 탭은 자리를 지키되 ⌘N 대신 나갔다는 표시가 뜬다.
        let sb_undocked: Vec<bool> =
            (0..sb_labels.len()).map(|i| self.window_is_undocked(i)).collect();
        // 방 탭을 끌고 있는 동안 떨어질 자리. 탭 자체는 제자리에 두고 삽입선만
        // 그린다 — 실제 이동은 release 뿐이라, 놓기 전엔 "여기로 간다"만 알면 된다.
        let win_drag_target: Option<usize> =
            self.win_tab_drag.as_ref().filter(|d| d.active).map(|d| d.target);
        // Which tab the cursor is over (for hover affordance + showing × only
        // where the user is pointing, Warp-style).
        let sb_cursor = self.cursor_px;
        let sb_hover = sb_tabs
            .iter()
            .find(|(_, r)| {
                sb_cursor.0 >= r.0
                    && sb_cursor.0 <= r.0 + r.2
                    && sb_cursor.1 >= r.1
                    && sb_cursor.1 <= r.1 + r.3
            })
            .map(|(i, _)| *i);
        // 조합 중인 글자는 **조합기 주인**에게만 그린다. 예전엔 pane 을 안 가려서,
        // 터미널에서 치는 한글이 열려 있는 편집기에도 같이 떴다(거노: "입력이
        // 동시에 되고"). 주인은 `ime_focus` 가 이미 알고 있다.
        let md_preedit = self.preedit.clone();
        let ime_editor: Option<String> = match &self.ime_focus {
            Some(crate::ImeFocus::Editor(id)) => Some(id.clone()),
            _ => None,
        };
        // Raw-editor cursor blink phase (shared with the terminal cursor), read
        // before the gpu borrow so the editor cursor blinks in step.
        let raw_cursor_on = self.cursor_blink_on(std::time::Instant::now());
        // In-pane tab hit rects, collected during the header paint (needs the
        // measured tab widths) and published to self after the gpu borrow.
        let mut tab_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        let mut tab_close_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        // 파일 탭 hover 시 뜨는 팝아웃(별도창) 아이콘 hit rect: (pane id, 탭 idx, rect).
        let mut tab_popout_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        // 계정이 바뀐 pane 의 「재시작」 칩 — gpu 대여가 끝난 뒤 self 로 옮긴다.
        let mut restart_chip_hits: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let mut plus_hits: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        // Tab-overflow windowing per pane: (id, effective first, visible
        // count, active tab this frame) — written back to ws.panes after the
        // gpu borrow so the wheel handler and next frame's reveal check see
        // the clamped values.
        let mut pane_tab_windowing: Vec<(String, usize, usize, usize)> = Vec::new();
        let mut image_btn_hits: Vec<(String, ImageBtn, (f32, f32, f32, f32))> = Vec::new();
        // Terminal-pane right-action cluster hit rects. Rebuilt every frame
        // so a stale rect can't outlive its glyph after a layout change.
        let mut pane_action_hits: Vec<(String, ActionKind, (f32, f32, f32, f32))> = Vec::new();
        let mut confirm_btn_hits: Vec<(ConfirmBtn, (f32, f32, f32, f32))> = Vec::new();
        let mut restore_btn_hits: Vec<(RestoreBtn, (f32, f32, f32, f32))> = Vec::new();
        // Settings entry lives in its own wgpu window now (auxwin.rs); the main
        // frame only draws the sidebar "Settings" entry rect for hit-testing.
        let win_h_logical = win_px.1 / scale;
        let settings_btn = self.settings_btn_rect(win_h_logical);
        self.settings_btn_rect = settings_btn;
        self.feedback_btn_rect = self.feedback_btn_rect(win_h_logical);
        // 트레이 기하는 `&self` 메서드라 아래 `self.gpu.as_mut()` 빌림 안에서는
        // 못 부른다 — 다른 chrome rect 들과 같이 여기서 미리 읽는다.
        let sidebar_tray = self.sidebar_tray_rects(win_h_logical);
        let file_tree_toggle = self.file_tree_toggle_rect();
        let sidebar_toggle = self.sidebar_toggle_rect();
        // 타이틀바 제목을 가운데 세울 때 오른쪽 한계로 쓴다 — 같은 이유로 여기서
        // 미리 읽는다(`g` 가 self.gpu 를 잡은 뒤엔 &self 메서드를 못 부른다).
        let git_col_toggle = self.git_col_toggle_rect();
        // Caret blink for the commit-modal message box, computed before `g`
        // borrows `self.gpu` (the blink helper takes `&self`).
        let commit_caret_on = self.cursor_blink_on(std::time::Instant::now());
        // Per-header completion-flash strength, sampled before `g` borrows
        // `self.gpu` (the header loop can't call `&self` while `g` is live).
        let header_flash: Vec<Option<f32>> =
            headers.iter().map(|h| self.notify_flash_factor(&h.id)).collect();
        // "빠른 파일" 목록 — &self 메서드라 아래 &mut self.gpu 빌림 안에서는 못 부른다.
        // 빌림 전에 스냅샷(파일트리 렌더에서 로컬로 소비).
        let quick_files_list = self.quick_files();
        // 아래 &mut self.gpu 빌림 안에서 &self 메서드를 못 부른다 — 미리 스냅샷.
        let dock_own_reserve = self.dock_reserve_h();
        // 학생 도트 배너 가시 상태 → 애니 타이머(handler.rs)와 damage 게이트
        // (render_frame)가 참조. 배너가 사라진 프레임에 false로 떨어져
        // 애니 redraw 펌프가 저절로 멈춘다.
        STUDENT_SPRITE_ANIMATING.store(
            // waiting(승인 대기)·standing(입력박스 위)은 렌더 펌프가 없는 정적
            // 상태에서도 idle 애니가 돌아야 해서 이 타이머에 의존한다. 스피너
            // 도트는 working 30fps 펌프가 있고, statusline 프사는 정적이라 불필요.
            !banner_slots.is_empty() || !waiting_slots.is_empty() || !standing_slots.is_empty(),
            std::sync::atomic::Ordering::Relaxed,
        );
        // `[Image #N]` 썸네일 — transcript 를 읽는 일이라 gpu 를 빌리기 전에 끝낸다.
        let tip_box = tip_hit.as_ref().map(|(_, _, b)| *b);
        let image_tip = self
            .pump_image_tip(tip_hit.map(|(pane, n, _)| (pane, n)))
            .zip(tip_box);
        if let Some(g) = self.gpu.as_mut() {
            g.clear_chrome();
            // Upload any image pane's pixels once, then queue each for this
            // frame. The image pass (in g.render) paints under the chrome so
            // pane headers / focus ring / dim overlay land on top.
            for (id, image, _, _, rot, _) in &image_slots {
                // Per-(image, rotation, frame) cache key. The image pointer is in
                // the key because one pane can hold several image tabs — keying on
                // pane id alone made the 2nd image tab collide with the 1st's
                // texture (has_image hit → 2nd/switched image showed the 1st's
                // pixels, 거노: 같은 pane에 이미지 띄우면 이전 게 덮어써짐).
                let cur = image.cur_idx();
                let key = format!("{id}-p{:x}-r{rot}-f{cur}", Arc::as_ptr(image) as usize);
                if !g.has_image(&key) {
                    let (rgba, w, h) = rotate_rgba_cw(image.cur_rgba(), image.w, image.h, *rot);
                    g.upload_image(&key, &rgba, w, h);
                }
            }
            g.draw_cells(&slot_views);
            for (id, image, (bx, by, bw, bh), zoom, rot, (pan_x, pan_y)) in &image_slots {
                let key = format!("{id}-p{:x}-r{rot}-f{}", Arc::as_ptr(image) as usize, image.cur_idx());
                g.queue_image(&key, *bx, *by, *bw, *bh, *zoom, *pan_x, *pan_y);
            }
            paint_inline_images(g, &inline_slots);
            // 학생 도트 — Clawd 배너 자리. idle 4프레임을 캐릭터당 1회 일괄
            // 업로드해 모든 pane이 공유하고, 매 프레임 시간 기반으로 현재
            // 프레임만 queue한다(재렌더는 배너 애니 타이머가 깨워줌).
            // 디코딩 실패 시 queue_image가 조용히 skip.
            let anim_ms = self.version_anim_start.elapsed().as_millis() as u64;
            // 배너·스피너·승인대기·standing·프사는 별도창(auxwin)도 그린다 —
            // 그리기와 이미지 키는 `paint_student_overlays` 한 곳에 있고, 여기선
            // 이 창의 좌표로 모은 자리만 넘긴다.
            let student_slots = StudentOverlays {
                banner: std::mem::take(&mut banner_slots),
                spinner: std::mem::take(&mut spinner_slots),
                waiting: std::mem::take(&mut waiting_slots),
                standing: std::mem::take(&mut standing_slots),
                profile: std::mem::take(&mut profile_slots),
            };
            paint_student_overlays(g, &student_slots, anim_ms);
            // Claude Code 스크롤 sticky prompt: 텍스트·흰 배경은 위 스캔에서 원본
            // 셀을 선명화(등폭 유지)해 이미 그려졌다. 여기선 클릭 rect(셀 영역)만
            // STICKY_PILLS 로 mouse handler·seek 에 넘긴다 — 클릭 = "그 프롬프트가
            // 화면에 들어올 때까지 위로 스크롤"(begin_sticky_seek).
            STICKY_PILLS.with(|s| s.borrow_mut().clear());
            // 클릭 영역은 **여기서 한 번만** 비운다. 아래 sticky 루프와 그보다 뒤의
            // 턴 헤더 루프가 같은 통에 담으므로, 뒤쪽에서 또 비우면 앞에서 담은
            // 화살표가 통째로 사라진다 — 화면은 멀쩡한데 안 눌리는, 스크린샷이
            // 절대 못 잡는 부류다(실제로 그렇게 짰다가 여기서 잡았다).
            crate::turnjump::TURN_HITS.with(|s| s.borrow_mut().clear());
            for (px, py, pw, ph, text, pane_id, a_up, a_down) in &sticky_pill_slots {
                STICKY_PILLS.with(|s| {
                    s.borrow_mut().push((pane_id.clone(), (*px, *py, *pw, *ph), text.clone()))
                });
                // pill 에 얹은 ↑↓ — 바 클릭(위로 되짚기)보다 **나중에** 담아야
                // 겹치는 자리에서 화살표가 이긴다(조회가 역순이다).
                crate::turnjump::TURN_HITS.with(|s| {
                    let mut v = s.borrow_mut();
                    if let Some(r) = a_up {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::SeekPrev));
                    }
                    if let Some(r) = a_down {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::SeekNext));
                    }
                });
            }
            // 대화 턴 헤더의 클릭 영역. 그림은 이미 셀로 그려졌고 여기선 자리만
            // 넘긴다. **바를 먼저, 화살표를 나중에** 담는 순서가 곧 우선순위다 —
            // 조회가 역순이라 겹치는 자리에서 화살표가 이긴다(화면에서 위에 있는
            // 것이 클릭도 가져간다).
            for (pane_id, bar, up, down, h) in &turn_header_slots {
                crate::turnjump::TURN_HITS.with(|s| {
                    let mut v = s.borrow_mut();
                    v.push((pane_id.clone(), *bar, crate::turnjump::TurnHit::Jump(h.cur_abs)));
                    if let (Some(r), Some(a)) = (up, h.prev_abs) {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::Prev(a)));
                    }
                    if let (Some(r), Some(a)) = (down, h.next_abs) {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::Next(a)));
                    }
                });
            }
            // agents 뷰 SCHALE 로고 — Clawd 자리(또는 헤더 왼쪽 여백)에 정적 1프레임.
            // 교실 배경 — 셀 뒤(이미지 패스, cover-fit). agents/resume 피커 pane 만.
            if !classroom_slots.is_empty() {
                if !g.has_image("schale:classroom") {
                    if let Some((rgba, w, h)) = schale_classroom_rgba() {
                        g.upload_image("schale:classroom", &rgba, w, h);
                    }
                }
                for (bx, by, bw, bh) in &classroom_slots {
                    g.queue_image_cover("schale:classroom", *bx, *by, *bw, *bh);
                }
            }
            if !schale_logo_slots.is_empty() {
                if !g.has_image("schale:logo") {
                    if let Some((rgba, w, h)) = schale_logo_rgba() {
                        g.upload_image("schale:logo", &rgba, w, h);
                    }
                }
                for (bx, by, bw, bh) in &schale_logo_slots {
                    g.queue_image_above("schale:logo", *bx, *by, *bw, *bh);
                }
            }
            // /rename 세션명 아웃라인 — 입력박스 위 구분선 이름을 사각 테두리로(4변).
            for (x, y, w, h, col) in &title_outline_slots {
                let t = 1.5_f32;
                g.rect(*x, *y, *w, t, *col);
                g.rect(*x, *y + *h - t, *w, t, *col);
                g.rect(*x, *y, t, *h, *col);
                g.rect(*x + *w - t, *y, t, *h, *col);
            }
            // Markdown is laid out into chrome glyphs/rects here — after the
            // (empty) cell pass, before pane headers/borders so those land on
            // top. The returned content height feeds scroll clamping.
            // Rebuilt fresh each frame so a pane toggled out of raw mode (or
            // closed) drops its caret hit box.
            self.md_body_rects.clear();
            let mut find_btn_hits: Vec<(String, FindBtn, (f32, f32, f32, f32))> = Vec::new();
            for (
                id,
                doc,
                (bx, by, bw, bh),
                scroll,
                raw_mode,
                lines,
                cursor,
                sel,
                h_scroll,
                lang,
                find,
                complete,
                diags,
                folds,
                wrap,
                extra,
            ) in &md_slots
            {
                let content_h = if *raw_mode {
                    let lines = lines.as_ref().map_or(&[][..], |v| v.as_slice());
                    // Stash the body box so a mouse click can hit-test to a caret
                    // position (md_click_caret reads this).
                    self.md_body_rects.insert(id.clone(), (*bx, *by, *bw, *bh));
                    // 조합 중인 한글은 포커스를 가진 쪽에 그린다 — 찾기 바가
                    // 열려 있는데 문서 캐럿에 preedit 이 뜨면 어디에 쓰고 있는지
                    // 화면이 거짓말을 한다.
                    let mine = ime_editor.as_deref() == Some(id.as_str());
                    let pe = if mine { md_preedit.as_str() } else { "" };
                    let (body_pe, bar_pe): (&str, &str) = match find {
                        Some(_) => ("", pe),
                        None => (pe, ""),
                    };
                    let h = g.draw_raw_editor(
                        lines,
                        *cursor,
                        *sel,
                        *bx,
                        *by,
                        *bw,
                        *bh,
                        *scroll,
                        *h_scroll,
                        lang,
                        body_pe,
                        raw_cursor_on,
                        find.as_ref().map(|f| (f.hits.as_slice(), f.idx)),
                        complete.as_ref().map(|(i, s, c)| (i.as_slice(), *s, *c)),
                        diags,
                        folds,
                        *wrap,
                        extra,
                    );
                    if let Some(f) = find {
                        for (btn, r) in
                            Self::draw_find_bar(g, f, *bx, *by, *bw, bar_pe, raw_cursor_on, sb_cursor)
                        {
                            find_btn_hits.push((id.clone(), btn, r));
                        }
                    }
                    h
                } else {
                    // Upload this doc's inline images once (keyed per block).
                    for im in &doc.images {
                        if !g.has_image(&im.key) {
                            g.upload_image(&im.key, &im.rgba, im.w, im.h);
                        }
                    }
                    // 이 pane 의 선택만 넘긴다 — 마크다운 pane 이 둘일 때 다른
                    // pane 의 범위로 띠를 깔면 안 된다.
                    let sel = self
                        .md_render_sel
                        .as_ref()
                        .filter(|s| s.pane == *id)
                        .map(|s| (s.anchor.0, s.anchor.1, s.end.0, s.end.1));
                    let h =
                        g.draw_markdown(&doc.blocks, doc.gen, *bx, *by, *bw, *bh, *scroll, sel);
                    // 이 pane 이 그린 낱말 사각형을 옮겨 둔다 — 복사·히트테스트가
                    // 읽고, block_ys 와 같은 이유로 pane 별로 갈라야 한다.
                    let words = std::mem::take(&mut g.md_word_rects);
                    self.md_word_rects.insert(id.clone(), words);
                    // 블록별 문서좌표 y 를 pane 별로 옮겨 둔다 — Gpu 쪽은 pane
                    // 을 모르고 매 프레임 덮어써서, 마크다운 pane 이 둘이면
                    // 마지막 것만 남는다.
                    let ys = std::mem::take(&mut g.md_block_ys);
                    // Raw→Render 토글이 남긴 앵커: 이제야 새 레이아웃의 y 가
                    // 생겼으니 보던 줄을 화면 맨 위로 되돌린다. 한 프레임 늦는
                    // 건 어쩔 수 없다 — 위치는 그려봐야 알 수 있어서다.
                    if let Some(line) = self.md_scroll_anchor.remove(id) {
                        let i = doc.block_lines.partition_point(|&l| l <= line).saturating_sub(1);
                        let want = ys.get(i).copied().unwrap_or(0.0).max(0.0);
                        if (want - *scroll).abs() > 0.5 {
                            if let Ok(mut ws) = self.ws.lock() {
                                if let Some(pane) = ws.panes.get_mut(id) {
                                    pane.dirty = true;
                                    if let Some(m) = pane.markdown_mut() {
                                        m.scroll = want;
                                    }
                                }
                            }
                        }
                    }
                    self.md_block_ys.insert(id.clone(), ys);
                    h
                };
                self.md_content_h.insert(id.clone(), content_h);
            }
            self.md_find_rects = find_btn_hits;
            // 호버 툴팁 — pane 을 다 그린 뒤에 얹는다. pane 안에서 그리면 툴팁이
            // 경계를 넘는 순간 다음 pane 이 위를 덮어 반쪽만 남는다.
            if let Some((tip, hx, hy)) = self
                .hover
                .as_ref()
                .and_then(|h| h.text.as_ref().map(|t| (t.clone(), h.at.0, h.at.1)))
            {
                Self::draw_hover_tip(g, &tip, hx, hy, win_px.0 / scale, win_px.1 / scale);
            }
            // `[Image #N]` 썸네일 — 같은 이유로 pane 을 다 그린 뒤다. 텍스처를
            // 놓는 일이 있어 툴팁이 없는 프레임에도 부른다.
            Self::paint_image_tip(g, image_tip, win_px.0 / scale, win_px.1 / scale);
            // 크롬 판 — 위 스트립과 사이드바 칼럼이 이어진 ㄴ 자다. 본문보다 한 톤
            // 들려 있어 터미널이 그 위에 얹힌 것처럼 읽힌다.
            //
            // 사이드바 칼럼을 여기서(스트립과 같은 시점에) 칠하는 건 신호등 때문이다.
            // 칼럼이 y=0 까지 올라와야 신호등이 사이드바 위에 앉는데, 아래쪽에서 칠하면
            // 스트립에 이미 그린 토글 아이콘을 덮어 버린다.
            g.rect(0.0, 0.0, win_px.0 / scale, TITLE_HEIGHT, theme::panel_bg());
            if tab_strip_w > 0.0 {
                g.rect(0.0, 0.0, tab_strip_w, sb_win_h, theme::panel_bg());
                g.rect(tab_strip_w - 1.0, 0.0, 1.0, sb_win_h, theme::border());
            }
            // 사이드바 토글. 자리는 `sidebar_toggle_rect` 가 정한다 — 접혔으면
            // 신호등 오른쪽, 폈으면 사이드바 오른쪽 위. 글리프는 그대로다(왼쪽
            // 칼럼이 찬 판 모양). 탭이 위로 가면 토글할 세로 스트립이 없다.
            if !self.tabs_on_top {
                let (bx, by, bw, bh) = sidebar_toggle;
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                // Brighter when the sidebar is open (state indicator) or on
                // hover; the panel-left SVG shape stays constant.
                let active = tab_strip_w > 0.0;
                let fg = if hover || active { theme::text() } else { theme::text_dim() };
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "panel-left",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    fg,
                );
            }
            // File-tree toggle, just right of the sidebar toggle. Same chip
            // treatment; lit when the tree column is shown.
            {
                let (bx, by, bw, bh) = file_tree_toggle;
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                let active = tree_col_w > 0.0;
                let fg = if hover || active { theme::text() } else { theme::text_dim() };
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "folder-tree",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    fg,
                );
            }
            // Git-column toggle, parked at the right end of the title strip
            // (the column lives on the right). This used to be drawn inline from
            // rounded rects to avoid adding an asset; that predates the shape
            // axis, and a hand-drawn 3px radius can't follow a pixel silhouette
            // the way the icon set does. It's `panel-right` now — the mirror of
            // the sidebar toggle's `panel-left`, which is what it always meant.
            {
                let bw = 26.0_f32;
                let bh = 22.0_f32;
                #[cfg(not(windows))]
                let bx = win_px.0 / scale - bw - 8.0;
                // Windows paints its own min/max/close at the right edge; shove
                // the git-column toggle left of that cluster so they don't stack.
                #[cfg(windows)]
                let bx = Self::win_control_rects(win_px.0 / scale)[0].0 - 2.0 - bw;
                let by = (TITLE_HEIGHT - bh) / 2.0;
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                let active = git_col_w > 0.0;
                let fg = if hover || active { theme::text() } else { theme::text_dim() };
                let gs = 15.0_f32;
                g.queue_icon("panel-right", bx + (bw - gs) / 2.0, by + (bh - gs) / 2.0, gs, fg);
            }
            // Windows frameless window controls (min / max / close) at the
            // strip's right edge. Native decorations are off on Windows, so we
            // paint and route these ourselves — same chip family as the toggles.
            #[cfg(windows)]
            {
                let ctrls = Self::win_control_rects(win_px.0 / scale);
                let icons = ["minus", "maximize", "x"];
                for (i, &(bx, by, bw, bh)) in ctrls.iter().enumerate() {
                    let hover = sb_cursor.0 >= bx
                        && sb_cursor.0 <= bx + bw
                        && sb_cursor.1 >= by
                        && sb_cursor.1 <= by + bh;
                    if hover {
                        hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                    }
                    let fg = if hover { theme::text() } else { theme::text_dim() };
                    let isz = theme::ICON_SIZE;
                    g.queue_icon(
                        icons[i],
                        bx + (bw - isz) / 2.0,
                        by + (bh - isz) / 2.0,
                        isz,
                        fg,
                    );
                }
            }
            // 위 스트립: 활성 세션 알약 + 현재 경로. 세로 탭 배치 전용 —
            // 탭이 위로 가면 탭들이 이 자리를 쓴다.
            if !self.tabs_on_top {
                let (tbx, _, tbw, _) = file_tree_toggle;
                let px0 = tbx + tbw + 12.0;
                let ty = (TITLE_HEIGHT - chrome_font) / 2.0;
                // 경로는 알약 뒤에 온다 — "무엇을 보고 있나" 다음이 "어디인가"다.
                // 폭은 알약을 그린 뒤에야 정해지므로 자리만 잡아 두고 아래에서 그린다.
                // Title-bar cwd chip follows the FOCUSED pane's shell cwd —
                // resolved through pane_current_cwd: the ~700ms cwd cache first
                // (which prefers the shell's OSC 9;9 report — the only accurate
                // source under PowerShell, whose process cwd never moves), then
                // the shell pid's real cwd. Falls back to kasaterm's own cwd
                // when the pane has no PTY (image / markdown) or nothing
                // resolved. Reading the cache also keeps this off the
                // per-frame lsof / ReadProcessMemory path it used to take.
                let cwd_str = {
                    let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
                    // Borrow the two fields explicitly rather than calling
                    // `self.pane_current_cwd()` — `self.gpu` is already mutably
                    // borrowed above, so only a disjoint field borrow compiles.
                    let cache = &self.pane_cwd_cache;
                    let pty = &self.pty;
                    active
                        .and_then(|id| {
                            cache.get(&id).cloned().or_else(|| {
                                pty.get(&id).and_then(|p| {
                                    p.reported_cwd()
                                        .or_else(|| p.shell_pid().and_then(socket::pid_cwd))
                                })
                            })
                        })
                        .or_else(|| std::env::current_dir().ok())
                        .map(|p| Self::shorten_cwd(&p))
                        .unwrap_or_default()
                };
                // Active pane title (OSC 0/2 or shell process name).
                // Active pane's accent (surface.set_color) recolors the
                // title text too, so it matches the per-pane tabs.
                let title_color = {
                    let ws = self.ws.lock().unwrap();
                    ws.active_pane
                        .as_deref()
                        .and_then(|id| ws.panes.get(id))
                        .and_then(|p| p.color)
                        .unwrap_or_else(theme::text)
                };
                // active pane 의 claude 세션이 bg_agents(background kind)에 있으면
                // 포크/백그라운드 배지. pane_claude_sid = 실제 세션(fork 시 갈라진 것).
                let title_is_bg = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.active_pane.clone())
                    .and_then(|id| self.pane_claude_sid.get(&id).cloned())
                    .is_some_and(|sid| {
                        self.bg_agents.lock().map(|m| m.contains_key(&sid)).unwrap_or(false)
                    });
                let title_text: String = {
                    let ws = self.ws.lock().unwrap();
                    let active = ws.active_pane.clone();
                    // claude code 가 이 pane 의 foreground 프로세스면 타이틀바에
                    // "학생 이름 · 작업명"(거노: claude code 일 때만 학생 이름). zsh 등
                    // 일반 셸은 기존 process · tty 폴백. session-id 매칭은 /resume 시
                    // 실제 sessionId 가 주입값과 어긋나 깨졌다(거노) → foreground 프로세스명
                    // ("claude")으로 판정해 resume·--session-id 무관하게 견고하다.
                    let claude_char = active
                        .as_deref()
                        .filter(|id| {
                            self.pty
                                .get(*id)
                                .and_then(|p| p.active_agent())
                                .is_some()
                        })
                        .and_then(|id| {
                            // 프사와 동일 규칙(display_pane_char 인라인 — gpu 가변 차용
                            // 중이라 메서드 호출 불가, 필드 접근은 분리 캡처로 허용):
                            // 뷰 pane 은 파싱 전 스폰 랜덤을 타이틀바에도 안 올린다.
                            self.pane_claude_sid
                                .get(id)
                                .and_then(|sid| kasa_mcp::character::session_character(sid))
                                .or_else(|| {
                                    let view = self
                                        .pty
                                        .get(id)
                                        .map(|p| p.is_claude_agents())
                                        .unwrap_or(false);
                                    if view {
                                        None
                                    } else {
                                        ws.pane_character.get(id).cloned()
                                    }
                                })
                        })
                        .filter(|c| !c.is_empty());
                    // active pane 이 claude agents 목록 뷰면 타이틀바도 SCHALE(작업명 유지).
                    let agents_active = active
                        .as_deref()
                        .map_or(false, |id| agents_view_panes.contains(id));
                    if agents_active {
                        let work = active
                            .as_deref()
                            .and_then(|id| ws.panes.get(id).and_then(|p| p.title.clone()))
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|s| !s.is_empty());
                        match work {
                            Some(w) => format!("샬레  ·  {w}"),
                            None => "샬레".to_string(),
                        }
                    } else if let Some(c) = claude_char {
                        let work = active
                            .as_deref()
                            .and_then(|id| ws.panes.get(id).and_then(|p| p.title.clone()))
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|s| !s.is_empty());
                        // pane 아이디를 캐릭터 뒤에 (거노 2026-08-05: "칩 위치를 바꿔
                        // pane아이디 이런데에, 거기는 /rename 들어갈 자리니까").
                        //
                        // 헤더 띠가 아니라 **타이틀바**에 붙이는 이유: 학생 헤더 띠는
                        // 거노가 폐기했고(main.rs:2146) 학생 정체는 그때 타이틀바로
                        // 옮겨졌다. 단일 탭 pane 은 `has_header()` 가 false 라 띠 쪽에만
                        // 붙이면 **거노 화면엔 안 보인다** — 하네스만 통과하는 그 모양이
                        // 오늘 두 번 물었다. 띠를 되살리면 거노가 회수한 세로 공간이
                        // pane 마다 다시 나가므로 그건 그의 결정이다.
                        let with_id = match active.as_deref() {
                            Some(id) => format!("{c} {id}"),
                            None => c,
                        };
                        match work {
                            Some(w) => format!("{with_id}  ·  {w}"),
                            None => with_id,
                        }
                    } else {
                        let title = active
                            .as_deref()
                            .and_then(|id| ws.panes.get(id).map(|p| (id.to_string(), p.title.clone())))
                            .and_then(|(id, osc)| {
                                osc.filter(|s| !s.is_empty()).or_else(|| {
                                    self.pty
                                        .get(&id)
                                        .and_then(|p| p.active_process_name())
                                        .filter(|s| !s.is_empty())
                                })
                            })
                            .unwrap_or_default();
                        // Append the pane's real OS tty (ghostty-style).
                        let tty = active.as_deref().and_then(|id| {
                            self.pane_tty_cache
                                .get(id)
                                .cloned()
                                .or_else(|| self.pty.get(id).and_then(|p| p.tty().map(str::to_string)))
                        });
                        match (title.is_empty(), tty) {
                            (false, Some(t)) => format!("{title}  ·  {t}"),
                            (true, Some(t)) => t,
                            (_, None) => title,
                        }
                    }
                };
                // 제목은 은은한 배경 칩에 담는다 — 아이콘·경로·토글이 늘어선 한 줄에서
                // "이게 지금 열린 탭"이라고 자리를 묶어 주되, 눌리는 것은 아니다.
                //
                // 배경은 flat(round_rect)이어야 한다. panel_rect 로 그렸더니 픽셀
                // 실루엣의 검은 테두리·하드 섀도가 붙어 떠오른 버튼처럼 보였는데,
                // 이건 클릭 대상이 아니라 표시라 눌리는 신호를 주면 안 된다.
                //
                // 가운데에 세우는 것은 **이름 칩 하나**다. 경로는 파일트리 버튼
                // 오른쪽 제자리에 남는다(거노) — 둘을 한 덩어리로 묶어 가운데를
                // 잡으면 뒤에 붙는 경로 길이만큼 이름이 왼쪽으로 밀려, 정작
                // 가운데 오는 것은 이름과 경로 사이 빈 자리가 된다. 칩이 경로와
                // 오른쪽 토글 사이에 안 들어갈 만큼 길면 중앙을 포기하고 경로
                // 오른쪽에 붙인다 — 잘리는 것보다 낫다.
                // 포크/백그라운드 세션이면 이름 뒤에 dim 배지(⑂ = 분기 기호).
                const BG_BADGE: &str = "  ⑂ bg";
                let gl = 14.0_f32;
                let pad = 10.0_f32;
                let ph = 26.0_f32;
                let py = (TITLE_HEIGHT - ph) / 2.0;
                let isz = theme::ICON_SIZE;
                let (tw, bw) = if title_text.is_empty() {
                    (0.0, 0.0)
                } else {
                    (
                        g.measure_chrome_text(&title_text, chrome_font, true),
                        if title_is_bg {
                            g.measure_chrome_text(BG_BADGE, chrome_font, false)
                        } else {
                            0.0
                        },
                    )
                };
                let pw = if title_text.is_empty() {
                    0.0
                } else {
                    pad + gl + 7.0 + tw + bw + pad
                };
                let cwd_w = if cwd_str.is_empty() {
                    0.0
                } else {
                    12.0 + isz + 6.0 + g.measure_chrome_text(&cwd_str, chrome_font, false)
                };
                let win_w = win_px.0 / scale;
                let right_lim = git_col_toggle.map_or(win_w - 8.0, |(x, ..)| x - 12.0);
                let left_lim = px0 + cwd_w;
                let start = ((win_w - pw) / 2.0).clamp(left_lim, (right_lim - pw).max(left_lim));
                if !title_text.is_empty() {
                    round_rect(g, start, py, pw, ph, theme::radius_md(), theme::surface());
                    let icon_name = sb_labels
                        .get(sb_active)
                        .map(|(n, _)| n.as_str())
                        .unwrap_or(title_text.as_str());
                    g.queue_icon(
                        tab_icon_glyph(icon_name),
                        start + pad,
                        py + (ph - gl) / 2.0,
                        gl,
                        theme::text_dim(),
                    );
                    let tx = start + pad + gl + 7.0;
                    g.draw_text(
                        tx,
                        ty,
                        &title_text,
                        gpu::DrawOpts {
                            font_size: chrome_font,
                            color: title_color,
                            bold: true,
                            italic: false,
                        },
                    );
                    if title_is_bg {
                        g.draw_text(
                            tx + tw,
                            ty,
                            BG_BADGE,
                            gpu::DrawOpts {
                                font_size: chrome_font,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                }
                if !cwd_str.is_empty() {
                    let isz = theme::ICON_SIZE;
                    let cx0 = px0 + 12.0;
                    g.queue_icon("folder", cx0, (TITLE_HEIGHT - isz) / 2.0, isz, theme::text_mute());
                    g.draw_text(
                        cx0 + isz + 6.0,
                        ty,
                        &cwd_str,
                        gpu::DrawOpts {
                            font_size: chrome_font,
                            color: theme::text_dim(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
            }
            // Shell picker popup painter — stacked under the "+" button
            // (sb_plus) in either tab mode, so the side strip and the top-tab
            // bar share one popup. Layout + hit rects were computed before the
            // GPU borrow so clicks land on the same boxes we paint.
            let paint_shell_menu = |g: &mut gpu::GpuRenderer| {
                if !menu_open || shell_menu_layout.is_empty() {
                    return;
                }
                let (px, py, _, ph) = sb_plus;
                let backdrop_h = shell_menu_layout.len() as f32 * SHELL_ITEM_H + 8.0;
                let backdrop_y = if self.tabs_on_top {
                    py + ph
                } else {
                    py - backdrop_h
                };
                round_rect(
                    g,
                    px - 4.0,
                    backdrop_y,
                    menu_w_for_paint + 8.0,
                    backdrop_h,
                    theme::radius_md(),
                    theme::surface_active(),
                );
                for (_, label, icon, (ix, iy, iw, ih)) in &shell_menu_layout {
                    let hov = sb_cursor.0 >= *ix
                        && sb_cursor.0 <= *ix + *iw
                        && sb_cursor.1 >= *iy
                        && sb_cursor.1 <= *iy + *ih;
                    if hov {
                        hover_rect(g, *ix, *iy, *iw, *ih, theme::radius_md());
                    }
                    g.queue_icon(
                        icon,
                        *ix + 12.0,
                        *iy + (*ih - theme::ICON_SIZE) / 2.0,
                        theme::ICON_SIZE,
                        theme::text_dim(),
                    );
                    g.draw_text(
                        *ix + 38.0,
                        *iy + (*ih - 14.0) / 2.0,
                        label,
                        gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: false, italic: false },
                    );
                }
            };
            // Horizontal window tabs in the title strip (Windows Terminal-
            // style). Same rects + per-window state as the side strip — only
            // the paint differs: compact one-line pills, active cue = box +
            // top accent stroke, status dots on the leading glyph.
            if self.tabs_on_top {
                for (i, (tx, ty, tw, th)) in &sb_tabs {
                    let is_active = *i == sb_active;
                    let is_hover = sb_hover == Some(*i);
                    // 이미 활성인 탭도 누를 수 있는 자리다 — 배경이 안 바뀌어도
                    // 커서는 손가락이어야 한다.
                    g.hover_pointer |= is_hover;
                    if is_active {
                        round_rect(g, *tx, *ty, *tw, *th, theme::radius_sm(), theme::surface_active());
                        g.rect(*tx + 5.0, *ty, *tw - 10.0, ACTIVE_ACCENT_STROKE, theme::accent());
                    } else if is_hover {
                        hover_rect(g, *tx, *ty, *tw, *th, theme::radius_sm());
                    } else {
                        // Faint resting fill so background tabs still read as
                        // tabs (Windows Terminal-style), not floating labels.
                        let resting = theme::lerp(theme::surface_hover(), theme::bg(), 0.55);
                        round_rect(g, *tx, *ty, *tw, *th, theme::radius_sm(), resting);
                    }
                    let (name, _cwd) = sb_labels
                        .get(*i)
                        .cloned()
                        .unwrap_or_else(|| (format!("win {}", i + 1), String::new()));
                    let isz = 14.0_f32;
                    let icon_x = *tx + 8.0;
                    let icon_y = *ty + (*th - isz) / 2.0;
                    g.queue_icon(
                        tab_icon_glyph(&name),
                        icon_x,
                        icon_y,
                        isz,
                        if is_active { theme::text_dim() } else { theme::text_mute() },
                    );
                    // 세로 사이드바와 같은 규칙 — 자리 고정, 색이 말하고, 깜빡인다.
                    // 여기도 모서리 두 곳을 오가던 점 쌍을 하나로 합쳤다.
                    if sb_wait.get(*i).copied().unwrap_or(false) {
                        blink_dot(g, icon_x + isz - 3.0, icon_y - 3.0, 6.0, theme::attention(), 0.9);
                    } else if sb_alert.get(*i).copied().unwrap_or(false) {
                        blink_dot(g, icon_x + isz - 3.0, icon_y - 3.0, 6.0, theme::accent(), 1.6);
                    }
                    let show_close = sb_tabs.len() > 1 && (is_active || is_hover);
                    let text_x = icon_x + isz + 6.0;
                    let avail = (*tx + *tw - text_x - if show_close { 24.0 } else { 8.0 }).max(0.0);
                    let budget = (avail / 7.8).floor().max(2.0) as usize;
                    g.draw_text(
                        text_x,
                        *ty + (*th - 12.5) / 2.0,
                        &clip_display_width(&name, budget),
                        gpu::DrawOpts {
                            font_size: 12.5,
                            color: if is_active { theme::text() } else { theme::text_dim() },
                            bold: is_active,
                            italic: false,
                        },
                    );
                    if show_close {
                        if let Some((_, (cx, cy, cw, ch))) = sb_closes.iter().find(|(ci, _)| ci == i) {
                            let x_hover = sb_cursor.0 >= *cx
                                && sb_cursor.0 <= *cx + *cw
                                && sb_cursor.1 >= *cy
                                && sb_cursor.1 <= *cy + *ch;
                            if x_hover {
                                hover_rect(g, *cx, *cy, *cw, *ch, theme::radius_sm());
                            }
                            let xcol = if x_hover { theme::text() } else { theme::text_mute() };
                            g.queue_icon(
                                "x",
                                *cx + (*cw - 12.0) / 2.0,
                                *cy + (*ch - 12.0) / 2.0,
                                12.0,
                                xcol,
                            );
                        }
                    }
                }
                // "+" new-tab button after the last tab.
                let (px, py, pw, ph) = sb_plus;
                let plus_hover = sb_cursor.0 >= px
                    && sb_cursor.0 <= px + pw
                    && sb_cursor.1 >= py
                    && sb_cursor.1 <= py + ph;
                if plus_hover {
                    hover_rect(g, px, py, pw, ph, theme::radius_sm());
                }
                g.queue_icon(
                    "plus",
                    px + (pw - theme::ICON_SIZE) / 2.0,
                    py + (ph - theme::ICON_SIZE) / 2.0,
                    theme::ICON_SIZE,
                    theme::text_mute(),
                );
                // Overflow chevrons in the strip's reserved 14px end slots —
                // more tabs exist past this edge, wheel over the strip scrolls.
                if let Some((_, (fx, fy, _, fh))) = sb_tabs.first() {
                    let cis = 12.0_f32;
                    let cy = fy + (fh - cis) / 2.0;
                    if sb_over_before {
                        g.queue_icon("chevron-left", fx - 14.0, cy, cis, theme::text_mute());
                    }
                    if sb_over_after {
                        g.queue_icon("chevron-right", px + pw + 3.0, cy, cis, theme::text_mute());
                    }
                }
                // 재배치 드래그 중이면 떨어질 자리에 세로 막대. 마지막 탭 뒤로
                // 미는 경우만 끝 모서리에 붙는다(target == 마지막 + 1).
                if let Some(t) = win_drag_target {
                    let bar_x = sb_tabs
                        .iter()
                        .find(|(i, _)| *i == t)
                        .map(|(_, r)| r.0 - 2.0)
                        .or_else(|| {
                            sb_tabs
                                .last()
                                .filter(|(i, _)| t == i + 1)
                                .map(|(_, r)| r.0 + r.2 + 2.0)
                        });
                    if let (Some(bx), Some((_, fr))) = (bar_x, sb_tabs.first()) {
                        g.rect(bx - 1.5, fr.1, 3.0, fr.3, theme::accent());
                    }
                }
            }
            // Window-tab sidebar, Warp-style. Painted first so per-pane
            // headers / rings layer on top at the seam.
            if tab_strip_w > 0.0 {
                // 칼럼 바닥과 오른쪽 실선은 위 크롬 판에서 한 번에 칠했다.
                let multi = sb_tabs.len() > 1;
                let mut dock_back_hits: Vec<(usize, (f32, f32, f32, f32))> = Vec::new();
                for (i, (tx, ty, tw, th)) in &sb_tabs {
                    let is_active = *i == sb_active;
                    let is_hover = sb_hover == Some(*i);
                    // Selected tab: subtle rounded highlight box (no left
                    // accent bar). Non-selected: flat, only a faint box on
                    // hover. Warp-style.
                    g.hover_pointer |= is_hover;
                    // 밖에 나간 방은 **자리만 남긴다**. 채운 카드로 두면 여기 있는
                    // 것처럼 읽혀 눌렀다가 아무 일도 안 일어나고, 목록에서 빼면
                    // 어디로 갔는지 알 길이 없다(거노: "그 자리 빵꾸나게"). 점선이
                    // 그 사이를 말한다 — 자리는 네 것이지만 지금 비어 있다.
                    let out = sb_undocked.get(*i).copied().unwrap_or(false);
                    if out {
                        dashed_rect(g, *tx, *ty, *tw, *th, theme::with_alpha(theme::border(), 0xC0));
                    } else if is_active {
                        // 고른 방은 **테두리로만** 말한다(2026-08-11 지시: "선택한 방
                        // 아웃라인으로 포커스 바꾸고"). 채운 판이었을 땐 그 밝은
                        // 바탕이 카드 전체를 덮어, 그 위에 얹히는 상태색(알림 점·
                        // 대기 주황)이 같은 밝기 대역에서 겨뤘다 — 정작 봐야 할
                        // 신호가 "고름"에 묻혔다. 테두리는 자리만 두르고 안을 비운다.
                        //
                        // 되메우는 색이 `panel_bg` 인 건 이 스트립의 바탕이 그것이기
                        // 때문이다(이 함수 위쪽에서 칼럼째 칠한다). 링만 그리는
                        // 스트로크가 렌더러에 없어 안쪽을 바탕색으로 덮는 방식이다.
                        outline_rect(
                            g, *tx, *ty, *tw, *th, theme::radius_md(),
                            theme::accent(), SIDEBAR_ACTIVE_RING, theme::panel_bg(),
                        );
                    } else if is_hover {
                        panel_rect(g, *tx, *ty, *tw, *th, theme::radius_md(), theme::surface_hover());
                    }
                    // 방과 방 사이 실선. 활성·호버 카드만 판을 깔기 때문에, 조용한
                    // 방끼리는 3px 틈만 있고 경계가 없었다 — 두 줄짜리 카드가 죽
                    // 이어지면 어디까지가 한 방인지 안 읽힌다(거노: "구분선이 하나도
                    // 없어"). 활성 카드는 스스로 판이라 그 위아래엔 긋지 않는다.
                    if !is_active && *i + 1 < sb_tabs.len() && *i + 1 != sb_active {
                        let ly = (ty + th + SIDEBAR_TAB_GAP / 2.0).round();
                        g.rect(tx + 10.0, ly, tw - 20.0, 1.0, theme::with_alpha(theme::border(), 0x60));
                    }
                    // Icon chip: small rounded square with a glyph.
                    let (name, cwd) = sb_labels
                        .get(*i)
                        .cloned()
                        .unwrap_or_else(|| (format!("win {}", i + 1), String::new()));
                    // 채운 원형 칩이었다가 윤곽 글리프만 남겼다. 목록에서 세션을
                    // 가르는 건 이름인데, 칩이 행마다 하나씩 박히면 같은 크기·같은
                    // 색의 원들이 먼저 읽혀 정작 이름이 뒤로 밀린다.
                    let icon = 22.0_f32;
                    let icon_x = *tx + 12.0;
                    // 카드 **머리** 기준으로 가운데다. 카드 높이(`th`)로 재면 방을
                    // 펼친 순간 카드가 pane 줄만큼 길어져, 칩이 이름 두 줄을 떠나
                    // 목록 한가운데로 흘러내렸다.
                    let icon_y = *ty + (SIDEBAR_TAB_H - icon) / 2.0;
                    let glyph = 17.0_f32;
                    g.queue_icon(
                        tab_icon_glyph(&name),
                        icon_x + (icon - glyph) / 2.0,
                        icon_y + (icon - glyph) / 2.0,
                        glyph,
                        if is_active { theme::text() } else { theme::text_dim() },
                    );
                    // 상태 동그라미 **하나**. 예전엔 칩의 두 모서리에 점이 따로
                    // 있었다 — 작업 중은 오른쪽 위, 방금 끝남은 오른쪽 아래. 상태가
                    // 바뀔 때마다 점이 두 모서리를 오갔고, 그 움직임이 정작 무엇이
                    // 달라졌는지보다 먼저 눈에 들어왔다(2026-08-11 지시: "점 두개
                    // 왔다갔다거리는거 없애자"). 이제 자리는 하나로 고정하고 **색이**
                    // 무엇인지 말한다.
                    //
                    // 그리는 조건도 갈렸다. "작업 중"은 여기서 뺐다 — 펼친 방의 줄이
                    // 학생을 걷게 해서 이미 말하고 있고(아래 pane 줄), 카드가 그걸 또
                    // 말하면 같은 정보가 두 층에 겹친다. 여기 남는 건 **내 손이
                    // 필요한 것**뿐이라 늘 깜빡인다.
                    let wait = sb_wait.get(*i).copied().unwrap_or(false);
                    let alert = sb_alert.get(*i).copied().unwrap_or(false);
                    // 줄이 이미 말하고 있으면 머리는 조용히 둔다 — 접힌 방은 줄이
                    // 없어 여기가 유일한 자리다.
                    let head_wait = wait && !sb_row_wait_win.contains(i);
                    let head_alert = alert && !sb_row_alert_win.contains(i);
                    if head_wait || head_alert {
                        // 대기가 이긴다: 끝나서 알리는 것과 막혀서 부르는 것은 급한
                        // 정도가 다르고, `handle_attention` 이 unread 에도 넣기 때문에
                        // 둘은 자주 같이 선다.
                        let (c, period) = if head_wait {
                            (theme::attention(), 0.9)
                        } else {
                            (theme::accent(), 1.6)
                        };
                        let dsz = 9.0_f32;
                        blink_dot(
                            g,
                            icon_x + icon - dsz + 3.0,
                            icon_y - 3.0,
                            dsz,
                            c,
                            period,
                        );
                    }
                    // Two-line label to the right of the icon.
                    let text_x = icon_x + icon + 10.0;
                    let name_fg: [u8; 4] = if is_active {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    // 경로는 방을 가르는 유일한 단서일 때가 많다(이름이 죄다
                    // 폴더명이라 같아진다). `text_mute` 는 "있지만 안 읽어도 되는
                    // 것"의 톤이라 여기선 너무 물러나 있었다 — 한 단 올린다.
                    let cwd_fg: [u8; 4] = theme::text_dim();
                    // 밖에 나간 방은 × 를 내주고 되돌리기 버튼이 그 자리를 쓴다.
                    // 둘은 카드 오른쪽 위 같은 칸이라 나눠 가질 수 없고, 안 보이는
                    // 창의 claude 를 여기서 죽일 일도 아니다(닫으려면 그 창에서).
                    // 예전엔 hover 하는 순간 되돌리기가 × 로 바뀌어, 누르러 가면
                    // 버튼이 사라졌다.
                    let undocked = sb_undocked.get(*i).copied().unwrap_or(false);
                    let show_close = multi && (is_active || is_hover) && !undocked;
                    // Budgets are measured against the tab's own right edge, not
                    // the sidebar width: the label starts at `text_x` (inset +
                    // ordinal gutter + chip), so a sidebar-width budget overshoots
                    // by the inset at both ends and ran the name under the ×.
                    // The name shares its row with the × and reserves that slot
                    // (close box is 14 wide, 3 from the edge, +6 breathing room);
                    // the cwd line sits below the × and gets the width back.
                    let tab_right = *tx + *tw;
                    // 창 번호는 곧 단축키다(Cmd+숫자, input.rs `win_digit`) — 맨
                    // 숫자를 왼쪽 여백에 두면 "몇 번째"까지만 말하지만, 이름 옆의
                    // `⌘1` 은 "이 키로 온다"까지 말한다. 9 까지만 매핑돼 있고, ×
                    // 가 뜨는 동안은 같은 자리라 물러난다.
                    // 밖에 나가 있는 방은 그 자리를 되돌리기 버튼이 쓴다 — 그 키는
                    // 방을 메인에 다시 그리는 게 아니라 별도 창을 앞으로 가져오므로
                    // (switch_window 라우팅), 키 힌트를 남기면 거짓말이 된다.
                    let kbd =
                        (!show_close && !undocked && *i < 9).then(|| format!("\u{2318}{}", *i + 1));
                    let kfs = 11.0_f32;
                    let kbd_w = kbd.as_deref().map_or(0.0, |k| g.measure_chrome_text(k, kfs, false));
                    const UNDOCK_ICON: f32 = 13.0;
                    let right_slot = if undocked { UNDOCK_ICON } else { kbd_w };
                    let name_budget = (tab_right
                        - if show_close { 23.0 } else { 8.0 + right_slot + 6.0 }
                        - text_x)
                        .max(0.0);
                    // 아랫줄 오른쪽은 이제 펼치기 배지 몫이다. 여기 있던 pane 별
                    // 상태 점은 뺐다 — 방 목록은 "어느 방으로 갈까"를 고르는 자리고,
                    // pane 하나하나의 상태는 방을 펴면 그 줄이 이미 말한다. 둘 다
                    // 두면 같은 정보가 두 층에 겹쳐 목록이 시끄러워진다(거노:
                    // "학생 목록 말고 윈도우 목록에선 없애").
                    let badge_w = sb_expand.get(*i).copied().flatten().map_or(0.0, |r| r.2 + 14.0);
                    let cwd_budget = (tab_right - 8.0 - badge_w - text_x).max(0.0);
                    // Clip before drawing — `draw_text` also borrows `g`.
                    let name_txt = clip_px(g, &name, 13.5, is_active, name_budget);
                    g.draw_text(
                        text_x,
                        *ty + 11.0,
                        &name_txt,
                        gpu::DrawOpts {
                            font_size: 13.5,
                            color: name_fg,
                            bold: is_active,
                            italic: false,
                        },
                    );
                    if let Some(k) = kbd {
                        g.draw_text(
                            tab_right - 8.0 - kbd_w,
                            *ty + 12.0,
                            &k,
                            gpu::DrawOpts { font_size: kfs, color: theme::text_mute(), bold: false, italic: false },
                        );
                    }
                    // 밖에 나간 방 자리엔 **되돌리는 버튼**을 둔다. 여기 있던
                    // external-link 는 점선 슬롯이 이미 하는 말("나가 있다")을 한 번
                    // 더 할 뿐이었고, 정작 다시 넣는 길은 별도 창을 찾아 닫는 것뿐
                    // 이었다. 나간 자리가 곧 돌아올 자리다.
                    if undocked {
                        let ix = tab_right - 8.0 - UNDOCK_ICON;
                        let iy = *ty + 9.0;
                        let hit = (ix - 6.0, iy - 5.0, UNDOCK_ICON + 12.0, UNDOCK_ICON + 10.0);
                        let hov = sb_cursor.0 >= hit.0
                            && sb_cursor.0 <= hit.0 + hit.2
                            && sb_cursor.1 >= hit.1
                            && sb_cursor.1 <= hit.1 + hit.3;
                        g.hover_pointer |= hov;
                        if hov {
                            round_rect(
                                g,
                                hit.0,
                                hit.1,
                                hit.2,
                                hit.3,
                                theme::radius_sm(),
                                theme::surface_hover(),
                            );
                        }
                        g.queue_icon(
                            "undo-2",
                            ix,
                            iy,
                            UNDOCK_ICON,
                            if hov { theme::accent() } else { theme::text_dim() },
                        );
                        dock_back_hits.push((*i, hit));
                    }
                    if !cwd.is_empty() {
                        let cwd_txt = clip_px(g, &cwd, 11.0, false, cwd_budget);
                        g.draw_text(
                            text_x,
                            *ty + 30.0,
                            &cwd_txt,
                            gpu::DrawOpts {
                                font_size: 11.0,
                                color: cwd_fg,
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    // 펼치기 배지 — 삼각형 + pane 개수. 방 전환의 유일한 예외라
                    // 평소에도 테두리를 둘러 "여긴 버튼"이라고 말해 둔다. 사각은
                    // 클릭 판정과 같은 것을 쓴다(`window_expand_rect`).
                    if let Some(er) = sb_expand.get(*i).copied().flatten() {
                        let hov = sb_cursor.0 >= er.0
                            && sb_cursor.0 <= er.0 + er.2
                            && sb_cursor.1 >= er.1
                            && sb_cursor.1 <= er.1 + er.3;
                        g.hover_pointer |= hov;
                        // 밝기는 **이 카드 위에서** 정해진다. 고정 톤을 쓰면 활성
                        // 카드(더 밝은 판) 위에서 들리기는커녕 되레 어두워졌다.
                        let base = if is_active {
                            theme::surface_active()
                        } else if is_hover {
                            theme::surface_hover()
                        } else {
                            theme::panel_bg()
                        };
                        round_rect(g, er.0 - 1.0, er.1 - 1.0, er.2 + 2.0, er.3 + 2.0,
                            theme::radius_sm(), theme::border());
                        round_rect(g, er.0, er.1, er.2, er.3, theme::radius_sm(),
                            theme::raised_on(base, hov));
                        let fg = if hov { theme::text() } else { theme::text_dim() };
                        // 삼각형은 목록이 절반 열렸을 때 넘어간다 — 누르자마자
                        // 바뀌면 아직 닫힌 목록 위에서 이미 열린 표시가 된다.
                        g.queue_icon(
                            if sb_expand_t.get(*i).copied().unwrap_or(0.0) >= 0.5 {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            },
                            er.0 + 7.0,
                            er.1 + 5.0,
                            10.0,
                            fg,
                        );
                        let n = sb_dots.get(*i).map_or(0, |v| v.len()).to_string();
                        g.draw_text(
                            er.0 + 21.0,
                            er.1 + 5.0,
                            &n,
                            gpu::DrawOpts { font_size: 11.0, color: fg, bold: false, italic: false },
                        );
                    }
                    // 뷰 전환 — 펼치기 배지와 같은 칩으로 그려 "여기도 버튼"이 한눈에
                    // 읽히게 한다. 아이콘은 **누르면 갈 곳**이다(지금 모드가 아니라):
                    // 지금 뭘 보고 있는지는 바로 아래 카드 본문이 이미 말하고 있어,
                    // 버튼까지 그걸 되풀이하면 정작 무엇이 일어날지는 아무도 안 말한다.
                    if let Some((vr, list_view)) = sb_view.get(*i).copied().flatten() {
                        let hov = sb_cursor.0 >= vr.0
                            && sb_cursor.0 <= vr.0 + vr.2
                            && sb_cursor.1 >= vr.1
                            && sb_cursor.1 <= vr.1 + vr.3;
                        g.hover_pointer |= hov;
                        let base = if is_active {
                            theme::surface_active()
                        } else if is_hover {
                            theme::surface_hover()
                        } else {
                            theme::panel_bg()
                        };
                        round_rect(g, vr.0 - 1.0, vr.1 - 1.0, vr.2 + 2.0, vr.3 + 2.0,
                            theme::radius_sm(), theme::border());
                        round_rect(g, vr.0, vr.1, vr.2, vr.3, theme::radius_sm(),
                            theme::raised_on(base, hov));
                        g.queue_icon(
                            if list_view { "columns-2" } else { "rows-2" },
                            vr.0 + (vr.2 - 12.0) / 2.0,
                            vr.1 + (vr.3 - 12.0) / 2.0,
                            12.0,
                            if hov { theme::text() } else { theme::text_dim() },
                        );
                    }
                    // × close — only on the active or hovered tab (where the
                    // cursor is), so the strip stays clean otherwise. Hit
                    // rects exist for every tab; you hover before you click.
                    if show_close {
                        if let Some((_, (cx, cy, cw, ch))) =
                            sb_closes.iter().find(|(ci, _)| ci == i)
                        {
                            // Hover chip behind the × — same lift the pane-header
                            // close gets, so the sidebar close reads as clickable.
                            let x_hover = sb_cursor.0 >= *cx
                                && sb_cursor.0 <= *cx + *cw
                                && sb_cursor.1 >= *cy
                                && sb_cursor.1 <= *cy + *ch;
                            if x_hover {
                                hover_rect(g, *cx, *cy, *cw, *ch, theme::radius_sm());
                            }
                            let xcol = if x_hover { theme::text() } else { theme::text_mute() };
                            g.queue_icon(
                                "x",
                                *cx + (*cw - theme::ICON_SIZE) / 2.0,
                                *cy + (*ch - theme::ICON_SIZE) / 2.0,
                                theme::ICON_SIZE,
                                xcol,
                            );
                        }
                    }
                }
                self.window_dock_rects = dock_back_hits;
                // 펼친 방의 pane 줄. 탭 카드가 그 자리를 이미 비워 뒀으므로(레이아웃이
                // 카드 높이에 목록만큼을 더해 준다) 여기서는 채우기만 한다.
                // 방 배치도 — 목록보다 **먼저** 그린다(행 hover 판이 위에 와야 한다).
                // 목록은 "누가 있나"만 말하고 어느 칸이 화면 어디인지는 못 말한다.
                for ((_, id, r), info) in sb_mini.iter().zip(sb_mini_info.iter()) {
                    let (mx, my, mw, mh) = *r;
                    let cur = sb_active_pane.as_deref() == Some(id.as_str());
                    let hov = sb_cursor.0 >= mx
                        && sb_cursor.0 <= mx + mw
                        && sb_cursor.1 >= my
                        && sb_cursor.1 <= my + mh;
                    g.hover_pointer |= hov;
                    // 손이 필요한 칸은 **칸째 숨쉰다**(2026-08-11 지시: "점 말고 칸이
                    // 빛나게"). 모서리 점으로도 말해 봤는데 6px 짜리가 얼굴 옆에 붙으니
                    // 칸이 작을수록 얼룩처럼 읽혔고, 좁은 칸에서는 아예 그려지지도
                    // 않았다(`mw > 16` 게이트). 칸 전체는 크기와 무관하게 보인다.
                    let signal = if info.waiting {
                        Some((theme::attention(), 0.9))
                    } else if info.alert {
                        Some((theme::accent(), 1.6))
                    } else {
                        None
                    };
                    round_rect(
                        g,
                        mx,
                        my,
                        mw,
                        mh,
                        2.0,
                        // 신호가 테두리를 가져간다 — 「내가 여기 있다」(cur)보다
                        // 「나를 기다린다」가 급한 소식이다.
                        if let Some((c, _)) = signal {
                            c
                        } else if cur {
                            theme::accent()
                        } else if hov {
                            theme::surface_hover()
                        } else {
                            theme::with_alpha(theme::border(), 0x66)
                        },
                    );
                    // 활성 칸은 **테두리로만** 표시한다. 통으로 칠하면 pane 이 하나인
                    // 방에서 카드 머리 아래가 통짜 accent 덩어리가 되어, 배치도가
                    // 아니라 잘못 칠해진 자리로 읽힌다(실측).
                    if (cur || signal.is_some()) && mw > 5.0 && mh > 5.0 {
                        round_rect(
                            g,
                            mx + 1.5,
                            my + 1.5,
                            mw - 3.0,
                            mh - 3.0,
                            1.5,
                            if cur { theme::surface_active() } else { theme::panel_bg() },
                        );
                    }
                    // 숨쉬는 건 안쪽 판이다. 테두리까지 같이 흐려지면 칸의 윤곽이
                    // 주기마다 사라져 배치도가 통째로 일렁인다.
                    if let Some((col, period)) = signal {
                        if mw > 5.0 && mh > 5.0 {
                            let mut c = col;
                            c[3] = (30.0 + 120.0 * blink(anim_phase_secs(), period)) as u8;
                            round_rect(g, mx + 1.5, my + 1.5, mw - 3.0, mh - 3.0, 1.5, c);
                        }
                    }
                    // 칸이 **누구 자리인지** 말한다. 목록을 걷어낸 이상 얼굴이 여기
                    // 없으면 사이드바 어디에도 학생이 없다(거노 2026-08-11: "미니맵은
                    // 학생뭔지 보여야해"). 도는 중이면 줄에서 그랬듯 걷는다.
                    let face = (mw.min(mh) - 8.0).clamp(10.0, 26.0);
                    let fx = mx + (mw - face) / 2.0;
                    let fy = my + (mh - face) / 2.0;
                    let walked = info.busy
                        && draw_student_walk(g, &info.who, fx - 2.0, fy - 2.0, face + 4.0, anim_phase_secs());
                    if !walked && !draw_student_face_anim(g, &info.who, fx, fy, face, anim_phase_secs()) {
                        // 학생이 없는 자리 — 빈 칸으로 두면 "여긴 뭐지"가 되므로
                        // 그 칸이 무엇인지 말해 둔다(웹=globe · 이미지 · md · 터미널).
                        let isz = face.min(16.0);
                        g.queue_icon(
                            info.icon,
                            mx + (mw - isz) / 2.0,
                            my + (mh - isz) / 2.0,
                            isz,
                            theme::text_dim(),
                        );
                    }
                }
                for (k, ((wi, _, r), info)) in
                    sb_rows.iter().zip(sb_row_info.iter()).enumerate()
                {
                    let (who, label, col, is_cur) =
                        (&info.who, &info.label, &info.color, info.is_cur);
                    let (rx, ry, rw, rh) = *r;
                    // 줄 사이 실선 — 같은 방 안의 칸막이라 방과 방을 가르는
                    // 카드 테두리보다 옅어야 한다. 첫 줄 위에는 안 긋는다(카드
                    // 머리와 목록은 이미 여백으로 갈려 있다).
                    if k > 0 && sb_rows.get(k - 1).map(|(pw, _, _)| pw == wi).unwrap_or(false) {
                        g.rect(rx + 6.0, ry, rw - 12.0, 1.0, theme::with_alpha(theme::border(), 0x50));
                    }
                    let row_hover = sb_cursor.0 >= rx
                        && sb_cursor.0 <= rx + rw
                        && sb_cursor.1 >= ry
                        && sb_cursor.1 <= ry + rh;
                    if row_hover {
                        round_rect(g, rx, ry, rw, rh, theme::radius_sm(), theme::surface_hover());
                    }
                    // 지금 보고 있는 pane 은 왼쪽 띠로 — 목록이 방을 넘나들어서
                    // 표시가 없으면 "내가 있는 곳"을 매번 번호로 대조하게 된다.
                    if is_cur {
                        g.rect(rx, ry + 3.0, 2.0, rh - 6.0, theme::accent());
                    }
                    // 도는 중이면 학생이 **걷는다**(2026-08-11 지시: "진행중인거 학생
                    // 워크로 나오게하고"). 얼굴 GIF 는 도는 동안에도 가만히 앉아 있어
                    // 줄만 봐서는 이 pane 이 일하는지 멈춰 있는지 알 수 없었고, 그걸
                    // 대신 말하던 게 카드의 작업 점이었다 — 걷게 하면 그 점이 필요
                    // 없어진다. 원본이 256 정사각(전신 + 여백)이라 상자도 정사각이다.
                    // 얼굴보다 4px 키우는 건 그 여백 때문 — 같은 크기로 두면 캐릭터가
                    // 얼굴 GIF 보다 작아 보여 걷기 시작할 때 줄이 움찔한다.
                    let face = rh - 6.0;
                    let walked = info.busy
                        && draw_student_walk(g, who, rx + 5.0, ry + 1.0, rh - 2.0, anim_phase_secs());
                    let has_face = walked
                        || draw_student_face_anim(
                            g, who, rx + 7.0, ry + 3.0, face, anim_phase_secs(),
                        );
                    if !has_face {
                        if info.icon != "terminal" {
                            // 웹·이미지·md pane 줄 — 상태 점 대신 종류 아이콘.
                            // 상태는 줄 끝 점이 이미 말한다.
                            g.queue_icon(
                                info.icon,
                                rx + 7.0,
                                ry + (rh - 12.0) / 2.0,
                                12.0,
                                theme::text_dim(),
                            );
                        } else {
                            circle_rect(g, rx + 9.0, ry + rh / 2.0 - 3.0, 6.0, *col);
                        }
                    }
                    let name_x = rx + 7.0 + face + 6.0;
                    let budget = (rx + rw - 14.0 - name_x).max(0.0);
                    let txt = clip_px(g, label, 11.0, false, budget);
                    g.draw_text(
                        name_x,
                        ry + (rh - 11.0) / 2.0,
                        &txt,
                        gpu::DrawOpts {
                            font_size: 11.0,
                            // 숨긴 줄은 한 단 더 낮춘다 — 목록에 남아 있되 「지금 화면에
                            // 있는 것」과 한눈에 갈려야 한다.
                            color: if info.stashed {
                                theme::text_mute()
                            } else if is_cur {
                                theme::text()
                            } else {
                                theme::text_dim()
                            },
                            bold: false,
                            italic: false,
                        },
                    );
                    // 줄 끝 상태 점. 손이 필요한 줄에서는 **이 점이 깜빡인다** —
                    // 예전엔 줄 전체를 숨쉬게 해서 알렸는데, 넓은 판이 은근히 밝아지는
                    // 신호는 22px 줄에서는 배경 얼룩처럼 읽혔고 두 줄이 동시에 서면
                    // 목록이 통째로 일렁였다(2026-08-11 지시: "숨쉬기말고 동그라미
                    // 깜빡이게"). 점은 이미 그 줄의 상태를 말하던 자리라, 새 표시를
                    // 더하는 대신 있던 것을 깜빡이게 하면 목록에 늘어나는 게 없다.
                    let dot_x = rx + rw - 6.0;
                    let dot_y = ry + rh / 2.0 - 3.0;
                    if info.stashed {
                        // 숨김 표시가 상태 점 자리를 대신 쓴다. 치워 둔 줄에 상태 점을
                        // 그대로 두면 화면에 있는 줄과 구분이 안 된다 — 그리고 어차피
                        // 그 상태(도는 중·기다림)는 화면에 없는 pane 의 것이라 지금
                        // 손댈 수 있는 신호가 아니다.
                        // ⚠️ 이름은 `icon_svg`(gpu.rs)에 **등록된 것**이어야 한다 — 없는
                        // 이름은 오류 없이 아무것도 안 그린다(실측: "disabled" 로 두어
                        // 표시가 통째로 사라졌고, 흐린 글자만 남아 원인이 안 보였다).
                        g.queue_icon("minus", dot_x - 1.5, dot_y - 1.5, 9.0, theme::text_mute());
                    } else if info.waiting {
                        blink_dot(g, dot_x, dot_y, 6.0, theme::attention(), 0.9);
                    } else if info.alert {
                        blink_dot(g, dot_x, dot_y, 6.0, theme::accent(), 1.6);
                    } else {
                        circle_rect(g, dot_x, dot_y, 6.0, *col);
                    }
                }
                // 끌고 있는 줄이 떨어질 자리 — 대상 줄의 위/아래 모서리에 긋는다.
                // 끌리는 줄 자신은 옅게 낮춰 "지금 손에 들려 있다"를 남긴다.
                if let Some((tid, before, src)) = sb_row_drop.as_ref() {
                    for (_, id, r) in sb_rows.iter() {
                        if id == src {
                            g.rect(r.0, r.1, r.2, r.3, theme::with_alpha(theme::bg(), 0x88));
                        }
                        if id == tid {
                            let ly = if *before { r.1 } else { r.1 + r.3 - 2.0 };
                            g.rect(r.0 + 4.0, ly, r.2 - 8.0, 2.0, theme::accent());
                        }
                    }
                }
                // "+" new-window button under the last tab: flat, faint box on
                // hover, centred glyph.
                let (px, py, pw, ph) = sb_plus;
                let plus_hover = sb_cursor.0 >= px
                    && sb_cursor.0 <= px + pw
                    && sb_cursor.1 >= py
                    && sb_cursor.1 <= py + ph;
                if plus_hover {
                    hover_rect(g, px, py, pw, ph, theme::radius_md());
                }
                g.queue_icon(
                    "plus",
                    px + (pw - theme::ICON_SIZE) / 2.0,
                    py + (ph - theme::ICON_SIZE) / 2.0,
                    theme::ICON_SIZE,
                    theme::text_mute(),
                );
                // Overflow chevrons: up in the slot above the first tab, down
                // under the "+" — more windows exist past that edge, wheel
                // over the strip scrolls the run.
                if let Some((_, (ftx, _, ftw, _))) = sb_tabs.first() {
                    let cis = 12.0_f32;
                    let ccx = ftx + (ftw - cis) / 2.0;
                    if sb_over_before {
                        g.queue_icon("chevron-up", ccx, TITLE_HEIGHT + 3.0, cis, theme::text_mute());
                    }
                    if sb_over_after {
                        g.queue_icon("chevron-down", ccx, py + ph + 4.0, cis, theme::text_mute());
                    }
                }
                // 재배치 드래그 중이면 떨어질 자리에 가로 막대. 마지막 탭 아래로
                // 미는 경우만 끝 모서리에 붙는다(target == 마지막 + 1).
                if let Some(t) = win_drag_target {
                    let bar_y = sb_tabs
                        .iter()
                        .find(|(i, _)| *i == t)
                        .map(|(_, r)| r.1 - SIDEBAR_TAB_GAP / 2.0)
                        .or_else(|| {
                            sb_tabs
                                .last()
                                .filter(|(i, _)| t == i + 1)
                                .map(|(_, r)| r.1 + r.3 + SIDEBAR_TAB_GAP / 2.0)
                        });
                    if let (Some(by), Some((_, fr))) = (bar_y, sb_tabs.first()) {
                        g.rect(fr.0, by - 1.5, fr.2, 3.0, theme::accent());
                    }
                }
                // ── 하단 트레이 ── 새 세션 · 피드백 · 설정. 목록과 얇은 선으로
                // 갈라 "목록의 마지막 항목"이 아니라 별도 층으로 읽히게 한다.
                // "+" 피커가 열려 있으면 스킵 — 팝업이 이 자리를 덮는데 아이콘
                // 글리프는 rect 위 레이어라 비쳐 올라온다(가려지는 chrome 은 안
                // 그린다는 관례).
                if !menu_open {
                    if let Some((line_y, _, fb, st)) = sidebar_tray {
                        g.rect(
                            SIDEBAR_TAB_INSET,
                            line_y,
                            (tab_strip_w - SIDEBAR_TAB_INSET * 2.0).max(0.0),
                            1.0,
                            theme::border(),
                        );
                        let settings_on = self.settings_open;
                        for (r, icon, on) in
                            [(fb, "message-square-warning", false), (st, "settings-2", settings_on)]
                        {
                            let (bx, by, bw, bh) = r;
                            let hover = sb_cursor.0 >= bx
                                && sb_cursor.0 <= bx + bw
                                && sb_cursor.1 >= by
                                && sb_cursor.1 <= by + bh;
                            g.hover_pointer |= hover;
                            if on {
                                round_rect(g, bx, by, bw, bh, theme::radius_sm(), theme::surface_active());
                            } else if hover {
                                hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                            }
                            g.queue_icon(
                                icon,
                                bx + (bw - theme::ICON_SIZE) / 2.0,
                                by + (bh - theme::ICON_SIZE) / 2.0,
                                theme::ICON_SIZE,
                                if hover || on { theme::text() } else { theme::text_mute() },
                            );
                        }
                    }
                }
                // pane 행 우클릭 메뉴 — 이 칼럼에서 **마지막**에 그린다(다른 것 위에
                // 떠야 한다). 골격은 파일트리·Info 메뉴와 같은 것을 쓴다.
                if let Some((mx0, my0, _, pane)) = self.sidebar_menu.clone() {
                    // 이미 숨긴 줄이면 되돌리기 한 갈래만 낸다 — 같은 자리에서 같은
                    // 동작을 토글로 부르는 편이 항목 두 개를 늘 보여주는 것보다 낫다.
                    let hidden =
                        self.closed_panes.iter().any(|c| c.stashed && c.alive && c.pane_id == pane);
                    let items: [(SidebarMenuAction, &str); 1] = if hidden {
                        [(SidebarMenuAction::Unhide, "다시 보이기")]
                    } else {
                        [(SidebarMenuAction::Hide, "pane 숨기기")]
                    };
                    const MIH: f32 = 28.0;
                    let widest = items
                        .iter()
                        .map(|(_, l)| g.measure_chrome_text(l, 13.0, false))
                        .fold(0.0f32, f32::max);
                    let mw = (widest + 32.0).min((tab_strip_w - 8.0).max(80.0));
                    let mh = 12.0 + items.len() as f32 * MIH;
                    // 사이드바 안에 가둔다 — 넘치면 오른쪽 파일트리 위로 삐져나간다.
                    // 시저로 자를 수도 있지만, 반쯤 잘린 메뉴는 읽을 수가 없다.
                    // 안 보이게 자르는 것보다 자리를 옮겨 다 보이는 편이 낫다.
                    let mx = mx0.min((tab_strip_w - mw - 4.0).max(4.0)).max(4.0);
                    let my = my0.min((sb_win_h - mh - 6.0).max(TITLE_HEIGHT)).max(TITLE_HEIGHT);
                    panel_rect_outlined(g, mx, my, mw, mh, theme::radius_md(), theme::surface());
                    self.sidebar_menu_rects.clear();
                    for (i, (a, label)) in items.iter().enumerate() {
                        let r = (mx + 4.0, my + 6.0 + i as f32 * MIH, mw - 8.0, MIH);
                        let hov = sb_cursor.0 >= r.0
                            && sb_cursor.0 <= r.0 + r.2
                            && sb_cursor.1 >= r.1
                            && sb_cursor.1 <= r.1 + r.3;
                        g.hover_pointer |= hov;
                        if hov {
                            hover_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm());
                        }
                        g.draw_text(
                            r.0 + 12.0,
                            r.1 + (MIH - 13.0) / 2.0,
                            label,
                            gpu::DrawOpts {
                                font_size: 13.0,
                                color: theme::text(),
                                bold: false,
                                italic: false,
                            },
                        );
                        self.sidebar_menu_rects.push((*a, r));
                    }
                }
            }
            // ── File-tree column ── independent of the tab strip, parked just
            // right of it (VSCode explorer). Root = active pane's cwd; folders
            // first — click a folder to expand, a file to preview. Rows laid
            // out + hit rects cached here (window-tab pattern); the read_dir
            // build lives in refresh_file_tree, never per-frame. (Settings is
            // its own window now, so it no longer masks this column.)
            if tree_col_w > 0.0 {
                let col_h = (sb_win_h - TITLE_HEIGHT).max(0.0);
                // Own background + right hairline so the column reads as a
                // distinct pane between the tabs and the cell grid.
                g.rect(tree_col_x, TITLE_HEIGHT, tree_col_w, col_h, theme::panel_bg());
                g.rect(
                    tree_col_x + tree_col_w - 1.0,
                    TITLE_HEIGHT,
                    1.0,
                    col_h,
                    theme::border(),
                );
                let inset = SIDEBAR_TAB_INSET;
                let item_h = 26.0_f32;
                let row_x = tree_col_x + inset;
                let row_w = (tree_col_w - inset * 2.0).max(0.0);
                // Search box pinned to the column top; the tree starts below it.
                let search_box_h = 28.0_f32;
                let sbx_y = TITLE_HEIGHT + 8.0;
                // Reserve room on the right for the new-folder / new-file
                // buttons; the search box takes what's left.
                let btn_sz = 24.0_f32;
                let btn_gap = 4.0_f32;
                let buttons_w = btn_sz * 2.0 + btn_gap;
                let search_w = (row_w - buttons_w - 6.0).max(40.0);
                {
                    let active = self.file_tree.search_active;
                    let fill = if active { theme::surface_active() } else { theme::surface() };
                    round_rect(g, row_x, sbx_y, search_w, search_box_h, theme::radius_sm(), theme::border());
                    round_rect(g, row_x + 1.0, sbx_y + 1.0, search_w - 2.0, search_box_h - 2.0, theme::radius_sm() - 1.0, fill);
                    let ic = if active { theme::text() } else { theme::text_dim() };
                    g.queue_icon("folder-tree", row_x + 8.0, sbx_y + (search_box_h - 14.0) / 2.0, 14.0, ic);
                    // 캐럿은 커서 자리다 — 늘 끝에 붙이면 가운데를 고치는 동안
                    // 화면이 거짓말을 한다. 커서 앞뒤로 갈라 「앞 → 조합 중 글자
                    // → 뒤」로 붙여 그리고, 캐럿은 그 앞부분 폭에 세운다.
                    let (head, tail) =
                        crate::lineedit::split(&self.file_tree.search_query, self.file_tree.search_cursor);
                    let mut head = head;
                    if active && self.in_preedit {
                        head.push_str(&self.preedit);
                    }
                    let caret_w = g.measure_chrome_text(&head, 13.0, false);
                    let shown = format!("{head}{tail}");
                    let (txt, col) = if shown.is_empty() {
                        ("검색…".to_string(), theme::text_mute())
                    } else {
                        (shown, theme::text())
                    };
                    g.draw_text(row_x + 30.0, sbx_y + (search_box_h - 13.0) / 2.0, &txt,
                        gpu::DrawOpts { font_size: 13.0, color: col, bold: false, italic: false });
                    // Blinking text caret when the box has focus.
                    if active && commit_caret_on {
                        g.rect(row_x + 30.0 + caret_w, sbx_y + (search_box_h - 14.0) / 2.0,
                            1.5, 14.0, theme::text());
                    }
                    self.file_tree.search_rect = (row_x, sbx_y, search_w, search_box_h);
                    // New-folder / new-file buttons.
                    let (mx, my) = self.cursor_px;
                    let bty = sbx_y + (search_box_h - btn_sz) / 2.0;
                    let nf_x = row_x + search_w + 6.0;
                    let nfile_x = nf_x + btn_sz + btn_gap;
                    for (bx, icon) in [(nf_x, "folder-plus"), (nfile_x, "file-plus")] {
                        let hover = mx >= bx && mx <= bx + btn_sz && my >= bty && my <= bty + btn_sz;
                        if hover {
                            hover_rect(g, bx, bty, btn_sz, btn_sz, theme::radius_sm());
                        }
                        let ic = if hover { theme::text() } else { theme::text_dim() };
                        g.queue_icon(icon, bx + (btn_sz - 15.0) / 2.0, bty + (btn_sz - 15.0) / 2.0, 15.0, ic);
                    }
                    self.file_tree.new_folder_rect = (nf_x, bty, btn_sz, btn_sz);
                    self.file_tree.new_file_rect = (nfile_x, bty, btn_sz, btn_sz);
                }
                // Inline "new file/folder" naming row, pinned above the tree.
                let mut tree_top = sbx_y + search_box_h + 8.0;
                if let Some((is_dir, buf)) = self.file_tree.new.clone() {
                    let iy = tree_top;
                    round_rect(g, row_x, iy, row_w, item_h, theme::radius_sm(), theme::surface_active());
                    g.rect(row_x, iy + 2.0, 2.0, item_h - 4.0, theme::accent());
                    g.queue_icon(if is_dir { "folder" } else { "file" }, row_x + 18.0, iy + (item_h - 16.0) / 2.0, 16.0, theme::text());
                    let (mut head, tail) = crate::lineedit::split(&buf, self.file_tree.edit_cursor);
                    if self.in_preedit {
                        head.push_str(&self.preedit);
                    }
                    let caret_w = g.measure_chrome_text(&head, 13.0, false);
                    let shown = format!("{head}{tail}");
                    let (txt, col) = if shown.is_empty() {
                        ((if is_dir { "폴더 이름…" } else { "파일 이름…" }).to_string(), theme::text_mute())
                    } else {
                        (shown, theme::text())
                    };
                    g.draw_text(row_x + 44.0, iy + (item_h - 13.0) / 2.0, &txt,
                        gpu::DrawOpts { font_size: 13.0, color: col, bold: false, italic: false });
                    if commit_caret_on {
                        g.rect(row_x + 44.0 + caret_w, iy + (item_h - 14.0) / 2.0, 1.5, 14.0, theme::text());
                    }
                    self.file_tree.new_row_rect = (row_x, iy, row_w, item_h);
                    tree_top += item_h;
                } else {
                    self.file_tree.new_row_rect = (0.0, 0.0, 0.0, 0.0);
                }
                // File the focused pane is currently showing — its row gets an
                // active tint + accent bar so the sidebar tracks the open file.
                // Inlined (not the `active_preview_path` helper) so it borrows
                // only `self.ws`, disjoint from the `g` mutable borrow alive here.
                let active_file: Option<std::path::PathBuf> = self.ws.lock().ok().and_then(|ws| {
                    ws.active_pane
                        .as_ref()
                        .and_then(|id| ws.panes.get(id).and_then(|p| p.preview_path.clone()))
                });
                // ── 빠른 파일 고정 섹션 ── 여기선 높이만 잡아 start_y 를 확정한다.
                // 그리기는 아래 원래 자리(트리 본문 앞)에서 한다 — 시저가 생기기
                // 전에는 스크롤로 start_y 위까지 올라온 트리 항목을 이 섹션의 불투명
                // 배경으로 **나중에 덮어야** 했고, 그 때문에 그리기만 뒤로 밀려 있었다.
                let quick = &quick_files_list;
                let quick_top = tree_top;
                let quick_h = if quick.is_empty() {
                    0.0
                } else {
                    // 헤더(19) + 항목들(item_h*n) + 구분선(4+7)
                    19.0 + quick.len() as f32 * item_h + 11.0
                };
                tree_top += quick_h;
                let start_y = tree_top;
                // 본문 geometry 를 스크롤 처리에 넘겨주기 위해 저장: start_y 는 검색박스
                // + 빠른파일 섹션(항목 수만큼 동적) 아래 첫 행 y, visible_h 는 dock 을
                // 뺀 창 끝까지. input.rs 가 이걸로 max_scroll 을 정확히 clamp 한다.
                let bottom_h = if self.docked.is_empty() && self.zoomed_pane.is_none() {
                    0.0
                } else {
                    DOCK_HEIGHT
                } + status_h;
                let body_visible_h = (sb_win_h - bottom_h - start_y).max(0.0);
                self.file_tree.body_rect = (row_x, start_y, row_w, body_visible_h);
                let win_h = win_px.1 / scale;
                let step = 14.0_f32; // per-depth indent width
                let mut rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))> = Vec::new();
                // `file_tree_nodes` already holds the right set: a query swaps it
                // for whole-tree search hits (file_tree_search_collect), empty
                // restores the expanded tree. So just render it as-is.
                let vis_nodes: Vec<&FileNode> = self.file_tree.nodes.iter().collect();
                // ── 빠른 파일 고정 섹션 ── 트리 본문 **앞**에 그린다. 원래 자리다.
                //
                // 한동안 트리 뒤로 미뤄 뒀었는데, 이유는 레이아웃이 아니라 클리핑이
                // 없어서였다: 스크롤로 `start_y` 위까지 올라온 트리 항목을 막을 길이
                // 이 섹션의 불투명 배경으로 덮는 것뿐이었다. 이제 시저가 그 위를
                // 자르므로 덮을 것이 없고, 덮기를 위해 순서를 뒤집어 둘 이유도 없다.
                //
                // 순서를 되돌리는 편이 나은 이유: 「나중에 덮는다」는 배경이 불투명할
                // 때만 성립하는 약속이라, 이 섹션에 반투명 배경이나 둥근 모서리가
                // 붙는 순간 조용히 깨진다. 그리는 차례가 곧 z-order 인 편이 읽기도 쉽다.
                self.file_tree.quick_rects.clear();
                if !quick.is_empty() {
                    g.rect(tree_col_x, quick_top, tree_col_w - 1.0, quick_h, theme::panel_bg());
                    let mut qy = quick_top;
                    g.draw_text(
                        row_x + 6.0,
                        qy + 3.0,
                        "빠른 파일",
                        gpu::DrawOpts { font_size: 10.5, color: theme::text_mute(), bold: false, italic: false },
                    );
                    qy += 19.0;
                    let (qmx, qmy) = self.cursor_px;
                    for (label, path, icon) in quick {
                        let y = qy;
                        let hovered = qmx >= row_x && qmx <= row_x + row_w && qmy >= y && qmy <= y + item_h;
                        let is_open = active_file.as_deref() == Some(path.as_path());
                        if hovered {
                            hover_rect(g, row_x, y, row_w, item_h, theme::radius_sm());
                        } else if is_open {
                            round_rect(g, row_x, y, row_w, item_h, theme::radius_sm(), theme::surface_active());
                        }
                        if is_open {
                            g.rect(row_x, y + 2.0, 2.0, item_h - 4.0, theme::accent());
                        }
                        let isz = 16.0_f32;
                        let iy = y + (item_h - isz) / 2.0;
                        let icon_x = row_x + 18.0;
                        let col = if hovered || is_open { theme::text() } else { theme::text_dim() };
                        g.queue_icon(icon, icon_x, iy, isz, col);
                        g.draw_text(
                            icon_x + isz + 8.0,
                            y + (item_h - 13.0) / 2.0,
                            label,
                            gpu::DrawOpts { font_size: 13.0, color: col, bold: false, italic: false },
                        );
                        self.file_tree.quick_rects.push((path.clone(), (row_x, y, row_w, item_h)));
                        qy += item_h;
                    }
                    // 구분선 — 빠른 파일과 트리 본문 사이 하이라인.
                    qy += 4.0;
                    g.rect(row_x, qy, row_w, 1.0, theme::with_alpha(theme::border(), 0x88));
                }
                // git 표시는 **배지 폴러**가 채운 맵을 읽는다(git 컬럼 폴러가 아니라)
                // — 컬럼 폴러는 그 패널이 열렸을 때만 돌아서, 파일트리 표시가 남의
                // 패널 개폐에 묶여 버린다. 배지는 모든 pane 의 cwd 로 항상 돈다.
                // 루프 **밖에서 한 번만** 잠근다: 행마다 잠그면 폴러와 프레임당
                // 수십 번 부딪히고, 복사하면 프레임마다 맵을 통째로 clone 한다.
                let ft_cwd = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.active_pane.clone())
                    .and_then(|id| self.pane_cwd_cache.get(&id).cloned());
                let git_badges = self.window_git.lock().ok();
                let git_marks = ft_cwd
                    .as_ref()
                    .zip(git_badges.as_ref())
                    .and_then(|(p, m)| m.get(p))
                    .map(|b| &b.marks);
                // 트리 본문을 시저로 가둔다. 아래쪽 경계는 지금까지 컬링이 쓰던
                // `win_h` 그대로다 — 여기서 칼럼 끝(`view_bottom`)으로 좁히면 그 아래
                // 그려지던 행이 사라져 다른 변경이 섞인다. 위쪽만 진짜로 자른다.
                g.push_clip(tree_col_x, start_y, tree_col_w, (win_h - start_y).max(0.0));
                // 커서가 잘려 안 보이는 쪽에 있는데 행의 보이는 부분에 hover 배경이
                // 그려지는 것은 시저가 못 막는다 — hover 판정은 `file_tree.hover`
                // (마우스 이동 때 히트렉트로 정해진다)라 히트렉트를 자르면 함께 막힌다.
                for (idx, node) in vis_nodes.iter().enumerate() {
                    let node = *node;
                    let y = start_y - self.file_tree.scroll + idx as f32 * item_h;
                    // 완전히 밖인 항목만 건너뛴다. 위로 반쯤 걸친 항목은 **그리고**
                    // 시저가 자른다 — 예전엔 여기서 통째로 스킵했고, 그래야 했던 이유가
                    // "덮어 줄 배경이 나중에 온다"였다. 잘라 낼 수 있게 된 지금은 반쯤
                    // 걸친 행이 반쯤 보이는 것이 맞다.
                    if !g.clip_visible(row_x, y, row_w, item_h) {
                        continue;
                    }
                    let hovered =
                        self.file_tree.hover.as_deref() == Some(node.path.as_path());
                    let expanded =
                        node.is_dir && self.file_tree.expanded.contains(&node.path);
                    let is_open = active_file.as_deref() == Some(node.path.as_path());
                    let is_selected = self.file_tree.selected.as_deref()
                        == Some(node.path.as_path())
                        || self.file_tree.selected_more.contains(&node.path);
                    // Row background: hover wins; the open file / Cmd+Delete
                    // selection keeps a solid active tint + accent bar; an open
                    // folder keeps a faint tint so the branch reads as a group.
                    if hovered {
                        hover_rect(g, row_x, y, row_w, item_h, theme::radius_sm());
                    } else if is_open || is_selected {
                        round_rect(g, row_x, y, row_w, item_h, theme::radius_sm(), theme::surface_active());
                    } else if expanded {
                        round_rect(g, row_x, y, row_w, item_h, theme::radius_sm(), theme::with_alpha(theme::surface_hover(), 0x33));
                    }
                    if is_open || is_selected {
                        // Accent rail on the left edge — VSCode "active file" cue.
                        g.rect(row_x, y + 2.0, 2.0, item_h - 4.0, theme::accent());
                    }
                    // Indent guides — one faint rule per ancestor level so deep
                    // nesting stays legible.
                    for d in 0..node.depth {
                        let gx = row_x + 6.0 + d as f32 * step;
                        g.rect(gx, y, 1.0, item_h, theme::with_alpha(theme::border(), 0x55));
                    }
                    let base_x = row_x + node.depth as f32 * step;
                    let isz = 16.0_f32;
                    let iy = y + (item_h - isz) / 2.0;
                    let font = 13.0_f32;
                    // Chevron column (folders only); files align past it.
                    if node.is_dir {
                        let chev = if expanded { "chevron-down" } else { "chevron-right" };
                        let cc = if hovered { theme::text() } else { theme::text_mute() };
                        g.queue_icon(chev, base_x + 2.0, y + (item_h - 12.0) / 2.0, 12.0, cc);
                    }
                    let icon_x = base_x + 18.0;
                    // Folders keep the single-color outline glyph (row-state
                    // tint); files get the branded file-type icon (ft/*, full
                    // color via FLAG_COLOR) with alpha carrying the ignored /
                    // idle / hover states instead of a tint. Unknown types fall
                    // back to the monochrome "file" glyph.
                    let icon_color = if node.ignored {
                        theme::with_alpha(theme::text_dim(), 0x99)
                    } else if hovered || is_open {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    if node.is_dir {
                        // 레포는 폴더 대신 브랜치 아이콘 — 펼침 화살표가 이미
                        // 폴더성을 말해 주므로 정보가 줄지 않고, 목록에서 어느
                        // 게 레포인지 한눈에 갈린다(거노).
                        let ic = if node.is_repo { "git-branch" } else { "folder" };
                        g.queue_icon(ic, icon_x, iy, isz, icon_color);
                    } else if let Some(ft) = file_icon(&node.name) {
                        let alpha = if node.ignored {
                            0.35
                        } else if hovered || is_open {
                            1.0
                        } else {
                            0.85
                        };
                        g.queue_icon_colored(ft, icon_x, iy, isz, alpha);
                    } else {
                        g.queue_icon("file", icon_x, iy, isz, icon_color);
                    }
                    // Folders read brighter than files (soft hierarchy); ignored
                    // rows are muted; hover/open lift to full strength.
                    // git 마커가 있으면 이름 색이 그걸 따른다 — 배지만으론 좁은
                    // 사이드바에서 눈에 안 들어온다(VSCode 도 이름을 물들인다).
                    // ignored 행은 제외: gitignore 된 것은 애초에 status 에 안 나오고,
                    // 나온다 해도 흐리게 두는 게 이 행의 뜻이다.
                    let git_mark = (!node.ignored)
                        .then(|| git_marks.and_then(|m| m.get(&node.path).copied()))
                        .flatten();
                    let mark_color = |m: char| match m {
                        'M' => theme::syn_type(),
                        'A' | 'U' => theme::success(),
                        'D' => theme::danger(),
                        _ => theme::text_dim(),
                    };
                    let fg = if let Some(m) = git_mark {
                        mark_color(m)
                    } else if node.ignored {
                        theme::text_mute()
                    } else if hovered || is_open || node.is_dir {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    let text_x = icon_x + isz + 8.0;
                    // Clip the name to the column width with an ellipsis — long
                    // hashed file names (webp/jpg) otherwise overflow the sidebar
                    // straight into the terminal grid.
                    // 배지 자리를 먼저 떼어 둔다 — 안 그러면 긴 이름이 배지 밑을
                    // 파고들어 글자와 마커가 겹친다.
                    let badge_w = if git_mark.is_some() { 13.0 } else { 0.0 };
                    let avail = (row_x + row_w - text_x - 4.0 - badge_w).max(0.0);
                    let label = if g.measure_chrome_text(&node.name, font, false) <= avail {
                        node.name.clone()
                    } else {
                        let mut s = String::new();
                        for ch in node.name.chars() {
                            let mut trial = s.clone();
                            trial.push(ch);
                            trial.push('…');
                            if g.measure_chrome_text(&trial, font, false) > avail {
                                break;
                            }
                            s.push(ch);
                        }
                        s.push('…');
                        s
                    };
                    // Inline rename: this row's name turns into an edit box with
                    // a caret instead of the static label (same input path as the
                    // new-file/folder row).
                    let editing = self
                        .file_tree
                        .rename
                        .as_ref()
                        .filter(|(p, _)| p == &node.path)
                        .map(|(_, n)| n.clone());
                    if let Some(name) = editing {
                        let (mut head, tail) =
                            crate::lineedit::split(&name, self.file_tree.edit_cursor);
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_w = g.measure_chrome_text(&head, font, false);
                        let shown = format!("{head}{tail}");
                        let (txt, tcol) = if shown.is_empty() {
                            ("이름…".to_string(), theme::text_mute())
                        } else {
                            (shown, theme::text())
                        };
                        g.draw_text(
                            text_x,
                            y + (item_h - font) / 2.0,
                            &txt,
                            gpu::DrawOpts { font_size: font, color: tcol, bold: false, italic: false },
                        );
                        if commit_caret_on {
                            g.rect(text_x + caret_w, y + (item_h - 14.0) / 2.0, 1.5, 14.0, theme::text());
                        }
                        self.file_tree.rename_row_rect = (row_x, y, row_w, item_h);
                    } else {
                        g.draw_text(
                            text_x,
                            y + (item_h - font) / 2.0,
                            &label,
                            gpu::DrawOpts { font_size: font, color: fg, bold: false, italic: node.ignored },
                        );
                        // 행 오른쪽 끝 상태 배지. 파일은 글자(M/A/U)로 무엇이
                        // 바뀌었는지까지 말하고, 폴더는 점 하나 — 폴더의 글자는
                        // 자손 여럿을 하나로 뭉친 것이라 "M" 이라 쓰면 그 폴더가
                        // 수정됐다는 오해를 준다. 점은 "안에 뭔가 있다"만 말한다.
                        if let Some(m) = git_mark {
                            let c = mark_color(m);
                            if node.is_dir {
                                let d = 6.0_f32;
                                round_rect(
                                    g,
                                    row_x + row_w - 4.0 - d,
                                    y + (item_h - d) / 2.0,
                                    d,
                                    d,
                                    d / 2.0,
                                    c,
                                );
                            } else {
                                let bf = 10.0_f32;
                                let bw = g.measure_chrome_text(&m.to_string(), bf, true);
                                g.draw_text(
                                    row_x + row_w - 4.0 - bw,
                                    y + (item_h - bf) / 2.0,
                                    &m.to_string(),
                                    gpu::DrawOpts { font_size: bf, color: c, bold: true, italic: false },
                                );
                            }
                        }
                    }
                    // 눌리는 자리는 클립과의 교집합이다. 시저는 픽셀만 자르지 클릭은
                    // 안 자르므로, 원본을 담으면 「빠른 파일」 뒤로 스크롤된 행이
                    // 그대로 눌린다 — 예전 통째 스킵이 막아 주던 것이 바로 이것이고,
                    // 스킵을 지웠으니 여기서 대신 막아야 한다.
                    if let Some(hr) = g.clip_hit((row_x, y, row_w, item_h)) {
                        rects.push((node.path.clone(), hr));
                    }
                }
                g.pop_clip();
                self.file_tree.rects = rects;

                // Overflow affordances: a soft fade at whichever edge still has
                // hidden rows, plus a hover-only scrollbar thumb. The viewport
                // runs from the first row (`start_y`, already below the search
                // box / inline new-row) to the column bottom, so the fade never
                // eats the chrome above it.
                let view_top = start_y;
                let view_bottom = TITLE_HEIGHT + col_h;
                let viewport_h = (view_bottom - view_top).max(0.0);
                let content_h = self.file_tree.nodes.len() as f32 * item_h;
                if content_h > viewport_h + 0.5 {
                    let overflow = content_h - viewport_h;
                    let scroll = self.file_tree.scroll;
                    let fade_h = 28.0_f32;
                    let strips = 16;
                    let strip_h = fade_h / strips as f32 + 0.5;
                    // Top fade ramps in over the first `fade_h` of scroll so it
                    // appears gently instead of snapping on at the first pixel.
                    if scroll > 0.5 {
                        let k = (scroll / fade_h).min(1.0);
                        for i in 0..strips {
                            let t = i as f32 / (strips - 1) as f32; // 0 top → 1 bottom of band
                            let a = ((1.0 - t) * 0.92 * k * 255.0) as u8;
                            g.rect(tree_col_x, view_top + t * fade_h, tree_col_w - 1.0, strip_h, theme::with_alpha(theme::bg(), a));
                        }
                    }
                    // Bottom fade — rows still hidden below the last visible line.
                    if scroll < overflow - 0.5 {
                        let k = ((overflow - scroll) / fade_h).min(1.0);
                        for i in 0..strips {
                            let t = i as f32 / (strips - 1) as f32; // 0 top → 1 bottom of band
                            let a = (t * 0.92 * k * 255.0) as u8;
                            g.rect(tree_col_x, view_bottom - fade_h + t * fade_h, tree_col_w - 1.0, strip_h, theme::with_alpha(theme::bg(), a));
                        }
                    }
                    // Scrollbar thumb — only while the cursor hovers the column,
                    // so the chrome stays clean when you're reading, not scrolling.
                    let (mx, my) = self.cursor_px;
                    let over_col = mx >= tree_col_x
                        && mx < tree_col_x + tree_col_w
                        && my >= view_top
                        && my < view_bottom;
                    if over_col {
                        let thumb_h = (viewport_h * viewport_h / content_h).max(28.0);
                        let thumb_y =
                            view_top + (viewport_h - thumb_h) * (scroll / overflow).clamp(0.0, 1.0);
                        pill_rect(g, tree_col_x + tree_col_w - 6.0, thumb_y, 3.5, thumb_h, theme::with_alpha(theme::text(), 0x66));
                    }
                }
                // Right-click context menu — painted last in the column so it
                // overlays the rows. Items + hit rects build straight into
                // ctx_menu_rects (g borrows only self.gpu, disjoint from these).
                self.file_tree.ctx_menu_rects.clear();
                if let Some((rawx, rawy)) = self.file_tree.ctx_menu {
                    let sel_n = self.file_tree.selected_more.len()
                        + self.file_tree.selected.is_some() as usize;
                    let del_label = if sel_n > 1 {
                        format!("{sel_n}개 삭제")
                    } else {
                        "휴지통으로 삭제".to_string()
                    };
                    #[cfg(target_os = "macos")]
                    let reveal_label = "Finder에서 보기";
                    #[cfg(not(target_os = "macos"))]
                    let reveal_label = "탐색기에서 보기";
                    // (action, label, danger, separator-before). "…에서 열기"는
                    // 설치된 앱 수만큼 늘어나므로 배열이 아니라 Vec 이다.
                    // "…로/으로" 대신 "…에서"로 통일한 건 조사 때문 — 영문
                    // 앱 이름은 받침 판정이 안 서고, "Finder에서 보기"와도
                    // 어울린다.
                    let mut items: Vec<(crate::FtMenuAction, String, bool, bool)> = Vec::new();
                    for (i, (name, _)) in crate::proc::open_with_apps().iter().enumerate() {
                        items.push((
                            crate::FtMenuAction::OpenWith(i),
                            format!("{name}에서 열기"),
                            false,
                            false,
                        ));
                    }
                    items.push((
                        crate::FtMenuAction::OpenDefault,
                        "기본 앱으로 열기".to_string(),
                        false,
                        false,
                    ));
                    let first_sep = !items.is_empty();
                    items.extend([
                        (crate::FtMenuAction::NewFile, "새 파일".to_string(), false, first_sep),
                        (crate::FtMenuAction::NewFolder, "새 폴더".to_string(), false, false),
                        (crate::FtMenuAction::Rename, "이름 변경".to_string(), false, true),
                        (crate::FtMenuAction::CopyPath, "경로 복사".to_string(), false, false),
                        (crate::FtMenuAction::Reveal, reveal_label.to_string(), false, false),
                        (crate::FtMenuAction::Delete, String::new(), true, true),
                    ]);
                    let mih = 28.0_f32;
                    let sep = 7.0_f32;
                    let pad = 6.0_f32;
                    // 폭은 가장 긴 항목에 맞춘다. 고정 200px 이던 시절엔 앱
                    // 이름이 긴 항목("Antigravity에서 열기")이 오른쪽 테두리를
                    // 넘어 잘렸다 — 항목이 기기마다 다르니 폭도 그래야 한다.
                    let widest = items
                        .iter()
                        .map(|(action, label, _, _)| {
                            let s = if matches!(action, crate::FtMenuAction::Delete) {
                                del_label.as_str()
                            } else {
                                label.as_str()
                            };
                            g.measure_chrome_text(s, 13.0, false)
                        })
                        .fold(0.0_f32, f32::max);
                    let menu_w = (widest + 32.0).max(200.0);
                    let nsep = items.iter().filter(|(_, _, _, s)| *s).count() as f32;
                    let menu_h = pad * 2.0 + items.len() as f32 * mih + nsep * sep;
                    let win_w = win_px.0 / scale;
                    let mx = rawx.min(win_w - menu_w - 6.0).max(tree_col_x + 2.0);
                    let my = rawy.min(win_h - menu_h - 6.0).max(TITLE_HEIGHT + 2.0);
                    panel_rect_outlined(g, mx, my, menu_w, menu_h, theme::radius_md(), theme::surface());
                    let (curx, cury) = self.cursor_px;
                    let mut iy = my + pad;
                    for (action, label, danger, sep_before) in items {
                        if sep_before {
                            g.rect(mx + pad, iy + sep * 0.5, menu_w - pad * 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));
                            iy += sep;
                        }
                        let r = (mx + 4.0, iy, menu_w - 8.0, mih);
                        let hov = curx >= r.0 && curx <= r.0 + r.2 && cury >= r.1 && cury <= r.1 + r.3;
                        if hov {
                            hover_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm());
                        }
                        let lbl = if matches!(action, crate::FtMenuAction::Delete) {
                            del_label.as_str()
                        } else {
                            label.as_str()
                        };
                        let color = if danger { theme::danger() } else { theme::text() };
                        g.draw_text(
                            r.0 + 12.0,
                            r.1 + (mih - 13.0) / 2.0,
                            lbl,
                            gpu::DrawOpts { font_size: 13.0, color, bold: false, italic: false },
                        );
                        self.file_tree.ctx_menu_rects.push((action, r));
                        iy += mih;
                    }
                }
            }
            // "+" 피커 팝업 — 사이드바 Settings 행·파일트리 위로 뜨는 오버레이라 그 뒤에
            // 한 번만 그린다(먼저 그리면 나중에 그린 chrome 텍스트가 팝업 위로 비친다).
            paint_shell_menu(g);
            // ── Git column ── right-hand chrome mirroring the file-tree column
            // on the left, but native instead of the old floating webview: the
            // poller fills `git_view` off-thread and this paints branch +
            // change list + Commit/Push, caching file-row / button hit rects
            // for the mouse handler. window_cells already reserved its width so
            // no pane overlaps it; it stops above the dock so the dock bar and
            // the action buttons never fight for the same strip.
            self.git.col_file_rects.clear();
            self.git.col_btn_rects.clear();
            self.git.path_hdr_rect = None;
            self.git.branch_hdr_rect = None;
            self.git.path_menu_rects.clear();
            self.git.branch_menu_rects.clear();
            // 계정 칩 rect 는 Info 탭 블록에서만 채워진다 — 여기서 매 프레임 비우지
            // 않으면 다른 탭으로 옮기거나 칼럼을 닫아도 옛 좌표가 남아, 그 자리에
            // 무엇이 놓이든 클릭이 계정 드롭다운에 먼저 먹힌다(2026-08-11 지적:
            // 세션 탭의 「전체」칩이 안 눌리고 계정 메뉴가 열렸다).
            self.account_chip_rect = None;
            if git_col_w > 0.0 && self.info.tab == state::SideTab::Git {
                // 상태줄은 늘 있으므로 dock 과 달리 조건 없이 함께 뺀다 — 안 빼면
                // 칼럼 바닥(= 최근 커밋 목록의 마지막 줄)이 그 띠 뒤로 들어가 가려진다.
                // 높이는 이제 설정에서 바뀌므로 상수가 아니라 `status_h` 를 쓴다.
                let bottom_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT } + status_h;
                let gcx0 = git_col_x + 14.0;
                let gcw = (git_col_w - 28.0).max(0.0);
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - bottom_h).max(top);
                // Background + left hairline so the column reads as its own pane.
                g.rect(git_col_x, top, git_col_w, bottom - top, theme::panel_bg());
                g.rect(git_col_x, top, 1.0, bottom - top, theme::border());
                // ── Row 0: Git | Info 탭 + ⤢ ✕ (두 탭 공통 머리)
                let mut y = info::draw_side_tabs(
                    g,
                    self.cursor_px,
                    &mut self.info,
                    &mut self.git,
                    git_col_x,
                    git_col_w,
                    top,
                );
                // ── Row 1: ~path : branch  ····  N · +ins -del
                // Path click → repo picker, branch click → switcher (rects below).
                {
                    let path_disp = git_view
                        .cwd
                        .as_ref()
                        .map(|p| crate::session::tilde_home(&p.to_string_lossy()))
                        .unwrap_or_else(|| "—".to_string());
                    let pcol = if self.git.col_pinned_cwd.is_some() { theme::accent() } else { theme::text_dim() };
                    let px = g.draw_text(gcx0, y, &path_disp, gpu::DrawOpts { font_size: 12.0, color: pcol, bold: false, italic: false });
                    self.git.path_hdr_rect = Some((gcx0 - 3.0, y - 3.0, (px - gcx0) + 6.0, 19.0));
                    if !git_view.no_repo {
                        let branch = if git_view.branch.is_empty() { "—" } else { git_view.branch.as_str() };
                        let cx2 = g.draw_text(px, y, " : ", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                        let bend = g.draw_text(cx2, y, branch, gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false });
                        self.git.branch_hdr_rect = Some((cx2 - 3.0, y - 3.0, (bend - cx2) + 6.0, 19.0));
                        // ahead/behind counts vs origin, as plain text right after
                        // the branch (↑ unpushed, ↓ unpulled). Push/pull actions
                        // live in the Commit split-button dropdown, not here.
                        let mut hx = bend + 10.0;
                        if git_view.ahead > 0 {
                            hx = g.draw_text(hx, y, &format!("↑{}", git_view.ahead),
                                gpu::DrawOpts { font_size: 12.0, color: theme::accent(), bold: false, italic: false }) + 8.0;
                        }
                        if git_view.behind > 0 {
                            g.draw_text(hx, y, &format!("↓{}", git_view.behind),
                                gpu::DrawOpts { font_size: 12.0, color: theme::text_dim(), bold: false, italic: false });
                        }
                        // N · +ins -del, right-aligned just left of the buttons.
                        let files = git_view.staged.len() + git_view.unstaged.len();
                        let fnum = files.to_string();
                        let plus = format!("+{}", git_view.insertions);
                        let minus = format!("-{}", git_view.deletions);
                        let total = 16.0
                            + g.measure_chrome_text(&fnum, 12.0, false)
                            + 8.0
                            + g.measure_chrome_text(&plus, 12.0, false)
                            + 5.0
                            + g.measure_chrome_text(&minus, 12.0, false);
                        let sx0 = git_col_x + git_col_w - 12.0 - total;
                        if sx0 > bend + 14.0 {
                            g.queue_icon("file-text", sx0, y, 12.0, theme::text_mute());
                            let mut sx = sx0 + 16.0;
                            sx = g.draw_text(sx, y, &fnum, gpu::DrawOpts { font_size: 12.0, color: theme::text_dim(), bold: false, italic: false });
                            sx = g.draw_text(sx + 4.0, y, "·", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                            sx = g.draw_text(sx + 4.0, y, &plus, gpu::DrawOpts { font_size: 12.0, color: theme::success(), bold: false, italic: false });
                            g.draw_text(sx + 4.0, y, &minus, gpu::DrawOpts { font_size: 12.0, color: DIFF_RED, bold: false, italic: false });
                        }
                    }
                }
                y += 27.0;
                // ── Row 2: ⎇ Uncommitted changes ···· [ ⎯o Commit | ▾ ]
                let list_top;
                // Reserve the column foot for the recent-commits preview; the
                // change list clips to what's left above it.
                // 구역이 변경 목록을 통째로 삼키지 못하게 하는 상한. 사용자가 잡아
                // 둔 높이에도 같은 상한을 걸어야 한다 — 안 그러면 창을 줄였을 때
                // 화면엔 안 들어가는 높이로 커밋 수백 개를 계속 가져온다.
                let commits_cap = (bottom - TITLE_HEIGHT) * 0.72;
                let pinned_h = self.git.col_commits_h.map(|h| h.min(commits_cap));
                // 폴러에게 「몇 개까지」를 건넨다. 잡아 둔 높이에 들어가는 줄 수
                // (머리 24px + 줄당 20px)가 곧 그 수다 — 5개 고정이던 자리라, 늘려
                // 놓고도 빈 칸만 보이면 크기조절이 아무 일도 안 한 것처럼 읽힌다.
                self.git.col_commit_want.store(
                    match pinned_h {
                        Some(h) => (((h - 24.0) / 20.0).floor() as i64).clamp(1, 200) as usize,
                        None => GIT_RECENT_COMMITS_DEFAULT,
                    },
                    std::sync::atomic::Ordering::Relaxed,
                );
                let commits_h = if git_view.recent_commits.is_empty() {
                    0.0
                } else {
                    // 잡아 둔 높이가 있으면 그게 정본이고, 없으면 가져온 커밋 수에
                    // 맞춘다. 펼친 커밋의 파일 목록·diff 는 어느 쪽이든 그 위에
                    // 더한다 — 펼침은 잠깐이라 잡아 둔 높이를 갈아치울 값이 아니고,
                    // 안 더하면 펼치자마자 그 내용이 잘린다.
                    let mut h = pinned_h
                        .unwrap_or_else(|| 24.0 + git_view.recent_commits.len() as f32 * 20.0);
                    // An expanded commit grows the foot by its file list (+ any
                    // expanded file's diff), pushing the change list up.
                    if let Some(eh) = self.git.col_commit_expanded.clone() {
                        if let Some(files) = self.git.col_commit_files_cache.get(&eh) {
                            h += files.len().max(1) as f32 * 18.0;
                            for (path, _, _) in files {
                                if self
                                    .git
                                    .col_commit_file_expanded
                                    .contains(&(eh.clone(), path.clone()))
                                {
                                    if let Some(d) = self
                                        .git
                                        .col_commit_diff_cache
                                        .get(&(eh.clone(), path.clone()))
                                    {
                                        h += d.len() as f32 * 13.0;
                                    }
                                }
                            }
                        }
                    }
                    // Don't let the foot swallow the whole change list.
                    h.min(commits_cap)
                };
                let input_top = bottom - commits_h;
                if git_view.no_repo {
                    g.draw_text(gcx0, y, "git 저장소가 아닙니다", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                    self.git.commit_btn_rect = None;
                    self.git.commit_caret_rect = None;
                    list_top = y + 8.0;
                } else {
                    g.queue_icon("git-branch", gcx0, y + 1.0, 13.0, theme::text_mute());
                    g.draw_text(gcx0 + 18.0, y, "Uncommitted changes", gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false });
                    let bh = 24.0_f32;
                    let by = y - 4.0;
                    let caret_w = 20.0_f32;
                    let busy = self.git.op;
                    let can_commit = !git_view.staged.is_empty() || !git_view.unstaged.is_empty();
                    // While a git op runs, the button shows a spinner + "Pushing…"
                    // and ignores clicks. No uncommitted changes but commits to
                    // sync → the primary button becomes the sync action
                    // (GitHub-Desktop style); with changes it's Commit. 당길 것이
                    // 있으면 **Pull 이 Push 보다 먼저**다(2026-08-16 「풀있을때 풀먼저
                    // 뜨게」) — 당기기 전의 Push 는 어차피 원격이 거절한다. The caret
                    // dropdown always offers the full set (Commit / Push / Pull /
                    // Create PR).
                    let pull_mode = busy.is_none() && !can_commit && git_view.behind > 0;
                    let push_mode =
                        busy.is_none() && !can_commit && !pull_mode && git_view.ahead > 0;
                    let can_drop = busy.is_none()
                        && (can_commit || git_view.ahead > 0 || git_view.behind > 0);
                    let main_active = busy.is_none() && (can_commit || push_mode || pull_mode);
                    let main_label = if let Some(op) = busy {
                        format!("{op}…")
                    } else if pull_mode {
                        format!("Pull  {}", git_view.behind)
                    } else if push_mode {
                        format!("Push  {}", git_view.ahead)
                    } else {
                        "Commit".to_string()
                    };
                    let main_icon = if pull_mode {
                        "arrow-down"
                    } else if push_mode {
                        "arrow-up"
                    } else {
                        "git-commit-horizontal"
                    };
                    let lw = g.measure_chrome_text(&main_label, 12.0, true);
                    let main_w = 24.0 + lw + 10.0;
                    let total_w = main_w + caret_w;
                    let bx = git_col_x + git_col_w - 12.0 - total_w;
                    let mhov = self.cursor_px.0 >= bx && self.cursor_px.0 <= bx + main_w && self.cursor_px.1 >= by && self.cursor_px.1 <= by + bh;
                    let chov = self.cursor_px.0 >= bx + main_w && self.cursor_px.0 <= bx + total_w && self.cursor_px.1 >= by && self.cursor_px.1 <= by + bh;
                    let base = if can_drop || busy.is_some() { theme::surface_active() } else { theme::with_alpha(theme::surface_hover(), 0x66) };
                    round_rect(g, bx, by, total_w, bh, theme::radius_sm(), base);
                    if main_active && mhov { round_rect(g, bx, by, main_w, bh, theme::radius_sm(), theme::accent()); }
                    if can_drop && chov { round_rect(g, bx + main_w, by, caret_w, bh, theme::radius_sm(), theme::accent()); }
                    g.rect(bx + main_w, by + 5.0, 1.0, bh - 10.0, theme::with_alpha(theme::bg(), 0x99));
                    let fg_main = if main_active || busy.is_some() { theme::text() } else { theme::text_mute() };
                    let fg_caret = if can_drop { theme::text() } else { theme::text_mute() };
                    if busy.is_some() {
                        // Spinner: 8 dots round the icon slot, the bright one
                        // chasing round once a second.
                        let scx = bx + 14.0;
                        let scy = by + bh / 2.0;
                        let head = (time_secs * 1.1).fract();
                        for i in 0..8 {
                            let ang = (i as f32 / 8.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                            let p = i as f32 / 8.0;
                            let mut dd = head - p;
                            if dd < 0.0 { dd += 1.0; }
                            let a = (1.0 - dd).powf(1.6);
                            let d = 1.5_f32;
                            circle_rect(g, scx + ang.cos() * 5.5 - d, scy + ang.sin() * 5.5 - d, d * 2.0, theme::with_alpha(theme::text(), 30 + (a * 220.0) as u8));
                        }
                    } else {
                        g.queue_icon(main_icon, bx + 8.0, by + (bh - 13.0) / 2.0, 13.0, fg_main);
                    }
                    g.draw_text(bx + 24.0, by + (bh - 12.0) / 2.0, &main_label, gpu::DrawOpts { font_size: 12.0, color: fg_main, bold: true, italic: false });
                    g.draw_text(bx + main_w + (caret_w - 7.0) / 2.0, by + (bh - 11.0) / 2.0, "▾", gpu::DrawOpts { font_size: 11.0, color: fg_caret, bold: false, italic: false });
                    self.git.commit_btn_rect = Some((bx, by, main_w, bh));
                    self.git.commit_caret_rect = Some((bx + main_w, by, caret_w, bh));
                    y += 24.0;
                    g.rect(gcx0, y, gcw, 1.0, theme::with_alpha(theme::border(), 0x80));
                    list_top = y + 10.0;
                }
                    // 휠이 읽을 기하. 목록이 없는 갈래에서도 반드시 써야 한다 —
                    // 안 쓰면 직전 프레임의 값이 남아, 변경이 사라진 뒤에도 휠이
                    // 없는 목록을 스크롤한다.
                    self.git.col_list_extent = ((input_top - list_top).max(0.0), 0.0);
                    if git_view.clean {
                        circle_rect(g, gcx0, list_top + 4.0, 8.0, theme::success());
                        g.draw_text(
                            gcx0 + 15.0,
                            list_top + 1.0,
                            "변경 없음",
                            gpu::DrawOpts { font_size: 12.0, color: theme::text_dim(), bold: false, italic: false },
                        );
                    } else {
                        let item_h = 22.0_f32;
                        let header_h = 21.0_f32;
                        let dline_h = 15.0_f32;
                        let gutter_w = 30.0_f32;
                        let mut rects: Vec<(bool, String, (f32, f32, f32, f32))> = Vec::new();
                        let mut stage_rects: Vec<(bool, String, (f32, f32, f32, f32))> = Vec::new();
                        let mut discard_rects: Vec<(String, bool, (f32, f32, f32, f32))> = Vec::new();
                        let mut open_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                        // Two stacked sections (VSCode model). `staged` true =
                        // "Staged Changes" (− unstages); false = "Changes" (+
                        // stages). Both scroll together off git_col_scroll.
                        let mut y_cur = list_top - self.git.col_scroll;
                        // While a menu is up, skip the change list entirely — its
                        // text/icons draw in the glyph layer (above the dim quad)
                        // so they'd otherwise bleed through the menu.
                        let menus_open = self.git.commit_menu_open
                            || self.git.path_menu_open
                            || self.git.branch_menu_open;
                        // 목록은 `list_top`~`input_top` 안에 가둔다. 지금까지는 행마다
                        // 「완전히 밖이면 건너뛴다」로만 걸러, 위로 반쯤 걸친 행이 통째로
                        // 그려져 Commit 버튼 줄과 구분선을 덮었다. 루프 **밖**에서 한 번만
                        // 세운다 — 안에서 세우면 행마다 세그먼트가 둘씩 쌓인다.
                        g.push_clip(
                            git_col_x,
                            list_top,
                            git_col_w,
                            (input_top - list_top).max(0.0),
                        );
                        // 커서가 잘려 안 보이는 쪽에 있는데 행의 보이는 쪽에 하이라이트가
                        // 그려지는 것은 시저가 못 막는다(그 하이라이트는 클립 안이니까).
                        // 그래서 이 목록 안에서는 걸러 낸 커서를 쓴다. 저장되는 히트렉트는
                        // 그것과 별개로 루프 끝에서 교집합을 낸다 — `handler.rs` 가 **다음
                        // 클릭 좌표로 다시** 검사하므로 커서를 거른 것만으로는 안 된다.
                        let cur = match g.clip_hit((self.cursor_px.0, self.cursor_px.1, 1.0, 1.0)) {
                            Some(_) => self.cursor_px,
                            None => (f32::MIN, f32::MIN),
                        };
                        for (title, staged, files) in [
                            ("Staged Changes", true, &git_view.staged),
                            ("Changes", false, &git_view.unstaged),
                        ] {
                            if files.is_empty() {
                                continue;
                            }
                            // Section header (count) — 완전히 밖일 때만 건너뛴다.
                            // 경계는 손으로 다시 쓰지 않고 클립에게 묻는다.
                            if !menus_open && g.clip_visible(git_col_x, y_cur, git_col_w, header_h) {
                                g.draw_text(
                                    gcx0,
                                    y_cur + 5.0,
                                    &format!("{}  {}", title, files.len()),
                                    gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: true, italic: false },
                                );
                            }
                            y_cur += header_h;
                            for (marker, path) in files.iter() {
                                let ry = y_cur;
                                y_cur += item_h;
                                let expanded = self.git.col_expanded.contains(&(staged, path.clone()));
                                let row_visible =
                                    !menus_open && g.clip_visible(git_col_x, ry, git_col_w, item_h);
                                if row_visible {
                                    let hovered = cur.0 >= git_col_x
                                        && cur.0 <= git_col_x + git_col_w
                                        && cur.1 >= ry
                                        && cur.1 < ry + item_h;
                                    if hovered {
                                        hover_rect(g, gcx0 - 5.0, ry, gcw + 10.0, item_h, theme::radius_sm());
                                    }
                                    // Expander chevron at the row's left edge.
                                    g.queue_icon(
                                        if expanded { "chevron-down" } else { "chevron-right" },
                                        gcx0,
                                        ry + (item_h - 12.0) / 2.0,
                                        12.0,
                                        theme::text_mute(),
                                    );
                                    let untracked = *marker == 'U';
                                    // Filename bright, parent dir dim after it (so the
                                    // name stays readable even when the path is long).
                                    // No status badge — chevron + name, cursor-style.
                                    let fname = path.rsplit('/').next().unwrap_or(path.as_str());
                                    let dir = path.strip_suffix(fname).unwrap_or("").trim_end_matches('/');
                                    let tx = gcx0 + 20.0;
                                    let ty = ry + (item_h - 12.0) / 2.0;
                                    let endx = g.draw_text(
                                        tx,
                                        ty,
                                        fname,
                                        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: false, italic: false },
                                    );
                                    if !dir.is_empty() {
                                        g.draw_text(
                                            endx + 7.0,
                                            ty + 0.5,
                                            dir,
                                            gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
                                        );
                                    }
                                    // Action cluster (cursor style), always visible
                                    // right-to-left: +/− stage · ↩ discard · ⤴ open.
                                    // numstat (+ins -del) sits just left of them.
                                    let aw = 19.0_f32;
                                    let agap = 1.0_f32;
                                    let mut ax = git_col_x + git_col_w - 12.0 - aw;
                                    let icon_dim = if hovered { theme::text_dim() } else { theme::with_alpha(theme::text_dim(), 0x88) };
                                    {
                                        let bh = cur.0 >= ax && cur.0 <= ax + aw && cur.1 >= ry && cur.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::radius_sm(), theme::surface_active());
                                        }
                                        g.queue_icon(if staged { "minus" } else { "plus" }, ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { theme::text() } else { icon_dim });
                                        stage_rects.push((!staged, path.clone(), (ax - 1.0, ry, aw + 2.0, item_h)));
                                        ax -= aw + agap;
                                    }
                                    {
                                        let bh = cur.0 >= ax && cur.0 <= ax + aw && cur.1 >= ry && cur.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::radius_sm(), theme::surface_active());
                                        }
                                        g.queue_icon("undo-2", ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { DIFF_RED } else { icon_dim });
                                        discard_rects.push((path.clone(), untracked, (ax - 1.0, ry, aw + 2.0, item_h)));
                                        ax -= aw + agap;
                                    }
                                    {
                                        let bh = cur.0 >= ax && cur.0 <= ax + aw && cur.1 >= ry && cur.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::radius_sm(), theme::surface_active());
                                        }
                                        g.queue_icon("external-link", ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { theme::text() } else { icon_dim });
                                        open_rects.push((path.clone(), (ax - 1.0, ry, aw + 2.0, item_h)));
                                    }
                                    // numstat — right-aligned just left of the actions.
                                    if let Some((ins, del)) = git_view.numstat.get(path) {
                                        if *ins > 0 || *del > 0 {
                                            let minus = format!("-{del}");
                                            let plus = format!("+{ins}");
                                            let wm = g.measure_chrome_text(&minus, 11.0, false);
                                            let wp = g.measure_chrome_text(&plus, 11.0, false);
                                            let mut rx = ax - 4.0;
                                            if *del > 0 {
                                                rx -= wm;
                                                g.draw_text(rx, ty, &minus, gpu::DrawOpts { font_size: 11.0, color: DIFF_RED, bold: false, italic: false });
                                                rx -= 5.0;
                                            }
                                            if *ins > 0 {
                                                rx -= wp;
                                                g.draw_text(rx, ty, &plus, gpu::DrawOpts { font_size: 11.0, color: theme::success(), bold: false, italic: false });
                                            }
                                        }
                                    }
                                    rects.push((staged, path.clone(), (git_col_x, ry, git_col_w, item_h)));
                                }
                                // Inline unified diff for an expanded row, syntax-
                                // highlighted with the same tokenizer the code-block
                                // overlay uses. Numbered gutter + tinted +/- bands.
                                if expanded {
                                    let lang = code_lang_for_path(std::path::Path::new(path.as_str()));
                                    if let Some(rows_d) = self.git.col_diff_cache.get(&(staged, path.clone())) {
                                        for dl in rows_d.iter() {
                                            let dy = y_cur;
                                            y_cur += dline_h;
                                            if !g.clip_visible(git_col_x, dy, git_col_w, dline_h) {
                                                continue;
                                            }
                                            use kasa_mcp::git::DiffLineKind as K;
                                            let (bg, sign, scol) = match dl.kind {
                                                K::Add => (theme::with_alpha(theme::success(), 0x22), "+", theme::success()),
                                                K::Del => (theme::with_alpha(DIFF_RED, 0x22), "-", DIFF_RED),
                                                K::Hunk => (theme::with_alpha(theme::accent(), 0x14), "", theme::text_mute()),
                                                K::Context => ([0, 0, 0, 0], " ", theme::text_mute()),
                                            };
                                            if bg[3] > 0 {
                                                g.rect(gcx0 - 5.0, dy, gcw + 10.0, dline_h, bg);
                                            }
                                            if dl.kind == K::Hunk {
                                                g.draw_text(
                                                    gcx0,
                                                    dy + 1.5,
                                                    dl.text.trim_end(),
                                                    gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
                                                );
                                                continue;
                                            }
                                            // Line number gutter (new side, else old).
                                            if let Some(n) = dl.new_no.or(dl.old_no) {
                                                let ns = n.to_string();
                                                let nw = g.measure_chrome_text(&ns, 10.0, false);
                                                g.draw_text(
                                                    gcx0 + gutter_w - nw - 4.0,
                                                    dy + 1.5,
                                                    &ns,
                                                    gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
                                                );
                                            }
                                            g.draw_text(
                                                gcx0 + gutter_w,
                                                dy + 1.5,
                                                sign,
                                                gpu::DrawOpts { font_size: 11.0, color: scol, bold: false, italic: false },
                                            );
                                            let mut tx = gcx0 + gutter_w + 9.0;
                                            for (tok, col) in gpu::highlight_code_line(dl.text.trim_end(), lang, theme::text_dim()) {
                                                tx = g.draw_text(
                                                    tx,
                                                    dy + 1.5,
                                                    &tok,
                                                    gpu::DrawOpts { font_size: 11.0, color: col, bold: false, italic: false },
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // 히트렉트를 목록 구역과 교집합 낸다. 이 넷은 `handler.rs` 가
                        // **다음 클릭 좌표로 다시** 검사하므로, 위에서 커서를 거른 것과는
                        // 별개로 rect 자체가 잘려 있어야 한다. 안 자르면 화면은 완벽한데
                        // Commit 버튼 뒤로 스크롤된 행의 「되돌리기」가 눌린다 — 되돌릴 수
                        // 없는 동작이고, 스크린샷이 절대 못 잡는 부류다.
                        macro_rules! clip_rects {
                            ($v:expr, $i:tt) => {
                                $v.retain_mut(|e| match g.clip_hit(e.$i) {
                                    Some(h) => {
                                        e.$i = h;
                                        true
                                    }
                                    None => false,
                                })
                            };
                        }
                        clip_rects!(rects, 2);
                        clip_rects!(stage_rects, 2);
                        clip_rects!(discard_rects, 2);
                        clip_rects!(open_rects, 1);
                        g.pop_clip();
                        // 휠에게 넘기는 기하. `y_cur` 는 `list_top - col_scroll` 에서
                        // 출발해 섹션 머리·파일 행·펼친 diff 줄을 **실제로 그린 만큼**
                        // 지나왔으므로, 스크롤을 되더하면 그게 곧 내용 높이다. 휠이
                        // 자기 힘으로는 못 구하는 값이라(펼친 diff 는 캐시를 뒤져야
                        // 나온다) 여기서 써 준다.
                        self.git.col_list_extent = (
                            (input_top - list_top).max(0.0),
                            (y_cur + self.git.col_scroll - list_top).max(0.0),
                        );
                        self.git.col_file_rects = rects;
                        self.git.col_stage_rects = stage_rects;
                        self.git.col_discard_rects = discard_rects;
                        self.git.col_open_rects = open_rects;
                    }
                    // ── Recent commits, pinned to the column foot. Double-click a
                    // commit row to expand its changed-file list inline (GitLens-
                    // graph style); a file row then expands its diff.
                    self.git.col_commit_rects.clear();
                    self.git.col_commit_file_rects.clear();
                    self.git.col_commits_grip = None;
                    if !git_view.recent_commits.is_empty() {
                        let (curx, cury) = self.cursor_px;
                        let foot = bottom - 2.0;
                        let clip_r = git_col_x + git_col_w - 12.0;
                        let mut cy2 = input_top + 6.0;
                        // 구역 머리의 가로선이 곧 크기조절 손잡이다. 잡는 띠는 선보다
                        // 두껍게(위아래 4px) 잡는다 — 1px 선을 정확히 맞춰 눌러야 하면
                        // 손잡이가 있다는 걸 알아도 못 쓴다. 이 자리는 매 프레임
                        // 변경 목록 길이·펼침에 따라 움직여서 handler 가 스스로는
                        // 못 구한다.
                        let grip = (git_col_x, cy2 - 6.0, git_col_w, 9.0);
                        self.git.col_commits_grip = Some(grip);
                        let grip_hot = self.git.col_commits_resize.is_some()
                            || (curx >= grip.0
                                && curx <= grip.0 + grip.2
                                && cury >= grip.1
                                && cury <= grip.1 + grip.3);
                        let (line_col, line_h) = if grip_hot {
                            (theme::accent(), 2.0)
                        } else {
                            (theme::with_alpha(theme::border(), 0x80), 1.0)
                        };
                        g.rect(gcx0, cy2 - 2.0, gcw, line_h, line_col);
                        g.draw_text(gcx0, cy2 + 4.0, "최근 커밋", gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: true, italic: false });
                        cy2 += 22.0;
                        for (hash, subj) in &git_view.recent_commits {
                            if cy2 > foot {
                                break;
                            }
                            let expanded = self.git.col_commit_expanded.as_deref() == Some(hash.as_str());
                            let rowr = (gcx0 - 5.0, cy2 - 3.0, gcw + 10.0, 19.0);
                            let hov = curx >= rowr.0 && curx <= rowr.0 + rowr.2 && cury >= rowr.1 && cury <= rowr.1 + rowr.3;
                            if expanded {
                                g.rect(rowr.0, rowr.1, rowr.2, rowr.3, theme::with_alpha(theme::accent(), 0x18));
                            } else if hov {
                                g.rect(rowr.0, rowr.1, rowr.2, rowr.3, theme::surface_hover());
                            }
                            let chev = if expanded { "chevron-down" } else { "chevron-right" };
                            g.queue_icon(chev, gcx0, cy2 - 1.0, 11.0, theme::text_mute());
                            let hxc = g.draw_text(gcx0 + 14.0, cy2, hash, gpu::DrawOpts { font_size: 11.0, color: theme::accent(), bold: false, italic: false });
                            g.draw_text_clipped(hxc + 8.0, cy2, subj, gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false }, gcx0, clip_r);
                            self.git.col_commit_rects.push((hash.clone(), rowr));
                            cy2 += 20.0;
                            if !expanded {
                                continue;
                            }
                            // Changed-file list for the expanded commit.
                            let files = self.git.col_commit_files_cache.get(hash).cloned().unwrap_or_default();
                            if files.is_empty() {
                                g.draw_text(gcx0 + 20.0, cy2, "(변경 없음)", gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: true });
                                cy2 += 16.0;
                            }
                            for (path, add, del) in &files {
                                if cy2 > foot {
                                    break;
                                }
                                let fexp = self.git.col_commit_file_expanded.contains(&(hash.clone(), path.clone()));
                                let fr = (gcx0 + 14.0, cy2 - 2.0, gcw - 14.0, 17.0);
                                let fhov = curx >= fr.0 && curx <= fr.0 + fr.2 && cury >= fr.1 && cury <= fr.1 + fr.3;
                                if fexp {
                                    g.rect(fr.0, fr.1, fr.2, fr.3, theme::with_alpha(theme::accent(), 0x10));
                                } else if fhov {
                                    g.rect(fr.0, fr.1, fr.2, fr.3, theme::surface_hover());
                                }
                                let fname = std::path::Path::new(path.as_str())
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.clone());
                                let stat = format!("+{add} -{del}");
                                let sw = g.measure_chrome_text(&stat, 10.0, false);
                                g.draw_text_clipped(
                                    gcx0 + 20.0,
                                    cy2,
                                    &fname,
                                    gpu::DrawOpts { font_size: 11.0, color: if fexp { theme::text() } else { theme::text_dim() }, bold: false, italic: false },
                                    gcx0 + 20.0,
                                    clip_r - sw - 8.0,
                                );
                                g.draw_text(clip_r - sw, cy2, &stat, gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false });
                                self.git.col_commit_file_rects.push((hash.clone(), path.clone(), fr));
                                cy2 += 18.0;
                                if !fexp {
                                    continue;
                                }
                                // Inline diff for the expanded file (tinted +/- bands).
                                let diff = self
                                    .git
                                    .col_commit_diff_cache
                                    .get(&(hash.clone(), path.clone()))
                                    .cloned()
                                    .unwrap_or_default();
                                use kasa_mcp::git::DiffLineKind as K;
                                for dl in diff.iter() {
                                    if cy2 > foot {
                                        break;
                                    }
                                    let (bg, scol) = match dl.kind {
                                        K::Add => (theme::with_alpha(theme::success(), 0x22), theme::success()),
                                        K::Del => (theme::with_alpha(theme::danger(), 0x22), theme::danger()),
                                        K::Hunk => (theme::with_alpha(theme::accent(), 0x14), theme::text_mute()),
                                        K::Context => ([0, 0, 0, 0], theme::text_mute()),
                                    };
                                    if bg[3] > 0 {
                                        g.rect(gcx0 + 14.0, cy2 - 1.0, gcw - 14.0, 13.0, bg);
                                    }
                                    let prefix = match dl.kind {
                                        K::Add => "+",
                                        K::Del => "-",
                                        _ => " ",
                                    };
                                    let txt = format!("{prefix}{}", dl.text.trim_end());
                                    g.draw_text_clipped(
                                        gcx0 + 20.0,
                                        cy2,
                                        &txt,
                                        gpu::DrawOpts { font_size: 10.0, color: scol, bold: false, italic: false },
                                        gcx0 + 20.0,
                                        clip_r,
                                    );
                                    cy2 += 13.0;
                                }
                            }
                        }
                    }
                // Dropdowns (path picker / branch switcher) paint last so they
                // overlay the list + buttons. Built from the precomputed repo
                // list and the poller's branch list.
                git_paint_dropdowns(
                    g,
                    git_col_x,
                    git_col_w,
                    TITLE_HEIGHT,
                    self.git.path_hdr_rect,
                    self.git.branch_hdr_rect,
                    self.git.path_menu_open,
                    self.git.branch_menu_open,
                    &git_repo_list,
                    &self.git.col_pinned_cwd,
                    &git_view.branches,
                    &git_view.branch,
                    &mut self.git.path_menu_rects,
                    &mut self.git.branch_menu_rects,
                );
                // ── Commit-button dropdown (Commit / Push / Create PR)
                self.git.commit_menu_rects.clear();
                if self.git.commit_menu_open {
                    if let Some((ccx, ccy, ccw, cch)) = self.git.commit_caret_rect {
                        // Dim the panel behind the menu so the change-list rows
                        // (and their hover buttons) don't bleed alongside it.
                        g.rect(git_col_x, top, git_col_w, bottom - top, theme::with_alpha([0, 0, 0, 255], 0xB0));
                        // Push/Pull carry their ahead/behind counts so you can
                        // see what's pending before clicking.
                        let push_label = if git_view.ahead > 0 {
                            format!("Push  {}", git_view.ahead)
                        } else {
                            "Push".to_string()
                        };
                        let pull_label = if git_view.behind > 0 {
                            format!("Pull  {}", git_view.behind)
                        } else {
                            "Pull".to_string()
                        };
                        // 당길 것이 있으면 Pull 을 Push 위로 — 기본 버튼과 같은 우선
                        // 순위(당기기 전의 Push 는 원격이 거절한다).
                        let (sync_a, sync_b) = if git_view.behind > 0 {
                            (
                                ("arrow-down", pull_label, GitCommitAction::Pull),
                                ("arrow-up", push_label, GitCommitAction::Push),
                            )
                        } else {
                            (
                                ("arrow-up", push_label, GitCommitAction::Push),
                                ("arrow-down", pull_label, GitCommitAction::Pull),
                            )
                        };
                        let items: [(&str, String, GitCommitAction); 4] = [
                            ("git-commit-horizontal", "Commit".to_string(), GitCommitAction::Commit),
                            sync_a,
                            sync_b,
                            ("github", "Create PR".to_string(), GitCommitAction::CreatePr),
                        ];
                        let iw = 190.0_f32;
                        let ih = 34.0_f32;
                        let mh = ih * items.len() as f32 + 8.0;
                        let mx = (ccx + ccw - iw).max(git_col_x + 8.0);
                        let my = ccy + cch + 4.0;
                        panel_rect_outlined(g, mx, my, iw, mh, theme::radius_md(), theme::surface());
                        let mut iy = my + 4.0;
                        for (icon, label, act) in items {
                            let hov = self.cursor_px.0 >= mx && self.cursor_px.0 <= mx + iw && self.cursor_px.1 >= iy && self.cursor_px.1 <= iy + ih;
                            if hov {
                                hover_rect(g, mx + 4.0, iy, iw - 8.0, ih, theme::radius_sm());
                            }
                            g.queue_icon(icon, mx + 14.0, iy + (ih - 15.0) / 2.0, 15.0, theme::text_dim());
                            g.draw_text(mx + 38.0, iy + (ih - 13.0) / 2.0, &label, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                            self.git.commit_menu_rects.push((act, (mx, iy, iw, ih)));
                            iy += ih;
                        }
                    }
                }
            }
            // Info 탭 — 같은 칼럼, 같은 머리, 본문만 다르다. git 본문과 형제
            // 블록으로 두는 편이 거대한 git 블록을 통째로 else 로 감싸는 것보다
            // diff 가 얕고, 각 탭이 자기 배경부터 그려 잔상이 남지 않는다.
            if git_col_w > 0.0 && self.info.tab == state::SideTab::Info {
                // 상태줄은 늘 있으므로 조건 없이 함께 뺀다 — 안 빼면 패널 바닥이
                // 그 띠 위로 덮여 그려진다. 시저는 `push_clip` 을 세운 자리에만
                // 걸리는데 여기는 그 바깥이라, 자리를 미리 빼 두는 이 계산이 정본이다.
                let bottom_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT } + status_h;
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - bottom_h).max(top);
                g.rect(git_col_x, top, git_col_w, bottom - top, theme::panel_bg());
                g.rect(git_col_x, top, 1.0, bottom - top, theme::border());
                let body_top = info::draw_side_tabs(
                    g,
                    self.cursor_px,
                    &mut self.info,
                    &mut self.git,
                    git_col_x,
                    git_col_w,
                    top,
                );
                // 계정을 하나라도 추가했을 때만 이름을 붙인다.
                let acct_label = (!self.set_claude_accounts.is_empty()).then(|| {
                    let id = self.set_claude_account.as_str();
                    match self.set_claude_accounts.iter().position(|a| a.id == id) {
                        Some(i) => crate::settings::account_display(
                            id,
                            &self.set_claude_accounts[i].label,
                            &format!("계정 {}", i + 2),
                        ),
                        None => crate::settings::account_display("", "", "기본"),
                    }
                });
                let (body_top, acct_rect) = info::draw_info_actions(
                    g,
                    self.cursor_px,
                    &mut self.info,
                    acct_label.as_deref(),
                    claude_usage_pct.as_ref(),
                    self.account_menu,
                    crate::socket::read_shim_inject(),
                    git_col_x,
                    git_col_w,
                    body_top,
                );
                self.account_chip_rect = acct_rect;
                info::draw_info_col(
                    g,
                    self.cursor_px,
                    &mut self.info,
                    &self.closed_panes,
                    git_col_x,
                    git_col_w,
                    body_top,
                    bottom,
                );
            }
            // 세션 기록 탭 — git/Info 와 형제 블록. 같은 칼럼·같은 머리를 쓰고
            // 본문만 다르다.
            if git_col_w > 0.0 && self.info.tab == state::SideTab::Sessions {
                // 상태줄은 늘 있으므로 조건 없이 함께 뺀다 — 안 빼면 패널 바닥이
                // 그 띠 위로 덮여 그려진다. 시저는 `push_clip` 을 세운 자리에만
                // 걸리는데 여기는 그 바깥이라, 자리를 미리 빼 두는 이 계산이 정본이다.
                let bottom_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT } + status_h;
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - bottom_h).max(top);
                g.rect(git_col_x, top, git_col_w, bottom - top, theme::panel_bg());
                g.rect(git_col_x, top, 1.0, bottom - top, theme::border());
                let body_top = info::draw_side_tabs(
                    g,
                    self.cursor_px,
                    &mut self.info,
                    &mut self.git,
                    git_col_x,
                    git_col_w,
                    top,
                );
                sesscol::draw_sessions_col(
                    g,
                    self.cursor_px,
                    &mut self.sessions_col,
                    git_col_x,
                    git_col_w,
                    body_top,
                    bottom,
                );
            }
            // MCP·Skill 탭 — 위 둘과 형제 블록.
            if git_col_w > 0.0 && self.info.tab == state::SideTab::Mcp {
                // 상태줄은 늘 있으므로 조건 없이 함께 뺀다 — 안 빼면 패널 바닥이
                // 그 띠 위로 덮여 그려진다. 시저는 `push_clip` 을 세운 자리에만
                // 걸리는데 여기는 그 바깥이라, 자리를 미리 빼 두는 이 계산이 정본이다.
                let bottom_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT } + status_h;
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - bottom_h).max(top);
                g.rect(git_col_x, top, git_col_w, bottom - top, theme::panel_bg());
                g.rect(git_col_x, top, 1.0, bottom - top, theme::border());
                let body_top = info::draw_side_tabs(
                    g,
                    self.cursor_px,
                    &mut self.info,
                    &mut self.git,
                    git_col_x,
                    git_col_w,
                    top,
                );
                mcpcol::draw_mcp_col(
                    g,
                    self.cursor_px,
                    &mut self.mcp_col,
                    git_col_x,
                    git_col_w,
                    body_top,
                    bottom,
                );
            }
            // Per-pane header bar. The band is the unified BG (same as the
            // body) so there's no depth seam; a bottom hairline separates it
            // from the cell grid. The active tab is marked by a raised pill +
            // a top accent strip — not a darker "cage" — and only the active
            // tab carries a × (so clicking any inactive tab just switches).
            // (drop_pane, target) — drives the insertion bar; updated to
            // the pane the cursor is currently over (cross-pane drag).
            // Suppressed whenever the zone-overlay rectangle is showing
            // for the same drag — two simultaneous indicators is what
            // the "pane 이동이랑 같이 떠" report was about. Falls back
            // to the bar only when the cursor is outside every pane box
            // (gap / window edge).
            let tab_drag_info: Option<(String, usize)> = self
                .tab_drag
                .as_ref()
                .filter(|d| d.active && !zone_overlay_active)
                .map(|d| (d.drop_pane.clone(), d.target));
            // (source_pane, source_idx) — the tab being lifted. The source
            // tab is drawn at reduced alpha so it reads as "in transit"
            // while the user drags it into another strip.
            let tab_drag_src: Option<(String, usize)> = self
                .tab_drag
                .as_ref()
                .filter(|d| d.active)
                .map(|d| (d.pane.clone(), d.from));
            let hover_info: Option<(String, usize)> = self.pane_tab_hover.clone();
            // Active-tab top accents we need to repaint after the pane
            // dividers (BORDER) draw, so a horizontal split's seam doesn't
            // wipe the accent of the lower pane's active tab.
            let mut deferred_accents: Vec<(f32, f32, f32, [u8; 4])> = Vec::new();
            for (hi, h) in headers.iter().enumerate() {
                // Completion flash: a finished pane's header pulses SUCCESS for
                // ~1.8s (notify_flash) and fades back to BG, so a Stop-hook
                // notification has an in-window visual even when the desktop
                // alert is suppressed (focused pane).
                let hdr_bg = match header_flash[hi] {
                    Some(k) => theme::lerp(theme::bg(), theme::success(), 0.7 * k),
                    None => theme::bg(),
                };
                g.rect(h.x, h.y, h.w, PANE_HEADER_HEIGHT, hdr_bg);
                // Working indicator: a ~32% segment sweeps the header bottom on
                // a 1.2s loop while this pane is busy (claude running) — the
                // "로딩바" the user picked. 2px over a faint accent rail; idle
                // panes draw nothing. about_to_wait keeps frames coming (a
                // cheap GPU-time present, no chrome rebuild) while a pane is busy.
                if h.compacting {
                    // compact 중 — 쓸림 대신 왼쪽부터 채워지는 바. compact 는 끝이 있는
                    // 작업이라 이 모양이 상태를 옳게 읽히고, 화면에 뜨는 알림이 teammate
                    // 메시지에 가려져도 헤더는 남는다(거노 2026-08-13: "가끔 sm으로
                    // 가려질때도 있어"). busy 보다 먼저 봐야 한다 — compact 중에도 스피너가
                    // 돌아 busy 가 함께 참이고, 순서가 뒤면 늘 쓸림바가 이긴다.
                    let bar_h = 3.0;
                    let by = h.y + PANE_HEADER_HEIGHT - bar_h;
                    if let Some(p) = h.compact_pct {
                        // 화면의 `▰▰▱ N%` 에서 읽은 진짜 진행률(2026-08-13 지시)을
                        // **칸이 차오르는 눈금 + 숫자**로(2026-08-15 지시 — 연속
                        // 띠는 얼마나 남았는지 눈금이 없어 안 읽혔다). 숫자는
                        // 오른쪽 버튼 무리를 피해 게이지 끝에 얹는다.
                        let cf = 10.5;
                        let label = format!("{p}%");
                        let lw = g.measure_chrome_text(&label, cf, true);
                        let btn_zone = (theme::ICON_SIZE + 2.0) * 4.0 + 8.0;
                        let track_w = (h.w - btn_zone - lw - 12.0).max(30.0);
                        let used =
                            draw_compact_cells(g, h.x, by, track_w, bar_h, theme::accent(), p);
                        g.draw_text(
                            h.x + used + 5.0,
                            h.y + (PANE_HEADER_HEIGHT - cf) / 2.0,
                            &label,
                            gpu::DrawOpts {
                                font_size: cf,
                                color: theme::accent(),
                                bold: true,
                                italic: false,
                            },
                        );
                    } else {
                        g.compact_bar(h.x, by, h.w, bar_h, theme::accent());
                    }
                } else if h.busy {
                    let bar_h = 3.0;
                    let by = h.y + PANE_HEADER_HEIGHT - bar_h;
                    // One FLAG_WORKING_BAR quad — the shader sweeps the segment
                    // over a faint track from u.time, so there's no per-frame
                    // CPU phase math and no chrome rebuild to keep it moving.
                    g.working_bar(h.x, by, h.w, bar_h, theme::accent());
                } else if h.bg_active {
                    // Not visibly working, but a background shell / Monitor is
                    // in-flight — one FLAG_PULSE_BAR quad breathes the same accent
                    // rail on a slow 3s sine, a distinct rhythm from the sweep.
                    let bar_h = 3.0;
                    let by = h.y + PANE_HEADER_HEIGHT - bar_h;
                    g.pulse_bar(h.x, by, h.w, bar_h, theme::accent());
                }
                // 계정이 바뀌었는데 이 pane 은 옛 계정으로 돈다 — 헤더에 「⟳ 재시작」
                // 칩을 띄운다. 계정은 프로세스 env 라 뜰 때 박히고 도는 프로세스는 못
                // 바꾸므로, 되띄우는 것 말고는 새 계정으로 옮길 길이 없다.
                //
                // 오른쪽 버튼 무리보다 **먼저** 그린다 — 그쪽은 x 를 오른쪽 끝에서
                // 거꾸로 잡아 나가서, 나중에 그리면 칩 위에 겹친다.
                if let Some((from, to)) = self.pane_account_stale.get(&h.id) {
                    let label = format!("⟳ {from} → {to} 재시작");
                    let pad = 6.0;
                    let cw = g.measure_chrome_text(&label, chrome_font, true) + pad * 2.0;
                    let ch = PANE_HEADER_HEIGHT - 6.0;
                    // 오른쪽 버튼 무리를 피해 그 왼쪽에 붙인다. 자리가 모자라면
                    // 아예 안 그린다 — 겹쳐 그리면 둘 다 못 읽는다.
                    let btn_zone = (theme::ICON_SIZE + 2.0) * 4.0 + 8.0;
                    let cx = h.x + h.w - btn_zone - cw;
                    if cx > h.x + 8.0 {
                        let cy = h.y + 3.0;
                        g.round_rect_fill(cx, cy, cw, ch, 4.0, theme::attention());
                        g.draw_text(
                            cx + pad,
                            h.y + (PANE_HEADER_HEIGHT - chrome_font) / 2.0,
                            &label,
                            gpu::DrawOpts {
                                font_size: chrome_font,
                                color: theme::bg(),
                                bold: true,
                                italic: false,
                            },
                        );
                        restart_chip_hits.push((h.id.clone(), (cx, cy, cw, ch)));
                    }
                }
                // No bottom hairline: the band == body, and the active tab
                // flows straight into the cell grid (browser-tab feel).
                // Compact glyphs — a touch bigger than the label so icons
                // read, but no longer the bulky +10 of the old design.
                let icon_size = theme::ICON_SIZE;
                let text_y = h.y + (PANE_HEADER_HEIGHT - chrome_font) / 2.0;
                let icon_y = h.y + (PANE_HEADER_HEIGHT - icon_size) / 2.0;
                let act_fg: [u8; 4] = if h.is_active {
                    theme::text_dim()
                } else {
                    theme::with_alpha(theme::text_dim(), 0x6B)
                };
                // Right action button cluster. Terminal panes get
                // split-v / split-h (new-terminal and web were dropped —
                // the +button already opens a new shell, and the web
                // overlay added complexity for little payoff). Image panes
                // keep the 4-button zoom/rotate set.
                let abw = icon_size + 2.0;
                let agap = 2.0;
                let n_btn: f32 = if h.is_image || h.is_web { 4.0 } else { 3.0 };
                // Markdown panes show a "Rendered | Raw" segmented toggle instead
                // of an icon cluster; reserve its measured width on the right.
                let seg_font = 11.0_f32;
                let seg_pad = 9.0_f32;
                let (md_rendered_w, md_raw_w) = if h.is_markdown {
                    (
                        g.measure_chrome_text("Rendered", seg_font, false),
                        g.measure_chrome_text("Raw", seg_font, false),
                    )
                } else {
                    (0.0, 0.0)
                };
                let seg_w = md_rendered_w + md_raw_w + seg_pad * 4.0;
                let btn_cluster = if h.is_markdown {
                    seg_w + 12.0
                } else {
                    abw * n_btn + agap * (n_btn - 1.0) + 12.0
                };
                // ── 웹 pane 주소 pill 자리 예약 ── 탭 pill 과 우측 버튼 사이.
                // btn_cluster 에 얹어 아래 tabs_area 계산이 그대로 따라온다.
                // 탭에 최소 140px 을 남기고 남는 만큼(상한 420px)만 가진다 —
                // 좁은 pane(주소폭 70px 미만)에선 아예 접는다(탭·버튼이 먼저다).
                let plus_w_early = icon_size;
                let addr_w = if h.is_web {
                    (h.w - 8.0 - btn_cluster - plus_w_early - 16.0 - 140.0).clamp(0.0, 420.0)
                } else {
                    0.0
                };
                let addr_vis = addr_w >= 70.0;
                let btn_cluster = btn_cluster + if addr_vis { addr_w + 6.0 } else { 0.0 };
                // ── In-pane tab bar ── empty tabs = single tab from `label`.
                let tab_list: Vec<&str> = if h.tabs.is_empty() {
                    vec![h.label.as_str()]
                } else {
                    h.tabs.iter().map(|s| s.as_str()).collect()
                };
                // SVG icons are square at icon_size; reserve that exact width
                // (not a glyph measurement) so the × never crowds the tab edge.
                let close_w = icon_size;
                let plus_w = icon_size;
                // Each tab's title gets an equal share of the leftover width.
                let tabs_area = (h.w - 8.0 - btn_cluster - plus_w - 16.0).max(0.0);
                let gap = 6.0_f32;
                // Overflow windowing: whole tabs only — 이 띠는 클립을 안 세우므로
                // 반쪽 알약이 나온다. When they can't all fit at the 56px minimum,
                // show a contiguous run from `tab_first` and reserve 12px at
                // each end for the overflow chevrons; the wheel over the strip
                // steps the run.
                let n_tabs = tab_list.len();
                let fits = |area: f32| (((area + gap) / (56.0 + gap)) as usize).max(1);
                let overflowing = n_tabs > fits(tabs_area);
                let (strip_pad, area_eff) = if overflowing {
                    (12.0_f32, (tabs_area - 24.0).max(56.0))
                } else {
                    (0.0, tabs_area)
                };
                let n_vis = n_tabs.min(fits(area_eff));
                let mut first = h.tab_first.min(n_tabs - n_vis);
                // A tab switch since the last frame (click, close, shortcut —
                // whichever of the many sites) reveals the newly active tab;
                // plain wheel scrolling is left where the user put it.
                if h.active_tab != h.tab_last_active {
                    if h.active_tab < first {
                        first = h.active_tab;
                    } else if h.active_tab >= first + n_vis {
                        first = h.active_tab + 1 - n_vis;
                    }
                }
                pane_tab_windowing.push((h.id.clone(), first, n_vis, h.active_tab));
                let per_tab = if n_vis == 1 {
                    area_eff
                } else {
                    ((area_eff - gap * n_vis.saturating_sub(1) as f32) / n_vis as f32)
                        .clamp(56.0, 320.0)
                };
                // Left edge of each visible tab's pill, for the drag insertion bar.
                let mut tab_edges: Vec<f32> = Vec::with_capacity(n_vis);
                // Geometry for the post-loop structural border pass.
                let mut tabs_left: Option<f32> = None;
                let mut tabs_right_edge: f32 = 0.0;
                let mut inter_boundaries: Vec<f32> = Vec::new();
                let mut active_tab_box: Option<(f32, f32)> = None;
                let mut tx = h.x + 8.0 + strip_pad;
                for (i, tab) in tab_list.iter().enumerate().skip(first).take(n_vis) {
                    let tab_x0 = tx;
                    // This pane's active tab — gets the pill + focus strip + ×.
                    let active = tab_list.len() == 1 || i == h.active_tab;
                    let is_hover = hover_info
                        .as_ref()
                        .map(|(p, hi)| p == &h.id && *hi == i)
                        .unwrap_or(false);
                    // × on the active tab always; on inactive only while
                    // hovered. The width is reserved either way so hover
                    // doesn't shift the surrounding layout.
                    let show_x = active || is_hover;
                    let reserve_x = true;
                    let bright = active || is_hover;
                    // Tab being lifted in a cross-pane / reorder drag is
                    // drawn faint — reads as "in transit" against the
                    // insertion bar at the drop position.
                    let being_dragged = tab_drag_src
                        .as_ref()
                        .map(|(p, idx)| p == &h.id && *idx == i)
                        .unwrap_or(false);
                    let alpha_mul = if being_dragged { 0x55 } else { 0xFF };
                    let combine = |a: u8| ((a as u16 * alpha_mul as u16) / 0xFF) as u8;
                    // Per-pane accent (set via `surface.set_color`) recolors the
                    // tab-name text only; None = default chrome text. Brightness
                    // (active/hover) still rides on the alpha.
                    let label_fg = h.color.unwrap_or_else(theme::text);
                    let t_fg = if bright {
                        theme::with_alpha(label_fg, combine(0xFF))
                    } else {
                        theme::with_alpha(label_fg, combine(0x82))
                    };
                    let t_icon = if bright {
                        theme::with_alpha(theme::text_dim(), combine(0xFF))
                    } else {
                        theme::with_alpha(theme::text_dim(), combine(0x82))
                    };
                    // Truncate this tab's title to its share of the bar.
                    // × space is reserved on every tab — see `reserve_x`.
                    // No per-tab terminal glyph: the +button already signals
                    // "new shell"; doubling that icon on every tab was noise.
                    // File tabs reserve an extra icon slot (pop-out), left of ×.
                    // 터미널 탭도 undock(별도 OS 창) 아이콘을 같은 자리에 쓴다 —
                    // 이미지 pane 만 제외(뷰어라 별도창 대상 아님).
                    let can_popout = !h.is_image;
                    let popout_reserve = if can_popout { close_w + 4.0 } else { 0.0 };
                    let x_reserve = if reserve_x { close_w + 8.0 + popout_reserve } else { 0.0 };
                    let budget = (per_tab - x_reserve - 14.0).max(0.0);
                    let mut label = tab.to_string();
                    let mut lw = g.measure_chrome_text(&label, chrome_font, active);
                    if lw > budget {
                        while label.chars().count() > 1 {
                            label.pop();
                            lw = g.measure_chrome_text(&format!("{label}…"), chrome_font, active);
                            if lw <= budget {
                                break;
                            }
                        }
                        label.push('…');
                    }
                    // Pill geometry: label + reserved × slot (terminal icon
                    // removed — +button covers "new shell" duty).
                    let content_w = lw + x_reserve;
                    // First tab sits flush with the pane's left edge so the
                    // active tab's accent strip joins the pane divider with
                    // no visible gap — only while nothing is windowed off
                    // (the overflow chevron owns that sliver otherwise).
                    let box_x = if i == 0 && !overflowing { h.x } else { tab_x0 - 6.0 };
                    let box_right = tab_x0 + content_w + 6.0;
                    let tw = (box_right - box_x).max(0.0);
                    tab_edges.push(box_x);
                    if tabs_left.is_none() {
                        tabs_left = Some(box_x);
                    } else {
                        inter_boundaries.push(box_x);
                    }
                    tabs_right_edge = box_x + tw;
                    if active {
                        active_tab_box = Some((box_x, tw));
                    }
                    // Active tab keeps the band BG (= terminal body) — no
                    // fill — so the tab reads as continuous with the content
                    // below it. The accent top + broken bottom are what
                    // differentiate it. Structural lines drawn post-loop.
                    let stroke = 1.0_f32;
                    let _ = stroke;
                    let _ = t_icon;
                    let cx = g.draw_text(
                        tx,
                        text_y,
                        &label,
                        gpu::DrawOpts { font_size: chrome_font, color: t_fg, bold: active, italic: false },
                    );
                    // Pop-out icon (external-link): file tabs only, shown on the
                    // active or hovered tab. Sits left of the ×; clicking it
                    // moves the tab's editor into its own OS window.
                    let mut action_x = cx + 8.0;
                    if can_popout && show_x {
                        let po_x = action_x;
                        let chip = icon_size + 6.0;
                        let chip_x = po_x + (icon_size - chip) / 2.0;
                        let chip_y = h.y + (PANE_HEADER_HEIGHT - chip) / 2.0;
                        let (mx, my) = self.cursor_px;
                        let po_hover =
                            mx >= chip_x && mx <= chip_x + chip && my >= chip_y && my <= chip_y + chip;
                        if po_hover {
                            hover_rect(g, chip_x, chip_y, chip, chip, theme::radius_sm());
                        }
                        let pocol = if po_hover { theme::text() } else { t_icon };
                        g.queue_icon("external-link", po_x, icon_y, icon_size, pocol);
                        tab_popout_hits.push((h.id.clone(), i, (po_x - 2.0, h.y, icon_size + 4.0, PANE_HEADER_HEIGHT)));
                        action_x = po_x + close_w + 4.0;
                    }
                    if show_x {
                        let close_x = action_x;
                        // Hover chip behind the × — same lift the +button gets,
                        // so the close target reads as clickable on hover.
                        let chip = icon_size + 6.0;
                        let chip_x = close_x + (icon_size - chip) / 2.0;
                        let chip_y = h.y + (PANE_HEADER_HEIGHT - chip) / 2.0;
                        let (mx, my) = self.cursor_px;
                        let x_hover =
                            mx >= chip_x && mx <= chip_x + chip && my >= chip_y && my <= chip_y + chip;
                        if x_hover {
                            hover_rect(g, chip_x, chip_y, chip, chip, theme::radius_sm());
                        }
                        let xcol = if x_hover { theme::text() } else { t_icon };
                        g.queue_icon("x", close_x, icon_y, icon_size, xcol);
                        // × close hit (widen a little for an easy target).
                        tab_close_hits.push((h.id.clone(), i, (close_x - 2.0, h.y, icon_size + 4.0, PANE_HEADER_HEIGHT)));
                    }
                    // Whole-pill click/drag hit. Inactive tabs have no × inside,
                    // so the entire pill switches; the active tab's × is checked
                    // first by the handler.
                    tab_hits.push((h.id.clone(), i, (box_x, h.y, tw, PANE_HEADER_HEIGHT)));
                    tx = box_right + gap;
                }
                // Structural borders. Browser-tab pattern:
                //   - Top BORDER across the strip, with the active tab's
                //     segment painted in the focus color (same thickness).
                //   - Bottom BORDER across the strip but BROKEN under the
                //     active tab so the active opens straight into the body.
                //   - Vertical BORDER at each inter-tab boundary (single line
                //     shared between neighbours).
                // No outer left/right of the strip — the pane dividers fill
                // those roles, so leftmost-active never gets two stacked lines.
                if let Some(left) = tabs_left {
                    let stroke = 1.0_f32;
                    let band_w = (tabs_right_edge - left).max(0.0);
                    g.rect(left, h.y, band_w, stroke, theme::border());
                    // Bottom BORDER across the WHOLE pane header (tabs + plus
                    // button + action cluster), broken only under the active
                    // tab so it flows into the body.
                    let by = h.y + PANE_HEADER_HEIGHT - stroke;
                    let h_right = h.x + h.w;
                    if let Some((ax, aw)) = active_tab_box {
                        let lw = (ax - h.x).max(0.0);
                        g.rect(h.x, by, lw, stroke, theme::border());
                        let rx = ax + aw;
                        let rw = (h_right - rx).max(0.0);
                        g.rect(rx, by, rw, stroke, theme::border());
                    } else {
                        g.rect(h.x, by, h.w, stroke, theme::border());
                    }
                    for b in &inter_boundaries {
                        g.rect(*b, h.y, stroke, PANE_HEADER_HEIGHT, theme::border());
                    }
                    // Right edge of the strip — gives the last tab (often the
                    // active one when only the trailing tab is selected) a
                    // visible right boundary. Left edge is left to the pane
                    // divider so it never doubles up.
                    g.rect(tabs_right_edge - stroke, h.y, stroke, PANE_HEADER_HEIGHT, theme::border());
                    if let Some((ax, aw)) = active_tab_box {
                        let accent_col = if h.is_active { theme::accent() } else { theme::text() };
                        // accent 선은 BORDER stroke(1px)보다 살짝 굵게 — 활성 pane 강조.
                        g.rect(ax, h.y, aw, ACTIVE_ACCENT_STROKE, accent_col);
                        deferred_accents.push((ax, h.y, aw, accent_col));
                    }
                }
                // Drag insertion bar: 6px accent line spanning the strip.
                // 옛 2px는 Retina+at-speed drag에서 사실상 안 보였음.
                if let Some((ref dpane, target)) = tab_drag_info {
                    if *dpane == h.id {
                        // tab_edges holds visible tabs only — offset by `first`.
                        let bar_x = tab_edges
                            .get(target.saturating_sub(first))
                            .copied()
                            .unwrap_or(tx - gap);
                        g.rect(bar_x - 3.0, h.y + 1.0, 6.0, PANE_HEADER_HEIGHT - 2.0, theme::accent());
                    }
                }
                let (cur_x, cur_y) = self.cursor_px;
                let inside =
                    |rx: f32, ry: f32, rw: f32, rh: f32| cur_x >= rx && cur_x <= rx + rw && cur_y >= ry && cur_y <= ry + rh;
                // [+] new-tab button right after the tabs. Hover chip is a
                // tight rounded square centered on the glyph so the glow
                // hugs the icon instead of stretching across a tall band.
                // Hidden while a tab drag is active so the +button doesn't
                // sit on top of the insertion bar / accept a stray drop.
                let dragging_tab = tab_drag_src.is_some();
                let plus_iw = g.measure_chrome_text("\u{ea60}", icon_size, false);
                let chip_size = (icon_size + 6.0).max(plus_iw + 6.0);
                let chip_x = tx + (plus_iw - chip_size) / 2.0;
                let chip_y = h.y + (PANE_HEADER_HEIGHT - chip_size) / 2.0;
                let plus_rect = (chip_x, chip_y, chip_size, chip_size);
                let plus_hover = !dragging_tab && inside(plus_rect.0, plus_rect.1, plus_rect.2, plus_rect.3);
                if plus_hover {
                    hover_rect(g, plus_rect.0, plus_rect.1, plus_rect.2, plus_rect.3,
                        theme::radius_sm());
                }
                let plus_color = if plus_hover { theme::text() } else { act_fg };
                if !dragging_tab {
                    g.queue_icon("plus", tx, icon_y, icon_size, plus_color);
                    plus_hits.push((h.id.clone(), plus_rect));
                }
                // Overflow chevrons in the reserved end slots — more tabs
                // exist past this edge; the wheel over the strip scrolls.
                if overflowing {
                    let cis = 12.0_f32;
                    let ccy = h.y + (PANE_HEADER_HEIGHT - cis) / 2.0;
                    if first > 0 {
                        g.queue_icon("chevron-left", h.x + 4.0, ccy, cis, theme::text_mute());
                    }
                    if first + n_vis < n_tabs {
                        g.queue_icon(
                            "chevron-right",
                            tx + plus_iw + 8.0,
                            ccy,
                            cis,
                            theme::text_mute(),
                        );
                    }
                }
                // ── Right action buttons ── per-kind cluster: terminal panes
                // get new-terminal/web/split-v/split-h; image panes get
                // zoom-out / zoom-in / rotate / reset wired to the in-pane
                // image-view state mutated by forward_key as well.
                // Per cluster we carry either an ImageBtn (image pane) or an
                // ActionKind (terminal pane). Keeping both as Option in one
                // tuple keeps the paint loop unified.
                let action_set: Vec<(&str, Option<ImageBtn>, Option<ActionKind>)> = if h.is_image {
                    vec![
                        ("minus", Some(ImageBtn::ZoomOut), None),
                        ("plus", Some(ImageBtn::ZoomIn), None),
                        ("rotate-cw", Some(ImageBtn::Rotate), None),
                        ("maximize", Some(ImageBtn::Reset), None),
                    ]
                } else if h.is_web {
                    // 브라우저 컨트롤 — 뒤로/앞으로/새로고침/기본 브라우저로 열기.
                    // split·상태바 버튼은 웹 pane 에 안 맞아 통째로 갈아 끼운다.
                    // 로딩 중엔 리로드가 ×(정지)가 된다 — web_nav("reload") 가
                    // loading 을 보고 window.stop() 으로 간다(브라우저 관례).
                    let reload_icon = if h.web_loading { "x" } else { "rotate-cw" };
                    vec![
                        ("chevron-left", None, Some(ActionKind::WebBack)),
                        ("chevron-right", None, Some(ActionKind::WebForward)),
                        (reload_icon, None, Some(ActionKind::WebReload)),
                        ("external-link", None, Some(ActionKind::WebOpenExternal)),
                    ]
                } else if h.is_markdown {
                    // Markdown panes use a text "Rendered | Raw" segmented
                    // toggle (drawn below), not an icon cluster.
                    vec![]
                } else {
                    // The status-bar toggle reads "filled" (panel-bottom) when the
                    // bar is shown and "dashed" when it's collapsed, so the icon
                    // itself signals the current state. Visibility = global
                    // default flipped by the shown/hidden exception sets (mirrors
                    // `statusbar_visible`, inlined here under the gpu borrow).
                    let fvis = self.statusbar.shown.contains(&h.id)
                        || (!self.statusbar.hidden.contains(&h.id) && self.set_footer_default);
                    let sb_icon = if fvis { "panel-bottom" } else { "panel-bottom-dashed" };
                    vec![
                        (sb_icon, None, Some(ActionKind::ToggleStatusbar)),
                        ("columns-2", None, Some(ActionKind::SplitV)),
                        ("rows-2", None, Some(ActionKind::SplitH)),
                    ]
                };
                let mut bx = h.x + h.w - 8.0 - (abw * n_btn + agap * (n_btn - 1.0));
                for (ic, kind, action) in action_set {
                    let chip_size = icon_size + 6.0;
                    let chip_y = h.y + (PANE_HEADER_HEIGHT - chip_size) / 2.0;
                    let chip_x = bx + (abw - chip_size) / 2.0;
                    let hover = inside(chip_x, chip_y, chip_size, chip_size);
                    if hover {
                        hover_rect(g, chip_x, chip_y, chip_size, chip_size,
                            theme::radius_sm());
                    }
                    let color = if hover { theme::text() } else { act_fg };
                    g.queue_icon(
                        ic,
                        chip_x + (chip_size - icon_size) / 2.0,
                        chip_y + (chip_size - icon_size) / 2.0,
                        icon_size,
                        color,
                    );
                    if let Some(k) = kind {
                        image_btn_hits.push((h.id.clone(), k, (chip_x, chip_y, chip_size, chip_size)));
                    }
                    if let Some(a) = action {
                        pane_action_hits.push((h.id.clone(), a, (chip_x, chip_y, chip_size, chip_size)));
                    }
                    bx += abw + agap;
                }
                // ── 웹 pane 주소 pill ── 버튼 클러스터 왼쪽. 평소엔 현재 주소를
                // 흐리게 보여 주고, 클릭(또는 Cmd+L)하면 그 자리에서 인라인 편집
                // — 빈 버퍼 동안은 현재 주소가 자리표시자다(App.web_addr 주석).
                if addr_vis {
                    let ah = icon_size + 6.0;
                    let ay = h.y + (PANE_HEADER_HEIGHT - ah) / 2.0;
                    let ax = h.x + h.w - 8.0 - (abw * n_btn + agap * (n_btn - 1.0)) - 6.0
                        - addr_w;
                    let afont = 11.0_f32;
                    let tx = ax + 8.0;
                    let ty = ay + (ah - afont) / 2.0;
                    let clip_r = ax + addr_w - 8.0;
                    // 찾기 칸이 주소 pill 자리를 빌린다(서로 배타) — 접두 라벨과
                    // 자리표시자만 다르고 캐럿·클립 처리는 같다.
                    let finding = self
                        .web_find
                        .as_ref()
                        .filter(|e| e.pane == h.id)
                        .map(|e| (e.text.clone(), e.cursor));
                    let editing = if finding.is_some() {
                        None
                    } else {
                        self.web_addr
                            .as_ref()
                            .filter(|e| e.pane == h.id)
                            .map(|e| (e.text.clone(), e.cursor))
                    };
                    if let Some((text, cursor)) = finding {
                        round_rect(g, ax, ay, addr_w, ah, theme::radius_sm(), theme::bg());
                        let px = g.draw_text(
                            tx,
                            ty,
                            "찾기",
                            gpu::DrawOpts {
                                font_size: afont,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        ) + 6.0;
                        let (mut head, tail) = crate::lineedit::split(&text, cursor);
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_x = px + g.measure_chrome_text(&head, afont, false);
                        let shown = format!("{head}{tail}");
                        g.draw_text_clipped(
                            px,
                            ty,
                            &shown,
                            gpu::DrawOpts {
                                font_size: afont,
                                color: theme::text(),
                                bold: false,
                                italic: false,
                            },
                            px,
                            clip_r,
                        );
                        if commit_caret_on {
                            g.rect(
                                caret_x.min(clip_r),
                                ay + (ah - afont - 2.0) / 2.0,
                                1.5,
                                afont + 2.0,
                                theme::text(),
                            );
                        }
                    } else if let Some((text, cursor)) = editing {
                        // 편집 중: bg 로 가라앉혀 입력칸임을 보이고 캐럿을 세운다.
                        round_rect(g, ax, ay, addr_w, ah, theme::radius_sm(), theme::bg());
                        let (mut head, tail) = crate::lineedit::split(&text, cursor);
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_x = tx + g.measure_chrome_text(&head, afont, false);
                        let shown = format!("{head}{tail}");
                        if shown.is_empty() {
                            g.draw_text_clipped(
                                tx,
                                ty,
                                h.web_url.as_deref().unwrap_or(""),
                                gpu::DrawOpts {
                                    font_size: afont,
                                    color: theme::text_mute(),
                                    bold: false,
                                    italic: false,
                                },
                                tx,
                                clip_r,
                            );
                        } else {
                            g.draw_text_clipped(
                                tx,
                                ty,
                                &shown,
                                gpu::DrawOpts {
                                    font_size: afont,
                                    color: theme::text(),
                                    bold: false,
                                    italic: false,
                                },
                                tx,
                                clip_r,
                            );
                        }
                        if commit_caret_on {
                            g.rect(
                                caret_x.min(clip_r),
                                ay + (ah - afont - 2.0) / 2.0,
                                1.5,
                                afont + 2.0,
                                theme::text(),
                            );
                        }
                    } else {
                        let hover = inside(ax, ay, addr_w, ah);
                        g.hover_pointer |= hover;
                        round_rect(g, ax, ay, addr_w, ah, theme::radius_sm(), theme::surface());
                        if hover {
                            hover_rect(g, ax, ay, addr_w, ah, theme::radius_sm());
                        }
                        g.draw_text_clipped(
                            tx,
                            ty,
                            h.web_url.as_deref().unwrap_or(""),
                            gpu::DrawOpts {
                                font_size: afont,
                                color: if hover { theme::text() } else { theme::text_dim() },
                                bold: false,
                                italic: false,
                            },
                            tx,
                            clip_r,
                        );
                    }
                    pane_action_hits.push((
                        h.id.clone(),
                        ActionKind::WebAddress,
                        (ax, ay, addr_w, ah),
                    ));
                }
                // ── Markdown "Rendered | Raw" segmented toggle ── outer pill
                // with the active half filled; each half is its own hit rect so
                // a click sets that exact mode (vs flipping).
                if h.is_markdown {
                    let seg_h = icon_size + 6.0;
                    let seg_y = h.y + (PANE_HEADER_HEIGHT - seg_h) / 2.0;
                    let mut sx = h.x + h.w - 8.0 - seg_w;
                    round_rect(g, sx, seg_y, seg_w, seg_h, theme::radius_sm(), theme::surface());
                    let ty = seg_y + (seg_h - seg_font) / 2.0;
                    for (label, lw, raw) in
                        [("Rendered", md_rendered_w, false), ("Raw", md_raw_w, true)]
                    {
                        let cell_w = lw + seg_pad * 2.0;
                        let active = h.md_raw_mode == raw;
                        let hover = inside(sx, seg_y, cell_w, seg_h);
                        g.hover_pointer |= hover;
                        if active {
                            round_rect(g, sx, seg_y, cell_w, seg_h,
                                theme::radius_sm(), theme::surface_hover());
                        } else if hover {
                            hover_rect(g, sx, seg_y, cell_w, seg_h,
                                theme::radius_sm());
                        }
                        let color = if active { theme::text() } else { theme::text_dim() };
                        g.draw_text(
                            sx + seg_pad,
                            ty,
                            label,
                            gpu::DrawOpts { font_size: seg_font, color, bold: false, italic: false },
                        );
                        let act = if raw { ActionKind::MdRaw } else { ActionKind::MdRender };
                        pane_action_hits.push((h.id.clone(), act, (sx, seg_y, cell_w, seg_h)));
                        sx += cell_w;
                    }
                }
            }
            // Focus by contrast: unfocused panes fade their text only (via
            // PaneSlot.dim in draw_cells), not the whole box — no dark veil.
            // Ghostty-style: one hairline per interior split boundary, drawn
            // after the veil so the seam stays crisp on top. No per-pane box
            // border (that doubled into a thick seam between abutting panes
            // and read as caged tiles).
            for (sx, sy, sw, sh) in &pane_seams {
                g.rect(*sx, *sy, *sw, *sh, theme::border());
            }
            // Re-paint the active-tab accent strips so a horizontal pane
            // divider just above a pane doesn't wipe its accent color.
            for (ax, ay, aw, ac) in &deferred_accents {
                g.rect(*ax, *ay, *aw, ACTIVE_ACCENT_STROKE, *ac);
            }
            // ── ghostty식 pane 핸들(⋮) + active 보더 ───────────────────
            // 헤더 띠를 없앤 대신: ① active pane은 얇은 accent 보더로 강조
            // (비활성 dim과 함께 focus 단서) ② pane에 마우스를 올리면 우상단에
            // ⋮ 핸들이 떠서 클릭=메뉴(Phase 3)·드래그=이동(Phase 4) 진입점이 됨.
            // 설정 화면이 떠 있으면 pane 핸들·보더를 그리지 않는다 — 불투명 설정
            // backdrop 위로 ⋮ 가 비쳐 보이던 잔상(거노). hit-rect 도 비워 설정 영역
            // 클릭이 유령 핸들에 안 걸리게 한다.
            // active_pane + is_split + 헤더 보유 pane 집합을 한 번에 스냅샷 —
            // 루프 안에서 self를 재borrow하면 g(=&mut self.gpu)와 충돌하므로 미리
            // 모은다. statusbar 루프(아래)도 active 보더 inset 계산에 active_pane/
            // is_split을 쓰므로 settings 분기 밖, 더 넓은 스코프에 둔다.
            let is_split = footer_slots.len() > 1;
            // 테두리를 실제로 그린 pane 과 그 두께. 하단바(footer)는 나중에 그려지므로
            // 이 값만큼 안쪽으로 물러나야 테두리를 안 덮는다. 두 곳이 각자 조건을
            // 계산하면 반드시 어긋난다 — 줌 pane 은 테두리가 있는데 하단바는 그걸
            // 모르고 덮어 아래쪽만 끊겨 보였다(거노). 그린 쪽이 기록하고 덮는 쪽이 읽는다.
            let mut border_inset: HashMap<String, f32> = HashMap::new();
            // active_pane + pane 별 캐릭터명을 한 lock 으로 스냅샷 — 아래 pane 테두리
            // 루프가 g(=&mut self.gpu) 안이라 self 재borrow 불가. character_accent 폴백용.
            let (active_pane, pane_chars, tab_pids) = self
                .ws
                .lock()
                .ok()
                .map(|w| {
                    // 테두리 accent 도 표시 규칙(display_pane_char 인라인 — gpu 가변
                    // 차용 중) 공유 — 뷰 pane 은 파싱 전 스폰 랜덤 색을 두르지 않는다
                    // (거노: 진입 직후 다른 학생색).
                    let chars: HashMap<String, String> = w
                        .panes
                        .keys()
                        .filter_map(|id| {
                            // **키는 outer, 값은 활성 탭**. 테두리는 바깥 박스에 그리니
                            // 키는 leaf 여야 하고, 학생은 탭 pid 로 기록되니 값은 접어서
                            // 가져온다. 안 접으면 탭으로 띄운 학생이 무색으로 남는다.
                            let key = w.active_tab_pid(id);
                            self.pane_claude_sid
                                .get(&key)
                                .and_then(|sid| {
                                    kasa_mcp::character::session_character(sid)
                                })
                                .or_else(|| {
                                    let view = self
                                        .pty
                                        .get(&key)
                                        .map(|p| p.is_claude_agents())
                                        .unwrap_or(false);
                                    if view {
                                        None
                                    } else {
                                        w.pane_character.get(&key).cloned()
                                    }
                                })
                                .map(|c| (id.clone(), c))
                        })
                        .collect();
                    // outer → 활성 탭 pid. 아래 루프는 `g(=&mut self.gpu)` 를 잡고 있어
                    // `pty_for_pane` 같은 `&self` 메서드를 못 부른다 — 이 lock 한 번에
                    // 같이 떠 두고 거기서 필드 접근만 한다.
                    let tab_pids: HashMap<String, String> =
                        w.panes.keys().map(|id| (id.clone(), w.active_tab_pid(id))).collect();
                    (w.active_pane.clone(), chars, tab_pids)
                })
                .unwrap_or_default();
            // claude 가 foreground 인 pane 집합 — 테두리 게이트. 캐릭터는 pane spawn 시
            // 배정되지만(assign_character_env) 순수 셸엔 색을 안 씌우려면 타이틀바 학생
            // 이름과 동일 조건(active_process_name=="claude")을 써야 한다(거노: 클로드
            // 아니면 무테두리). active_process_name 은 500ms 캐시라 매 프레임 다중 pane
            // 호출도 가볍다. self.pty 접근이라 g(=&mut self.gpu) 잡은 루프 밖에서 스냅샷.
            let claude_panes: std::collections::HashSet<String> = footer_slots
                .iter()
                .filter(|(id, ..)| {
                    // outer 키가 아니라 활성 탭 pid — 탭에서 도는 클로드는 outer 로
                    // 안 잡혀 테두리 게이트를 통째로 못 지났다.
                    let key = tab_pids.get(id.as_str()).map_or(id.as_str(), String::as_str);
                    self.pty.get(key).and_then(|p| p.active_agent()).is_some()
                })
                .map(|(id, ..)| id.clone())
                .collect();
            // 줌 pane 은 claude 여부·split 여부와 무관하게 테두리를 두른다 — 줌의
            // 유일한 시각 단서라서(하단 dock 칩 하나로는 안 읽힌다). g(=&mut
            // self.gpu) 를 잡기 전에 스냅샷.
            let zoomed_now = self.zoomed_pane.clone();
            // 헤더를 실제로 그린 pane 집합 — 헤더 working bar 가 거기 뜨므로 footer 로딩바는
            // 이 pane 들을 건너뛴다. `ws.panes.has_header()` 가 아니라 방금 그린 `headers`
            // (pty_layout 기반)에서 뽑아야 ws.panes↔pty_layout 데싱크로 한 pane 에 헤더(위)·
            // footer(아래) 스윕바가 동시에 뜨는 "로딩바 두개" 버그가 안 난다(거노).
            let headered: std::collections::HashSet<String> =
                headers.iter().map(|h| h.id.clone()).collect();
            {
                let (hmx, hmy) = self.cursor_px;
                let accent = theme::accent_color(theme::accent_name());
                let anim_phase = anim_phase_secs();
                let mut handle_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                let mut zones: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                let mut menu_hits: Vec<(ActionKind, (f32, f32, f32, f32))> = Vec::new();
                const HANDLE: f32 = 22.0;
                const HMARGIN: f32 = 5.0;
                for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                    // pane 테두리 — 포커스된(active) claude pane 만 자기 학생 고정색
                    // 테두리(지금 어느 pane 을 보고 있는지 한눈에). 비활성·순수 셸은
                    // 무테두리 — 여러 pane 이 동시에 테두리를 둘러 지저분하던 걸 정리(거노).
                    let zoom_focus = zoomed_now.as_deref() == Some(fid.as_str());
                    if zoom_focus
                        || (is_split
                            && active_pane.as_deref() == Some(fid.as_str())
                            && claude_panes.contains(fid.as_str()))
                    {
                        // agents 목록 뷰는 SCHALE 블루 고정, 그 외엔 배정 학생색.
                        let border_col = if agents_view_panes.contains(fid.as_str()) {
                            theme::character_accent("샬레")
                        } else {
                            pane_chars
                                .get(fid.as_str())
                                // 학생색은 claude 가 도는 pane 에만 — 순수 셸을
                                // 줌했을 때 남의 학생색이 둘러지면 「저 pane 에
                                // 누가 있다」로 잘못 읽힌다. 줌은 accent 로.
                                .filter(|_| claude_panes.contains(fid.as_str()))
                                .and_then(|n| {
                                    theme::character_accent_n(
                                        n,
                                        theme::character_ordinal(&pane_chars, fid),
                                    )
                                })
                                // 줌은 학생이 없는 순수 셸에서도 테두리가 있어야
                                // 한다 — 없으면 줌 자체가 안 보인다.
                                .or_else(|| {
                                    zoom_focus
                                        .then(|| theme::accent_color(theme::accent_name()))
                                })
                        };
                        if let Some(col) = border_col {
                            // 줌은 조금 두껍게 — 여백 위에 홀로 뜬 카드의 윤곽선이다.
                            let t = if zoom_focus { 2.0_f32 } else { 1.5_f32 };
                            g.rect(*fx, *fy, *fw, t, col);
                            g.rect(*fx, fy + fbox_h - t, *fw, t, col);
                            g.rect(*fx, *fy, t, *fbox_h, col);
                            g.rect(fx + fw - t, *fy, t, *fbox_h, col);
                            border_inset.insert(fid.clone(), t);
                        }
                    }
                    // 로딩바 — claude 작업 중(pane_activity working)일 때 box 상단
                    // 얇은 스윕바. 헤더 띠 폐기 후 일반 pane 의 유일한 진행 표시(거노).
                    // 학생이름은 타이틀바(claude 실행 시), 로딩바는 working 시 — 역할 분리.
                    // compact 중이면 쓸림 대신 왼쪽부터 채워지는 바 — 헤더 pane 과 같은
                    // 형태 언어다. working 만 보던 시절엔 status 가 "compacting" 으로
                    // 좁혀지는 순간 헤더 없는 pane 은 표시가 통째로 사라졌다.
                    if !headered.contains(fid.as_str()) {
                        const BAR_H: f32 = 2.5;
                        let (st, pct) = self
                            .pane_activity
                            .get(fid)
                            .map_or(("", None), |a| (a.status.as_str(), a.compact_pct));
                        if st == "compacting" {
                            if let Some(p) = pct {
                                // 화면의 `▰▰▱ N%` 그대로 — 진짜 진행률(2026-08-13
                                // 지시)을 칸 게이지 + 숫자로(2026-08-15 지시, 헤더
                                // pane 과 같은 형태 언어). 숫자는 게이지 아래
                                // 오른쪽 끝 — compact 중에만 잠깐 얹힌다.
                                let cf = 10.0;
                                let label = format!("{p}%");
                                let lw = g.measure_chrome_text(&label, cf, true);
                                draw_compact_cells(g, *fx, *fy, *fw, BAR_H, accent, p);
                                g.draw_text(
                                    fx + fw - lw - 4.0,
                                    fy + BAR_H + 2.0,
                                    &label,
                                    gpu::DrawOpts {
                                        font_size: cf,
                                        color: accent,
                                        bold: true,
                                        italic: false,
                                    },
                                );
                            } else {
                                g.rect(*fx, *fy, *fw, BAR_H, theme::with_alpha(accent, 0x2e));
                                g.compact_bar(*fx, *fy, *fw, BAR_H, accent);
                            }
                        } else if st == "working" {
                            g.rect(*fx, *fy, *fw, BAR_H, theme::with_alpha(accent, 0x2e));
                            let seg = (fw * 0.32).clamp(36.0, 160.0);
                            let span = fw + seg;
                            let off = (anim_phase * 0.5).fract() * span - seg;
                            let sx = (fx + off).max(*fx);
                            let ex = (fx + off + seg).min(fx + fw);
                            if ex > sx {
                                g.rect(sx, *fy, ex - sx, BAR_H, accent);
                            }
                        }
                    }
                    // 손을 기다리는 pane — 네 변이 핑크로 깜빡인다(거노: "내가
                    // 엔터해야되거나 그런거는 핑크색으로 깜빡이게"). 로딩바(숨쉬기)
                    // 와 **뜻이 정반대**라 형태부터 갈랐다: 스윕바는 "놔둬도 진행
                    // 된다", 이 테두리는 "내가 손대야 풀린다". 상태가 배타적이라
                    // (working ≠ waiting) 둘이 한 pane 에 같이 뜨지 않는다.
                    //
                    // 포커스 테두리(학생색) 위에 덧그린다 — 지금 보고 있는 pane 이
                    // 물어보고 멈춘 경우, 급한 쪽이 이겨야 한다. border_inset 도
                    // 다시 적어 하단바가 이 테두리를 덮지 않게 한다.
                    // 메서드(`pane_needs_you`)를 쓰면 `&self` 를 통째로 빌려 렌더 루프의
                    // 가변 대여와 부딪힌다 — 필드만 집어 자유함수로 판정한다.
                    if self
                        .pane_activity
                        .get(fid)
                        .is_some_and(|a| crate::chrome::status_needs_you(&a.status))
                    {
                        let mut col = theme::attention();
                        col[3] = (90.0 + 165.0 * breathe(anim_phase, 1.1)) as u8;
                        let t = 2.0_f32;
                        g.rect(*fx, *fy, *fw, t, col);
                        g.rect(*fx, fy + fbox_h - t, *fw, t, col);
                        g.rect(*fx, *fy, t, *fbox_h, col);
                        g.rect(fx + fw - t, *fy, t, *fbox_h, col);
                        let inset = border_inset.entry(fid.clone()).or_insert(t);
                        *inset = inset.max(t);
                    }
                    // 헤더 있는 pane(image/md/탭 2개+)은 헤더에 컨트롤이 다 있으니
                    // ··· 핸들은 생략한다 — 중복 진입점 제거. 단 메뉴 자체는 그린다:
                    // 헤더 우클릭이 이 메뉴를 열어 상단바를 다시 접는 유일한 입구다
                    // (2026-08-13 지적: 점3개로 만든 헤더를 되돌릴 길이 없었다).
                    let is_headered = headered.contains(fid.as_str());
                    let hx = fx + (fw - HANDLE) / 2.0;
                    let hy = fy + HMARGIN;
                    if !is_headered {
                        // ⋮ 핸들 — 상단 중앙. 평소엔 완전히 숨김. pane 상단 30% 띠에
                        // 커서가 들어오면 흐릿하게 등장하고, ⋮ 바로 위로 가면 진해진다
                        // (그때 손모양 커서 — handler 측). 클릭=메뉴·드래그=이동.
                        let on_handle = hmx >= hx && hmx <= hx + HANDLE
                            && hmy >= hy && hmy <= hy + HANDLE;
                        let zone_h = fbox_h * 0.30;
                        let in_zone = hmx >= *fx && hmx <= fx + fw
                            && hmy >= *fy && hmy <= fy + zone_h;
                        let isz = 16.0_f32;
                        // glow/chip 없이 ⋮ 아이콘 자체만 숨김→흐릿→진함 3단계.
                        if on_handle || in_zone {
                            g.queue_icon("ellipsis-horizontal",
                                hx + (HANDLE - isz) / 2.0, hy + (HANDLE - isz) / 2.0, isz,
                                if on_handle { theme::text() } else { theme::with_alpha(theme::text(), 0x66) });
                        }
                        handle_rects.push((fid.clone(), (hx, hy, HANDLE, HANDLE)));
                        zones.push((fid.clone(), (*fx, *fy, *fw, zone_h)));
                    }
                    // ⋮ 메뉴 열림 → 이 pane ⋮ 아래 버튼3개(좌우분할·상하분할·닫기).
                    if self.handle_menu.as_deref() == Some(fid.as_str()) {
                        // 상태바(footer) 토글 아이콘은 현재 표시 상태를 그대로
                        // 드러낸다 — 보이면 panel-bottom, 접혀 있으면 dashed.
                        let fvis = self.statusbar.shown.contains(fid.as_str())
                            || (!self.statusbar.hidden.contains(fid.as_str()) && self.set_footer_default);
                        let sb_icon = if fvis { "panel-bottom" } else { "panel-bottom-dashed" };
                        // 상단바(헤더 띠)도 같은 방식 — 지금 보이는 상태를 아이콘이
                        // 그대로 드러낸다. hdr_vis 는 has_header() 와 같은 답이어야
                        // 하므로 pane 에 직접 물어본다(override 포함).
                        let hdr_vis = {
                            let ws = self.ws.lock().unwrap();
                            ws.panes.get(fid.as_str()).is_some_and(|p| p.has_header())
                        };
                        let hdr_icon = if hdr_vis { "panel-top" } else { "panel-top-dashed" };
                        let items = [
                            ("plus", ActionKind::NewTab),
                            // columns-2(세로선=좌우 2칸) → Horizontal(right),
                            // rows-2(가로선=상하 2칸) → Vertical(bottom). 아이콘이
                            // 곧 결과 배치다 — SplitDir 이름과는 반대 매핑.
                            ("columns-2", ActionKind::SplitH),
                            ("rows-2", ActionKind::SplitV),
                            // 별도창 — 헤더 없는 pane 의 유일한 undock 진입점
                            // (2026-08-13 지적 「점3개 메뉴에 별도창 버튼도 없고」).
                            ("external-link", ActionKind::Undock),
                            (hdr_icon, ActionKind::ToggleHeader),
                            (sb_icon, ActionKind::ToggleStatusbar),
                            ("maximize", ActionKind::ToggleZoom),
                            ("rotate-cw", ActionKind::RefreshRenderer),
                            ("x", ActionKind::Close),
                        ];
                        let bw = 30.0_f32;
                        let bh = 28.0_f32;
                        let gap = 2.0_f32;
                        let pad = 4.0_f32;
                        let n = items.len() as f32;
                        let mw = pad * 2.0 + bw * n + gap * (n - 1.0);
                        let mh = bh + pad * 2.0;
                        let mut mx = hx + HANDLE / 2.0 - mw / 2.0;
                        // pane 가장자리 안으로 클램프(좌측/우측 끝 pane). 단 메뉴가
                        // pane 보다 넓으면(좁은 3분할 + 아이콘 8개) 좌우 경계가 서로를
                        // 뒤집어 메뉴를 창 밖으로 밀어낸다 — 그땐 창 기준으로 물러선다.
                        // 메뉴는 pane 위에 뜨는 오버레이라 옆 pane 을 덮는 건 무방하다.
                        let (lo, hi) = if mw + 4.0 <= *fw {
                            (*fx + 2.0, *fx + *fw - mw - 2.0)
                        } else {
                            (2.0, (win_px.0 / scale - mw - 2.0).max(2.0))
                        };
                        mx = mx.clamp(lo, hi);
                        // 앵커: ⋮ 핸들 아래 / 헤더 pane 은 헤더 띠 바로 아래(핸들이
                        // 없고, 띠 위에 겹치면 우클릭한 자리가 가려진다).
                        let my = if is_headered {
                            fy + PANE_HEADER_HEIGHT + 3.0
                        } else {
                            hy + HANDLE + 3.0
                        };
                        round_rect(g, mx, my, mw, mh, theme::radius_sm(), theme::border());
                        round_rect(g, mx + 1.0, my + 1.0, mw - 2.0, mh - 2.0,
                            theme::radius_sm() - 1.0, theme::surface_hover());
                        let mut bx2 = mx + pad;
                        let by2 = my + pad;
                        for (icon, act) in items {
                            let on = hmx >= bx2 && hmx <= bx2 + bw && hmy >= by2 && hmy <= by2 + bh;
                            if on {
                                round_rect(g, bx2, by2, bw, bh, theme::radius_sm(), theme::surface_active());
                            }
                            let bisz = 16.0_f32;
                            g.queue_icon(icon, bx2 + (bw - bisz) / 2.0, by2 + (bh - bisz) / 2.0, bisz,
                                if on { theme::text() } else { theme::text_dim() });
                            menu_hits.push((act, (bx2, by2, bw, bh)));
                            bx2 += bw + gap;
                        }
                    }
                }
                self.pane_handle_rects = handle_rects;
                self.pane_top_zones = zones;
                self.handle_menu_hits = menu_hits;
            }
            // Per-pane status bar at the foot of each pane box: cwd + branch
            // chips (click → cd / checkout dropdowns) on the left, ± diff on
            // the right. The gpu borrow rules out &self method calls in here,
            // so visibility / cwd / badge all read the fields directly.
            self.statusbar.path_rects.clear();
            self.statusbar.branch_rects.clear();
            self.statusbar.toggle_rects.clear();
            self.statusbar.diff_rects.clear();
            let (sb_mx, sb_my) = self.cursor_px;
            for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                let fvis = self.statusbar.shown.contains(fid)
                    || (!self.statusbar.hidden.contains(fid) && self.set_footer_default);
                if !fvis || *fbox_h < pane_footer_h + 4.0 {
                    continue;
                }
                let bar_y = fy + fbox_h - pane_footer_h;
                // 테두리를 footer 배경이 덮지 않게 좌우·하단을 그 두께만큼 안쪽으로
                // 그린다 — 안 그러면 나중에 그려지는 footer bg 가 보더의 하단·좌우 끝을
                // 덮어 "선이 하단바를 제외하고 감싸는" 것처럼 보인다(거노). 두께는
                // 실제로 그린 쪽이 남긴 값을 쓴다(줌은 2.0, 분할 active 는 1.5).
                let bt = border_inset.get(fid.as_str()).copied().unwrap_or(0.0);
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, pane_footer_h - bt, theme::bg());
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, 1.0, theme::border());
                // Pill metrics shared by every chip. 12/13 은 앱을 통틀어 가장 작은
                // 글자·아이콘이었다 — 같은 화면의 사이드바(13~14)와 나란히 놓이니
                // 하단바만 축소된 것처럼 읽혔다(거노). 본문 단과 같은 단으로 올린다.
                let pill_h = 22.0_f32;
                let pill_y = bar_y + (pane_footer_h - pill_h) / 2.0;
                let icon_sz = 14.0_f32;
                let pad_x = 9.0_f32;
                let icon_gap = 6.0_f32;
                let chip_gap = 7.0_f32;
                let font = 13.0_f32;
                let txt_y = pill_y + (pill_h - font) / 2.0;
                let footer_hover = sb_my >= bar_y
                    && sb_my <= bar_y + pane_footer_h
                    && sb_mx >= *fx
                    && sb_mx <= fx + fw;
                let mut cx = fx + 8.0;
                let cwd = self.pane_cwd_cache.get(fid).cloned();
                // Home-relative cwd (~/…), matching the screenshot's breadcrumb.
                let disp = cwd
                    .as_ref()
                    .map(|p| nfc_hangul(&crate::session::tilde_home(&p.to_string_lossy())))
                    .unwrap_or_else(|| "—".to_string());
                // cwd pill — folder icon + path.
                {
                    let tw = g.measure_chrome_text(&disp, font, false);
                    let pw = pad_x + icon_sz + icon_gap + tw + pad_x;
                    let hov = sb_mx >= cx
                        && sb_mx <= cx + pw
                        && sb_my >= pill_y
                        && sb_my <= pill_y + pill_h;
                    round_rect(g, cx, pill_y, pw, pill_h, theme::radius_sm(), theme::border());
                    round_rect(g, cx + 1.0, pill_y + 1.0, pw - 2.0, pill_h - 2.0,
                        theme::radius_sm() - 1.0,
                        if hov { theme::surface_active() } else { theme::surface_hover() });
                    g.queue_icon("folder", cx + pad_x, pill_y + (pill_h - icon_sz) / 2.0, icon_sz, theme::text_dim());
                    g.draw_text(cx + pad_x + icon_sz + icon_gap, txt_y, &disp,
                        gpu::DrawOpts { font_size: font, color: theme::text(), bold: false, italic: false });
                    self.statusbar.path_rects.push((fid.clone(), (cx, pill_y, pw, pill_h)));
                    cx += pw + chip_gap;
                }
                if let Some(badge) = cwd
                    .as_ref()
                    .and_then(|p| self.window_git.lock().ok().and_then(|m| m.get(p).cloned()))
                {
                    // branch pill — git-branch icon + branch name.
                    {
                        let tw = g.measure_chrome_text(&badge.branch, font, false);
                        let pw = pad_x + icon_sz + icon_gap + tw + pad_x;
                        let hov = sb_mx >= cx
                            && sb_mx <= cx + pw
                            && sb_my >= pill_y
                            && sb_my <= pill_y + pill_h;
                        round_rect(g, cx, pill_y, pw, pill_h, theme::radius_sm(), theme::border());
                        round_rect(g, cx + 1.0, pill_y + 1.0, pw - 2.0, pill_h - 2.0,
                            theme::radius_sm() - 1.0,
                            if hov { theme::surface_active() } else { theme::surface_hover() });
                        g.queue_icon("git-branch", cx + pad_x, pill_y + (pill_h - icon_sz) / 2.0, icon_sz, theme::text_dim());
                        g.draw_text(cx + pad_x + icon_sz + icon_gap, txt_y, &badge.branch,
                            gpu::DrawOpts { font_size: font, color: theme::text(), bold: false, italic: false });
                        self.statusbar.branch_rects.push((fid.clone(), (cx, pill_y, pw, pill_h)));
                        cx += pw + chip_gap;
                    }
                    // diff pill — file icon + "N · +ins −del" (green / red).
                    if badge.files > 0 || badge.insertions > 0 || badge.deletions > 0 {
                        let files_s = badge.files.to_string();
                        let dot_s = " · ";
                        let plus_s = format!("+{}", badge.insertions);
                        let gap_s = " ";
                        let minus_s = format!("−{}", badge.deletions);
                        let content = g.measure_chrome_text(&files_s, font, false)
                            + g.measure_chrome_text(dot_s, font, false)
                            + g.measure_chrome_text(&plus_s, font, false)
                            + g.measure_chrome_text(gap_s, font, false)
                            + g.measure_chrome_text(&minus_s, font, false);
                        let pw = pad_x + icon_sz + icon_gap + content + pad_x;
                        let hov = sb_mx >= cx
                            && sb_mx <= cx + pw
                            && sb_my >= pill_y
                            && sb_my <= pill_y + pill_h;
                        round_rect(g, cx, pill_y, pw, pill_h, theme::radius_sm(), theme::border());
                        round_rect(g, cx + 1.0, pill_y + 1.0, pw - 2.0, pill_h - 2.0,
                            theme::radius_sm() - 1.0,
                            if hov { theme::surface_active() } else { theme::surface_hover() });
                        g.queue_icon("file-text", cx + pad_x, pill_y + (pill_h - icon_sz) / 2.0, icon_sz, theme::text_dim());
                        let mut tx = cx + pad_x + icon_sz + icon_gap;
                        tx = g.draw_text(tx, txt_y, &files_s,
                            gpu::DrawOpts { font_size: font, color: theme::text(), bold: false, italic: false });
                        tx = g.draw_text(tx, txt_y, dot_s,
                            gpu::DrawOpts { font_size: font, color: theme::text_mute(), bold: false, italic: false });
                        tx = g.draw_text(tx, txt_y, &plus_s,
                            gpu::DrawOpts { font_size: font, color: theme::success(), bold: false, italic: false });
                        tx = g.draw_text(tx, txt_y, gap_s,
                            gpu::DrawOpts { font_size: font, color: theme::text_mute(), bold: false, italic: false });
                        g.draw_text(tx, txt_y, &minus_s,
                            gpu::DrawOpts { font_size: font, color: theme::danger(), bold: false, italic: false });
                        self.statusbar.diff_rects.push((fid.clone(), (cx, pill_y, pw, pill_h)));
                        cx += pw + chip_gap;
                    }
                }
                let _ = cx;
                // Collapse handle — surfaced only on footer hover so the resting
                // bar matches the screenshot (chips only). Right edge.
                if footer_hover {
                    let h_sz = 13.0;
                    let h_x = fx + fw - h_sz - 8.0;
                    let h_y = bar_y + (pane_footer_h - h_sz) / 2.0;
                    let h_hover = sb_mx >= h_x - 4.0
                        && sb_mx <= h_x + h_sz + 4.0
                        && sb_my >= bar_y
                        && sb_my <= bar_y + pane_footer_h;
                    if h_hover {
                        hover_rect(g, h_x - 4.0, h_y - 2.0, h_sz + 8.0, h_sz + 4.0,
                            theme::radius_sm());
                    }
                    g.queue_icon("chevrons-down-up", h_x, h_y, h_sz,
                        if h_hover { theme::text() } else { theme::text_mute() });
                    self.statusbar.toggle_rects
                        .push((fid.clone(), (h_x - 4.0, bar_y, h_sz + 12.0, pane_footer_h)));
                }
            }
            Self::paint_gpu_overlays(g, &overlay);
            // Status-bar dropdown (directory picker / branch switcher), drawn
            // last so it overlays the cell grid + every bar. Anchored to the
            // chip that opened it and expanded UPWARD — the bar lives at the
            // pane's bottom, so a downward menu would fall off the edge.
            self.statusbar.menu_dir_rects.clear();
            self.statusbar.menu_branch_rects.clear();
            if let Some((menu_pid, kind)) = self.statusbar.menu.clone() {
                let anchor = match kind {
                    StatusbarMenu::Path => self
                        .statusbar.path_rects
                        .iter()
                        .find(|(p, _)| *p == menu_pid)
                        .map(|(_, r)| *r),
                    StatusbarMenu::Branch => self
                        .statusbar.branch_rects
                        .iter()
                        .find(|(p, _)| *p == menu_pid)
                        .map(|(_, r)| *r),
                };
                if let Some((ax, ay, _aw, _ah)) = anchor {
                    // Item labels (and the value each row carries on click).
                    // Dir names normalized NFC so macOS-decomposed Hangul reads
                    // as composed syllables, not scattered jamo.
                    let is_path = matches!(kind, StatusbarMenu::Path);
                    let labels: Vec<String> = match kind {
                        StatusbarMenu::Path => self
                            .statusbar.menu_dirs
                            .iter()
                            .enumerate()
                            .map(|(i, p)| {
                                if i == 0 {
                                    ".. (상위 폴더)".to_string()
                                } else {
                                    nfc_hangul(p.file_name().and_then(|s| s.to_str()).unwrap_or("?"))
                                }
                            })
                            .collect(),
                        StatusbarMenu::Branch => self.statusbar.menu_branches.clone(),
                    };
                    // Live-search filter (path picker only). Inlined as field
                    // reads — the gpu borrow (`g`) rules out &self method calls.
                    let q = self.statusbar.menu_search.to_lowercase();
                    let fidx: Vec<usize> = if is_path {
                        self.statusbar.menu_dirs
                            .iter()
                            .enumerate()
                            .filter(|(i, p)| {
                                q.is_empty()
                                    || *i == 0
                                    || p.file_name()
                                        .and_then(|s| s.to_str())
                                        .map(|s| nfc_hangul(s).to_lowercase().contains(&q))
                                        .unwrap_or(false)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    } else {
                        (0..labels.len()).collect()
                    };
                    let item_h = if is_path { 28.0 } else { 24.0 };
                    // Search field band at the top of the path picker.
                    let search_h = if is_path { 34.0 } else { 0.0 };
                    let max_rows = 12usize;
                    let total = fidx.len();
                    let view_rows = total.min(max_rows);
                    let menu_w = if is_path { 300.0_f32 } else { 240.0_f32 };
                    let menu_h = search_h + item_h * view_rows.max(1) as f32 + 8.0;
                    let menu_x = ax.min((win_px.0 / scale) - menu_w - 8.0).max(4.0);
                    let menu_y = (ay - menu_h - 2.0).max(TITLE_HEIGHT + 2.0);
                    // Whole-row scroll: 이 메뉴는 클립을 안 세우므로 반쪽 행이
                    // 둥근 모서리 밖으로 삐져나간다. 휠 오프셋을 행 단위로 스냅해
                    // 정수 행씩 넘긴다. 시저를 세워도 되지만 둥근 모서리는 시저의
                    // 직사각형으로 못 흉내 낸다 — 모서리에서 각지게 잘린다.
                    let overflow = total.saturating_sub(view_rows);
                    let scroll = self.statusbar.menu_scroll.clamp(0.0, overflow as f32 * item_h);
                    self.statusbar.menu_scroll = scroll;
                    let first = ((scroll / item_h).round() as usize).min(overflow);
                    self.statusbar.menu_rect = Some((menu_x, menu_y, menu_w, menu_h));
                    panel_rect_outlined(g, menu_x, menu_y, menu_w, menu_h, theme::radius_md(), theme::surface());
                    let rows_top = menu_y + 4.0 + search_h;
                    // Inset search field + live query (or dim placeholder). Typing
                    // anywhere while the picker is open feeds this (forward_key).
                    if is_path {
                        let fy = menu_y + 6.0;
                        let fh = search_h - 8.0;
                        round_rect(g, menu_x + 8.0, fy, menu_w - 16.0, fh, theme::radius_sm(), theme::bg());
                        g.queue_icon("folder-tree", menu_x + 16.0, fy + (fh - 14.0) / 2.0, 14.0, theme::text_dim());
                        let (mut head, tail) = crate::lineedit::split(
                            &self.statusbar.menu_search,
                            self.statusbar.menu_search_cursor,
                        );
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_w = g.measure_chrome_text(&head, 13.0, false);
                        let shown = format!("{head}{tail}");
                        let (txt, col) = if shown.is_empty() {
                            ("디렉터리 검색…".to_string(), theme::text_mute())
                        } else {
                            (shown, theme::text())
                        };
                        g.draw_text(menu_x + 38.0, fy + (fh - 13.0) / 2.0, &txt,
                            gpu::DrawOpts { font_size: 13.0, color: col, bold: false, italic: false });
                        // 이 칸엔 캐럿이 없었다 — 끝에만 붙는 칸이라 커서가 어딘지
                        // 물을 일이 없었기 때문이다. 이제 가운데를 고칠 수 있으니
                        // 자리를 보여 줘야 한다.
                        if commit_caret_on {
                            g.rect(menu_x + 38.0 + caret_w, fy + (fh - 14.0) / 2.0, 1.5, 14.0, theme::text());
                        }
                    }
                    if total == 0 {
                        g.draw_text(menu_x + 16.0, rows_top + 4.0, "(없음)",
                            gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                    }
                    let current_branch = matches!(kind, StatusbarMenu::Branch)
                        .then(|| {
                            self.pane_cwd_cache
                                .get(&menu_pid)
                                .and_then(|p| self.window_git.lock().ok().and_then(|m| m.get(p).map(|b| b.branch.clone())))
                        })
                        .flatten();
                    let font = if is_path { 13.0 } else { 12.0 };
                    for vis in 0..view_rows {
                        let Some(&i) = fidx.get(first + vis) else { break };
                        let Some(label) = labels.get(i) else { break };
                        let iy = rows_top + vis as f32 * item_h;
                        let row = (menu_x, iy, menu_w, item_h);
                        let hover = sb_mx >= row.0
                            && sb_mx <= row.0 + row.2
                            && sb_my >= row.1
                            && sb_my <= row.1 + row.3;
                        // Hovered row = bright accent fill (cursor's selected-item
                        // cue); its glyphs flip to dark for contrast.
                        if hover {
                            round_rect(g, row.0 + 2.0, row.1, row.2 - 4.0, row.3, theme::radius_sm(), theme::accent());
                        }
                        let is_current = current_branch.as_deref() == Some(label.as_str());
                        let mut text_x = menu_x + 12.0;
                        // Path picker: leading ↑ / folder / file icon per row.
                        if is_path {
                            let is_parent = i == 0;
                            let is_dir = is_parent
                                || self.statusbar.menu_dirs.get(i).map(|p| p.is_dir()).unwrap_or(false);
                            let glyph = if is_parent { "arrow-up" } else if is_dir { "folder" } else { "file" };
                            let icon_c = if hover { theme::bg() } else { theme::text_dim() };
                            g.queue_icon(glyph, text_x, iy + (item_h - 15.0) / 2.0, 15.0, icon_c);
                            text_x += 15.0 + 9.0;
                        }
                        let color = if hover {
                            theme::bg()
                        } else if is_current {
                            theme::accent()
                        } else {
                            theme::text()
                        };
                        g.draw_text(
                            text_x,
                            iy + (item_h - font) / 2.0,
                            label,
                            gpu::DrawOpts { font_size: font, color, bold: is_current, italic: false },
                        );
                        match kind {
                            StatusbarMenu::Path => self
                                .statusbar.menu_dir_rects
                                .push((self.statusbar.menu_dirs[i].clone(), row)),
                            StatusbarMenu::Branch => self
                                .statusbar.menu_branch_rects
                                .push((label.clone(), row)),
                        }
                    }
                    // Scrollbar — thin thumb on the right edge so overflow is
                    // visible; only when the list exceeds the viewport.
                    if overflow > 0 {
                        let track_x = menu_x + menu_w - 4.0;
                        let track_y = rows_top;
                        let track_h = view_rows as f32 * item_h;
                        let thumb_h = (track_h * view_rows as f32 / total as f32).max(18.0);
                        let thumb_y = track_y
                            + (track_h - thumb_h) * (first as f32 / overflow as f32);
                        pill_rect(g, track_x, thumb_y, 3.0, thumb_h, theme::with_alpha(theme::text(), 0x55));
                    }
                } else {
                    self.statusbar.menu_rect = None;
                }
            } else {
                self.statusbar.menu_rect = None;
            }
            // "복사됨" toast, bottom-center, brief fade after a block copy.
            if toast_alpha > 0.0 {
                let msg = "복사됨";
                let t_font = 13.0_f32;
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let text_w = g.measure_chrome_text(msg, t_font, false);
                let (px, py) = (14.0_f32, 8.0_f32);
                let box_w = text_w + px * 2.0;
                let box_h = t_font + py * 2.0;
                let bx = (win_w - box_w) / 2.0;
                let by = win_h - box_h - 24.0;
                let a = (235.0 * toast_alpha).round() as u8;
                round_rect(
                    g,
                    bx,
                    by,
                    box_w,
                    box_h,
                    theme::radius_md(),
                    theme::with_alpha(theme::surface_active(), a),
                );
                let ta = (255.0 * toast_alpha).round() as u8;
                g.draw_text(
                    bx + px,
                    by + py,
                    msg,
                    gpu::DrawOpts {
                        font_size: t_font,
                        color: theme::with_alpha(theme::success(), ta),
                        bold: true,
                        italic: false,
                    },
                );
            }
            // Collab completion toast, top-right: a sibling pane flipped
            // working→idle. Top-right so it never collides with the
            // bottom-center copy pill; longer hold (a sibling finishing is worth
            // a glance). Tap the board button to clear the unread badge.
            self.collab.toast_rect = None;
            self.collab.toast_approve_rect = None;
            self.collab.toast_deny_rect = None;
            if collab_toast_alpha > 0.0 {
                if let Some(msg) = collab_toast_msg.as_ref() {
                    let t_font = 13.0_f32;
                    let win_w = win_px.0 / scale;
                    let text_w = g.measure_chrome_text(msg, t_font, true);
                    let (px, py) = (14.0_f32, 8.0_f32);
                    // 승인 모드(sticky)면 텍스트 뒤에 [승인][거부] 칩이 붙는다 —
                    // 박스 폭에 미리 반영. (munder 승인 카드 축소판)
                    let chip_f = 12.0_f32;
                    let chip_pad = 10.0_f32;
                    let chip_gap = 8.0_f32;
                    let (ok_label, no_label) =
                        if update_toast_on { ("설치", "나중에") } else { ("승인", "거부") };
                    let (ok_w, no_w) = if collab_toast_action_on {
                        (
                            g.measure_chrome_text(ok_label, chip_f, true) + chip_pad * 2.0,
                            g.measure_chrome_text(no_label, chip_f, true) + chip_pad * 2.0,
                        )
                    } else {
                        (0.0, 0.0)
                    };
                    let chips_w = if collab_toast_action_on {
                        chip_gap + ok_w + chip_gap + no_w
                    } else {
                        0.0
                    };
                    let box_w = text_w + px * 2.0 + chips_w;
                    let box_h = t_font + py * 2.0;
                    let bx = win_w - box_w - 16.0;
                    let by = TITLE_HEIGHT + 12.0;
                    self.collab.toast_rect = Some((bx, by, box_w, box_h));
                    let a = (235.0 * collab_toast_alpha).round() as u8;
                    round_rect(
                        g,
                        bx,
                        by,
                        box_w,
                        box_h,
                        theme::radius_md(),
                        theme::with_alpha(theme::surface_active(), a),
                    );
                    let ta = (255.0 * collab_toast_alpha).round() as u8;
                    // 승인 대기 토스트는 경고 뉘앙스(텍스트가 ⚠로 시작) — 본문은
                    // 기본 텍스트색, 완료 토스트는 기존 success 색 유지.
                    let msg_color = if collab_toast_action_on {
                        theme::with_alpha(theme::text(), ta)
                    } else {
                        theme::with_alpha(theme::success(), ta)
                    };
                    g.draw_text(
                        bx + px,
                        by + py,
                        msg,
                        gpu::DrawOpts {
                            font_size: t_font,
                            color: msg_color,
                            bold: true,
                            italic: false,
                        },
                    );
                    if collab_toast_action_on {
                        let ch = box_h - 8.0;
                        let cy = by + 4.0;
                        let ty = cy + (ch - chip_f) / 2.0;
                        let ox = bx + px + text_w + chip_gap;
                        round_rect(
                            g,
                            ox,
                            cy,
                            ok_w,
                            ch,
                            theme::radius_sm(),
                            theme::with_alpha(theme::success(), a),
                        );
                        g.draw_text(
                            ox + chip_pad,
                            ty,
                            ok_label,
                            gpu::DrawOpts {
                                font_size: chip_f,
                                color: theme::with_alpha(theme::fg(), ta),
                                bold: true,
                                italic: false,
                            },
                        );
                        self.collab.toast_approve_rect = Some((ox, cy, ok_w, ch));
                        let nx = ox + ok_w + chip_gap;
                        round_rect(
                            g,
                            nx,
                            cy,
                            no_w,
                            ch,
                            theme::radius_sm(),
                            theme::with_alpha(theme::danger(), a),
                        );
                        g.draw_text(
                            nx + chip_pad,
                            ty,
                            no_label,
                            gpu::DrawOpts {
                                font_size: chip_f,
                                color: theme::with_alpha(theme::fg(), ta),
                                bold: true,
                                italic: false,
                            },
                        );
                        self.collab.toast_deny_rect = Some((nx, cy, no_w, ch));
                    }
                }
            }
            // Alt/Option held → tmux "display-panes": each pane shows its %N
            // big + centered on an accent pill, so the user can read the id
            // (for `tell %N`, focus, etc.) without it crowding the header.
            // Works in single-pane too — body_rects covers every pane.
            if self.show_pane_numbers {
                // `body_rects` keys on the pane leaf id (== first tab's pid), so a
                // pane with several tabs would flash the same number on every tab.
                // Show the *active tab's* real id instead — that's the `%N` its
                // claude sees in KASATERM_PANE_ID and the one `tell`/`rename`
                // target. Falls back to the leaf id for image/markdown tabs (no pid).
                let ws = self.ws.lock().unwrap();
                for (id, rect) in &body_rects {
                    let (rx, ry, rw, rh) = *rect;
                    if rw < 24.0 || rh < 24.0 {
                        continue;
                    }
                    let shown: String = ws
                        .panes
                        .get(id)
                        .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
                        .unwrap_or_else(|| id.clone());
                    let font = (rh * 0.4).clamp(24.0, 72.0);
                    let tw = g.measure_chrome_text(&shown, font, true);
                    let pad = font * 0.4;
                    let box_w = tw + pad * 2.0;
                    let box_h = font + pad * 2.0;
                    let bx = rx + (rw - box_w) / 2.0;
                    let by = ry + (rh - box_h) / 2.0;
                    round_rect(
                        g,
                        bx,
                        by,
                        box_w,
                        box_h,
                        theme::radius_md(),
                        theme::with_alpha(theme::accent(), 0xE6),
                    );
                    g.draw_text(
                        bx + pad,
                        by + pad,
                        &shown,
                        gpu::DrawOpts {
                            font_size: font,
                            color: [0xFF, 0xFF, 0xFF, 0xFF],
                            bold: true,
                            italic: false,
                        },
                    );
                }
            }
            // Bottom dock bar: chips for panes folded out of the layout
            // (window_cells reserves DOCK_HEIGHT below the grid when non-empty).
            // Click a chip to restore (undock); its × kills the pane.
            // Dock bar: docked panes (chips, ×=kill) OR — while a pane is zoomed
            // — the hidden sibling panes, so the maximize visibly "sends the
            // others to the dock" and a sibling chip click switches the zoom to
            // it. zoom siblings have no × (they're live panes, not parked).
            let mut dock_items: Vec<(String, String, bool)> = if let Some(z) = self.zoomed_pane.clone() {
                let ws = self.ws.lock().unwrap();
                self.pty_layout
                    .as_ref()
                    .map(|t| {
                        t.leaves()
                            .iter()
                            .filter(|l| **l != z.as_str())
                            .map(|l| {
                                let label = ws
                                    .panes
                                    .get(*l)
                                    .and_then(|p| {
                                        p.tabs.get(p.active_tab).and_then(|tb| tb.title.clone())
                                    })
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| l.to_string());
                                (l.to_string(), label, false)
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                self.docked
                    .iter()
                    .map(|d| {
                        (
                            d.id.clone(),
                            if d.label.is_empty() { "shell".to_string() } else { d.label.clone() },
                            true,
                        )
                    })
                    .collect()
            };
            // 닫은 pane 은 여기 안 선다 — 되살리기는 Info 의 「되살리기」 섹션이
            // 맡는다. 하단바에 두면 pane 하나 닫을 때마다 띠가 생겨 그리드가 통째로
            // 재배치되고, 그 띠가 포커스 테두리 아랫변을 덮었다(거노).
            //
            // 접어 둔 별도창은 여기 선다. id 는 `aux:<i>` 로 접두사를 붙여 pane id
            // 와 섞이지 않게 한다 — 클릭 라우팅이 둘을 같은 목록에서 가른다.
            for (i, h) in self.hidden_aux.iter().enumerate() {
                dock_items.push((format!("aux:{i}"), h.label.clone(), false));
            }
            // 칩이 하나도 없어도 **예약된** 띠는 칠한다 — 안 칠하면 그리드가 비워 둔
            // 자리에 창 배경이 그대로 비쳐 바닥에 검은 틈이 생긴다.
            //
            // ⚠️판정은 dock **자신의** 예약(`dock_reserve_h`)으로 한다. 상태줄이 생기며
            // `bottom_reserve_h()`(= dock + 상태줄)가 무조건 양수가 됐는데, 그 값으로
            // 걸었더니 숨긴 pane 이 하나도 없어도 빈 dock 띠가 항상 칠해져 마지막 셀
            // 줄들을 덮었다 — 그리드는 상태줄 몫만 비워 둔 상태라 「의문의 하단바」로
            // 보였다(2026-08-12 지적).
            if dock_own_reserve > 0.0 {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                // 상태줄 **위**에 앉는다 — 상태줄은 창 맨 바닥에 고정이고 dock 은
                // 접힌 pane 이 있을 때만 나타났다 사라지는 띠라, 순서가 반대면
                // 접을 때마다 상태줄이 위아래로 뛴다.
                let bar_y = win_h - status_h - DOCK_HEIGHT;
                // Confine the dock to the pane-grid band: it must not bleed under
                // the session-tab strip / file tree on the left or the git column
                // on the right. Same bounds the cell grid uses in window_cells().
                let grid_x = sidebar_w + WINDOW_PADDING;
                let grid_right = win_w - git_col_w - WINDOW_PADDING;
                let grid_w = (grid_right - grid_x).max(0.0);
                // 크롬 판 색 — 사이드바·우측 칼럼과 같은 층이다. SURFACE 는 코드블록의
                // 가장 어두운 층이라 여기 쓰면 본문 옆에서 검은 틈처럼 읽혔다.
                g.rect(grid_x, bar_y, grid_w, DOCK_HEIGHT, theme::panel_bg());
                g.rect(grid_x, bar_y, grid_w, 1.0, theme::border());
                let chip_h = DOCK_HEIGHT - 12.0;
                let cy = bar_y + 6.0;
                let icon = theme::ICON_SIZE;
                let (mx, my) = (self.cursor_px.0 / scale, self.cursor_px.1 / scale);
                let mut cx = grid_x + 8.0;
                let mut chip_hits = Vec::new();
                let mut chip_close_hits = Vec::new();
                for (id, label, killable) in dock_items.iter() {
                    let lw = g.measure_chrome_text(label, chrome_font, false);
                    let chip_w = if *killable { lw + icon + 24.0 } else { lw + 20.0 };
                    let hover = mx >= cx && mx <= cx + chip_w && my >= cy && my <= cy + chip_h;
                    round_rect(
                        g,
                        cx,
                        cy,
                        chip_w,
                        chip_h,
                        theme::radius_sm(),
                        if hover { theme::surface_hover() } else { theme::surface_active() },
                    );
                    g.draw_text(
                        cx + 10.0,
                        cy + (chip_h - chrome_font) / 2.0 + 1.0,
                        label,
                        gpu::DrawOpts {
                            font_size: chrome_font,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                    if *killable {
                        let close_x = cx + chip_w - icon - 6.0;
                        g.queue_icon("x", close_x, cy + (chip_h - icon) / 2.0, icon, theme::text_dim());
                        chip_close_hits.push((id.clone(), (close_x - 2.0, cy, icon + 6.0, chip_h)));
                        chip_hits.push((id.clone(), (cx, cy, chip_w - icon - 8.0, chip_h)));
                    } else {
                        chip_hits.push((id.clone(), (cx, cy, chip_w, chip_h)));
                    }
                    cx += chip_w + 6.0;
                }
                self.dock_chip_rects = chip_hits;
                self.dock_chip_close_rects = chip_close_hits;
            } else {
                self.dock_chip_rects.clear();
                self.dock_chip_close_rects.clear();
            }
            // ── 하단 상태줄 ─────────────────────────────────────────────────
            // 창 맨 아래 한 줄. **계정 한도가 늘 보이는 자리**다 — 패널을 열어야
            // 보이면 「지금 얼마나 남았나」를 확인하려는 순간마다 손이 한 번 더 가고,
            // 그 손이 아까워 안 보다가 한도에 부딪힌다(거노 2026-08-11 「orca랑
            // 똑같이 하단바 그 형식으로」). 형식은 Orca 하단바에서 가져왔다:
            // 게이지 + 퍼센트 + 언제 풀리는지, 폭이 좁아지면 정해진 순서로 무너진다.
            {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let sy = win_h - status_h;
                g.rect(0.0, sy, win_w, status_h, theme::panel_bg());
                g.rect(0.0, sy, win_w, 1.0, theme::border());

                let badge = self.claude_usage.lock().ok().and_then(|v| v.clone());
                // 활성 슬롯 이름. 계정을 하나도 안 더했으면 이름 자체가 의미 없다.
                let acct_name = (!self.set_claude_accounts.is_empty()).then(|| {
                    let id = self.set_claude_account.as_str();
                    match self.set_claude_accounts.iter().position(|a| a.id == id) {
                        Some(i) => crate::settings::account_display(
                            id,
                            &self.set_claude_accounts[i].label,
                            &format!("계정 {}", i + 2),
                        ),
                        None => crate::settings::account_display("", "", "기본"),
                    }
                });

                let fs = 11.0_f32;
                let ty = sy + (status_h - fs) / 2.0 - 1.0;
                let mut x = 12.0_f32;
                let seg_x0 = x;
                // 클로드 로고 — 이 숫자가 「클로드 한도」라는 것을 그림이 먼저
                // 말한다(2026-08-16 「클로드사용량 로고도 넣어주고」). 계정 이름은
                // 좁아지면 빠지는 값이라 로고가 유일한 정체 표식이 되는 폭이 있다.
                if win_w >= 500.0 {
                    g.queue_icon("claude", x, sy + (status_h - 12.0) / 2.0, 12.0, theme::text_dim());
                    x += 17.0;
                }

                // 게이지 — Orca 처럼 **항상 중립색**이다. 하단바에서까지 빨갛게 하면
                // 시야 끝에서 늘 깜빡이는 경고가 되어 오히려 안 보게 된다. 위험은
                // 숫자 색으로만 말한다(드롭다운·Info pill 과 같은 임계값).
                // 한도 — **5시간이 먼저고 주간이 그 옆**이다(2026-08-15 지시
                // 「5시간 한도 먼저 보여주고 7일 한도는 눌렀을 때만」).
                //
                // 「눌렀을 때만」을 곧이곧대로 주간을 **숨기는** 것으로 읽으면 2026-08-05
                // 사고가 되돌아온다: 그때 5시간이 0%, 주간이 95% 였는데 하단바가 0% 를
                // 띄워 「3계정 다 소진이야? info엔 다 0퍼로뜨는데」가 됐다. 그래서 둘을
                // 나란히 두고, 폭이 모자랄 때만 급한 쪽을 남긴다 — 요청도 지켜지고
                // 그 사고도 안 돌아온다(거노 확정: 「둘 다 나란히」).
                //
                // 게이지는 Orca 처럼 **항상 중립색**이다. 하단바에서까지 빨갛게 하면
                // 시야 끝에서 늘 깜빡이는 경고가 되어 오히려 안 보게 된다. 위험은
                // 숫자 색으로만 말한다(드롭다운·Info pill 과 같은 임계값).
                const GW: f32 = 40.0;
                const GH: f32 = 6.0;
                let gy = sy + (status_h - GH) / 2.0;
                let pct_col = |p: f32| {
                    if p >= 90.0 {
                        theme::danger()
                    } else if p >= 70.0 {
                        theme::syn_number()
                    } else {
                        theme::text()
                    }
                };
                // **떠나온 계정의 숫자를 이어 그리지 않는다.** 계정을 바꾸면 이름은
                // 그 자리에서 바뀌는데 새 사용량은 1.1~2.0초(평균 1.6초) 뒤에 온다
                // (토키 실측 2026-08-15). 그 사이 「새 계정 이름 + 옛 계정 %」가
                // 그려지는데, 한도를 보고 계정을 고르는 기능이라 이 조합은 그냥
                // 거짓말이다. 배지가 어느 계정에서 나온 값인지 들고 다니므로
                // (`account_dir`) 활성 슬롯과 대조해 다르면 읽는 중으로 둔다.
                // 폴러가 조회한 자리와 **같은 규칙**으로 계산해야 한다 — 활성 계정은
                // 작업대라, 여기서 금고 경로를 쓰면 매번 「읽는 중」으로 보인다.
                // ⚠️`runtime_dir_for` 를 그대로 부르면 안 된다 — 활성 계정을 물으면
                // 자격증명을 읽느라 `security` 를 자식 프로세스로 띄우고(14ms),
                // 이 자리는 상태줄이라 **프레임마다** 돈다. pane 여럿이 동시에
                // 출력해 프레임이 쉼 없이 뜨는 동안 메인 스레드의 88%가 그
                // 대기였다(2026-08-18 실측). 캐시판은 답이 같고 전환 때 무효화된다.
                let active_dir = crate::claude_auth::runtime_dir_for_cached(
                    &self.set_claude_account,
                    &self.set_claude_account,
                )
                .map_or(String::new(), |p| p.to_string_lossy().into_owned());
                let switching = badge.as_ref().is_some_and(|b| b.account_dir != active_dir);

                // `windows` 는 5시간이 앞이고, `pct`/`label` 은 **가장 급한** 창이다.
                // 좁을 때 후자로 떨어지는 것이 요점 — 자리가 하나뿐이면 급한 쪽을
                // 보여야 한다. `None` 은 「읽는 중」 — 자리는 잡되 숫자는 안 말한다.
                let wins: Vec<(String, Option<f32>)> = match badge.as_ref() {
                    Some(b) if win_w >= 760.0 && b.windows.len() > 1 => b
                        .windows
                        .iter()
                        .map(|(l, p)| (l.clone(), (!switching).then_some(*p)))
                        .collect(),
                    Some(b) => vec![(b.label.clone(), (!switching).then_some(b.pct))],
                    None => Vec::new(),
                };
                if wins.is_empty() {
                    // 값이 없으면 `—`. 0% 로 그리면 「여유 있음」이라는 거짓말이 되고,
                    // 그게 옮길지 말지를 정확히 반대로 만든다(드롭다운과 같은 규칙).
                    g.draw_text(
                        x,
                        ty,
                        "—",
                        gpu::DrawOpts {
                            font_size: fs,
                            color: theme::text_dim(),
                            bold: false,
                            italic: false,
                        },
                    );
                    x += g.measure_chrome_text("—", fs, true) + 10.0;
                }
                let stale = badge.as_ref().is_some_and(|b| b.stale);
                for (i, (label, pct)) in wins.iter().enumerate() {
                    if i > 0 {
                        x += 12.0;
                    }
                    // 창 이름은 둘을 나란히 둘 때 **반드시** 있어야 한다 — 없으면
                    // 12% 와 95% 중 어느 쪽이 5시간인지 알 길이 없다. 하나만 그릴
                    // 때는 좁은 창이라 접는다(그때는 급한 쪽이라는 것만 알면 된다).
                    if wins.len() > 1 || win_w >= 900.0 {
                        g.draw_text(
                            x,
                            ty,
                            label,
                            gpu::DrawOpts {
                                font_size: fs,
                                color: theme::text_dim(),
                                bold: false,
                                italic: false,
                            },
                        );
                        x += g.measure_chrome_text(label, fs, true) + 5.0;
                    }
                    if win_w >= 500.0 {
                        // 트랙이 보여야 «얼마나 남았나»가 읽힌다 — 채움만 그리면 15%
                        // 짜리 짧은 막대가 어디까지 갈 수 있는 것인지 알 수가 없어서
                        // 그냥 얼룩이 된다(첫 캡처에서 실제로 그랬다).
                        g.rect(x, gy, GW, GH, theme::with_alpha(theme::text_dim(), 90));
                        // 읽는 중이면 **트랙만**. 빈 트랙은 0% 처럼 보일 수 있지만
                        // 옆의 숫자가 `…` 라 「모른다」로 읽힌다 — 채움을 그리면
                        // 그 순간 옛 숫자가 되살아난다.
                        if let Some(p) = pct {
                            let fw = (GW * (p / 100.0).clamp(0.0, 1.0)).max(2.0);
                            g.rect(x, gy, fw, GH, theme::with_alpha(theme::text(), 210));
                        }
                        x += GW + 6.0;
                    }
                    let (s, col) = match pct {
                        Some(p) if stale => (format!("~{p:.0}%"), pct_col(*p)),
                        Some(p) => (format!("{p:.0}%"), pct_col(*p)),
                        None => ("…".to_string(), theme::text_dim()),
                    };
                    g.draw_text(
                        x,
                        ty,
                        &s,
                        gpu::DrawOpts { font_size: fs, color: col, bold: false, italic: false },
                    );
                    x += g.measure_chrome_text(&s, fs, true);
                }
                // 언제 풀리는지는 5시간 창에 대해서만, 그것도 아주 넓을 때만. 퍼센트가
                // 같아도 12분 뒤면 기다리면 되고 3시간 뒤면 지금 옮겨야 한다 — 다만
                // 두 창을 나란히 두고 나면 자리가 없어서, 좁아지면 팝오버로 물러난다.
                // 전환 중엔 이것도 빼야 한다 — 초기화 시각은 떠나온 계정 것이라
                // 게이지만 가리고 여기를 남기면 거짓말이 옆칸으로 옮겨갈 뿐이다.
                if let (Some(b), true) = (badge.as_ref(), win_w >= 1100.0 && !switching) {
                    if let Some(l) = crate::resets_in_label(b.resets_at) {
                        let s = format!("· {l}");
                        g.draw_text(
                            x + 8.0,
                            ty,
                            &s,
                            gpu::DrawOpts {
                                font_size: fs,
                                color: theme::text_dim(),
                                bold: false,
                                italic: false,
                            },
                        );
                        x += 8.0 + g.measure_chrome_text(&s, fs, true);
                    }
                }
                x += 10.0;

                // 계정 이름은 가장 먼저 버린다 — 한도 숫자가 이 줄의 존재 이유고,
                // 이름은 드롭다운을 열면 어차피 맨 위에 있다.
                //
                // 라벨을 안 지은 슬롯은 이름이 이메일로 폴백되는데, 그걸 통째로 적으면
                // 한 줄의 절반을 주소가 먹는다. **@ 앞만** 남긴다 — 계정을 가리는 데는
                // 그걸로 충분하고(오늘 넷이 같은 계정인 걸 못 알아본 게 문제였지 주소
                // 뒷부분을 몰라서가 아니다), 전체는 드롭다운에 그대로 있다.
                //
                // 다만 **겹치면 안 줄인다.** 슬롯 둘이 같은 아이디에 다른 도메인이면
                // (`goenho0613@naver` · `goenho0613@gmail`) 화면에서 통째로 같은 글자가
                // 되어, 지금 어느 계정인지 이 자리로는 알 수가 없다(토키 실측
                // 2026-08-15). 겹칠 때만 도메인 앞머리를 붙여 가른다 — 안 겹치면
                // 예전대로 짧게.
                if let (Some(n), true) = (acct_name.as_ref(), win_w >= 720.0) {
                    let others: Vec<String> =
                        std::iter::once(crate::settings::account_display("", "", "기본"))
                            .chain(self.set_claude_accounts.iter().enumerate().map(|(i, a)| {
                                crate::settings::account_display(
                                    &a.id,
                                    &a.label,
                                    &format!("계정 {}", i + 2),
                                )
                            }))
                            .collect();
                    let short = statusbar_account_short(n, &others);
                    let short = short.as_str();
                    g.draw_text(
                        x,
                        ty,
                        short,
                        gpu::DrawOpts {
                            font_size: fs,
                            color: theme::text_dim(),
                            bold: false,
                            italic: false,
                        },
                    );
                    x += g.measure_chrome_text(short, fs, true);
                }

                // 세그먼트 전체가 손잡이다 — 게이지든 숫자든 이름이든 누르면 열린다.
                let acct_r = (seg_x0 - 6.0, sy, (x - seg_x0 + 12.0).max(24.0), status_h);
                // 세그먼트가 곧 계정 스위처 손잡이다 — 손모양이 없으면 눌러 볼
                // 생각조차 안 든다(거노 2026-08-12). 채움은 주지 않는다: 세그먼트
                // 폭은 텍스트를 다 그린 뒤에야 확정되고, 이 렌더는 나중에 그린 것이
                // 위로 오므로 여기서 사각형을 깔면 방금 쓴 글자를 덮는다.
                {
                    let (hx, hy) = self.cursor_px;
                    g.hover_pointer |= hx >= acct_r.0
                        && hx <= acct_r.0 + acct_r.2
                        && hy >= acct_r.1
                        && hy <= acct_r.1 + acct_r.3;
                }
                self.status_account_rect = Some(acct_r);

                // 바깥주소(터널) 스위치 — 이 줄의 **오른쪽 끝**(2026-08-15 지시
                // 「하단우측」). 폰 하단바는 좁고, 문이 닫히면 폰은 접속 자체가
                // 안 돼 스위치를 폰에 둘 이유가 없다 — 여닫는 손은 맥이다.
                // 점이 상태다: 초록=열림, 흐림=닫힘. 누르면 handler 가 토글한다.
                {
                    // 「바깥」이었다 — 무엇이 바깥인지 말해 주지 않는 이름이라
                    // 바꿨다(2026-08-15 지시 「바깥이라는거 좀 이상한데」). 지구본이
                    // 뜻을 지고, 두 글자가 그걸 못 읽는 경우를 받치고, 나머지 설명은
                    // 팝오버 제목이 한다.
                    let label = "원격";
                    let icon = 12.0_f32;
                    let dot = 6.0_f32;
                    let gap = 5.0_f32;
                    let on = self.statusbar.tunnel_on == Some(true);
                    let tw = g.measure_chrome_text(label, fs, false);
                    let seg_w = icon + gap + tw + gap + dot;
                    let tx = win_w - 12.0 - seg_w;
                    let col = if on { theme::text() } else { theme::text_dim() };
                    g.queue_icon("globe", tx, sy + (status_h - icon) / 2.0, icon, col);
                    g.draw_text(
                        tx + icon + gap,
                        ty,
                        label,
                        gpu::DrawOpts { font_size: fs, color: col, bold: false, italic: false },
                    );
                    // 점은 이름 뒤로 옮겼다 — 상태(열림/닫힘)는 이름을 읽은 **다음에**
                    // 궁금해지는 것이고, 앞에 두면 지구본과 나란히 서서 둘 다 뜻이 흐려진다.
                    round_rect(
                        g,
                        tx + icon + gap + tw + gap,
                        sy + (status_h - dot) / 2.0,
                        dot,
                        dot,
                        dot / 2.0,
                        if on {
                            theme::success()
                        } else {
                            theme::with_alpha(theme::text_dim(), 140)
                        },
                    );
                    let r = (tx - 8.0, sy, seg_w + 20.0, status_h);
                    {
                        let (hx, hy) = self.cursor_px;
                        g.hover_pointer |=
                            hx >= r.0 && hx <= r.0 + r.2 && hy >= r.1 && hy <= r.1 + r.3;
                    }
                    self.statusbar.tunnel_rect = Some(r);

                    // 바깥 스위치 왼쪽으로 리소스 → 포트 순서(Orca 하단바처럼 —
                    // 2026-08-15 지시 「포트 하단바로」·「리소스사용량도」).
                    let mut rx = tx - 8.0;
                    // 리소스 — 앱 + 학생 트리 합. 폭이 좁으면 먼저 버린다:
                    // 이 줄의 존재 이유는 한도(왼쪽)와 조작(바깥·포트)이다.
                    self.statusbar.res_rect = None;
                    if let (Some((cpu, rss)), true) = (self.statusbar.res, win_w >= 640.0) {
                        let gb = rss as f32 / (1024.0 * 1024.0 * 1024.0);
                        let label = if gb >= 1.0 {
                            format!("{cpu:.0}% · {gb:.1}G")
                        } else {
                            format!("{cpu:.0}% · {:.0}M", gb * 1024.0)
                        };
                        let lw = g.measure_chrome_text(&label, fs, false);
                        rx -= lw + 12.0;
                        let open = matches!(
                            self.statusbar.popover,
                            Some((state::StatusbarPopover::Usage, _))
                        );
                        g.draw_text(
                            rx,
                            ty,
                            &label,
                            gpu::DrawOpts {
                                font_size: fs,
                                color: if open { theme::text() } else { theme::text_dim() },
                                bold: false,
                                italic: false,
                            },
                        );
                        let rr = (rx - 6.0, sy, lw + 12.0, status_h);
                        {
                            let (hx, hy) = self.cursor_px;
                            g.hover_pointer |= hx >= rr.0
                                && hx <= rr.0 + rr.2
                                && hy >= rr.1
                                && hy <= rr.1 + rr.3;
                        }
                        self.statusbar.res_rect = Some(rr);
                    }
                    // 포트 — 열려 있는 워크스페이스 포트 **개수**다. 예전엔 이 앱의
                    // `:8765` 만 적었는데, 그건 이미 알고 있는 값이라 자리를 쓰면서
                    // 아무것도 안 알렸다. 개수는 "지금 뭔가 떠 있나" 에 답하고, 눌러
                    // 펼치면 그 목록이 나온다(2026-08-15 지시 「포트 하단바로」).
                    self.statusbar.port_rect = None;
                    {
                        let n = self.info.view.ports.len();
                        let label = n.to_string();
                        let icon = 12.0_f32;
                        let gap = 4.0_f32;
                        let lw = g.measure_chrome_text(&label, fs, false);
                        let seg = icon + gap + lw;
                        rx -= seg + 12.0;
                        let open =
                            matches!(self.statusbar.popover, Some((state::StatusbarPopover::Ports, _)));
                        let col = if open || n > 0 { theme::text() } else { theme::text_dim() };
                        g.queue_icon("plug", rx, sy + (status_h - icon) / 2.0, icon, col);
                        g.draw_text(
                            rx + icon + gap,
                            ty,
                            &label,
                            gpu::DrawOpts { font_size: fs, color: col, bold: false, italic: false },
                        );
                        let pr = (rx - 6.0, sy, seg + 12.0, status_h);
                        {
                            let (hx, hy) = self.cursor_px;
                            g.hover_pointer |= hx >= pr.0
                                && hx <= pr.0 + pr.2
                                && hy >= pr.1
                                && hy <= pr.1 + pr.3;
                        }
                        self.statusbar.port_rect = Some(pr);
                    }
                }
                // 팝오버는 상태줄 **뒤**다 — 같은 자리 위로 떠야 하고, 칩을 그린
                // 뒤라야 앵커 사각형이 이번 프레임 값으로 서 있다.
                crate::statusbar::paint_popover(
                    g,
                    &mut self.statusbar,
                    &self.info.view,
                    self.cursor_px,
                    win_w,
                    win_h,
                );
            }
            // 통째 이동(header/handle·단일탭 tab 드래그)은 실제 레이아웃이 라이브로
            // reflow 되므로 오버레이가 없다 — 진짜 재배치가 곧 프리뷰다. 파란 drop-zone
            // 박스는 라이브가 아닌 tab 드래그(멀티탭 탭 추출)의 착지 지점 힌트로만 남긴다.
            if let Some((zx, zy, zw, zh)) = drop_zone_rect {
                g.rect(zx, zy, zw, zh, theme::with_alpha(theme::accent(), 90));
            }
            // Launch build banner, bottom-right, painted last so it sits
            // on top. Faint and short-lived — fades out after a few
            // seconds. Coords are logical px (gpu promotes to physical).
            // 계정 드롭다운 — 앵커는 Info 탭 머리의 계정 행(`account_chip_rect`,
            // info::draw_info_actions 가 채운다). 패널 본문 위로 떠야 해서 그 안에서
            // 같이 못 그리고, 모든 pane·오버레이가 끝난 여기서 마지막에 그린다.
            //
            // 계정 행이 곧 계정 스위처다(거노 요청) — 거기 보이는 한도가 **활성
            // 계정의** 것이라, 이름을 같은 행에 적고 클릭을 전환에 쓰는 게 별도
            // 칩보다 정직하다.
            self.account_menu_hits.clear();
            // 앵커는 **연 손잡이**를 따라간다. 손잡이가 둘이라(Info 탭 계정 행 ·
            // 상태줄) 하나로 고정하면 다른 쪽에서 열었을 때 메뉴가 화면 반대편에
            // 뜬다. 기록이 없으면 옛 동작대로 계정 행에 붙인다.
            let anchor = self.account_menu_anchor.or(self.account_chip_rect);
            if let (true, Some((ax, ay, aw, ah))) = (self.account_menu, anchor) {
                let (hmx, hmy) = self.cursor_px;
                let f = 13.0_f32;
                let pad = 4.0_f32;
                let pad_x = 10.0_f32;
                let icon = theme::ICON_SIZE;
                let compact = self.set_usage_compact;
                let win_h = win_px.1 / scale;
                let win_w = win_px.0 / scale;

                // ── 값 읽기 ──────────────────────────────────────────────────
                // 슬롯별 한도표. 폴러가 계정 디렉터리를 키로 채운다.
                let usage_of = |id: &str| -> Option<crate::UsageBadge> {
                    // 폴러가 조회한 자리가 곧 키다 — 활성 계정만 작업대라 여기서도
                    // 같은 규칙을 써야 그 한 줄이 빈칸이 되지 않는다.
                    // 메뉴가 열려 있는 동안 계정 수만큼 **매 프레임** 돈다 —
                    // 활성 계정 차례에서 프로세스를 띄우므로 캐시판을 쓴다.
                    let key = crate::claude_auth::runtime_dir_for_cached(id, &self.set_claude_account)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.claude_usage_all.lock().ok()?.get(&key).cloned()
                };
                // 로스터 행은 **활성 계정의** 한도를 말한다. 표에 아직 없으면 상태줄이
                // 쓰는 값으로 떨어진다 — 둘 다 지금 계정을 가리키므로 숫자가 갈리지 않는다.
                let claude_badge = usage_of(&self.set_claude_account)
                    .or_else(|| self.claude_usage.lock().ok().and_then(|v| v.clone()));

                // `62% 씀 · 5h` — 퍼센트가 먼저다. 창 이름이 앞에 오면 눈이 «어느 창인가»
                // 를 먼저 읽는데, 정작 판단을 가르는 건 숫자다.
                let usage_text = |b: &crate::UsageBadge| -> String {
                    let head = if b.stale {
                        format!("~{:.0}% 씀", b.pct)
                    } else {
                        format!("{:.0}% 씀", b.pct)
                    };
                    format!("{head} {}", b.label)
                };
                // `3시간 54분 뒤 초기화`. 90% 라도 12분 뒤면 기다리면 되고 3시간 뒤면 지금
                // 옮겨야 한다 — 퍼센트만으로는 그 둘이 구별되지 않는다.
                let resets_text = |b: &crate::UsageBadge| -> Option<String> {
                    let at = b.resets_at?;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let left = at.saturating_sub(now);
                    if left == 0 {
                        return Some("곧 초기화".to_string());
                    }
                    let (h, m) = (left / 3600, (left % 3600) / 60);
                    Some(match (h, m) {
                        (0, m) => format!("{m}분 뒤 초기화"),
                        (h, 0) => format!("{h}시간 뒤 초기화"),
                        (h, m) => format!("{h}시간 {m}분 뒤 초기화"),
                    })
                };
                // 임계는 60/80. 그 아래는 초록이 아니라 **중립**이다 — 초록은 "좋다"는
                // 신호라 늘 켜져 있으면 아무 말도 안 하는 색이 된다.
                let pct_col = |pct: f32| {
                    if pct >= 80.0 {
                        theme::danger()
                    } else if pct >= 60.0 {
                        theme::syn_number()
                    } else {
                        theme::text()
                    }
                };

                // 제공자 두 줄. **사용률 높은 순** — 옮길 곳을 고르려고 여는 목록이라
                // 급한 쪽이 위로 와야 한다. 값이 없는 쪽(codex 는 한도 조회 경로가 아예
                // 없다)은 -1 로 두어 뒤로 민다.
                let codex_signed_in = crate::settings::codex_identity(&self.set_codex_account).is_some();
                let mut provs: Vec<(AccountProvider, f32)> = vec![
                    (AccountProvider::Claude, claude_badge.as_ref().map_or(-1.0, |b| b.pct)),
                    (AccountProvider::Codex, -1.0),
                ];
                provs.sort_by(|a, b| b.1.total_cmp(&a.1));

                // ── 치수 ────────────────────────────────────────────────────
                let mw = 340.0_f32;
                let head_h = 26.0_f32;
                let seg_h = 28.0_f32;
                let row_h = 28.0_f32;
                let prow_h = if compact { 30.0 } else { 46.0 };
                let rule = 5.0_f32;
                let mh = pad * 2.0
                    + head_h
                    + seg_h
                    + rule
                    + prow_h * provs.len() as f32
                    + rule
                    + row_h * 2.0;
                // 아래로 펼치되 자리가 없으면 위로 뒤집는다. 손잡이 하나가 창 맨 아래
                // 상태줄이라(늘 보이는 자리) 아래로만 펼치면 메뉴가 통째로 창 밖에
                // 그려졌다 — 열리기는 열리는데 화면엔 아무 일도 안 일어난 것처럼 보인다
                // (2026-08-12 지적: "눌러도 안 열린다").
                let mx = (ax + aw - mw).max(4.0);
                let below = ay + ah + 4.0;
                let my = if below + mh <= win_h - 4.0 {
                    below
                } else {
                    (ay - mh - 4.0).max(4.0)
                };
                // 패널 배경과 팝업 배경은 6단계밖에 안 벌어져서, 색만으로는 이게 떠 있는
                // 메뉴인지 패널의 한 구역인지 읽히지 않았다(거노: 뒤가 비쳐 보인다).
                // 층 선언은 색이 아니라 그림자·테두리가 하는 일이다.
                panel_rect_outlined(g, mx, my, mw, mh, theme::radius_sm(), theme::surface_hover());
                let mut ry = my + pad;

                // ── 머리: Usage · all agents ────────────────────────────────
                g.draw_text(
                    mx + pad_x,
                    ry + (head_h - f) / 2.0 - 1.0,
                    "사용량",
                    gpu::DrawOpts { font_size: f, color: theme::text(), bold: true, italic: false },
                );
                {
                    // 「이 앱에서 도는 학생 전부의 합」이라는 뜻 — 계정 하나를 여러
                    // pane 이 나눠 쓰므로, 이 숫자가 내 pane 것이 아님을 밝혀야 한다.
                    let sub = "학생 전체";
                    let sf = f - 2.0;
                    let sw = g.measure_chrome_text(sub, sf, false);
                    g.draw_text(
                        mx + mw - pad_x - icon - 8.0 - sw,
                        ry + (head_h - sf) / 2.0 - 1.0,
                        sub,
                        gpu::DrawOpts {
                            font_size: sf,
                            color: theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                    g.queue_icon(
                        "rotate-cw",
                        mx + mw - pad_x - icon,
                        ry + (head_h - icon) / 2.0,
                        icon,
                        theme::text_dim(),
                    );
                }
                ry += head_h;

                // ── 밀도 선택 ───────────────────────────────────────────────
                {
                    let cw = (mw - pad_x * 2.0) / 2.0;
                    for (i, (label, want)) in [("자세히", false), ("간단히", true)]
                        .into_iter()
                        .enumerate()
                    {
                        let cx = mx + pad_x + cw * i as f32;
                        let r = (cx, ry + 2.0, cw, seg_h - 6.0);
                        let on = compact == want;
                        let hover = hmx >= r.0 && hmx <= r.0 + r.2 && hmy >= r.1 && hmy <= r.1 + r.3;
                        g.hover_pointer |= hover;
                        if on || hover {
                            round_rect(
                                g, r.0, r.1, r.2, r.3, theme::radius_sm(),
                                if on { theme::surface_active() } else { theme::with_alpha(theme::surface_active(), 0x66) },
                            );
                        }
                        let lf = f - 2.0;
                        let lw = g.measure_chrome_text(label, lf, on);
                        g.draw_text(
                            r.0 + (r.2 - lw) / 2.0,
                            r.1 + (r.3 - lf) / 2.0 - 1.0,
                            label,
                            gpu::DrawOpts {
                                font_size: lf,
                                color: if on { theme::text() } else { theme::text_mute() },
                                bold: on,
                                italic: false,
                            },
                        );
                        self.account_menu_hits.push((AccountMenuItem::Density(want), r));
                    }
                }
                ry += seg_h;
                g.rect(mx + pad, ry + 2.0, mw - pad * 2.0, 1.0, theme::border());
                ry += rule;

                // ── 제공자 행 ───────────────────────────────────────────────
                let mut sub_anchor: Option<(AccountProvider, f32)> = None;
                for (p, _) in provs.iter().copied() {
                    let open = self.account_menu_provider == Some(p);
                    let on = hmx >= mx && hmx <= mx + mw && hmy >= ry && hmy <= ry + prow_h;
                    g.hover_pointer |= on;
                    if on || open {
                        round_rect(g, mx + pad, ry, mw - pad * 2.0, prow_h,
                            theme::radius_sm(), theme::surface_active());
                    }
                    let line1 = ry + if compact { (prow_h - f) / 2.0 - 1.0 } else { 7.0 };
                    g.queue_icon(
                        p.icon(),
                        mx + pad_x,
                        line1 + (f - icon) / 2.0,
                        icon,
                        theme::text(),
                    );
                    let name_x = mx + pad_x + icon + 8.0;
                    g.draw_text(
                        name_x, line1, p.label(),
                        gpu::DrawOpts { font_size: f, color: theme::text(), bold: false, italic: false },
                    );
                    // 오른쪽 끝은 언제나 › — 이 행이 열리는 행이라는 유일한 표시다.
                    g.queue_icon(
                        "chevron-right",
                        mx + mw - pad_x - icon,
                        ry + (prow_h - icon) / 2.0,
                        icon,
                        theme::text_dim(),
                    );
                    let right = mx + mw - pad_x - icon - 6.0;
                    let badge = match p {
                        AccountProvider::Claude => claude_badge.clone(),
                        AccountProvider::Codex => None,
                    };
                    match (&badge, p) {
                        // 값이 있으면: 첫 줄 오른쪽에 리셋, 둘째 줄에 창별 막대.
                        (Some(b), _) => {
                            if compact {
                                let t = usage_text(b);
                                let tf = f - 1.0;
                                let tw = g.measure_chrome_text(t.as_str(), tf, true);
                                g.draw_text(
                                    right - tw, line1, &t,
                                    gpu::DrawOpts { font_size: tf, color: pct_col(b.pct), bold: true, italic: false },
                                );
                            } else {
                                if let Some(t) = resets_text(b) {
                                    let tf = f - 2.0;
                                    let tw = g.measure_chrome_text(t.as_str(), tf, false);
                                    g.draw_text(
                                        right - tw, line1 + 1.0, &t,
                                        gpu::DrawOpts { font_size: tf, color: theme::text_mute(), bold: false, italic: false },
                                    );
                                }
                                // 둘째 줄: [창 이름][막대][퍼센트] — **창마다 하나씩**.
                                // 하단바는 좁아서 급할 땐 한 창으로 접히므로, 펼친
                                // 여기서는 5시간과 주간이 **둘 다** 보여야 한다
                                // (2026-08-15 지시 「7일 한도는 눌렀을 때만」의 그 자리).
                                // 막대는 트랙을 함께 그린다 — 채움만 있으면 15% 짜리가
                                // 어디까지 갈 수 있는 것인지 알 수가 없어 그냥 얼룩이 된다.
                                let l2 = ry + prow_h - 17.0;
                                draw_usage_windows(g, name_x, l2, right, f - 3.0, b);
                            }
                        }
                        // codex 는 한도 조회 경로가 없다. 그 자리에 로그인 여부를 적는다 —
                        // 아직 로그인 안 한 슬롯은 이름이 폴백되어, 표시가 없으면 멀쩡한
                        // 계정과 구별이 안 된다.
                        (None, AccountProvider::Codex) => {
                            let (t, col) = if codex_signed_in {
                                ("기록 없음", theme::text_mute())
                            } else {
                                ("로그인 안 됨", theme::danger())
                            };
                            let tf = f - 2.0;
                            let tw = g.measure_chrome_text(t, tf, false);
                            g.draw_text(
                                right - tw, ry + (prow_h - tf) / 2.0 - 1.0, t,
                                gpu::DrawOpts { font_size: tf, color: col, bold: false, italic: false },
                            );
                        }
                        (None, AccountProvider::Claude) => {
                            let t = "기록 없음";
                            let tf = f - 2.0;
                            let tw = g.measure_chrome_text(t, tf, false);
                            g.draw_text(
                                right - tw, ry + (prow_h - tf) / 2.0 - 1.0, t,
                                gpu::DrawOpts { font_size: tf, color: theme::text_mute(), bold: false, italic: false },
                            );
                        }
                    }
                    self.account_menu_hits
                        .push((AccountMenuItem::Provider(p), (mx, ry, mw, prow_h)));
                    if open {
                        sub_anchor = Some((p, ry));
                    }
                    ry += prow_h;
                }

                // ── 하단 액션 ───────────────────────────────────────────────
                g.rect(mx + pad, ry + 2.0, mw - pad * 2.0, 1.0, theme::border());
                ry += rule;
                for (item, label) in [
                    (AccountMenuItem::UsageDetails, "사용 내역 자세히"),
                    (AccountMenuItem::ManageAccounts, "계정 관리…"),
                ] {
                    let on = hmx >= mx && hmx <= mx + mw && hmy >= ry && hmy <= ry + row_h;
                    g.hover_pointer |= on;
                    if on {
                        round_rect(g, mx + pad, ry, mw - pad * 2.0, row_h,
                            theme::radius_sm(), theme::surface_active());
                    }
                    g.draw_text(
                        mx + pad_x, ry + (row_h - f) / 2.0 - 1.0, label,
                        gpu::DrawOpts { font_size: f, color: theme::text_dim(), bold: false, italic: false },
                    );
                    g.queue_icon(
                        "chevron-right",
                        mx + mw - pad_x - icon,
                        ry + (row_h - icon) / 2.0,
                        icon,
                        theme::text_dim(),
                    );
                    self.account_menu_hits.push((item, (mx, ry, mw, row_h)));
                    ry += row_h;
                }

                // ── 계정 목록(서브메뉴) ─────────────────────────────────────
                // 로스터 오른쪽에 붙는다. 계정을 첫 화면에 늘어놓지 않는 이유는 위
                // `AccountMenuItem::Provider` 주석에 있다.
                if let Some((p, py)) = sub_anchor {
                    let rows: Vec<(String, String, bool)> = match p {
                        AccountProvider::Claude => {
                            // "기본" 행은 계정 미선택일 때만 — 슬롯이 활성이면 기본
                            // 자리가 곧 그 계정의 작업대라 같은 로그인이 두 줄로 떠
                            // 계정이 하나 더 있는 것처럼 읽힌다(2026-08-17 「왜
                            // 다섯개로 떠」, 설정 화면 카드 목록과 같은 규칙).
                            let mut v = Vec::new();
                            if self.set_claude_account.is_empty() {
                                v.push((
                                    String::new(),
                                    crate::settings::account_display("", "", "기본"),
                                    true,
                                ));
                            }
                            v.extend(self.set_claude_accounts.iter().enumerate().map(|(i, a)| {
                                (
                                    a.id.clone(),
                                    crate::settings::account_display(
                                        &a.id, &a.label, &format!("계정 {}", i + 2),
                                    ),
                                    self.set_claude_account == a.id,
                                )
                            }));
                            v
                        }
                        AccountProvider::Codex => {
                            // 라벨이 없으면 그 슬롯의 실제 이메일로 부른다 — claude 쪽
                            // `account_display` 와 같은 규칙이다.
                            let name = |id: &str, label: &str, fallback: String| -> String {
                                let ident = crate::settings::codex_identity(id);
                                match (label.is_empty(), ident) {
                                    (true, Some(e)) => e,
                                    (true, None) => fallback,
                                    (false, Some(e)) if !label.contains(&e) => format!("{label} · {e}"),
                                    (false, _) => label.to_string(),
                                }
                            };
                            let mut v = vec![(
                                String::new(),
                                name("", "", "기본".to_string()),
                                self.set_codex_account.is_empty(),
                            )];
                            v.extend(self.set_codex_accounts.iter().enumerate().map(|(i, a)| {
                                (
                                    a.id.clone(),
                                    name(&a.id, &a.label, format!("계정 {}", i + 2)),
                                    self.set_codex_account == a.id,
                                )
                            }));
                            v
                        }
                    };
                    let sw = 300.0_f32;
                    let lab_h = 24.0_f32;
                    // **고르기 전에** 각 계정의 5시간·7일이 둘 다 보여야 한다(거노
                    // 2026-08-15 「계정전환전에 5시간 7일 한도 보이게」). 누르면 그 자리서
                    // 전환되므로 눌러 보고 판단할 수가 없다. 막대 두 벌은 이름과 한 줄에
                    // 못 들어가니 행을 두 줄로 키운다 — 「간단히」 밀도에서는 예전처럼
                    // 한 줄에 글자로만.
                    let two_line = p == AccountProvider::Claude && !compact;
                    let arow_h = if two_line { 44.0 } else { row_h };
                    let sh = pad * 2.0 + lab_h + arow_h * rows.len() as f32 + rule + row_h;
                    // 로스터 오른쪽에 두되, 창 밖으로 나가면 왼쪽으로 접는다.
                    let sx = if mx + mw + 4.0 + sw <= win_w - 4.0 {
                        mx + mw + 4.0
                    } else {
                        (mx - sw - 4.0).max(4.0)
                    };
                    let sy = (py - pad).min(win_h - sh - 4.0).max(4.0);
                    panel_rect_outlined(g, sx, sy, sw, sh, theme::radius_sm(), theme::surface_hover());
                    let mut sry = sy + pad;
                    {
                        let t = format!("{} 계정", p.label());
                        let lf = f - 2.0;
                        g.draw_text(
                            sx + pad_x, sry + (lab_h - lf) / 2.0 - 1.0, &t,
                            gpu::DrawOpts { font_size: lf, color: theme::text_mute(), bold: true, italic: false },
                        );
                        sry += lab_h;
                    }
                    for (id, label, active) in rows {
                        let on = hmx >= sx && hmx <= sx + sw && hmy >= sry && hmy <= sry + arow_h;
                        // 활성 행은 갈 곳이 없다 — hover 도 히트박스도 손모양도 없다.
                        g.hover_pointer |= on && !active;
                        if on && !active {
                            round_rect(g, sx + pad, sry, sw - pad * 2.0, arow_h,
                                theme::radius_sm(), theme::surface_active());
                        }
                        // 두 줄일 때 이름은 위, 막대는 아래. 한 줄이면 예전대로 가운데.
                        let line1 =
                            if two_line { sry + 7.0 } else { sry + (arow_h - f) / 2.0 - 1.0 };
                        g.draw_text(
                            sx + pad_x, line1, &label,
                            gpu::DrawOpts {
                                font_size: f,
                                color: if active { theme::text() } else { theme::text_dim() },
                                bold: active,
                                italic: false,
                            },
                        );
                        let tf = f - 3.0;
                        let right = sx + sw - pad_x;
                        // 활성 표시는 오른쪽 배지. 체크 아이콘이나 왼쪽 막대와 달리,
                        // 그 자리에 다른 계정이 쓰는 한도 숫자와 같은 층으로 읽힌다.
                        if active {
                            let t = "사용 중";
                            let tw = g.measure_chrome_text(t, tf, true);
                            g.draw_text(
                                right - tw, line1 + if two_line { 0.0 } else { (f - tf) / 2.0 }, t,
                                gpu::DrawOpts { font_size: tf, color: theme::text_mute(), bold: true, italic: false },
                            );
                        } else if p == AccountProvider::Codex
                            && crate::settings::codex_identity(&id).is_none()
                        {
                            let t = "로그인";
                            let tw = g.measure_chrome_text(t, tf, true);
                            g.draw_text(
                                right - tw, line1 + if two_line { 0.0 } else { (f - tf) / 2.0 }, t,
                                gpu::DrawOpts { font_size: tf, color: theme::danger(), bold: true, italic: false },
                            );
                        }
                        // 한도는 **활성 슬롯도 포함해** 전부 적는다 — 「지금 이만큼 썼으니
                        // 저기로 옮긴다」를 정하는 자리라 떠날 쪽 숫자가 빠지면 비교가 안
                        // 된다. codex 는 한도 조회 경로가 아예 없어 이 자리가 없다.
                        if p == AccountProvider::Claude {
                            match (usage_of(&id), two_line) {
                                (Some(b), true) => {
                                    draw_usage_windows(
                                        g, sx + pad_x, sry + arow_h - 16.0, right, tf, &b,
                                    );
                                }
                                (Some(b), false) => {
                                    let t = usage_text(&b);
                                    let tw = g.measure_chrome_text(t.as_str(), tf, true);
                                    // 「사용 중」 배지와 겹치지 않게 그 왼쪽으로 물린다.
                                    let bx = if active {
                                        right - g.measure_chrome_text("사용 중", tf, true) - 8.0
                                    } else {
                                        right
                                    };
                                    g.draw_text(
                                        bx - tw, sry + (arow_h - tf) / 2.0 - 1.0, &t,
                                        gpu::DrawOpts { font_size: tf, color: pct_col(b.pct), bold: true, italic: false },
                                    );
                                }
                                // 값이 없으면 **빈칸으로 두지 않는다.** 빈칸은 「여유
                                // 있음」으로 읽혀서, 옮길지 말지를 정확히 반대로 만든다.
                                //
                                // 「조회 중」이라고는 안 한다 — 오래 안 쓴 슬롯은 OAuth
                                // 토큰이 8시간쯤에 만료되고 갱신은 그 계정으로 claude 를
                                // 돌릴 때 일어나므로, 기다려도 영영 안 온다. 곧 온다고
                                // 말해 놓고 안 오는 것이 모른다고 말하는 것보다 나쁘다.
                                (None, _) => {
                                    let t = "한도 모름";
                                    let ty2 = if two_line {
                                        sry + arow_h - 16.0
                                    } else {
                                        sry + (arow_h - tf) / 2.0 - 1.0
                                    };
                                    let tx = if two_line {
                                        sx + pad_x
                                    } else {
                                        right - g.measure_chrome_text(t, tf, false)
                                    };
                                    g.draw_text(
                                        tx, ty2, t,
                                        gpu::DrawOpts { font_size: tf, color: theme::text_mute(), bold: false, italic: false },
                                    );
                                }
                            }
                        }
                        if !active {
                            self.account_menu_hits
                                .push((AccountMenuItem::Select(p, id), (sx, sry, sw, arow_h)));
                        }
                        sry += arow_h;
                    }
                    g.rect(sx + pad, sry + 2.0, sw - pad * 2.0, 1.0, theme::border());
                    sry += rule;
                    {
                        let on = hmx >= sx && hmx <= sx + sw && hmy >= sry && hmy <= sry + row_h;
                        g.hover_pointer |= on;
                        if on {
                            round_rect(g, sx + pad, sry, sw - pad * 2.0, row_h,
                                theme::radius_sm(), theme::surface_active());
                        }
                        g.draw_text(
                            sx + pad_x, sry + (row_h - f) / 2.0 - 1.0, "계정 관리…",
                            gpu::DrawOpts { font_size: f, color: theme::text_dim(), bold: false, italic: false },
                        );
                        self.account_menu_hits
                            .push((AccountMenuItem::ManageAccounts, (sx, sry, sw, row_h)));
                    }
                }
            }
            let v_alpha = version_alpha;
            if v_alpha > 0.0 {
                let label = Self::version_label();
                let v_font = 11.0_f32;
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let text_w = g.measure_chrome_text(&label, v_font, false);
                let margin = 8.0;
                let x = (win_w - text_w - margin).max(margin);
                // 상태줄 **위**로. 창 바닥에 붙이던 자리인데 그 자리는 이제 상태줄이
                // 쓰고, 버전이 그 위에 덧그려져 자원 수치와 글자가 포개졌다(부팅 후
                // 몇 초라 놓치기 쉽다 — 2026-08-15 캡처에서 잡았다).
                let y = win_h - status_h - v_font - margin;
                let a = (170.0 * v_alpha).round() as u8;
                g.draw_text(
                    x,
                    y,
                    &label,
                    gpu::DrawOpts {
                        font_size: v_font,
                        color: theme::with_alpha(theme::text_dim(), a),
                        bold: false,
                        italic: false,
                    },
                );
            }
            // 커밋 모달 — 전면 스크림을 깐 진짜 대화상자라, 창 안의 모든 것보다
            // 나중에 그려져야 한다. 사이드바 블록 안에서 그리던 동안엔 그 뒤에
            // 오는 pane 헤더·divider·활성 보더가 카드 위를 가로질렀다(거노).
            // ── Commit modal (screenshot #5): dim + centered card.
            self.git.commit_modal_rects.clear();
            if self.git.commit_modal_open {
                // Full-window dim + centered card (not clipped to the git
                // column) so the modal reads as a real dialog and nothing
                // behind it bleeds through.
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(0.0, 0.0, win_w, win_h, theme::with_alpha([0, 0, 0, 255], 0xCC));
                let bw = 560.0_f32.min(win_w - 60.0).max(0.0);
                let bx = (win_w - bw) / 2.0;
                let bh = (win_h - TITLE_HEIGHT - 60.0).min(660.0).max(0.0);
                let bxy = TITLE_HEIGHT + (win_h - TITLE_HEIGHT - bh) / 2.0;
                round_rect(g, bx - 1.0, bxy - 1.0, bw + 2.0, bh + 2.0, theme::radius_md(), theme::with_alpha(theme::border(), 0xFF));
                round_rect(g, bx, bxy, bw, bh, theme::radius_md(), theme::bg());
                let pad = 22.0_f32;
                let cx = bx + pad;
                let cw = bw - pad * 2.0;
                let mut my = bxy + pad;
                // Header: icon chip + X
                round_rect(g, cx, my, 36.0, 36.0, theme::radius_sm(), theme::surface_active());
                g.queue_icon("git-commit-horizontal", cx + 10.0, my + 10.0, 16.0, theme::text());
                let xx = bx + bw - pad - 16.0;
                let xhov = self.cursor_px.0 >= xx - 5.0 && self.cursor_px.0 <= xx + 21.0 && self.cursor_px.1 >= my && self.cursor_px.1 <= my + 24.0;
                g.queue_icon("x", xx, my + 4.0, 16.0, if xhov { theme::text() } else { theme::text_mute() });
                self.git.commit_modal_rects.push((GitModalBtn::Close, (xx - 5.0, my, 26.0, 26.0)));
                my += 36.0 + 18.0;
                g.draw_text(cx, my, "Commit your changes", gpu::DrawOpts { font_size: 19.0, color: theme::text(), bold: true, italic: false });
                my += 36.0;
                // Branch
                g.draw_text(cx, my, "Branch", gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: false, italic: false });
                my += 22.0;
                g.queue_icon("git-branch", cx, my, 15.0, theme::text_dim());
                let mbranch = if git_view.branch.is_empty() { "—" } else { git_view.branch.as_str() };
                g.draw_text(cx + 22.0, my + 1.0, mbranch, gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: false, italic: false });
                my += 34.0;
                // Changes + Include unstaged toggle
                g.draw_text(cx, my, "Changes", gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: false, italic: false });
                let tw = 38.0_f32;
                let th = 20.0_f32;
                let tx = bx + bw - pad - tw;
                let tlbl = "Include unstaged";
                let tlw = g.measure_chrome_text(tlbl, 13.0, false);
                g.draw_text(tx - 8.0 - tlw, my, tlbl, gpu::DrawOpts { font_size: 13.0, color: theme::text_dim(), bold: false, italic: false });
                let on = self.git.commit_modal_include_unstaged;
                pill_rect(g, tx, my - 2.0, tw, th, if on { theme::accent() } else { theme::surface_active() });
                let knob = th - 6.0;
                let kx = if on { tx + tw - knob - 3.0 } else { tx + 3.0 };
                circle_rect(g, kx, my - 2.0 + 3.0, knob, [255, 255, 255, 255]);
                self.git.commit_modal_rects.push((GitModalBtn::IncludeUnstaged, (tx - 4.0, my - 5.0, tw + 8.0, th + 8.0)));
                my += 28.0;
                // File list box
                let lh = (bh * 0.28).min(180.0).max(60.0);
                panel_rect_outlined(g, cx, my, cw, lh, theme::radius_sm(), theme::surface());
                let nf = git_view.staged.len() + git_view.unstaged.len();
                let mut fx = g.draw_text(cx + 12.0, my + 10.0, &format!("{} files", nf), gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false });
                fx = g.draw_text(fx + 10.0, my + 10.0, &format!("+{}", git_view.insertions), gpu::DrawOpts { font_size: 13.0, color: theme::success(), bold: false, italic: false });
                g.draw_text(fx + 8.0, my + 10.0, &format!("-{}", git_view.deletions), gpu::DrawOpts { font_size: 13.0, color: DIFF_RED, bold: false, italic: false });
                let mut ly = my + 34.0;
                for (_m, path) in git_view.staged.iter().chain(git_view.unstaged.iter()) {
                    if ly > my + lh - 18.0 {
                        break;
                    }
                    let fname = path.rsplit('/').next().unwrap_or(path.as_str());
                    let dir = path.strip_suffix(fname).unwrap_or("").trim_end_matches('/');
                    let ex = g.draw_text(cx + 12.0, ly, fname, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                    if !dir.is_empty() {
                        g.draw_text(ex + 7.0, ly + 0.5, dir, gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false });
                    }
                    if let Some((ins, del)) = git_view.numstat.get(path) {
                        let minus = format!("-{del}");
                        let plus = format!("+{ins}");
                        let wm = g.measure_chrome_text(&minus, 12.0, false);
                        let wp = g.measure_chrome_text(&plus, 12.0, false);
                        let mut rx = cx + cw - 12.0;
                        if *del > 0 {
                            rx -= wm;
                            g.draw_text(rx, ly, &minus, gpu::DrawOpts { font_size: 12.0, color: DIFF_RED, bold: false, italic: false });
                            rx -= 6.0;
                        }
                        if *ins > 0 {
                            rx -= wp;
                            g.draw_text(rx, ly, &plus, gpu::DrawOpts { font_size: 12.0, color: theme::success(), bold: false, italic: false });
                        }
                    }
                    ly += 22.0;
                }
                my += lh + 18.0;
                // Commit message box
                g.draw_text(cx, my, "Commit message", gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: false, italic: false });
                my += 22.0;
                let inh = 70.0_f32;
                if self.git.commit_focused {
                    round_rect(g, cx - 1.0, my - 1.0, cw + 2.0, inh + 2.0, theme::radius_sm(), theme::accent());
                }
                panel_rect(g, cx, my, cw, inh, theme::radius_sm(), theme::surface());
                let itx = cx + 10.0;
                let ity = my + 9.0;
                let preedit = if self.git.commit_focused { self.preedit.as_str() } else { "" };
                if self.git.commit_msg.is_empty() && preedit.is_empty() {
                    g.draw_text(itx, ity, "변경 사항 설명…", gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: false, italic: false });
                }
                let cur = self.git.commit_cursor.min(self.git.commit_msg.chars().count());
                let before: String = self.git.commit_msg.chars().take(cur).collect();
                let after: String = self.git.commit_msg.chars().skip(cur).collect();
                let mut px = g.draw_text(itx, ity, &before, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                let caret_x = px;
                if !preedit.is_empty() {
                    px = g.draw_text(px, ity, preedit, gpu::DrawOpts { font_size: 13.0, color: theme::accent(), bold: false, italic: false });
                }
                if !after.is_empty() {
                    g.draw_text(px, ity, &after, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                }
                if self.git.commit_focused && preedit.is_empty() && commit_caret_on {
                    g.rect(caret_x, ity, 1.5, 14.0, theme::text());
                }
                self.git.commit_input_rect = Some((cx, my, cw, inh));
                my += inh + 14.0;
                // Commit / Commit and push buttons (full width)
                let bbh = 36.0_f32;
                for (icon, label, btn) in [
                    ("git-commit-horizontal", "Commit", GitModalBtn::Commit),
                    ("arrow-up", "Commit and push", GitModalBtn::CommitAndPush),
                ] {
                    let hov = self.cursor_px.0 >= cx && self.cursor_px.0 <= cx + cw && self.cursor_px.1 >= my && self.cursor_px.1 <= my + bbh;
                    panel_rect(g, cx, my, cw, bbh, theme::radius_sm(), if hov { theme::surface_hover() } else { theme::surface_active() });
                    g.queue_icon(icon, cx + 14.0, my + (bbh - 15.0) / 2.0, 15.0, theme::text());
                    g.draw_text(cx + 38.0, my + (bbh - 13.0) / 2.0, label, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                    self.git.commit_modal_rects.push((btn, (cx, my, cw, bbh)));
                    my += bbh + 8.0;
                }
                // Cancel / Confirm (bottom-right)
                let confirm_w = 96.0_f32;
                let cancel_w = 80.0_f32;
                let cby = bxy + bh - pad - 34.0;
                let conf_x = bx + bw - pad - confirm_w;
                let canc_x = conf_x - 10.0 - cancel_w;
                let conf_hov = self.cursor_px.0 >= conf_x && self.cursor_px.0 <= conf_x + confirm_w && self.cursor_px.1 >= cby && self.cursor_px.1 <= cby + 34.0;
                let canc_hov = self.cursor_px.0 >= canc_x && self.cursor_px.0 <= canc_x + cancel_w && self.cursor_px.1 >= cby && self.cursor_px.1 <= cby + 34.0;
                let wcanc = g.measure_chrome_text("Cancel", 13.0, false);
                g.draw_text(canc_x + (cancel_w - wcanc) / 2.0, cby + 10.0, "Cancel", gpu::DrawOpts { font_size: 13.0, color: if canc_hov { theme::text() } else { theme::text_dim() }, bold: false, italic: false });
                self.git.commit_modal_rects.push((GitModalBtn::Cancel, (canc_x, cby, cancel_w, 34.0)));
                panel_rect(g, conf_x, cby, confirm_w, 34.0, theme::radius_sm(), if conf_hov { theme::accent() } else { theme::surface_active() });
                let wconf = g.measure_chrome_text("Confirm", 13.0, true);
                g.draw_text(conf_x + (confirm_w - wconf) / 2.0, cby + 10.0, "Confirm", gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false });
                self.git.commit_modal_rects.push((GitModalBtn::Confirm, (conf_x, cby, confirm_w, 34.0)));
            }
            // Confirm-close modal: a dim scrim + centered card with 취소/닫기,
            // queued last so it sits over every pane, overlay and toast.
            if let Some(dlg) = self.confirm_close.clone() {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(0.0, 0.0, win_w, win_h, theme::with_alpha([0, 0, 0, 255], 0xB0));
                let dirty = matches!(dlg.why, crate::CloseWhy::Dirty(_));
                let what = match dlg.action {
                    crate::PendingClose::Window => "앱을",
                    crate::PendingClose::Session(_) => "이 세션을",
                    crate::PendingClose::AuxEditor(_) => "이 창을",
                    _ => "이 탭을",
                };
                let (title, subtitle) = match &dlg.why {
                    crate::CloseWhy::Busy(proc) => (
                        format!("{proc} 실행 중이에요"),
                        format!("{what} 닫을까요?"),
                    ),
                    // 파일 이름을 보여줘야 뭘 잃는지 안다. 셋을 넘으면 카드가
                    // 감당 못 하니 나머지는 개수로 접는다.
                    crate::CloseWhy::Dirty(docs) => {
                        let head: Vec<&str> =
                            docs.iter().take(3).map(|(_, n)| n.as_str()).collect();
                        let rest = docs.len().saturating_sub(head.len());
                        let names = if rest > 0 {
                            format!("{} 외 {rest}개", head.join(", "))
                        } else {
                            head.join(", ")
                        };
                        (
                            "저장하지 않은 변경이 있어요".to_string(),
                            format!("{names} — {what} 닫을까요?"),
                        )
                    }
                    // 왜 하나가 아니라 방이 닫히는지부터 말한다 — Cmd+W 는 「하나
                    // 닫기」로 익힌 키라, 이유 없이 방 확인이 뜨면 오작동처럼 읽힌다.
                    crate::CloseWhy::LastPane => (
                        "이 방의 마지막 pane 이에요".to_string(),
                        format!("{what} 닫을까요?"),
                    ),
                };
                let title = &title;
                let subtitle = subtitle.as_str();
                // 부제에 파일 이름이 들어와 길이가 변하니 카드도 재서 정한다 —
                // 고정 폭이던 시절엔 이름 셋이 그대로 카드 밖으로 나갔다.
                let pad = 24.0_f32;
                let title_w = g.measure_chrome_text(title, 15.0, true);
                let sub_w = g.measure_chrome_text(subtitle, 13.0, false);
                let card_w = (title_w.max(sub_w) + pad * 2.0)
                    .clamp(if dirty { 420.0 } else { 360.0 }, (win_w - 48.0).max(420.0));
                let card_h = 168.0_f32;
                let cx0 = ((win_w - card_w) / 2.0).round();
                let cy0 = ((win_h - card_h) / 2.0).round();
                panel_rect_outlined(g, cx0, cy0, card_w, card_h, theme::radius_md(), theme::surface_active());
                g.draw_text(
                    cx0 + pad,
                    cy0 + 30.0,
                    &title,
                    gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
                );
                g.draw_text(
                    cx0 + pad,
                    cy0 + 60.0,
                    subtitle,
                    gpu::DrawOpts { font_size: 13.0, color: theme::text_dim(), bold: false, italic: false },
                );
                let (mx, my) = self.cursor_px;
                let bf = 13.0_f32;
                let bpad = 18.0_f32;
                let btn_h = 34.0_f32;
                let btn_y = cy0 + card_h - 20.0 - btn_h;
                // 오른쪽부터 왼쪽으로 쌓는다 — 기본 동작이 오른쪽 끝에 오는 배치.
                let mut right = cx0 + card_w - pad;
                // 채움색이 곧 뜻이다: accent = 기본 동작, danger = 되돌릴 수
                // 없는 쪽, 무채색 = 그 외. 저장은 파괴적이지 않으니 빨강을 안 쓴다.
                let mut button = |g: &mut gpu::GpuRenderer,
                                  hits: &mut Vec<(crate::ConfirmBtn, (f32, f32, f32, f32))>,
                                  label: &str,
                                  btn: crate::ConfirmBtn,
                                  tone: Option<[u8; 4]>| {
                    let w = g.measure_chrome_text(label, bf, tone.is_some()) + bpad * 2.0;
                    let x = right - w;
                    right = x - 10.0;
                    let hot = mx >= x && mx <= x + w && my >= btn_y && my <= btn_y + btn_h;
                    g.hover_pointer |= hot;
                    let (fill, fg, bold) = match tone {
                        Some(c) => (
                            theme::with_alpha(c, if hot { 0xFF } else { 0xDD }),
                            [0xFF, 0xFF, 0xFF, 0xFF],
                            true,
                        ),
                        None => (
                            theme::raised_on(theme::surface_active(), hot),
                            theme::text(),
                            false,
                        ),
                    };
                    // 무채색 쪽은 테두리로 선다 — 주액션과 갈리는 축이 색 하나가
                    // 아니라 형태여야 흑백에서도 어느 쪽이 기본인지 읽힌다.
                    if tone.is_some() {
                        panel_rect(g, x, btn_y, w, btn_h, theme::radius_sm(), fill);
                    } else {
                        panel_rect_outlined(g, x, btn_y, w, btn_h, theme::radius_sm(), fill);
                    }
                    g.draw_text(
                        x + bpad,
                        btn_y + (btn_h - bf) / 2.0,
                        label,
                        gpu::DrawOpts { font_size: bf, color: fg, bold, italic: false },
                    );
                    hits.push((btn, (x, btn_y, w, btn_h)));
                };
                if dirty {
                    // 저장이 기본이라 오른쪽 끝 — 실수로 끝을 눌러도 안전한 쪽이
                    // 걸리게. 편집분을 버리는 "저장 안 함" 은 그 왼쪽에 빨강으로.
                    let acc = theme::accent();
                    button(g, &mut confirm_btn_hits, "저장", crate::ConfirmBtn::Save, Some(acc));
                    let dg = theme::danger();
                    button(g, &mut confirm_btn_hits, "저장 안 함", crate::ConfirmBtn::Close, Some(dg));
                } else {
                    let dg = theme::danger();
                    button(g, &mut confirm_btn_hits, "닫기", crate::ConfirmBtn::Close, Some(dg));
                }
                button(g, &mut confirm_btn_hits, "취소", crate::ConfirmBtn::Cancel, None);
            }
            // Chrome-style restore prompt: dim scrim + centered card offering to
            // reopen the last session's panes. Queued after the confirm modal so
            // it sits over everything at launch. [복원] rebuilds the workspace,
            // [새로 시작] keeps the fresh session.
            if let Some(state) = self.restore_prompt.clone() {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(0.0, 0.0, win_w, win_h, theme::with_alpha([0, 0, 0, 255], 0xB0));
                let n = crate::App::count_claude_panes(&state);
                // claude 가 없는 창도 복원 대상이라, 있을 때만 그 수를 덧붙인다.
                let total = crate::App::count_panes(&state);
                let subtitle = if n > 0 {
                    format!("claude 세션 {n}개를 포함한 pane {total}개를 마지막 레이아웃 그대로 이어서 켭니다")
                } else {
                    format!("pane {total}개를 마지막 레이아웃 그대로 이어서 켭니다")
                };
                const RESTORE_TITLE: &str = "이전 세션을 복원할까요?";
                let pad = 26.0_f32;
                let bf = 13.0_f32;
                let bpad = 18.0_f32;
                let btn_h = 34.0_f32;
                let btn_gap = 8.0_f32;
                // 두 버튼은 **같은 폭**으로 간다. 글자 수대로 재면 기본 액션인 "복원"(2자)이
                // 부액션 "새로 시작"(4자)보다 좁아져, 색을 걷어내면 큰 쪽이 주액션으로
                // 읽혔다 — 위계가 accent 파랑 하나에만 얹혀 있었다는 뜻이다.
                let btn_w = g
                    .measure_chrome_text("새로 시작", bf, false)
                    .max(g.measure_chrome_text("복원", bf, true))
                    + bpad * 2.0;
                // 카드 폭은 두 줄을 실측해서 나온다. 448 을 박아 두었더니 크롬 페이스를
                // 픽셀로 바꾸는 순간 부제 꼬리가 카드 밖으로 잘렸다 — 같은 문장이
                // pane 수 자릿수로도 길어지니 애초에 잴 일이었다.
                let title_w = g.measure_chrome_text(RESTORE_TITLE, 15.0, true);
                let sub_w = g.measure_chrome_text(&subtitle, 13.0, false);
                let body_w = title_w.max(sub_w).max(btn_w * 2.0 + btn_gap);
                let card_w = (body_w + pad * 2.0).clamp(448.0, (win_w - 48.0).max(448.0));
                // 높이도 내용에서 나온다. 176 을 박아 두었더니 글 두 줄이 위에 몰리고
                // 그 아래 60px 가 아무 이유 없이 비어, 카드가 대충 얹힌 것처럼 보였다.
                let title_y = 28.0_f32;
                let sub_y = title_y + 30.0;
                let btn_dy = sub_y + 40.0;
                let card_h = btn_dy + btn_h + 26.0;
                let cx0 = ((win_w - card_w) / 2.0).round();
                let cy0 = ((win_h - card_h) / 2.0).round();
                panel_rect_outlined(g, cx0, cy0, card_w, card_h, theme::radius_md(), theme::surface_active());
                g.draw_text(
                    cx0 + pad,
                    cy0 + title_y,
                    RESTORE_TITLE,
                    gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
                );
                g.draw_text(
                    cx0 + pad,
                    cy0 + sub_y,
                    &subtitle,
                    gpu::DrawOpts { font_size: 13.0, color: theme::text_dim(), bold: false, italic: false },
                );
                let (mx, my) = self.cursor_px;
                let btn_y = cy0 + btn_dy;
                let hit = |x: f32| mx >= x && mx <= x + btn_w && my >= btn_y && my <= btn_y + btn_h;
                // 복원 (primary/accent), flush to the card's right edge.
                let restore_x = cx0 + card_w - pad - btn_w;
                let restore_hover = hit(restore_x);
                g.hover_pointer |= restore_hover;
                panel_rect(
                    g,
                    restore_x,
                    btn_y,
                    btn_w,
                    btn_h,
                    theme::radius_sm(),
                    theme::with_alpha(theme::accent(), if restore_hover { 0xFF } else { 0xDD }),
                );
                let rl_w = g.measure_chrome_text("복원", bf, true);
                g.draw_text(
                    restore_x + (btn_w - rl_w) / 2.0,
                    btn_y + (btn_h - bf) / 2.0,
                    "복원",
                    gpu::DrawOpts { font_size: bf, color: theme::fg(), bold: true, italic: false },
                );
                restore_btn_hits.push((crate::RestoreBtn::Restore, (restore_x, btn_y, btn_w, btn_h)));
                // 새로 시작, to its left. 채움 대신 테두리로 갈린다 — 주액션과 갈리는 축이
                // 색 하나가 아니라 형태여야 흑백에서도 어느 쪽이 기본인지 읽힌다.
                let fresh_x = restore_x - btn_gap - btn_w;
                let fresh_hover = hit(fresh_x);
                g.hover_pointer |= fresh_hover;
                panel_rect_outlined(
                    g,
                    fresh_x,
                    btn_y,
                    btn_w,
                    btn_h,
                    theme::radius_sm(),
                    theme::raised_on(theme::surface_active(), fresh_hover),
                );
                let fl_w = g.measure_chrome_text("새로 시작", bf, false);
                g.draw_text(
                    fresh_x + (btn_w - fl_w) / 2.0,
                    btn_y + (btn_h - bf) / 2.0,
                    "새로 시작",
                    gpu::DrawOpts { font_size: bf, color: theme::text(), bold: false, italic: false },
                );
                restore_btn_hits.push((crate::RestoreBtn::Fresh, (fresh_x, btn_y, btn_w, btn_h)));
            }
            // File-tree drag ghost — a small pill trailing the cursor with the
            // dragged item's name, drawn last so it floats over everything.
            if let Some(drag) = self.file_tree.drag.as_ref() {
                if drag.active {
                    let name = drag
                        .path
                        .file_name()
                        .map(|n| nfc_hangul(&n.to_string_lossy()))
                        .unwrap_or_default();
                    let is_dir = self
                        .file_tree.nodes
                        .iter()
                        .find(|n| n.path == drag.path)
                        .map(|n| n.is_dir)
                        .unwrap_or(false);
                    let (cx, cy) = self.cursor_px;
                    let gf = 12.0_f32;
                    let tw = g.measure_chrome_text(&name, gf, false);
                    let pill_w = 18.0 + tw + 16.0;
                    let pill_h = 22.0_f32;
                    let gx = cx + 12.0;
                    let gy = cy + 10.0;
                    round_rect(g, gx, gy, pill_w, pill_h, theme::radius_sm(), theme::accent());
                    round_rect(g, gx + 1.0, gy + 1.0, pill_w - 2.0, pill_h - 2.0,
                        theme::radius_sm() - 1.0, theme::with_alpha(theme::surface_active(), 0xF5));
                    g.queue_icon(if is_dir { "folder" } else { "file" },
                        gx + 6.0, gy + (pill_h - 14.0) / 2.0, 14.0, theme::text());
                    g.draw_text(gx + 24.0, gy + (pill_h - gf) / 2.0, &name,
                        gpu::DrawOpts { font_size: gf, color: theme::text(), bold: false, italic: false });
                }
            }
            // Pane header drag ghost — 잡은 pane 이 커서를 따라오는 pill(파일트리
            // drag ghost 와 동일 방식). 거노: pane 을 잡았을 때 "잡혔다"는 피드백이
            // 없어 마우스가 안 따라오는 느낌. 라벨은 display_pane_char(캐릭터 표시명,
            // 없으면 pane id). update_live_drag(라이브 재배치)와 별개의 최상단 층이라
            // 미리보기 무손상 — 커서가 사이드바로 나가 자리 프리뷰가 원위치로 돌아가도
            // 이 pill 은 계속 커서를 따라와 무엇을 어디로 옮기는지 보여준다.
            if self.header_drag.as_ref().is_some_and(|hd| hd.active) {
                let pane_id = self.header_drag.as_ref().unwrap().pane.clone();
                // display_pane_char 를 인라인 — g(self 가변 빌림)가 살아있어 &self
                // 메서드는 못 부르고 필드 직접 접근만 된다(파일트리 ghost 와 동일 제약).
                let label = self
                    .pane_claude_sid
                    .get(&pane_id)
                    .and_then(|sid| kasa_mcp::character::session_character(sid))
                    .or_else(|| {
                        let agents = self
                            .pty
                            .get(&pane_id)
                            .map(|p| p.is_claude_agents())
                            .unwrap_or(false);
                        if agents {
                            None
                        } else {
                            self.ws
                                .lock()
                                .ok()
                                .and_then(|ws| ws.pane_character.get(&pane_id).cloned())
                        }
                    })
                    .unwrap_or(pane_id);
                let (cx, cy) = self.cursor_px;
                let gf = 12.0_f32;
                let tw = g.measure_chrome_text(&label, gf, true);
                let pill_w = 14.0 + tw + 14.0;
                let pill_h = 22.0_f32;
                let gx = cx + 12.0;
                let gy = cy + 10.0;
                round_rect(g, gx, gy, pill_w, pill_h, theme::radius_sm(), theme::accent());
                round_rect(
                    g,
                    gx + 1.0,
                    gy + 1.0,
                    pill_w - 2.0,
                    pill_h - 2.0,
                    theme::radius_sm() - 1.0,
                    theme::with_alpha(theme::surface_active(), 0xF5),
                );
                g.draw_text(
                    gx + 14.0,
                    gy + (pill_h - gf) / 2.0,
                    &label,
                    gpu::DrawOpts { font_size: gf, color: theme::accent(), bold: true, italic: false },
                );
            }
            // 테마 전환 — 옛 배경색이 픽셀 블록으로 부서지며 걷힌다. 맨 마지막에
            // 그려 화면 전체(터미널·크롬·모달)를 한 장으로 덮는다.
            if let Some((at, old_bg)) = self.theme_fx {
                let t = at.elapsed().as_secs_f32() / THEME_FX_SECS;
                if t >= 1.0 {
                    self.theme_fx = None;
                } else {
                    paint_theme_dissolve(g, t, old_bg, win_px.0 / scale, win_px.1 / scale);
                }
            }
            // 방 펼침이 도는 동안은 다음 장을 스스로 부른다 — 사이드바는 입력이
            // 없으면 다시 안 그려지므로, 손을 뗀 자리에서 목록이 멈춰 버린다.
            if let Some((_, _, at)) = self.expand_anim {
                if at.elapsed().as_secs_f32() >= EXPAND_ANIM_SECS {
                    self.expand_anim = None;
                } else {
                    self.chrome_dirty = true;
                }
            }
            if let Err(e) = g.render(&slot_views, scale, time_secs, true) {
                eprintln!("[gpu] render error: {e:?}");
            }
        }
        self.confirm_btn_rects = confirm_btn_hits;
        self.restore_btn_rects = restore_btn_hits;
        self.pane_tab_rects = tab_hits;
        self.pane_tab_close_rects = tab_close_hits;
        self.pane_tab_popout_rects = tab_popout_hits;
        self.pane_restart_chip_rects = restart_chip_hits;
        self.pane_plus_rects = plus_hits;
        // Tab-windowing write-back: clamped first + fit count for the wheel
        // handler, and this frame's active tab for the next reveal check.
        // No dirty flip — this must not schedule another frame.
        if let Ok(mut ws) = self.ws.lock() {
            for (id, first, vis, act) in &pane_tab_windowing {
                if let Some(p) = ws.panes.get_mut(id) {
                    p.tab_first = *first;
                    p.tab_vis = *vis;
                    p.tab_last_active = *act;
                }
            }
        }
        self.image_btn_rects = image_btn_hits;
        self.pane_action_hits = pane_action_hits;
        // body_rects collected per pane in case future overlays need them.
        let _ = body_rects;
        // Damage flags get cleared here (parity with sugarloaf path
        // below) so successive frames short-circuit on idle.
        if let Ok(mut ws) = self.ws.lock() {
            for pane in ws.panes.values_mut() {
                pane.dirty = false;
            }
        }
        self.chrome_dirty = false;
        // A bake that found no room during this frame left blank cells behind.
        // The repack happens at the top of the next frame — but an idle app
        // paints no next frame, so the blanks would just sit there. Ask for it.
        if self.gpu.as_ref().is_some_and(|g| g.atlas_needs_another_frame()) {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        // Keep the frame loop alive while a git op spins, so the spinner
        // animates until GitOpDone clears it.
        if self.git.op.is_some() {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Find/replace bar, floated over the top-right of a raw editor's body box
    /// (`x`/`y`/`w`). Returns its clickable rects in logical px.
    ///
    /// 호버 툴팁 — 마우스 아래에 뜨는 작은 상자. rust-analyzer 는 타입에 문서
    /// 전체를 붙여 주기도 해서 가로·세로 둘 다 자른다: 화면 절반을 덮는 툴팁은
    /// 정보가 아니라 방해다.
    /// `[Image #N]` 글자 옆에 그 그림의 썸네일을 띄운다.
    ///
    /// 텍스처 키는 픽셀 버퍼의 주소라, 툴팁이 다른 그림으로 바뀌면 키도 바뀐다 —
    /// 앞 키를 놓지 않으면 호버할 때마다 GPU 메모리가 는다. 그래서 툴팁이 없는
    /// 프레임(`tip` = `None`)에도 불러 정리할 기회를 준다.
    fn paint_image_tip(
        g: &mut gpu::GpuRenderer,
        tip: Option<((Arc<Vec<u8>>, u32, u32), (f32, f32, f32, f32))>,
        win_w: f32,
        win_h: f32,
    ) {
        thread_local! {
            static LAST_KEY: std::cell::RefCell<Option<String>> =
                const { std::cell::RefCell::new(None) };
        }
        let Some(((rgba, iw, ih), (ax, ay, _aw, ah))) = tip else {
            LAST_KEY.with(|l| {
                if let Some(old) = l.borrow_mut().take() {
                    g.drop_image(&old);
                }
            });
            return;
        };
        if iw == 0 || ih == 0 {
            return;
        }
        let key = format!("imgtip:{:x}:{}", Arc::as_ptr(&rgba) as usize, rgba.len());
        LAST_KEY.with(|l| {
            let mut l = l.borrow_mut();
            if l.as_deref() != Some(key.as_str()) {
                if let Some(old) = l.take() {
                    g.drop_image(&old);
                }
                g.upload_image(&key, &rgba, iw, ih);
                *l = Some(key.clone());
            }
        });
        // 액자. 확대는 하지 않는다 — 작은 그림을 늘리면 흐려지기만 한다.
        const MAX_W: f32 = 320.0;
        const PAD: f32 = 5.0;
        let s = (MAX_W / iw as f32)
            .min((win_h * 0.5).max(120.0) / ih as f32)
            .min(1.0);
        let (dw, dh) = (iw as f32 * s, ih as f32 * s);
        let (w, h) = (dw + PAD * 2.0, dh + PAD * 2.0);
        // 글자 바로 아래가 기본 — 그 위는 방금 읽은 프롬프트라 덮으면 안 된다.
        // 아래가 모자라면 위로 뒤집고, 오른쪽으로 넘치면 왼쪽으로 민다.
        let x = ax.min(win_w - w - 4.0).max(4.0);
        let y = if ay + ah + 6.0 + h < win_h {
            ay + ah + 6.0
        } else {
            (ay - 6.0 - h).max(4.0)
        };
        let r = theme::radius_sm();
        // 뒷판을 한 겹 넓게 깔아 터미널 글자 위에서 액자가 떠 보이게 한다 —
        // 이 렌더러엔 그림자가 없다.
        g.round_rect_fill(
            x - 2.0,
            y - 1.0,
            w + 4.0,
            h + 5.0,
            r + 2.0,
            theme::with_alpha(theme::bg(), 0x66),
        );
        g.round_rect_fill(x, y, w, h, r, theme::surface());
        let edge = theme::border();
        g.rect(x, y, w, 1.0, edge);
        g.rect(x, y + h - 1.0, w, 1.0, edge);
        g.rect(x, y, 1.0, h, edge);
        g.rect(x + w - 1.0, y, 1.0, h, edge);
        // icon 패스라 방금 깐 액자 위에 온다.
        g.queue_image_above(&key, x + PAD, y + PAD, dw, dh);
    }

    fn draw_hover_tip(
        g: &mut gpu::GpuRenderer,
        text: &str,
        mx: f32,
        my: f32,
        win_w: f32,
        win_h: f32,
    ) {
        const MAX_LINES: usize = 10;
        const MAX_COLS: usize = 78;
        const PAD: f32 = 7.0;
        let (_, lh0) = g.raw_editor_metrics();
        let size = lh0 / 1.25 * 0.92;
        let lh = size * 1.35;
        let lines: Vec<String> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(MAX_LINES)
            .map(|l| {
                if l.chars().count() > MAX_COLS {
                    format!("{}…", l.chars().take(MAX_COLS).collect::<String>())
                } else {
                    l.to_string()
                }
            })
            .collect();
        if lines.is_empty() {
            return;
        }
        let tw = lines
            .iter()
            .map(|l| g.measure_pen_run(l, size, false, false))
            .fold(0.0f32, f32::max);
        let w = tw + PAD * 2.0;
        let h = lines.len() as f32 * lh + PAD * 2.0;
        // 마우스 아래가 기본. 아래가 모자라면 위로 뒤집고, 오른쪽으로 넘치면
        // 왼쪽으로 민다 — 창 밖으로 나간 툴팁은 그리나 마나다.
        let x = (mx + 12.0).min(win_w - w - 4.0).max(4.0);
        let y = if my + 18.0 + h < win_h {
            my + 18.0
        } else {
            (my - 12.0 - h).max(4.0)
        };
        g.rect(x, y, w, h, theme::surface());
        let edge = theme::border();
        g.rect(x, y, w, 1.0, edge);
        g.rect(x, y + h - 1.0, w, 1.0, edge);
        g.rect(x, y, 1.0, h, edge);
        g.rect(x + w - 1.0, y, 1.0, h, edge);
        for (i, l) in lines.iter().enumerate() {
            g.draw_text(
                x + PAD,
                y + PAD + i as f32 * lh,
                l,
                gpu::DrawOpts {
                    font_size: size,
                    color: theme::text(),
                    bold: false,
                    italic: false,
                },
            );
        }
    }

    /// It overlays rather than pushing the text down, so opening it never
    /// reflows what you were reading — the same reason VS Code floats its own.
    fn draw_find_bar(
        g: &mut gpu::GpuRenderer,
        f: &FindState,
        x: f32,
        y: f32,
        w: f32,
        preedit: &str,
        caret_on: bool,
        cursor: (f32, f32),
    ) -> Vec<(FindBtn, (f32, f32, f32, f32))> {
        const PAD: f32 = 8.0;
        const ROW: f32 = 26.0;
        const BTN: f32 = 24.0;
        const FIELD_W: f32 = 190.0;
        const COUNT_W: f32 = 58.0;
        const TOGGLE_W: f32 = 16.0;
        const FS: f32 = 12.0;
        let mut hits = Vec::new();

        let bar_w = PAD + TOGGLE_W + 6.0 + FIELD_W + COUNT_W + 6.0 + BTN * 3.0 + PAD;
        let rows = if f.replacing { 2.0 } else { 1.0 };
        let bar_h = PAD + ROW * rows + (rows - 1.0) * 4.0 + PAD;
        let x0 = (x + w - bar_w - 10.0).max(x + 4.0);
        let y0 = y + 6.0;
        // 얇은 테두리는 바깥에 한 겹 더 그려서 낸다 — 편집기 본문 위에 뜨는
        // 물건이라 경계가 없으면 글자에 파묻힌다.
        round_rect(g, x0 - 1.0, y0 - 1.0, bar_w + 2.0, bar_h + 2.0, theme::radius_md() + 1.0, theme::border());
        round_rect(g, x0, y0, bar_w, bar_h, theme::radius_md(), theme::surface());

        let text_baseline = |row_y: f32| row_y + (ROW - FS) * 0.5 - 1.0;
        let row1 = y0 + PAD;

        let hot = |r: (f32, f32, f32, f32)| {
            cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
        };

        // 바꾸기 행 펼침/접기.
        let tg = (x0 + PAD, row1 + (ROW - TOGGLE_W) * 0.5, TOGGLE_W, TOGGLE_W);
        let tg_hit = (tg.0 - 2.0, row1, TOGGLE_W + 4.0, ROW);
        let tg_hov = hot(tg_hit);
        if tg_hov {
            hover_rect(g, tg_hit.0, tg_hit.1, tg_hit.2, tg_hit.3, theme::radius_sm());
        }
        g.queue_icon(
            if f.replacing { "chevron-down" } else { "chevron-right" },
            tg.0,
            tg.1,
            TOGGLE_W,
            if tg_hov { theme::text() } else { theme::text_dim() },
        );
        hits.push((FindBtn::ToggleReplace, tg_hit));

        // 입력칸 하나를 그린다. 넘치는 글자는 왼쪽으로 밀어 끝(캐럿 쪽)을
        // 보여 준다 — 앞머리만 남으면 지금 뭘 치고 있는지 안 보인다.
        let field_x = x0 + PAD + TOGGLE_W + 6.0;
        let field = |g: &mut gpu::GpuRenderer,
                         row_y: f32,
                         width: f32,
                         text: &str,
                         placeholder: &str,
                         focused: bool,
                         pe: &str| {
            round_rect(g, field_x, row_y, width, ROW, theme::radius_sm(), theme::bg());
            if focused {
                round_rect(g, field_x, row_y, width, ROW, theme::radius_sm(), theme::with_alpha(theme::accent(), 0x22));
            }
            let inner_l = field_x + 7.0;
            let inner_r = field_x + width - 7.0;
            let by = text_baseline(row_y);
            if text.is_empty() && pe.is_empty() {
                g.draw_text(
                    inner_l,
                    by,
                    placeholder,
                    gpu::DrawOpts { font_size: FS, color: theme::text_mute(), bold: false, italic: false },
                );
                if focused && caret_on {
                    g.rect(inner_l, row_y + 5.0, 1.5, ROW - 10.0, theme::accent());
                }
                return;
            }
            let tw = g.measure_chrome_text(text, FS, false)
                + if pe.is_empty() { 0.0 } else { g.measure_chrome_text(pe, FS, false) };
            let shift = (tw - (inner_r - inner_l)).max(0.0);
            let mut pen = g.draw_text_clipped(
                inner_l - shift,
                by,
                text,
                gpu::DrawOpts { font_size: FS, color: theme::text(), bold: false, italic: false },
                inner_l,
                inner_r,
            );
            if !pe.is_empty() {
                let pw = g.measure_chrome_text(pe, FS, false);
                pen = g.draw_text_clipped(
                    pen,
                    by,
                    pe,
                    gpu::DrawOpts { font_size: FS, color: theme::text(), bold: false, italic: false },
                    inner_l,
                    inner_r,
                );
                g.rect(pen - pw, row_y + ROW - 6.0, pw, 1.5, theme::accent());
            }
            if focused && caret_on && pen <= inner_r {
                g.rect(pen + 1.0, row_y + 5.0, 1.5, ROW - 10.0, theme::accent());
            }
        };

        field(
            g,
            row1,
            FIELD_W,
            &f.query,
            "찾기",
            !f.focus_replace,
            preedit,
        );

        // n/m — 검색어가 있는데 0건이면 빨갛게. 빈 검색어는 아무 말도 안 한다.
        let count = if f.query.is_empty() {
            String::new()
        } else if f.hits.is_empty() {
            "결과 없음".to_string()
        } else {
            format!("{}/{}", f.idx + 1, f.hits.len())
        };
        if !count.is_empty() {
            let col = if f.hits.is_empty() { theme::danger() } else { theme::text_dim() };
            let cw = g.measure_chrome_text(&count, FS, false);
            g.draw_text(
                field_x + FIELD_W + COUNT_W - 8.0 - cw,
                text_baseline(row1),
                &count,
                gpu::DrawOpts { font_size: FS, color: col, bold: false, italic: false },
            );
        }

        let btn_x = field_x + FIELD_W + COUNT_W + 6.0;
        for (i, (icon, btn)) in [
            ("chevron-up", FindBtn::Prev),
            ("chevron-down", FindBtn::Next),
            ("x", FindBtn::Close),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = btn_x + BTN * i as f32;
            let dim = f.hits.is_empty() && btn != FindBtn::Close;
            let hov = hot((bx, row1, BTN, ROW));
            if hov {
                hover_rect(g, bx, row1, BTN, ROW, theme::radius_sm());
            }
            g.queue_icon(
                icon,
                bx + (BTN - 14.0) * 0.5,
                row1 + (ROW - 14.0) * 0.5,
                14.0,
                match (hov, dim) {
                    (true, _) => theme::text(),
                    (_, true) => theme::text_mute(),
                    _ => theme::text_dim(),
                },
            );
            hits.push((btn, (bx, row1, BTN, ROW)));
        }

        if f.replacing {
            let row2 = row1 + ROW + 4.0;
            field(g, row2, FIELD_W, &f.replace, "바꾸기", f.focus_replace, "");
            let mut lx = field_x + FIELD_W + 6.0;
            for (label, btn) in [("바꾸기", FindBtn::ReplaceOne), ("전부", FindBtn::ReplaceAll)] {
                let lw = g.measure_chrome_text(label, FS, false) + 14.0;
                let hov = hot((lx, row2, lw, ROW));
                if hov {
                    g.hover_pointer = true;
                }
                round_rect(g, lx, row2 + 2.0, lw, ROW - 4.0, theme::radius_sm(),
                    if hov { theme::surface_active() } else { theme::surface_hover() });
                g.draw_text(
                    lx + 7.0,
                    text_baseline(row2),
                    label,
                    gpu::DrawOpts {
                        font_size: FS,
                        color: if hov { theme::text() } else { theme::text_dim() },
                        bold: false,
                        italic: false,
                    },
                );
                hits.push((btn, (lx, row2, lw, ROW)));
                lx += lw + 5.0;
            }
        }
        hits
    }

    pub(crate) fn render_frame(&mut self) {
        self.probe_pane_labels();
        // commit_overlay's job ends the moment the echo lands and moves
        // the cursor. Retire it permanently then — otherwise erasing
        // back to the commit position re-satisfies `cursor == stored`
        // and the stale "안" reappears.
        if let Some(before) = self.commit_overlay.as_ref().map(|(_, b)| *b) {
            let cur = self.ws.lock().ok().and_then(|ws| {
                ws.active_pane.clone().and_then(|id| {
                    ws.panes
                        .get(&id)
                        .and_then(|p| p.term())
                        .map(|t| (t.cursor_row, t.cursor_col))
                })
            });
            if cur != Some(before) {
                self.commit_overlay = None;
            }
        }
        let t0 = Instant::now();
        let trace = std::env::var_os("KASATERM_PROFILE").is_some();
        let now = Instant::now();
        let blink_on = self.cursor_blink_on(now);
        // Damage gate: skip the GPU pass when nothing changed since
        // the last frame. winit keeps showing the previous swapchain
        // image, so the user sees the same picture without us
        // emitting 10k+ sugarloaf calls. PTY updates flag the per-
        // pane dirty bit; chrome events flag `self.chrome_dirty`;
        // cursor blink phase toggles count separately.
        let blink_changed = blink_on != self.last_blink_on;
        // 보이는 pane 으로 한정한다 — 안 보이는 방의 pane 이 dirty 여도 그릴 그림이
        // 없는데, 전에는 그 하나가 프레임을 통째로 불렀다. 방마다 claude 를 띄우면
        // 다른 방의 스트리밍이 지금 보는 방의 프레임을 계속 태운다(2026-08-13).
        // `visible_pane_ids` 가 ws 락을 잡으므로 아래 락보다 **먼저** 부른다.
        let visible_panes = self.visible_pane_ids();
        let pty_dirty = self
            .ws
            .lock()
            .unwrap()
            .panes
            .iter()
            .any(|(id, p)| p.dirty && visible_panes.contains(id));
        // The launch banner fade is its own animation source: while it's
        // still visible the picture changes every frame, so force the GPU
        // pass even when panes are clean (about_to_wait re-arms WaitUntil
        // to keep waking us through the fade).
        let version_animating = self.version_alpha() > 0.0;
        // Same for the copy toast + collab completion toast: their fade changes
        // the picture every frame.
        let toast_animating =
            self.copy_toast_alpha() > 0.0 || self.collab_toast_alpha() > 0.0;
        // A busy pane's header bar sweeps every frame, so it's an animation
        // source too — keep painting while any pane is working.
        //
        // 단 **보이는** pane 만 센다. 그 바는 pane 헤더에 그려지므로 다른 방의 pane 은
        // 아무리 바빠도 화면에 없다. 좁히지 않으면 claude 를 여러 방에 띄운 것만으로
        // 상시 애니메이션 모드가 되어, 유휴여도 30fps 로 9~11ms 프레임을 계속 갈았다.
        // (사이드바에 뜨는 다른 방의 상태 표시는 정적이고, 깜빡이는 것들은
        // `window_alert`·`status_needs_you` 가 따로 펌프를 건다 — 여기서 좁혀도 안 멈춘다.)
        let bar_animating = self.pane_activity.iter().any(|(id, a)| {
            a.status != "idle" && !a.status.is_empty() && visible_panes.contains(id)
        });
        // Split "needs a full chrome+grid rebuild" from "only the working-bar
        // sweep advances". A bar-only frame redraws cached chrome with a fresh
        // GPU time uniform — no clear_chrome, no per-pane grid clone, no draw-
        // list rebuild — so a busy pane no longer pins the CPU at 30fps.
        // A running git op spins a button spinner every frame.
        let git_op_animating = self.git.op.is_some();
        // 학생 도트 배너(Clawd 자리)가 보이는 동안은 idle 애니가 그림을
        // 바꾼다 — 전용 타이머(handler.rs)가 깨운 redraw 를 여기서
        // 통과시켜야 프레임이 넘어간다.
        let banner_animating =
            STUDENT_SPRITE_ANIMATING.load(std::sync::atomic::Ordering::Relaxed);
        // ultracode 혜성은 셀 그리드(`composed`) 위에 얹혀 66ms 마다 위상이 바뀐다.
        // 그런데 이 게이트에 그 사유가 없어서, claude 가 idle 이면 통과하는 게 커서
        // blink(530ms) 뿐이었다 — 혜성이 프레임당 2.8셀이 아니라 **22셀씩** 튀어
        // 「흐르는 빛」이 아니라 순간이동으로 보였고, `KASATERM_NOBLINK=1` 이면 아예
        // 멈췄다. 66ms 타이머(handler.rs)가 깨운 redraw 는 `chrome_dirty` 를 세우지
        // 않으므로 여기서 직접 통과시켜야 한다.
        //
        // ⚠️`ULTRA_COMET_ANIMATING` 원자값을 쓰면 안 된다 — 그건 「어느 방에든
        // ultracode pane 이 하나라도 있으면 true」라, 위 `pty_dirty`·`bar_animating`
        // 을 보이는 pane 으로 좁힌 것을 통째로 되돌린다(다른 방의 ultracode 하나가
        // 지금 보는 방을 상시 15fps 로 태운다). 이미 잡아 둔 `visible_panes` 로
        // 좁힌다 — `pane_ultracode` 의 키는 `pane_claude_sid` 의 키(pane/pty id)라
        // `visible_pane_ids()` 와 같은 네임스페이스다(보조 탭도 접혀 들어온다).
        let comet_animating = self
            .pane_ultracode
            .iter()
            .any(|id| visible_panes.contains(id));
        let rebuild = pty_dirty
            || self.chrome_dirty
            || blink_changed
            || version_animating
            || toast_animating
            || git_op_animating
            || banner_animating
            // 혜성은 그리드에 얹히므로 `bar_animating` 처럼 bar-only 경로로 두면 안 된다
            // — 전체 프레임을 다시 그려야 위상이 반영된다.
            || comet_animating;
        if !rebuild && !bar_animating {
            return;
        }
        self.last_blink_on = blink_on;
        if self.window.is_none() { return; }
        let scale = self.effective_scale();
        // Self-heal: if the GPU renderer's internal scale drifted from the
        // window's effective scale, every logical→physical mapping is off by
        // that ratio and the whole frame (chrome included) compresses into a
        // corner. This happens whenever a DPI change reaches the renderer
        // without a matching set_scale (a ScaleFactorChanged we didn't fully
        // apply, sleep/wake, clamshell). Re-sync once before drawing so a bad
        // frame fixes itself on the very next paint instead of staying broken.
        let drifted = self
            .gpu
            .as_ref()
            .map_or(false, |g| (g.scale() - scale).abs() > 0.001);
        if drifted {
            self.apply_effective_scale();
        }
        // 같은 이유로 **크기**도 자가치유한다. 지금까지 scale 만 되잡았는데,
        // 어긋날 수 있는 건 둘이고 크기 쪽은 한 번 틀어지면 되돌릴 장치가
        // 아예 없었다(실측: 스왑체인만 절반으로 만들어 두면 6초 뒤에도 그대로).
        //
        // 순서가 중요하다 — 뷰부터 창에 되맞춘다. 뷰가 작아지면 레이어도
        // `inner_size()` 도 스왑체인도 같이 작아져 앱 내부에선 완벽히 일관돼
        // 보이고, 어긋난 건 창과 뷰 사이뿐이라 크기 대조로는 안 잡힌다.
        // 정상 상태에선 둘 다 msg_send 몇 번·정수 비교 두 번이라 사실상 공짜다.
        // 어긋날 수 있는 자리는 둘이 아니라 셋이었다. 뷰도 스왑체인도 창과
        // 맞는데 **레이어의 backing scale 만** 옛 화면에 머무는 상태가 있고,
        // 그때 두 불변식은 나란히 "이상 없음" 이라 답한다 — 39번 수정이
        // 모니터 이동을 못 잡은 이유가 이 침묵이었다.
        if let Some(w) = self.window.clone() {
            let refit = gpu::ensure_view_fills_window(&w);
            let rescaled = gpu::ensure_layer_scale_matches(&w);
            let want = w.inner_size();
            let stale = self
                .gpu
                .as_ref()
                .map_or(false, |g| g.surface_size() != (want.width, want.height));
            // cs 를 고쳤으면 drawable 을 다시 잡아야 짝이 맞는다(`resize` 는
            // 같은 크기로 불러도 `surface.configure` 를 다시 태운다).
            if refit || rescaled || stale {
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(want.width, want.height);
                }
                self.apply_effective_scale();
            }
        }
        // gpu path takes over the whole frame — no chrome yet, just
        // the cell grid through the cell-renderer pipeline.
        if self.gpu.is_some() {
            let time_secs = self.version_anim_start.elapsed().as_secs_f32();
            // (echo-stale 격리) bar-only 경로 임시 제거 — busy여도 항상 전체
            // render_frame_gpu로 cells를 다시 그려 echo가 stale되지 않게.
            let _ = rebuild;
            self.render_frame_gpu(scale, time_secs);
            // 리드백은 render_frame_gpu 안에서 device.poll(Wait) 로 끝나므로 여기선
            // 파일이 이미 디스크에 있다. 회신을 여기 두는 이유가 그것 — 무장 시점에
            // 답하면 받는 쪽이 아직 없는 파일을 Read 한다.
            self.settle_pane_captures();
            if trace {
                eprintln!(
                    "[render-gpu] {}us since_input={}ms",
                    t0.elapsed().as_micros(),
                    now.saturating_duration_since(self.last_input_at).as_millis()
                );
            }
            return;
        }
    }
}


/// compact 진행 게이지 — 칸이 차오르는 눈금(2026-08-15 지시: 연속 띠는 얼마나
/// 남았는지 눈금이 없어 안 읽혔다). 찬 칸은 accent 원색, 빈 칸은 흐린 트랙.
/// 헤더 하단과 머리 없는 pane 상단이 같은 형태 언어를 쓰도록 한 손으로 그린다.
/// 반환: 게이지가 실제로 차지한 폭(숫자를 그 오른쪽에 얹을 때 쓴다).
fn draw_compact_cells(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    bar_h: f32,
    accent: [u8; 4],
    pct: u8,
) -> f32 {
    let seg_w = 7.0_f32;
    let gap = 2.0_f32;
    let n = (((w + gap) / (seg_w + gap)).floor() as usize).max(1);
    let filled = ((pct.min(100) as f32 / 100.0) * n as f32).round() as usize;
    for i in 0..n {
        let sx = x + i as f32 * (seg_w + gap);
        let col = if i < filled {
            accent
        } else {
            theme::with_alpha(accent, 0x2e)
        };
        g.rect(sx, y, seg_w, bar_h, col);
    }
    n as f32 * (seg_w + gap) - gap
}

/// 하단바에 적을 계정 이름. 라벨을 안 지은 슬롯은 이름이 이메일로 폴백되는데
/// 통째로 적으면 한 줄의 절반을 주소가 먹는다 — 그래서 `@` 앞만 남긴다.
///
/// **겹치면 안 줄인다.** 슬롯 둘이 같은 아이디에 다른 도메인이면
/// (`goenho0613@naver.com` · `goenho0613@gmail.com`) 화면에서 통째로 같은 글자가 되어,
/// 지금 어느 계정인지 이 자리로는 알 수가 없다(토키 실측 2026-08-15). 그때만
/// 도메인 앞머리를 붙여 가른다(`goenho0613·gmail`) — 짧은 채로 갈리는 것이 요점이라
/// 도메인 전체는 안 쓴다.
///
/// `others` 는 자기 자신을 포함해도 된다(같은 문자열은 겹침으로 안 센다).
pub(crate) fn statusbar_account_short(name: &str, others: &[String]) -> String {
    fn local(s: &str) -> &str {
        s.split_once('@').map(|(a, _)| a).unwrap_or(s)
    }
    let dup = others.iter().any(|o| o != name && local(o) == local(name));
    match name.split_once('@') {
        Some((a, d)) if dup => format!("{a}·{}", d.split('.').next().unwrap_or(d)),
        Some((a, _)) => a.to_string(),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statusbar_account_name_keeps_the_domain_only_when_slots_collide() {
        let alone = vec!["goenho0613@gmail.com".to_string(), "2rami@sionic.ai".to_string()];
        assert_eq!(statusbar_account_short("goenho0613@gmail.com", &alone), "goenho0613");

        let clash = vec![
            "goenho0613@naver.com".to_string(),
            "goenho0613@gmail.com".to_string(),
        ];
        assert_eq!(statusbar_account_short("goenho0613@gmail.com", &clash), "goenho0613·gmail");
        assert_eq!(statusbar_account_short("goenho0613@naver.com", &clash), "goenho0613·naver");

        // 사람이 지은 라벨엔 `@` 가 없다 — 손대지 않는다.
        assert_eq!(statusbar_account_short("사이오닉팀플랜", &clash), "사이오닉팀플랜");
        // 라벨끼리 같아 보이는 경우도 붙일 도메인이 없으니 그대로 둔다.
        let same = vec!["기본".to_string(), "기본".to_string()];
        assert_eq!(statusbar_account_short("기본", &same), "기본");
    }
}

/// 사용량 임계 색. 60/80 이 경계고 그 아래는 초록이 아니라 **중립**이다 — 초록은
/// 「좋다」는 신호라 늘 켜져 있으면 아무 말도 안 하는 색이 된다.
pub(crate) fn usage_pct_color(pct: f32) -> [u8; 4] {
    if pct >= 80.0 {
        theme::danger()
    } else if pct >= 60.0 {
        theme::syn_number()
    } else {
        theme::text()
    }
}

/// 막대는 여유 구간에서 더 흐리다 — 숫자와 달리 늘 보이는 것이라, 안 급할 때까지
/// 또렷하면 목록 전체가 얼룩덜룩해져 급한 줄이 안 튄다.
pub(crate) fn usage_bar_color(pct: f32) -> [u8; 4] {
    if pct >= 80.0 {
        theme::danger()
    } else if pct >= 60.0 {
        theme::syn_number()
    } else {
        theme::with_alpha(theme::text_dim(), 0x66)
    }
}

/// `[창 이름][막대][퍼센트]` 를 창마다 하나씩 왼쪽부터. 5시간이 앞이다 — 지금 당장
/// 막히는 건 그쪽이고 주간은 「이번 주가 어떻게 흘러가나」라 참고에 가깝다.
///
/// `right` 를 넘칠 것 같으면 **그 창을 아예 안 그린다.** 잘린 막대는 값을 잘못
/// 읽히게 하므로 없느니만 못하다. 그린 만큼의 오른쪽 끝을 돌려준다.
///
/// 막대는 트랙을 함께 그린다 — 채움만 있으면 15% 짜리가 어디까지 갈 수 있는
/// 것인지 알 수가 없어 그냥 얼룩이 된다.
pub(crate) fn draw_usage_windows(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    right: f32,
    font: f32,
    b: &crate::UsageBadge,
) -> f32 {
    const GW: f32 = 28.0;
    const GH: f32 = 5.0;
    let gy = y + (font - GH) / 2.0;
    // `windows` 가 비는 건 옛 스냅샷을 되살렸을 때다 — 그때는 가장 급한 창 하나로.
    let wins: Vec<(String, f32)> =
        if b.windows.is_empty() { vec![(b.label.clone(), b.pct)] } else { b.windows.clone() };
    let mut bx = x;
    for (label, pct) in &wins {
        let (label, pct) = (label.as_str(), *pct);
        let need = g.measure_chrome_text(label, font, false)
            + 6.0
            + GW
            + 6.0
            + g.measure_chrome_text("100%", font, true);
        if bx + need > right {
            break;
        }
        g.draw_text(
            bx,
            y,
            label,
            gpu::DrawOpts { font_size: font, color: theme::text_mute(), bold: false, italic: false },
        );
        let gx = bx + g.measure_chrome_text(label, font, false) + 6.0;
        g.rect(gx, gy, GW, GH, theme::with_alpha(theme::text_dim(), 0x33));
        let w = (GW * (pct / 100.0).clamp(0.0, 1.0)).max(1.5);
        g.rect(gx, gy, w, GH, usage_bar_color(pct));
        // stale 은 `~` 로만 말한다 — 색까지 흐리면 「급하지 않다」로 읽힌다.
        let pt = if b.stale { format!("~{pct:.0}%") } else { format!("{pct:.0}%") };
        g.draw_text(
            gx + GW + 6.0,
            y,
            &pt,
            gpu::DrawOpts { font_size: font, color: usage_pct_color(pct), bold: true, italic: false },
        );
        bx = gx + GW + 6.0 + g.measure_chrome_text(&pt, font, true) + 12.0;
    }
    bx
}
