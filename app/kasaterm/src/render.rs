//! GPU 렌더 패스 — App 렌더 메서드(cell-renderer 파이프라인 + chrome 오버레이).
//! main.rs 의 impl App 에서 분리. struct App·자유함수·타입은 crate root 그대로 참조.
use super::*;

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
    /// 못 본 완료 — 이 줄이 느리게 숨쉰다.
    alert: bool,
    /// 승인·입력을 기다리는 중 — 이 줄이 핑크로 깜빡인다.
    waiting: bool,
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
                    let (cur_row, cur_col, cur_vis, cols) = match pane.term() {
                        Some(t) => (
                            t.cursor_row,
                            t.cursor_col,
                            t.cursor_visible,
                            t.cells.first().map(|r| r.len()).unwrap_or(80) as u16,
                        ),
                        None => (0, 0, false, 80),
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
            preedit_row,
            preedit_col,
            preedit,
            pane_x,
            pane_y,
            header_shift,
        ) = snap.unwrap_or((0, 0, false, 80, 0, 0, preedit_text.clone(), 0, 0, 0.0));
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
            g.rect(cx, cy, cw, ch, c);
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

    fn render_frame_gpu(&mut self, scale: f32, time_secs: f32) {
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
        let mut sticky_pill_slots: Vec<(f32, f32, f32, f32, String, String)> = Vec::new();
        // working 스피너(✻/braille) 자리 학생 도트(제자리 걸음): 같은 형태.
        let mut spinner_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // 승인 대기(approval prompt) 학생 도트(폴짝 바운스): 같은 형태.
        let mut waiting_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // statusline 자리표시자(U+FFFC) → 학생 프사(bust, 정적 1프레임).
        let mut profile_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // statusline 프사의 hover 확대·클릭용 (학생이름, slug, rect). profile_slots
        // 와 달리 학생 이름을 들고 있어(hover 큰 bust·클릭→학생설정 딥링크) — /resume
        // 피커 프사는 이름을 모르므로 여기 안 담고 statusline 프사만.
        let mut profile_face_hits: Vec<(String, &'static str, (f32, f32, f32, f32))> = Vec::new();
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
                let runs_claude = self
                    .pty
                    .get(tab_pid.as_str())
                    .and_then(|p| p.active_agent())
                    .is_some();
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
                // Claude Code 스크롤 sticky prompt → 웹뷰풍 pill. mouse-tracking
                // 중이라 뷰포트 스크롤 여부를 직접 못 안다 — "Jump to bottom" 힌트로
                // 게이트한다(find_sticky_prompt). 감지 행 셀은 스냅샷에서 blank 처리해
                // 원본 흐릿한 텍스트를 지우고, 그 자리에 pill 을 얹는다. 클릭 rect 는
                // 아래 chrome 패스에서 STICKY_PILLS 로 mouse handler 에 넘긴다.
                if let Some(sticky) = find_sticky_prompt(&composed) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let ncols = composed.get(sticky.row).map_or(0, |r| r.len());
                    let end = sticky.col_end.min(ncols);
                    // 흰 배경 pill 을 pane 양끝(col 0..ncols)까지 채운다(거노: "흰색
                    // 바탕 pane 양끝으로 다 채워"). 클릭 rect 도 행 전체 폭 — 흰 바탕
                    // 어디를 눌러도 seek(begin_sticky_seek)가 걸린다.
                    let px = body_left;
                    let py = body_top + sticky.row as f32 * sch;
                    sticky_pill_slots.push((
                        px,
                        py,
                        ncols as f32 * scw,
                        sch,
                        sticky.text.clone(),
                        id.clone(),
                    ));
                    if let Some(row) = composed.get_mut(sticky.row) {
                        // 원본 셀(등폭 그리드)을 지우지 않고 그 자리에서 선명화만
                        // 한다 — 흐릿(dim) 제거 + 흰 배경·검정 글자. draw_text
                        // (proportional)로 다시 그리던 옛 방식은 한글 wide glyph 를
                        // ink 폭으로 tighten 해 자간이 어긋나고 배경 폭도 텍스트와
                        // 안 맞았다(거노: "딱 안 맞아 자간 이상"). 그리드 셀은 등폭
                        // 이라 폭·자간이 원본과 정확히 일치한다. 텍스트 밖 셀은 흰
                        // 배경만 깔고 글자 잔재를 지워 pill 을 pane 양끝까지 연장한다.
                        for (i, cell) in row.iter_mut().enumerate() {
                            cell.dim = false;
                            cell.inverse = false;
                            cell.fg = kasa_bridge::screen::Color::Rgb(20, 22, 28);
                            cell.bg = kasa_bridge::screen::Color::Rgb(248, 249, 251);
                            if i < sticky.col_start || i >= end {
                                cell.ch = ' ';
                            }
                        }
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
                    for (br, bc) in find_clawd_banners(&composed) {
                        // br 은 스크롤로 위가 잘리면 음수, 아래가 잘리면 박스가
                        // 그리드 밖까지 이어진다 — 스프라이트는 pane 세로 범위로
                        // 클립해 셀 스크롤과 함께 자연스럽게 잘려 나가게 한다.
                        banner_slots.push((
                            slug,
                            (
                                body_left + bc as f32 * scw,
                                body_top + br as f32 * sch,
                                CLAWD_COLS as f32 * scw,
                                CLAWD_ROWS as f32 * sch,
                            ),
                            (body_top, body_top + composed.len() as f32 * sch),
                        ));
                        let r0 = br.max(0) as usize;
                        let r1 = (br + CLAWD_ROWS as isize)
                            .clamp(0, composed.len() as isize)
                            as usize;
                        for row in composed[r0..r1].iter_mut() {
                            for cell in row.iter_mut().skip(bc).take(CLAWD_COLS) {
                                *cell = GridCell::blank();
                            }
                        }
                        // 배너 타이틀 "Claude Code" 도 학생 이름으로 — 도트만
                        // 바뀌면 학생이 남의 이름표를 달고 서 있는 꼴(거노).
                        replace_banner_title(&mut composed, br, bc, name, accent);
                        // 웰컴 배너("Welcome back <user>!")면 도트 위 인사말 행을
                        // 배정 학생 페르소나 인사말로 — launcher 화면에선 no-op.
                        replace_welcome_greeting(&mut composed, br, name, accent);
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
                    if let Some((sr, sc)) = find_claude_spinner(&composed) {
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
                        }
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
                    } else if !crate::input::rows_show_working(&composed)
                        && crate::input::rows_show_approval_prompt(&composed).is_some()
                    {
                        if let Some((ar, ac)) = approval_anchor(&composed) {
                            pet_busy = true;
                            const DOT: f32 = 40.0;
                            let x = (body_left + (ac + 2) as f32 * scw)
                                .min(body_left + cols_now as f32 * scw - DOT);
                            let y = (body_top + (ar + 1) as f32 * sch - DOT).max(body_top);
                            waiting_slots.push((slug, (x, y, DOT, DOT)));
                        }
                    }
                    // statusline 학생 프사: statusline.py 가 kasaterm 안에서
                    // 학생 이름 대신 U+FFFC 자리표시자를 내보낸다. 그 셀을
                    // 비우고 그 자리에 프사(bust 96×96)를 statusline 행 바닥
                    // 정렬·STATUSLINE_FACE_ROWS 행 키로 얹는다 — 1행짜리는
                    // 너무 작았다(거노). icon 패스라 아래 테두리 줄 위에
                    // 스티커처럼 얹힌다.
                    // 스캔 방향과 앵커 규칙은 `find_statusline_face` 주석 참고 —
                    // 별도창(auxwin)이 같은 자리를 찍어야 해서 자유함수로 나가 있다.
                    // 입력창 위 standing 앵커. claude 는 statusline 자리표시자에서
                    // 출발하지만 codex 는 그게 없어(위 `find_filled_standing_anchor`
                    // 주석) 입력행에서 바로 잡는다. 둘 다 못 잡으면 안 세운다.
                    let mut stand_anchor: Option<(usize, f32)> = None;
                    if let Some((sr, sc, len)) = find_statusline_face(&composed) {
                        for cell in composed[sr].iter_mut().skip(sc).take(len) {
                            *cell = GridCell::blank();
                        }
                        let face_h = STATUSLINE_FACE_ROWS as f32 * sch;
                        let face_rect = (
                            body_left + sc as f32 * scw,
                            (body_top + (sr + 1) as f32 * sch - face_h).max(body_top),
                            len as f32 * scw,
                            face_h,
                        );
                        profile_slots.push((slug, face_rect));
                        profile_face_hits.push((name.to_string(), slug, face_rect));
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
                    {
                        if !pet_busy {
                            if let Some((anchor, left_c)) = stand_anchor {
                                let h = INPUT_STANDING_ROWS as f32 * sch;
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
                if let Some(accent) = prompt_accent {
                    style_prompt_box(&mut composed, accent);
                    // 칩 제거는 위 `runs_claude` 블록에서 이미 끝났다 — 여기서 한 번
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
                            - self.dock_reserve_h()
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
                // footer (`fbox_h < PANE_FOOTER_HEIGHT` → skipped). Treat a 0 span
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
                            (raw_lh - bottom_edge).max(0.0)
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
                        busy: self
                            .pane_activity
                            .get(&id)
                            .map(|a| a.status != "idle" && !a.status.is_empty())
                            .unwrap_or(false),
                        // A background shell / Monitor is running with no spinner —
                        // drives the slower header pulse bar when not busy.
                        bg_active: self
                            .pane_activity
                            .get(&id)
                            .map(|a| a.bg_active)
                            .unwrap_or(false),
                        color: pane.color,
                        is_markdown: pane.markdown().map_or(false, |m| m.is_md_doc),
                        md_raw_mode: pane.markdown().map_or(false, |m| m.raw_mode),
                        is_image: pane.image().is_some(),
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
        // Cache the × close-button hit rects (logical) for the mouse
        // handler, even before the GPU borrow below.
        let chrome_font = 14.0_f32;
        let close_size = chrome_font + 4.0;
        // × close sits inside the left tab, after [icon + title]. Approximate
        // the proportional label width (wide glyphs ~1em, ascii ~0.55em) so
        // the hit rect tracks the drawn glyph.
        self.pane_header_rects = headers
            .iter()
            .map(|h| {
                let label_w: f32 = h
                    .label
                    .chars()
                    .map(|c| {
                        if (c as u32) > 0x2000 {
                            chrome_font
                        } else {
                            chrome_font * 0.55
                        }
                    })
                    .sum();
                let close_x = h.x + 8.0 + (chrome_font + 6.0) + 6.0 + label_w + 8.0;
                let close = (
                    close_x,
                    h.y + (PANE_HEADER_HEIGHT - close_size) / 2.0,
                    close_size,
                    close_size,
                );
                (h.id.clone(), close)
            })
            .collect();
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
        let (sb_tabs, sb_closes, sb_plus, sb_rows) = self.sidebar_layout(sb_win_h);
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
        self.sidebar_row_rects = if sidebar_shown { sb_rows.clone() } else { Vec::new() };
        self.window_tab_close_rects = if sidebar_shown { sb_closes.clone() } else { Vec::new() };
        self.new_window_btn_rect = Some(sb_plus);
        // Shell picker popup layout, computed here (no GPU borrow) so the
        // click hit-list and the painted boxes share one source of truth.
        // Items stack directly under the "+" button.
        let menu_open = self.shell_menu_open;
        let shell_items: Vec<(&'static str, &'static str, String)> =
            if menu_open { available_shells() } else { Vec::new() };
        const SHELL_ITEM_H: f32 = 34.0;
        let menu_w_for_paint = sb_plus.2.max(210.0);
        let shell_menu_layout: Vec<(String, &'static str, &'static str, (f32, f32, f32, f32))> = {
            let (px, py, _, ph) = sb_plus;
            let mut iy = py + ph + 4.0;
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
        // Per-window "working" flag for the sidebar dot: true when any pane in
        // that window is mid-task (cross-window collab, from pane_activity). The
        // active window's tree lives in pty_layout (its slot is None); the rest
        // carry their own layout. Built here (no GPU borrow) so the paint loop
        // just indexes sb_busy[i].
        let sb_busy: Vec<bool> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i).iter().any(|id| {
                    self.pane_activity
                        .get(id)
                        .is_some_and(|a| a.status != "idle" && !a.status.is_empty())
                })
            })
            .collect();
        // Per-window "just finished" flag: any leaf with a live completion
        // flash. Lights a SUCCESS dot on the window's sidebar tab so a finish
        // in a window you aren't viewing is visible across the strip.
        let sb_done: Vec<bool> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i).iter().any(|id| self.notify_flash_factor(id).is_some())
            })
            .collect();
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
        // 펼친 방의 pane 한 줄씩 — 이름·색을 여기서 뽑아 둔다. `pane_character_if_known`
        // 이 `ws` 를 잠그므로 GPU 를 빌린 페인트 루프 안에서 부르면 그 자리에서 멈춘다.
        // 줄에 적는 건 **그 pane 이 무엇을 하고 있나**(claude · zsh · 편집기…)다.
        // 학생 이름은 얼굴이 이미 말하고 있어, 글자로 한 번 더 쓰면 같은 말이 두 번
        // 나오고 정작 pane 을 가르는 정보가 자리를 잃는다(거노: "학생이름은 빼고").
        let sb_row_info: Vec<SidebarRowInfo> = sb_rows
            .iter()
            .map(|(_, id, _)| {
                // 얼굴은 claude 가 붙은 pane 에만 — 셸만 도는 자리에 학생이 먼저 앉아
                // 있으면 목록이 "이미 일하는 중"이라고 거짓말한다.
                let who = self
                    .pane_claude_ready(id)
                    .then(|| self.pane_character_if_known(id))
                    .flatten()
                    .unwrap_or_default();
                let is_cur =
                    self.ws.lock().unwrap().active_pane.as_deref() == Some(id.as_str());
                // 안에서 도는 프로그램이 보낸 OSC 0/2 제목이 첫 번째 진실이다
                // (claude 는 뜨자마자 「✳ Claude Code」를 보낸다). `ws.panes` 의
                // 탭 제목은 사용자 rename 전용이라 여기선 늘 비어, 줄이 영영
                // `zsh` 로 남았다. 프로세스 이름은 그다음 폴백.
                let label = self
                    .pty
                    .get(id)
                    .and_then(|p| p.osc_title())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| Self::resolve_pane_label(&self.pty, id, None));
                let waiting = self
                    .pane_activity
                    .get(id)
                    .is_some_and(|a| a.status == "waiting");
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
                }
            })
            .collect();
        // 펼친 방에서 그 방의 알림·대기를 **줄이 이미 말하고 있는가**. 말하고 있으면
        // 카드 머리는 조용히 둔다 — 같은 뜻을 두 겹으로 칠하면 결국 방 전체가 빛나
        // 고치기 전으로 돌아간다. 접힌 방은 줄이 없으니 여기에 안 들고, 머리가 계속
        // 말한다(그때는 그게 유일한 자리다).
        let mut sb_row_alert_win: std::collections::HashSet<usize> = Default::default();
        let mut sb_row_wait_win: std::collections::HashSet<usize> = Default::default();
        for ((wi, _, _), info) in sb_rows.iter().zip(sb_row_info.iter()) {
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
                    self.pane_activity.get(id).is_some_and(|a| a.status == "waiting")
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
        let dock_reserve = self.dock_reserve_h();
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
                faces: Vec::new(),
            };
            paint_student_overlays(g, &student_slots, anim_ms);
            // Claude Code 스크롤 sticky prompt: 텍스트·흰 배경은 위 스캔에서 원본
            // 셀을 선명화(등폭 유지)해 이미 그려졌다. 여기선 클릭 rect(셀 영역)만
            // STICKY_PILLS 로 mouse handler·seek 에 넘긴다 — 클릭 = "그 프롬프트가
            // 화면에 들어올 때까지 위로 스크롤"(begin_sticky_seek).
            STICKY_PILLS.with(|s| s.borrow_mut().clear());
            for (px, py, pw, ph, text, pane_id) in &sticky_pill_slots {
                STICKY_PILLS.with(|s| {
                    s.borrow_mut().push((pane_id.clone(), (*px, *py, *pw, *ph), text.clone()))
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
            // 프사 hover 확대 — 커서가 statusline 프사 위면 큰 bust 를 그 위쪽에
            // 팝업(창 경계 클램프). statusline 은 창 하단이라 위로 띄운다. 매
            // 프레임 hover 재판정이라 커서가 벗어나면 저절로 사라진다(애니 없음).
            let (fmx, fmy) = self.cursor_px;
            if let Some((cname, slug, r)) = profile_face_hits
                .iter()
                .find(|(_, _, r)| fmx >= r.0 && fmx <= r.0 + r.2 && fmy >= r.1 && fmy <= r.1 + r.3)
            {
                paint_face_popup(g, cname, slug, *r, self.cell.h, win_px.0 / scale, TITLE_HEIGHT);
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
                round_rect(
                    g,
                    px - 4.0,
                    py + ph,
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
                    // 세로 사이드바와 같은 규칙 — 알림은 느린 숨, 대기는 danger 띠.
                    if sb_alert.get(*i).copied().unwrap_or(false) {
                        let mut c = theme::accent();
                        c[3] = (16.0 + 44.0 * breathe(anim_phase_secs(), 2.4)) as u8;
                        round_rect(g, *tx, *ty, *tw, *th, theme::radius_sm(), c);
                    }
                    if sb_wait.get(*i).copied().unwrap_or(false) {
                        let mut c = theme::attention();
                        c[3] = (140.0 + 115.0 * breathe(anim_phase_secs(), 1.1)) as u8;
                        g.rect(*tx, *ty + 3.0, 3.0, *th - 6.0, c);
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
                    // Working / done dots on the glyph's corners (same meaning
                    // as the side strip's chip dots).
                    if sb_busy.get(*i).copied().unwrap_or(false) {
                        let c = if sb_wait.get(*i).copied().unwrap_or(false) {
                            theme::attention()
                        } else {
                            theme::accent()
                        };
                        circle_rect(g, icon_x + isz - 3.0, icon_y - 3.0, 6.0, c);
                    }
                    if sb_done.get(*i).copied().unwrap_or(false) {
                        circle_rect(g, icon_x + isz - 3.0, icon_y + isz - 3.0, 6.0, theme::success());
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
                        panel_rect(g, *tx, *ty, *tw, *th, theme::radius_md(), theme::surface_active());
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
                    // 못 본 알림 — 카드 전체가 accent 로 **느리게 숨쉰다**. 예전엔
                    // 커서 블링크에 맞춰 켜졌다 꺼졌는데, 온/오프 토글은 시야
                    // 가장자리에서도 눈이 끌려가 작업을 방해했다(거노: "깜빡거리는
                    // 거 말고"). 밝기가 이어지면 있다는 건 알아도 잡아채지는 않는다.
                    // 칠하는 높이는 카드 **머리**까지다. 방을 펴면 카드가 pane 줄만큼
                    // 길어지는데 예전엔 그 길이를 다 칠해, 한 세션이 끝났을 뿐인데
                    // 방 전체가 빛났다(거노). 접힌 방은 머리가 곧 카드라 그대로다.
                    let head_h = th.min(SIDEBAR_TAB_H);
                    if sb_alert.get(*i).copied().unwrap_or(false)
                        && !sb_row_alert_win.contains(i)
                    {
                        let mut c = theme::accent();
                        c[3] = (16.0 + 44.0 * breathe(anim_phase_secs(), 2.4)) as u8;
                        round_rect(g, *tx, *ty, *tw, head_h, theme::radius_md(), c);
                    }
                    // 손을 기다리는 방 — 알림과 **색·자리·속도가 전부 갈린다**.
                    // 끝나서 알리는 것과 물어보고 멈춘 것은 급한 정도가 다른데,
                    // 예전엔 둘 다 같은 파란 깜빡임이라 구별이 안 됐다. 이쪽은
                    // 왼쪽 모서리에 attention(핑크) 띠로, 두 배 빠르게 숨쉰다.
                    if sb_wait.get(*i).copied().unwrap_or(false)
                        && !sb_row_wait_win.contains(i)
                    {
                        let mut c = theme::attention();
                        c[3] = (140.0 + 115.0 * breathe(anim_phase_secs(), 1.1)) as u8;
                        g.rect(*tx, *ty + 4.0, 3.0, head_h - 8.0, c);
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
                    // Working dot: this window has a pane mid-task (cross-window
                    // collab). Top-right of the icon chip, opposite the number
                    // badge (top-left) so the two never overlap. Static accent
                    // dot — the flowing bar lives on the in-window pane header.
                    if sb_busy.get(*i).copied().unwrap_or(false) {
                        let dsz = 9.0_f32;
                        let dx = icon_x + icon - dsz + 3.0;
                        let dy = icon_y - 3.0;
                        let c = if sb_wait.get(*i).copied().unwrap_or(false) {
                            theme::attention()
                        } else {
                            theme::accent()
                        };
                        circle_rect(g, dx, dy, dsz, c);
                    }
                    // Completion dot: a pane in this window just finished
                    // (notify_flash). SUCCESS green at the bottom-right corner so
                    // it never overlaps the working dot (top-right).
                    if sb_done.get(*i).copied().unwrap_or(false) {
                        let dsz = 9.0_f32;
                        let dx = icon_x + icon - dsz + 3.0;
                        let dy = icon_y + icon - dsz + 3.0;
                        circle_rect(g, dx, dy, dsz, theme::success());
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
                    // 못 본 완료 — **이 줄만** 느리게 숨쉰다. 예전엔 같은 칠이 방
                    // 카드 전체에 걸려, 세 pane 중 하나가 끝났는데 방이 통째로
                    // 빛났다(거노: "숨쉬기효과 윈도우전체가 아니라 완료된세션하나만").
                    // 속도·색은 그때 것을 그대로 옮겼다 — 바뀐 건 범위뿐이다.
                    if info.alert {
                        let mut c = theme::accent();
                        // 카드에 걸던 16..60 을 그대로 옮기면 22px 줄에선 안 보인다 —
                        // 같은 알파라도 칠하는 넓이가 1/3 이라 눈에 안 걸린다.
                        c[3] = (24.0 + 76.0 * breathe(anim_phase_secs(), 2.4)) as u8;
                        round_rect(g, rx, ry, rw, rh, theme::radius_sm(), c);
                    }
                    // 손을 기다리는 줄 — 숨쉬기와 **역할이 다르다**. 숨쉬기는 "돌고
                    // 있다/끝났다"는 알림이고 이건 "내가 엔터를 쳐야 풀린다"는 호출이라,
                    // 눈에 걸려야 맞다(거노: 핑크로 깜빡). 그래서 색은 attention,
                    // 리듬은 두 배 빠르고 진폭도 크다.
                    if info.waiting {
                        let mut c = theme::attention();
                        c[3] = (40.0 + 90.0 * breathe(anim_phase_secs(), 1.1)) as u8;
                        round_rect(g, rx, ry, rw, rh, theme::radius_sm(), c);
                        let mut edge = theme::attention();
                        edge[3] = (140.0 + 115.0 * breathe(anim_phase_secs(), 1.1)) as u8;
                        g.rect(rx, ry + 2.0, 2.0, rh - 4.0, edge);
                    }
                    // 지금 보고 있는 pane 은 왼쪽 띠로 — 목록이 방을 넘나들어서
                    // 표시가 없으면 "내가 있는 곳"을 매번 번호로 대조하게 된다.
                    if is_cur {
                        g.rect(rx, ry + 3.0, 2.0, rh - 6.0, theme::accent());
                    }
                    let face = rh - 6.0;
                    let has_face = draw_student_face_anim(
                        g, who, rx + 7.0, ry + 3.0, face, anim_phase_secs(),
                    );
                    if !has_face {
                        circle_rect(g, rx + 9.0, ry + rh / 2.0 - 3.0, 6.0, *col);
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
                            color: if is_cur { theme::text() } else { theme::text_dim() },
                            bold: false,
                            italic: false,
                        },
                    );
                    // 카드 머리의 요약 점과 오른쪽 끝을 맞춘다 — 세로로 어긋나면
                    // 같은 뜻의 점 둘이 다른 격자에 앉아 목록이 삐뚤어 보인다.
                    circle_rect(g, rx + rw - 6.0, ry + rh / 2.0 - 3.0, 6.0, *col);
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
                // ── 빠른 파일 고정 섹션 ── 여기선 높이만 잡아 start_y 를 확정하고,
                // 실제 그리기는 트리 본문 뒤로 미룬다. 이 렌더러는 scissor 가 없어
                // 스크롤로 start_y 위까지 올라온 트리 항목을 나중에 불투명 배경으로
                // 덮어야 겹침이 안 난다(개인 CLAUDE.md 행에 폴더가 파고들던 문제).
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
                let dock_h = if self.docked.is_empty() && self.zoomed_pane.is_none() {
                    0.0
                } else {
                    DOCK_HEIGHT
                };
                let body_visible_h = (sb_win_h - dock_h - start_y).max(0.0);
                self.file_tree.body_rect = (row_x, start_y, row_w, body_visible_h);
                let win_h = win_px.1 / scale;
                let step = 14.0_f32; // per-depth indent width
                let mut rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))> = Vec::new();
                // `file_tree_nodes` already holds the right set: a query swaps it
                // for whole-tree search hits (file_tree_search_collect), empty
                // restores the expanded tree. So just render it as-is.
                let vis_nodes: Vec<&FileNode> = self.file_tree.nodes.iter().collect();
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
                for (idx, node) in vis_nodes.iter().enumerate() {
                    let node = *node;
                    let y = start_y - self.file_tree.scroll + idx as f32 * item_h;
                    // start_y 위로 조금이라도 걸친 항목은 통째 스킵한다. 그리기만
                    // 놓고 보면 이제 뒤에 오는 빠른파일 배경이 덮어 주지만, 이 루프는
                    // 히트렉트도 같이 만들어서 — 덮인 행이 여전히 눌린다. 그리기와
                    // 히트를 함께 끊는 자리가 여기뿐이다. 상단 잘림은 위쪽 fade 가 가린다.
                    if y < start_y - 0.5 || y > win_h {
                        continue; // off-screen / 상단 부분걸침 → clip (hit rect 도 생략)
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
                    rects.push((node.path.clone(), (row_x, y, row_w, item_h)));
                }
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
                // ── 빠른 파일 섹션(지연 그리기) ── 트리 본문·페이드 뒤에 그려, 스크롤로
                // start_y 위로 올라온 트리 항목을 불투명 배경으로 덮는다(scissor 없는
                // 렌더러의 겹침 방지). 클릭=보조탭, Opt+클릭=별도창.
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
            if git_col_w > 0.0 && self.info.tab == state::SideTab::Git {
                let dock_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT };
                let gcx0 = git_col_x + 14.0;
                let gcw = (git_col_w - 28.0).max(0.0);
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - dock_h).max(top);
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
                    let home = std::env::var("HOME").ok();
                    let path_disp = git_view
                        .cwd
                        .as_ref()
                        .map(|p| {
                            let s = p.to_string_lossy().into_owned();
                            match &home {
                                Some(h) if s.starts_with(h.as_str()) => format!("~{}", &s[h.len()..]),
                                _ => s,
                            }
                        })
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
                let commits_h = if git_view.recent_commits.is_empty() {
                    0.0
                } else {
                    let mut h = 24.0 + git_view.recent_commits.len() as f32 * 20.0;
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
                    h.min((bottom - TITLE_HEIGHT) * 0.72)
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
                    // push → the primary button becomes "↑ Push N" (GitHub-Desktop
                    // style); with changes it's Commit. The caret dropdown always
                    // offers the full set (Commit / Push / Pull / Create PR).
                    let push_mode = busy.is_none() && !can_commit && git_view.ahead > 0;
                    let can_drop = busy.is_none() && (can_commit || git_view.ahead > 0);
                    let main_active = busy.is_none() && (can_commit || push_mode);
                    let main_label = if let Some(op) = busy {
                        format!("{op}…")
                    } else if push_mode {
                        format!("Push  {}", git_view.ahead)
                    } else {
                        "Commit".to_string()
                    };
                    let main_icon = if push_mode { "arrow-up" } else { "git-commit-horizontal" };
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
                        for (title, staged, files) in [
                            ("Staged Changes", true, &git_view.staged),
                            ("Changes", false, &git_view.unstaged),
                        ] {
                            if files.is_empty() {
                                continue;
                            }
                            // Section header (count) — clipped to the list zone.
                            if !menus_open && y_cur + header_h > list_top && y_cur < input_top {
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
                                let row_visible = !menus_open && !(ry + item_h < list_top || ry > input_top);
                                if row_visible {
                                    let hovered = self.cursor_px.0 >= git_col_x
                                        && self.cursor_px.0 <= git_col_x + git_col_w
                                        && self.cursor_px.1 >= ry
                                        && self.cursor_px.1 < ry + item_h;
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
                                        let bh = self.cursor_px.0 >= ax && self.cursor_px.0 <= ax + aw && self.cursor_px.1 >= ry && self.cursor_px.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::radius_sm(), theme::surface_active());
                                        }
                                        g.queue_icon(if staged { "minus" } else { "plus" }, ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { theme::text() } else { icon_dim });
                                        stage_rects.push((!staged, path.clone(), (ax - 1.0, ry, aw + 2.0, item_h)));
                                        ax -= aw + agap;
                                    }
                                    {
                                        let bh = self.cursor_px.0 >= ax && self.cursor_px.0 <= ax + aw && self.cursor_px.1 >= ry && self.cursor_px.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::radius_sm(), theme::surface_active());
                                        }
                                        g.queue_icon("undo-2", ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { DIFF_RED } else { icon_dim });
                                        discard_rects.push((path.clone(), untracked, (ax - 1.0, ry, aw + 2.0, item_h)));
                                        ax -= aw + agap;
                                    }
                                    {
                                        let bh = self.cursor_px.0 >= ax && self.cursor_px.0 <= ax + aw && self.cursor_px.1 >= ry && self.cursor_px.1 < ry + item_h;
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
                                            if dy + dline_h < list_top || dy > input_top {
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
                    if !git_view.recent_commits.is_empty() {
                        let (curx, cury) = self.cursor_px;
                        let foot = bottom - 2.0;
                        let clip_r = git_col_x + git_col_w - 12.0;
                        let mut cy2 = input_top + 6.0;
                        g.rect(gcx0, cy2 - 2.0, gcw, 1.0, theme::with_alpha(theme::border(), 0x80));
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
                        let items: [(&str, String, GitCommitAction); 4] = [
                            ("git-commit-horizontal", "Commit".to_string(), GitCommitAction::Commit),
                            ("arrow-up", push_label, GitCommitAction::Push),
                            ("arrow-down", pull_label, GitCommitAction::Pull),
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
                let dock_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT };
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - dock_h).max(top);
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
                if h.busy {
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
                let n_btn: f32 = if h.is_image { 4.0 } else { 3.0 };
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
                // Overflow windowing: whole tabs only (no scissor to clip a
                // partial pill). When they can't all fit at the 56px minimum,
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
                    if !headered.contains(fid.as_str())
                        && self
                            .pane_activity
                            .get(fid)
                            .map_or(false, |a| a.status == "working")
                    {
                        const BAR_H: f32 = 2.5;
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
                    // 손을 기다리는 pane — 네 변이 핑크로 깜빡인다(거노: "내가
                    // 엔터해야되거나 그런거는 핑크색으로 깜빡이게"). 로딩바(숨쉬기)
                    // 와 **뜻이 정반대**라 형태부터 갈랐다: 스윕바는 "놔둬도 진행
                    // 된다", 이 테두리는 "내가 손대야 풀린다". 상태가 배타적이라
                    // (working ≠ waiting) 둘이 한 pane 에 같이 뜨지 않는다.
                    //
                    // 포커스 테두리(학생색) 위에 덧그린다 — 지금 보고 있는 pane 이
                    // 물어보고 멈춘 경우, 급한 쪽이 이겨야 한다. border_inset 도
                    // 다시 적어 하단바가 이 테두리를 덮지 않게 한다.
                    if self
                        .pane_activity
                        .get(fid)
                        .is_some_and(|a| a.status == "waiting")
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
                    // ··· 핸들을 생략한다 — 중복 진입점 제거.
                    if headered.contains(fid.as_str()) {
                        continue;
                    }
                    // ⋮ 핸들 — 상단 중앙. 평소엔 완전히 숨김. pane 상단 30% 띠에
                    // 커서가 들어오면 흐릿하게 등장하고, ⋮ 바로 위로 가면 진해진다
                    // (그때 손모양 커서 — handler 측). 클릭=메뉴·드래그=이동.
                    let hx = fx + (fw - HANDLE) / 2.0;
                    let hy = fy + HMARGIN;
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
                        let my = hy + HANDLE + 3.0;
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
            let sb_home = std::env::var("HOME").ok();
            for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                let fvis = self.statusbar.shown.contains(fid)
                    || (!self.statusbar.hidden.contains(fid) && self.set_footer_default);
                if !fvis || *fbox_h < PANE_FOOTER_HEIGHT + 4.0 {
                    continue;
                }
                let bar_y = fy + fbox_h - PANE_FOOTER_HEIGHT;
                // 테두리를 footer 배경이 덮지 않게 좌우·하단을 그 두께만큼 안쪽으로
                // 그린다 — 안 그러면 나중에 그려지는 footer bg 가 보더의 하단·좌우 끝을
                // 덮어 "선이 하단바를 제외하고 감싸는" 것처럼 보인다(거노). 두께는
                // 실제로 그린 쪽이 남긴 값을 쓴다(줌은 2.0, 분할 active 는 1.5).
                let bt = border_inset.get(fid.as_str()).copied().unwrap_or(0.0);
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, PANE_FOOTER_HEIGHT - bt, theme::bg());
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, 1.0, theme::border());
                // Pill metrics shared by every chip. 12/13 은 앱을 통틀어 가장 작은
                // 글자·아이콘이었다 — 같은 화면의 사이드바(13~14)와 나란히 놓이니
                // 하단바만 축소된 것처럼 읽혔다(거노). 본문 단과 같은 단으로 올린다.
                let pill_h = 22.0_f32;
                let pill_y = bar_y + (PANE_FOOTER_HEIGHT - pill_h) / 2.0;
                let icon_sz = 14.0_f32;
                let pad_x = 9.0_f32;
                let icon_gap = 6.0_f32;
                let chip_gap = 7.0_f32;
                let font = 13.0_f32;
                let txt_y = pill_y + (pill_h - font) / 2.0;
                let footer_hover = sb_my >= bar_y
                    && sb_my <= bar_y + PANE_FOOTER_HEIGHT
                    && sb_mx >= *fx
                    && sb_mx <= fx + fw;
                let mut cx = fx + 8.0;
                let cwd = self.pane_cwd_cache.get(fid).cloned();
                // Home-relative cwd (~/…), matching the screenshot's breadcrumb.
                let disp = cwd
                    .as_ref()
                    .map(|p| {
                        let s = p.to_string_lossy().into_owned();
                        let s = match &sb_home {
                            Some(h) if s.starts_with(h.as_str()) => format!("~{}", &s[h.len()..]),
                            _ => s,
                        };
                        nfc_hangul(&s)
                    })
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
                    let h_y = bar_y + (PANE_FOOTER_HEIGHT - h_sz) / 2.0;
                    let h_hover = sb_mx >= h_x - 4.0
                        && sb_mx <= h_x + h_sz + 4.0
                        && sb_my >= bar_y
                        && sb_my <= bar_y + PANE_FOOTER_HEIGHT;
                    if h_hover {
                        hover_rect(g, h_x - 4.0, h_y - 2.0, h_sz + 8.0, h_sz + 4.0,
                            theme::radius_sm());
                    }
                    g.queue_icon("chevrons-down-up", h_x, h_y, h_sz,
                        if h_hover { theme::text() } else { theme::text_mute() });
                    self.statusbar.toggle_rects
                        .push((fid.clone(), (h_x - 4.0, bar_y, h_sz + 12.0, PANE_FOOTER_HEIGHT)));
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
                    // Whole-row scroll: this renderer has no scissor clip, so a
                    // partial row would spill past the rounded menu edge. Snap
                    // the wheel offset to row units and page by integer rows.
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
            // 칩이 하나도 없어도 예약된 띠는 칠한다 — 안 칠하면 그리드가 비워 둔
            // 자리에 창 배경이 그대로 비쳐 바닥에 검은 틈이 생긴다.
            if dock_reserve > 0.0 {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let bar_y = win_h - DOCK_HEIGHT;
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
            if let (true, Some((ax, ay, aw, ah))) = (self.account_menu, self.account_chip_rect) {
                let (hmx, hmy) = self.cursor_px;
                let f = 13.0_f32;
                let pad_x = 10.0_f32;
                // 첫 행은 언제나 기본(id `""`) — 설정 화면의 목록과 같은 순서.
                // 맨 아래 "설정에서 계정 추가…" 로 막다른 골목을 막는다.
                let mut rows: Vec<(AccountMenuItem, String)> = vec![(
                    AccountMenuItem::Select(String::new()),
                    crate::settings::account_display("", "", "기본"),
                )];
                rows.extend(self.set_claude_accounts.iter().enumerate().map(|(i, a)| {
                    (
                        AccountMenuItem::Select(a.id.clone()),
                        crate::settings::account_display(
                            &a.id,
                            &a.label,
                            &format!("계정 {}", i + 2),
                        ),
                    )
                }));
                rows.push((
                    AccountMenuItem::AddInSettings,
                    "설정에서 계정 추가…".to_string(),
                ));
                // 계정별 한도 — **누르기 전에** 보여야 한다(거노: "누르면 전환되버리잖아").
                // 폴러가 슬롯별로 채운 표(`claude_usage_all`)를 그대로 읽는다. 값이 없는
                // 계정(한 번도 조회 못 함·토큰 없음)은 빈칸으로 둔다 — 0% 로 그리면
                // "여유 있음"이라는 **거짓말**이 되고, 그게 옮길 곳을 고르는 판단을 망친다.
                let usage_of = |id: &str| -> Option<crate::UsageBadge> {
                    let key = crate::socket::claude_account_dir(id)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.claude_usage_all.lock().ok()?.get(&key).cloned()
                };
                // 표기·색은 **Info 탭 사용량 pill 과 같은 규칙**을 쓴다(info.rs
                // draw_info_actions): `7d 62%`, stale 이면 `~` 를 앞에. 같은 숫자가
                // 두 자리에서 다르게 보이면 어느 쪽을 믿을지가 문제가 된다.
                // 퍼센트 뒤에 **언제 풀리는지**를 붙인다 — 90% 라도 12분 뒤면 기다리면
                // 되고, 3시간 뒤면 지금 옮겨야 한다(거노 2026-08-07).
                let usage_text = |b: &Option<crate::UsageBadge>| -> String {
                    match b {
                        Some(b) => {
                            let head = if b.stale {
                                format!("~{} {:.0}%", b.label, b.pct)
                            } else {
                                format!("{} {:.0}%", b.label, b.pct)
                            };
                            match crate::resets_in_label(b.resets_at) {
                                Some(l) => format!("{head} · {l}"),
                                None => head,
                            }
                        }
                        None => "—".to_string(),
                    }
                };
                // 값이 없는 계정은 **빈칸이 아니라 `—`**. 빈칸은 "한도 여유"로 읽혀서,
                // 옮길 곳을 고르는 판단을 정확히 반대로 만든다. 오래 안 쓴 계정은 OAuth
                // 토큰이 만료돼(8시간쯤) usage 조회가 거부되므로 실제로 자주 생긴다 —
                // 그 계정으로 claude 를 한 번 돌리면 토큰이 갱신돼 숫자가 돌아온다.
                let row_usage = |item: &AccountMenuItem| -> Option<Option<crate::UsageBadge>> {
                    match item {
                        AccountMenuItem::Select(id) => Some(usage_of(id)),
                        _ => None,
                    }
                };
                let rh = 28.0_f32;
                let pad = 4.0_f32;
                // 한도 칸이 이름을 밀지 않게 폭 계산에 함께 넣는다.
                let gap = 14.0_f32;
                let mw = rows
                    .iter()
                    .map(|(item, l)| {
                        let mut w = g.measure_chrome_text(l.as_str(), f, true) + pad_x * 2.0;
                        if let Some(b) = row_usage(item) {
                            w += gap + g.measure_chrome_text(usage_text(&b).as_str(), f - 1.0, true);
                        }
                        w
                    })
                    .fold(aw, f32::max);
                // +5 = "추가" 행 위 구분선이 먹는 자리.
                let mh = pad * 2.0 + rh * rows.len() as f32 + 5.0;
                // 계정 행 오른쪽 끝에 맞춰 내린다 — 창 왼쪽으로는 안 넘어가게 클램프.
                let mx = (ax + aw - mw).max(4.0);
                let my = ay + ah + 4.0;
                // 패널 배경과 팝업 배경은 6단계밖에 안 벌어져서, 색만으로는 이게
                // 떠 있는 메뉴인지 패널의 한 구역인지 읽히지 않았다(거노: 뒤가
                // 비쳐 보인다). 층 선언은 색이 아니라 그림자·테두리가 하는 일이라
                // 그걸 위해 있는 공통 함수로 넘긴다 — 셸 메뉴·모달과 같은 언어.
                panel_rect_outlined(g, mx, my, mw, mh, theme::radius_sm(), theme::surface_hover());
                let mut ry = my + pad;
                for (item, label) in rows {
                    if item == AccountMenuItem::AddInSettings {
                        // 전환 목록과는 다른 종류의 동작이라 얇은 선으로 가른다.
                        g.rect(mx + pad, ry + 2.0, mw - pad * 2.0, 1.0, theme::border());
                        ry += 5.0;
                    }
                    let on = hmx >= mx && hmx <= mx + mw && hmy >= ry && hmy <= ry + rh;
                    let active = item == AccountMenuItem::Select(self.set_claude_account.clone());
                    if on {
                        round_rect(g, mx + pad, ry, mw - pad * 2.0, rh,
                            theme::radius_sm(), theme::surface_active());
                    }
                    if active {
                        // 활성 표시는 왼쪽 accent 막대 — 체크 아이콘보다 좁다.
                        pill_rect(g, mx + pad, ry + 5.0, 2.5, rh - 10.0, theme::accent());
                    }
                    g.draw_text(
                        mx + pad_x, ry + (rh - f) / 2.0 - 1.0, &label,
                        gpu::DrawOpts {
                            font_size: f,
                            color: if active { theme::text() } else { theme::text_dim() },
                            bold: active,
                            italic: false,
                        },
                    );
                    if let Some(b) = row_usage(&item) {
                        let u = usage_text(&b);
                        let uf = f - 1.0;
                        let uw = g.measure_chrome_text(u.as_str(), uf, true);
                        // 임계도 pill 과 같은 값(90 위험 · 70 주의). 옮길 곳을 고르려고
                        // 여는 목록이라 "여기도 꽉 찼다"가 이름만큼 빨리 읽혀야 한다.
                        // 모르는 값(`—`)은 색을 안 준다 — 초록으로 그리면 여유로 읽힌다.
                        let col = match &b {
                            None => theme::text_mute(),
                            Some(b) if b.pct >= 90.0 => theme::danger(),
                            Some(b) if b.pct >= 70.0 => theme::syn_number(),
                            Some(_) => theme::success(),
                        };
                        let col = if b.as_ref().is_some_and(|b| b.stale) {
                            theme::with_alpha(col, 0x99)
                        } else {
                            col
                        };
                        g.draw_text(
                            mx + mw - pad_x - uw, ry + (rh - uf) / 2.0 - 1.0, &u,
                            gpu::DrawOpts { font_size: uf, color: col, bold: true, italic: false },
                        );
                    }
                    self.account_menu_hits.push((item, (mx, ry, mw, rh)));
                    ry += rh;
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
                let y = win_h - v_font - margin;
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
        // statusline 프사 클릭 hit-test (학생이름, rect) — 프사 클릭 시 학생 설정
        // 별도창 딥링크. 매 프레임 재구축이라 stale rect 가 남지 않는다.
        self.face_hit_rects = profile_face_hits
            .iter()
            .map(|(n, _, r)| (n.clone(), *r))
            .collect();
        self.pane_tab_rects = tab_hits;
        self.pane_tab_close_rects = tab_close_hits;
        self.pane_tab_popout_rects = tab_popout_hits;
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
        let pty_dirty = self.ws.lock().unwrap().panes.values().any(|p| p.dirty);
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
        let bar_animating = self
            .pane_activity
            .values()
            .any(|a| a.status != "idle" && !a.status.is_empty());
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
        let rebuild = pty_dirty
            || self.chrome_dirty
            || blink_changed
            || version_animating
            || toast_animating
            || git_op_animating
            || banner_animating;
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

// ── Clawd 시작 배너 → 학생 도트 교체 헬퍼 ──────────────────────────────
// Claude Code 웰컴 박스의 Clawd 아트(블록문자 3행)를 감지해, 그 자리에
// 이 pane에 배정된 학생의 idle 도트(arona-ui walk 스프라이트 frame-00)를
// 그리기 위한 자유함수들. 감지는 캐릭터 배정 pane에 한정된다.

/// Clawd 아트가 차지하는 셀 박스 크기 (cols × rows).
/// diff 의 삭제 줄 수를 적는 빨강. git 칼럼과 커밋 모달이 한 화면에 같이 뜨는데
/// 둘이 각자 색을 들고 있으면 같은 뜻에 두 가지 빨강이 난다.
const DIFF_RED: [u8; 4] = [229, 83, 75, 255];

pub(crate) const CLAWD_COLS: usize = 9;
pub(crate) const CLAWD_ROWS: usize = 3;

/// 학생 도트 애니메이션 — idle(배너)·walk(로딩바) 모션별 프레임 수·주기.
const STUDENT_IDLE_FRAMES: usize = 4;
pub(crate) const STUDENT_ANIM_FRAME_MS: u64 = 200;
const STUDENT_WALK_FRAMES: usize = 6;
const STUDENT_WALK_FRAME_MS: f32 = 140.0;
/// statusline 프사 높이(행). statusline 행에 바닥 정렬하고 위로 이만큼
/// 침범한다 — 1행짜리 얼굴은 너무 작았다(거노). 2행 = statusline + 바로 위
/// 입력박스 아래 테두리 행까지. 3행이면 `❯` 입력행에 걸려 타이핑을 가린다.
pub(crate) const STATUSLINE_FACE_ROWS: usize = 2;
/// 입력박스 위 스페이서 행에 서 있는 학생(전신 idle)의 키(행). 발은 입력박스
/// 윗 테두리에 닿고 위는 스크롤백 꼬리라 몇 행 덮여도 무해 — 배너와 같은 키.
pub(crate) const INPUT_STANDING_ROWS: usize = 3;

/// 직전 프레임에 학생 도트 배너가 화면에 있었는지. 배너 애니 타이머
/// 스레드(handler.rs)가 이걸 보고 배너가 보일 때만 redraw를 깨운다 —
/// 배너가 없으면 sleep 루프만 돌아 idle 비용이 0에 수렴한다.
pub(crate) static STUDENT_SPRITE_ANIMATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);




/// 에이전트 TUI 의 입력 영역. 하네스마다 **모양이 다르다** — claude 는 `─` 보더
/// 두 줄이 입력행을 감싸고, codex 는 보더가 없는 대신 입력행 전체가 배경색으로
/// 칠해져 있다(실측 `bg=Rgb(63,69,77)`, 주변은 `Default`).
///
/// 판정을 한 곳에 모으는 이유는 `strip_agent_chip` 주석(아래)에 적힌 사고 그대로다:
/// 같은 일을 하는 사본이 각자 관문을 들면 한쪽만 고쳐져 조용히 어긋난다.
pub(crate) enum PromptBox {
    /// claude — `rows` 가 입력행, 그 바깥 `top`/`bottom` 이 대시 보더.
    Bordered { rows: std::ops::Range<usize>, top: usize, bottom: usize },
    /// codex — 보더가 없다. 칠할 것도 칩을 지울 자리도 입력행 자신뿐이다.
    Filled { rows: std::ops::Range<usize> },
}

impl PromptBox {
    pub(crate) fn rows(&self) -> std::ops::Range<usize> {
        match self {
            PromptBox::Bordered { rows, .. } | PromptBox::Filled { rows } => rows.clone(),
        }
    }
}

/// 에이전트 TUI 입력 영역 탐지 — 화면 하단에서 위로 찾는다.
///
/// **claude**: `─` 보더 두 줄 사이. 그 사이에 `❯` 마커 행이 있어야 인정한다
/// (권한 메뉴 등 다른 풀폭 박스 오인 방지).
///
/// **codex**: 보더가 없다. 대신 입력행이 **명시 배경색으로 통째로 칠해져** 있어서
/// 그걸 시그니처로 쓴다 — `›` 로 시작하고 그 행의 모든 글리프가 같은 non-Default
/// `bg` 를 공유하는 행. 배경 없이 `›` 만 보면 인용문·diff 를 입력창으로 오인한다.
fn prompt_box(rows: &[Vec<GridCell>]) -> Option<PromptBox> {
    fn is_border(r: &[GridCell]) -> bool {
        let (mut dash, mut glyph) = (0usize, 0usize);
        for c in r {
            if c.ch == '\0' || c.ch == ' ' {
                continue;
            }
            glyph += 1;
            if c.ch == '─' {
                dash += 1;
            }
        }
        dash >= 10 && dash * 2 >= glyph
    }
    // claude 입력박스 마커는 `❯`(U+276F, 또는 옛 `›`)뿐 — ASCII `>` 는 제외한다.
    // diff·git·노트 TUI 는 대시줄 사이에 ASCII `>`(인용·프롬프트) 를 흔히 둬서,
    // `>` 까지 마커로 치면 그 대시줄 쌍을 입력박스로 오인해 뜬금없는 빈 초록
    // 사각형을 덧그렸다(거노 2026-07-22).
    fn marker_row(r: &[GridCell]) -> bool {
        r.iter().find(|c| c.ch != ' ' && c.ch != '\0').is_some_and(|c| matches!(c.ch, '❯' | '›'))
    }
    if let Some(b2) = rows.iter().rposition(|r| is_border(r)) {
        if let Some(b1) = rows[..b2].iter().rposition(|r| is_border(r)) {
            let range = (b1 + 1)..b2;
            if !range.is_empty() && rows[range.clone()].iter().any(|r| marker_row(r)) {
                return Some(PromptBox::Bordered { rows: range, top: b1, bottom: b2 });
            }
        }
    }
    // codex — 칠해진 입력행. 행 전체가 같은 non-Default bg 를 쓰는 것이 시그니처고,
    // 마커(`›`)를 함께 요구해 배경만 남은 여백 행과 구별한다.
    let uniform_fill = |r: &[GridCell]| -> Option<kasa_bridge::screen::Color> {
        let mut fill: Option<kasa_bridge::screen::Color> = None;
        let mut glyphs = 0usize;
        for c in r.iter().filter(|c| c.ch != '\0') {
            if matches!(c.bg, kasa_bridge::screen::Color::Default) {
                return None;
            }
            if fill.is_some_and(|f| f != c.bg) {
                return None;
            }
            fill = Some(c.bg.clone());
            glyphs += 1;
        }
        (glyphs >= 8).then_some(fill?)
    };
    let f = rows
        .iter()
        .rposition(|r| marker_row(r) && uniform_fill(r).is_some())?;
    let fill = uniform_fill(&rows[f])?;
    // 입력창은 **여러 줄이다** — 마커 행 위아래로 같은 채움색 여백 행이 붙고,
    // 여러 줄을 입력하면 그만큼 자란다(실측 0.146.0: 여백-입력-여백 3줄). 마커
    // 행만 칠하면 가운데 한 줄만 색이 바뀌어 상자가 아니라 밑줄로 보인다(거노).
    let same = |r: &[GridCell]| uniform_fill(r).is_some_and(|c| c == fill);
    let mut start = f;
    while start > 0 && same(&rows[start - 1]) {
        start -= 1;
    }
    let mut end = f + 1;
    while end < rows.len() && same(&rows[end]) {
        end += 1;
    }
    Some(PromptBox::Filled { rows: start..end })
}

/// 학생 pane 입력박스의 양끝 보더 행(─ 줄 + @배지)을 claude 가 /color·
/// --agent-color 로 그린 명시색을 **무시하고** 학생 accent 로 강제 도색한다 —
/// pane 정체성 색과 항상 일치. (본문 틴트가 있던 시절엔 사이 행의 입력 글자를
/// 틴트에서 빼는 처리도 여기 있었는데, 본문이 테마 기본 fg 로 돌아가며 폐기.)
pub(crate) fn style_prompt_box(rows: &mut [Vec<GridCell>], accent: [u8; 4]) {
    let Some(bx) = prompt_box(rows) else { return };
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    match &bx {
        PromptBox::Bordered { top, bottom, .. } => {
            for i in [*top, *bottom] {
                for c in rows[i].iter_mut() {
                    // 세션명/테두리 줄 배경(claude --agent-color 로 채운 accent 밴드)을
                    // 터미널색으로 되돌린다 — 아웃라인(─ 대시·세션명 글자)만 accent 로
                    // 두고 배경은 안 칠한다(거노: 배경까지 채우면 글자가 묻힌다).
                    c.bg = kasa_bridge::screen::Color::Default;
                    if c.ch != ' ' && c.ch != '\0' {
                        c.fg = fg.clone();
                    }
                }
            }
        }
        // codex 는 칠할 보더가 없다. 이미 배경으로 칠해진 그 줄을 학생색 쪽으로
        // 끌어당긴다 — 거노 선택(2026-08-05). 원래 배경을 버리지 않고 섞는 이유는
        // 입력 글자가 묻히지 않게 하기 위해서다(보더 도색이 배경을 비우는 것과
        // 같은 이유). 여기서 fg 는 건드리지 않는다.
        PromptBox::Filled { rows: r } => {
            for i in r.clone() {
                for c in rows[i].iter_mut() {
                    if let kasa_bridge::screen::Color::Rgb(br, bg_, bb) = c.bg {
                        c.bg = tint_toward([br, bg_, bb], accent, PROMPT_TINT);
                    }
                }
            }
        }
    }
    // 입력행 왼쪽 ❯ 프롬프트 마커도 학생 accent 로 — claude --agent-color(8색
    // 근사)가 남으면 보더와 화살표 색이 어긋난다(거노). 마커 글리프 한 칸만
    // 칠하고 입력 글자는 테마 기본 fg 유지.
    for r in bx.rows() {
        if let Some(c) = rows[r]
            .iter_mut()
            .find(|c| c.ch != ' ' && c.ch != '\0')
            .filter(|c| matches!(c.ch, '❯' | '›' | '>'))
        {
            c.fg = fg.clone();
        }
    }
}

/// codex 입력줄 배경을 학생색 쪽으로 끌어당기는 비율. 글자가 묻히지 않을 만큼만.
const PROMPT_TINT: f32 = 0.22;

/// `base` 를 `accent` 쪽으로 `amount` 만큼 섞는다. 셀 배경은 알파가 없어
/// (`Color::Rgb` 뿐) 미리 합성해야 한다 — `theme::with_alpha` 를 못 쓰는 이유.
fn tint_toward(base: [u8; 3], accent: [u8; 4], amount: f32) -> kasa_bridge::screen::Color {
    let mix = |b: u8, a: u8| (b as f32 + (a as f32 - b as f32) * amount).round().clamp(0.0, 255.0) as u8;
    kasa_bridge::screen::Color::Rgb(
        mix(base[0], accent[0]),
        mix(base[1], accent[1]),
        mix(base[2], accent[2]),
    )
}

/// verbose OFF 에서 접힌 팀메시지 행 탐지 — 단수 "› Message from @<이름>" 또는
/// 복수 "› <N> messages from @<이름>". 반환은 (첫 글리프 col, 메시지 수, 보낸이
/// agent 이름). 이름 뒤에 다른 글자가 있으면(본문 안 인용 등) 접힌 줄이 아니라고
/// 본다 — 오탐이 실제 출력 텍스트를 덮어쓰면 안 된다.
fn teammate_collapsed_line(row: &[GridCell]) -> Option<(usize, usize, String)> {
    let chars: Vec<char> = row
        .iter()
        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
        .collect();
    let first = chars.iter().position(|&c| c != ' ')?;
    if !matches!(chars[first], '›' | '>') {
        return None;
    }
    // '›' 뒤: 단수 " Message from @<이름>" 또는 복수 " <N> messages from @<이름>".
    let rest: String = chars[first + 1..].iter().collect();
    let (count, after) = if let Some(a) = rest.strip_prefix(" Message from @") {
        (1usize, a.to_string())
    } else {
        let a = rest.strip_prefix(' ')?;
        let digits = a.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        let n: usize = a[..digits].parse().ok()?;
        let a2 = a[digits..].strip_prefix(" messages from @")?;
        (n, a2.to_string())
    };
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    // 이름 뒤는 공백, 또는 "(ctrl+o to expand)" 꼴 단축키 힌트 하나만 허용 —
    // claude v2.1.216+ 가 접힌 줄 끝에 힌트를 붙인다(chord 는 키바인딩 따라
    // 변주라 "to expand)" 종결로 판정). 그 외 텍스트는 본문 인용 오탐 방지.
    let tail = after[name.len()..].trim_matches(' ');
    if !tail.is_empty() && !(tail.starts_with('(') && tail.ends_with("to expand)")) {
        return None;
    }
    Some((first, count, name))
}

/// tell 주입 마커 `⟦캐릭터⟧ 본문` 감지 — kasaterm-cli tell 이 발신 pane 캐릭터를
/// 앞에 심는다(SendMessage 는 팀 경계 안이라 크로스-방 tell 만 화면에 발신자 앵커가
/// 필요). `character_accent` 유효 캐릭터만 인정해 거노가 우연히 친 `⟦…⟧` 오탐을
/// 막는다. 반환: (⟦ 시작 col, ⟧ 다음 col, 캐릭터명).
fn tell_marker_line(row: &[GridCell]) -> Option<(usize, usize, String)> {
    let chars: Vec<char> = row
        .iter()
        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
        .collect();
    let mut first = chars.iter().position(|&c| c != ' ')?;
    // claude TUI 는 제출된 user 턴을 `❯ ` 프롬프트 마커로 시작해 그린다 — 마커는
    // 그 뒤에 온다.
    if chars[first] == '❯' {
        first = chars[first + 1..]
            .iter()
            .position(|&c| c != ' ')
            .map(|i| first + 1 + i)?;
    }
    if chars[first] != '⟦' {
        return None;
    }
    let close_rel = chars[first + 1..].iter().position(|&c| c == '⟧')?;
    // wide 글자는 그리드에서 ch + blank 2셀이라, 이름 구간의 padding blank 를 뺀다.
    let name: String = chars[first + 1..first + 1 + close_rel]
        .iter()
        .filter(|&&c| c != ' ' && c != '\0')
        .collect();
    theme::character_accent(&name)?;
    Some((first, first + 1 + close_rel + 1, name))
}

/// tell·SendMessage 프사 셀 폭 — claude 가 접는 본문 들여쓰기(2칸)와 같아야
/// 본문 좌측선을 밀지 않는다. 셀이 세로 2:1 이라 2칸×1행이 곧 정사각 bust.
pub(crate) const TELL_FACE_COLS: usize = 2;

/// tell 마커 행을 발신 학생색으로 — 그 행 전체를 accent fg 로 칠해 SendMessage
/// 인라인과 시각을 맞춘다. 프사가 있는 캐릭터는 첫 줄 본문을 마커 시작 col 로
/// 당겨(= claude 의 `❯ ` 폭 2 = wrap 들여쓰기) 접힌 줄과 좌측선을 맞추고, 비워진
/// `❯` 자리 2칸에 아바타를 얹는다(호출측 이미지 패스) — 옛 배치는 첫 줄만 프사
/// 폭만큼 밀려 계단이 졌다(거노 2026-07-27). 위 행 헤더로 올리는 안은 claude 가
/// user 턴 앞에 빈 줄을 두지 않아 윗줄 글자를 덮어 기각(실측). slug 없는
/// 캐릭터만 `이름 ›` 인라인 폴백. 반환은 프사 rect 의 x 기준 col — 없으면 None.
fn restyle_tell_line(
    row: &mut [GridCell],
    marker_start: usize,
    marker_end: usize,
    name: &str,
    accent: [u8; 4],
) -> Option<usize> {
    use unicode_width::UnicodeWidthChar;
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    let end = marker_end.min(row.len());
    if theme::character_slug(name).is_some() {
        // 마커 뒤 본문을 마커 시작 col 로 당긴다(claude 의 `❯ ` 폭 = 2 = wrap
        // 들여쓰기라 첫 줄과 연속 줄이 같은 col 에 선다).
        let body_start = row[end..]
            .iter()
            .position(|c| c.ch != ' ' && c.ch != '\0')
            .map(|i| end + i)
            .unwrap_or(end);
        let shift = body_start.saturating_sub(marker_start);
        if shift > 0 {
            row[marker_start..].rotate_left(shift);
            let n = row.len();
            for c in row[n - shift..].iter_mut() {
                *c = GridCell::blank();
            }
        }
        // 비운 앞칸은 본문 셀의 배경(claude 가 깐 user 턴 하이라이트)을 물려받는다
        // — blank 로 두면 첫 줄만 배경이 늦게 시작해 wrap 연속 행과 계단이 진다.
        let pad = {
            let mut c = row[marker_start.min(row.len() - 1)].clone();
            c.ch = ' ';
            c
        };
        for c in row[..marker_start].iter_mut() {
            *c = pad.clone();
        }
        tint_row(row, accent);
        // 프사는 `❯` 가 있던 왼쪽 여백에 본문과 같은 행으로 — 위 행은 claude 가
        // 이전 블록(`✻ Worked for 5s` 등)으로 채워 두는 게 보통이라 헤더로 올리면
        // 그 글자를 덮는다(실측 2026-07-27). 여백이 프사 폭에 못 미치면(마커가
        // 행 머리) 프사를 포기하고 색만 입힌다.
        return (marker_start >= TELL_FACE_COLS).then_some(0);
    }
    let lead = row[..end]
        .iter()
        .position(|c| c.ch != ' ' && c.ch != '\0')
        .unwrap_or(0);
    for c in row[..end].iter_mut() {
        *c = GridCell::blank();
    }
    let label = format!("{name} ›");
    // 라벨을 본문 쪽(end)에 붙인다. 지우는 마커 `⟦이름⟧ ` 폭은 이름 길이에 따라
    // 가변인데 라벨은 고정폭이라, 왼쪽 정렬하면 남는 칸이 그대로 `›`—본문 사이
    // 갭으로 보였다(거노 2026-07-27: 이름이 길수록 더 벌어짐).
    let label_w: usize = label.chars().map(|c| c.width().unwrap_or(1).max(1)).sum();
    let start = end.saturating_sub(label_w + 1).max(lead);
    let mut w = start;
    for ch in label.chars() {
        let cw = ch.width().unwrap_or(1).max(1);
        if w + cw > end {
            break;
        }
        let mut cell = GridCell::blank();
        cell.ch = ch;
        cell.fg = fg.clone();
        row[w] = cell;
        if cw == 2 && w + 1 < end {
            let mut sp = GridCell::blank();
            sp.fg = fg.clone();
            row[w + 1] = sp;
        }
        w += cw;
    }
    // 본문(마커 뒤)도 학생색으로.
    tint_row(&mut row[end..], accent);
    None
}

/// 행의 비공백 글자 fg 를 accent 로 — tell 마커 행과 그 wrap 연속 행이 공유.
fn tint_row(row: &mut [GridCell], accent: [u8; 4]) {
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    for c in row.iter_mut() {
        if c.ch != ' ' && c.ch != '\0' {
            c.fg = fg.clone();
        }
    }
}

/// tell 마커 행의 wrap 연속 행 판정 — claude TUI 는 긴 user 턴을 2칸 들여쓰기
/// 행으로 wrap 한다. 들여쓰기가 정확히 2 이고 첫 글자가 TUI 구조 글리프가
/// 아니면 같은 메시지의 연속으로 본다(⎿·⏺ 등 다음 블록에서 끊김).
fn tell_wrap_continuation(row: &[GridCell]) -> bool {
    match row.iter().position(|c| c.ch != ' ' && c.ch != '\0') {
        Some(2) => !matches!(
            row[2].ch,
            '⏺' | '✻' | '⎿' | '│' | '⎢' | '❯' | '─' | '═' | '╌' | '⏵' | '·'
        ),
        _ => false,
    }
}

/// 팀원 agent 이름("aru-9c88")의 보낸 학생 accent — 로마자 앞부분(마지막 '-'
/// 앞)을 로스터로 역매핑. 로스터 밖(team-lead 등)은 transcript 태그의 color
/// 명 → 그것도 없으면 테마 accent.
/// 팀메시지 발신자 이름 → 학생 슬러그(프사 에셋 키). `from` 이 한글 표시명
/// ("프라나")인 경우와 agent-name 꼬리표가 붙은 슬러그("midori-2535") 둘 다 받는다.
fn teammate_sender_slug(name: &str) -> Option<&'static str> {
    if let Some(s) = theme::character_slug(name) {
        return Some(s);
    }
    let slug = name.rsplit_once('-').map(|(a, _)| a).unwrap_or(name);
    theme::slug_character(slug).and_then(theme::character_slug)
}

fn teammate_sender_accent(name: &str, tag_color: Option<&str>) -> [u8; 4] {
    // 발신자가 한글 캐릭터 표시명인 경우(F-2 인박스 규칙의 `from` = 발신 캐릭터명)
    // 를 먼저 본다 — 슬러그 경로만 타면 "프라나" 같은 이름이 매칭에 실패해 학생색
    // 대신 tag_color 폴백으로 떨어졌다(거노 2026-07-27: SendMessage 도 학생 테마).
    if let Some(c) = theme::character_accent(name) {
        return c;
    }
    let slug = name.rsplit_once('-').map(|(a, _)| a).unwrap_or(name);
    if let Some(c) = theme::slug_character(slug).and_then(theme::character_accent) {
        return c;
    }
    match tag_color {
        Some("red") => [224, 88, 78, 255],
        Some("orange") => [228, 140, 60, 255],
        Some("yellow") => [212, 180, 60, 255],
        Some("green") => [63, 170, 90, 255],
        Some("cyan") => [70, 180, 200, 255],
        Some("blue") => [90, 140, 230, 255],
        Some("purple") => [168, 118, 228, 255],
        Some("pink") => [228, 100, 160, 255],
        _ => theme::accent(),
    }
}

/// transcript 에서 회수한 팀메시지 원문(접힌 줄 전개·말풍선용).
#[derive(Clone)]
struct TeammateMsg {
    body: String,
    color: Option<String>,
    /// 화면에 뜬 이름이 쓸모없을 때(`@peer`) 태그에서 되찾은 진짜 발신자.
    /// 이게 있어야 학생색·프사·본문 조회가 이름으로 걸린다.
    sender: Option<String>,
}

/// `uds:/tmp/cc-socks/27516.sock` → 그 세션이 명부에 등록한 이름.
///
/// claude 는 cross-session 메시지를 `@peer` 라는 고정 라벨로 그린다(발신자 이름이
/// 명부에 멀쩡히 있어도 그렇다 — 2026-08-09 실측). 그 이름으로는 학생색도 프사도
/// 본문도 못 찾으므로, 태그가 실어 준 소켓 경로의 pid 로 명부를 되짚는다.
/// `from-name` 을 안 쓰는 이유는 그게 세션 이름이라 자동 제목에 덮이기 때문이다.
fn socket_pid(from: &str) -> Option<&str> {
    let pid = from.rsplit('/').next()?.strip_suffix(".sock")?;
    (!pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit())).then_some(pid)
}

fn peer_name_from_socket(from: &str) -> Option<String> {
    let pid = socket_pid(from)?;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let path = home.join(".claude/sessions").join(format!("{pid}.json"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let name = v.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// pane 에 도착한 남의 메시지 본문. 두 형식을 다 받는다.
///
/// 트리플을 걷어내기 전에는 팀 인박스만 있어 `<teammate-message teammate_id=… color=…>`
/// 하나였는데, cross-session 으로 옮기면서 `<cross-session-message from=… from-name=…>`
/// 이 새로 생겼다. 후자를 모르면 남의 메시지가 학생 테마 없이 claude 기본 표시
/// (`Message from @peer`)로만 뜬다 — 2026-08-09 회귀.
///
/// ⚠️ cross-session 태그엔 **`color` 가 없다.** 발신자 학생색은 여기서 못 얻으므로
/// 색 없이 돌려주고, 표시층이 이름으로 찾거나 기본색을 쓴다.
fn extract_teammate_msg(text: &str, sender: &str) -> Option<TeammateMsg> {
    extract_tagged_msg(text, sender, "<teammate-message", "teammate_id", "</teammate-message>")
        .or_else(|| {
            extract_tagged_msg(
                text,
                sender,
                "<cross-session-message",
                "from-name",
                "</cross-session-message>",
            )
        })
}

/// claude 가 cross-session 발신자를 부르는 고정 라벨. 이 이름으로는 아무것도 못 찾으므로
/// 태그를 이름 대조 없이 잡고 소켓 pid 로 진짜 발신자를 되찾는 신호로 쓴다.
const PEER_LABEL: &str = "peer";

/// 한 태그 형식에 대한 파싱 — 속성은 key="value" 나열(순서 무관).
fn extract_tagged_msg(
    text: &str,
    sender: &str,
    open: &str,
    id_attr: &str,
    close_tag: &str,
) -> Option<TeammateMsg> {
    let mut rest = text;
    loop {
        let s = rest.find(open)?;
        let after = &rest[s + open.len()..];
        let close = after.find('>')?;
        let attrs = &after[..close];
        let tail = &after[close + 1..];
        let attr = |key: &str| -> Option<String> {
            let pat = format!("{key}=\"");
            let a = attrs.find(&pat)? + pat.len();
            let e = attrs[a..].find('"')?;
            Some(attrs[a..a + e].to_string())
        };
        // `@peer` 로 뜬 줄은 이름 대조가 무의미하다 — 그 라벨은 발신자와 무관한
        // 고정값이라 어떤 태그와도 안 맞는다. 그래서 대조를 건너뛰고 최근 것을 잡되,
        // 소켓 pid 로 진짜 이름을 되찾아 함께 돌려준다(못 찾으면 라벨 그대로).
        let peer_probe = sender == PEER_LABEL && id_attr == "from-name";
        if peer_probe || attr(id_attr).as_deref() == Some(sender) {
            let end = tail.find(close_tag).unwrap_or(tail.len());
            return Some(TeammateMsg {
                body: tail[..end].trim().to_string(),
                color: attr("color"),
                sender: if peer_probe {
                    attr("from").as_deref().and_then(peer_name_from_socket)
                } else {
                    None
                },
            });
        }
        rest = tail;
    }
}

/// jsonl 한 줄의 user 턴 텍스트 — content 가 문자열이면 그대로, 배열이면
/// text 블록들을 이어붙인다(팀메시지는 둘 다로 도착할 수 있다).
fn jsonl_user_text(v: &serde_json::Value) -> Option<String> {
    let c = v.pointer("/message/content")?;
    if let Some(s) = c.as_str() {
        return Some(s.to_string());
    }
    let mut out = String::new();
    for b in c.as_array()? {
        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
            out.push_str(t);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// pane transcript tail 에서 sender 의 최신 팀메시지 — 파일 길이가 그대로면
/// 캐시 반환(프레임당 stat 1회), 대화가 자라 길이가 변했을 때만 재스캔.
fn latest_teammate_msg(path: &std::path::Path, sender: &str) -> Option<TeammateMsg> {
    type Cache =
        std::collections::HashMap<(std::path::PathBuf, String), (u64, Option<TeammateMsg>)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(Default::default);
    let len = std::fs::metadata(path).ok()?.len();
    let key = (path.to_path_buf(), sender.to_string());
    let mut map = CACHE.lock().ok()?;
    if let Some((l, m)) = map.get(&key) {
        if *l == len {
            return m.clone();
        }
    }
    let (tail, _) = crate::socket::read_tail(path, 256 * 1024);
    let found = tail.lines().rev().find_map(|l| {
        if !l.contains("<teammate-message") || !l.contains(sender) {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        extract_teammate_msg(&jsonl_user_text(&v)?, sender)
    });
    map.insert(key, (len, found.clone()));
    found
}

/// 행 전체가 공백/blank 인가 — 팀메시지 줄바꿈 전개가 이어 쓸 수 있는 행.
fn row_is_blank(row: &[GridCell]) -> bool {
    row.iter().all(|c| matches!(c.ch, ' ' | '\0'))
}

/// 문자열의 셀 폭 합(와이드 글리프 2칸).
fn cell_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars().map(|c| c.width().unwrap_or(1).max(1)).sum()
}

/// 셀 폭 기준 word-wrap — 첫 줄은 first_w, 이후 줄은 cont_w 폭, 최대
/// max_lines 줄. 공백 경계 우선, 줄보다 긴 단어는 글자 단위 분할.
/// 반환 = (줄들, 본문이 남아 잘렸는지).
fn wrap_body_cells(
    text: &str,
    first_w: usize,
    cont_w: usize,
    max_lines: usize,
) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split(' ') {
        let ww = cell_width(word);
        let limit = if lines.is_empty() { first_w } else { cont_w };
        let need = if cur.is_empty() { ww } else { cur_w + 1 + ww };
        if need <= limit {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += ww;
            continue;
        }
        if !cur.is_empty() {
            let full = lines.len() + 1 >= max_lines;
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            if full {
                return (lines, true);
            }
        }
        // 단어가 다음 줄에도 통째로 안 들어가면 글자 단위로 쪼갠다.
        let mut rest = word;
        loop {
            let limit = if lines.is_empty() { first_w } else { cont_w };
            if cell_width(rest) <= limit {
                cur = rest.to_string();
                cur_w = cell_width(&cur);
                break;
            }
            let mut take_b = 0usize;
            let mut tw = 0usize;
            for ch in rest.chars() {
                use unicode_width::UnicodeWidthChar;
                let cw = ch.width().unwrap_or(1).max(1);
                if tw + cw > limit {
                    break;
                }
                tw += cw;
                take_b += ch.len_utf8();
            }
            if take_b == 0 {
                // 폭 0/극단 — 무한루프 방지로 최소 한 글자는 넘긴다.
                take_b = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                if take_b == 0 {
                    break;
                }
            }
            let full = lines.len() + 1 >= max_lines;
            lines.push(rest[..take_b].to_string());
            rest = &rest[take_b..];
            if full {
                return (lines, true);
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    (lines, false)
}

/// 접힌 팀메시지를 학생색으로 전개(스냅샷 전용, 원본 그리드 무손상) — 본문이
/// 있으면 그 행을 "@ 이름❯ 본문"으로 갈아끼우고, **아래 blank 행이 있는 만큼
/// 줄바꿈으로 이어 쓴다**(거노: 한 줄 말줄임 말고 펼쳐서). 그리드는 reflow 가
/// 안 되니 빈 행 너머로 남는 본문은 '…' — 전문은 hover 말풍선이 담당. 다음
/// 항목과의 구분 blank 1행은 남기고, 뷰포트 바닥까지 전부 빈 경우엔 끝까지
/// 쓴다. 본문이 없으면 원문 글자에 색만. 와이드 글리프는 글자 + ' ' 스페이서
/// 2칸(배너 타이틀 치환과 같은 composed 경로 실측).
fn expand_teammate_message(
    rows: &mut [Vec<GridCell>],
    r: usize,
    start: usize,
    sender: &str,
    body: Option<&str>,
    accent: [u8; 4],
) -> Option<usize> {
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    let Some(body) = body else {
        for c in rows[r].iter_mut() {
            if c.ch != ' ' && c.ch != '\0' {
                c.fg = fg.clone();
            }
        }
        return None;
    };
    let cols = rows[r].len();
    if start >= cols || cols == 0 {
        return None;
    }
    let style = rows[r][start].clone();
    let blank_run = rows[r + 1..].iter().take_while(|w| row_is_blank(w)).count();
    let usable = if r + 1 + blank_run >= rows.len() {
        blank_run
    } else {
        blank_run.saturating_sub(1)
    };
    // 발신자가 배정 학생이면 이름 텍스트 대신 프사(bust) — tell 렌더와 같은 시각
    // 언어(거노 2026-07-27: SendMessage 도 학생 테마로). 프사는 첫 줄 왼쪽 여백
    // 2칸에 얹으므로(호출측 이미지 패스) 헤더는 그만큼 비운다 — 그 폭이 곧 이어
    // 쓰는 줄의 들여쓰기(indent = start+2)라 본문 좌측이 한 줄로 선다.
    let face_slug = teammate_sender_slug(sender);
    let head_start = start;
    let header = if face_slug.is_some() {
        "  ".to_string()
    } else {
        format!("@ {sender}❯ ")
    };
    let indent = start + 2;
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let (lines, truncated) = wrap_body_cells(
        &flat,
        cols.saturating_sub(head_start + cell_width(&header)),
        cols.saturating_sub(indent),
        1 + usable,
    );
    // 행 하나에 텍스트를 칠하는 공용 페인터 — 다음 칸 index 를 돌려준다.
    let put_line = |row: &mut [GridCell], mut w: usize, text: &str, bold: bool| -> usize {
        use unicode_width::UnicodeWidthChar;
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(1).max(1);
            if w + cw > row.len() {
                break;
            }
            let mut cell = style.clone();
            cell.ch = ch;
            cell.fg = fg.clone();
            cell.bold = bold;
            row[w] = cell;
            if cw == 2 {
                let mut sp = style.clone();
                sp.ch = ' ';
                sp.fg = fg.clone();
                sp.bold = bold;
                row[w + 1] = sp;
            }
            w += cw;
        }
        w
    };
    let ellipsis = |row: &mut [GridCell], w: usize| {
        let p = w.min(row.len() - 1);
        let mut cell = style.clone();
        cell.ch = '…';
        cell.fg = fg.clone();
        row[p] = cell;
        p + 1
    };
    let old_end = rows[r]
        .iter()
        .rposition(|c| c.ch != ' ' && c.ch != '\0')
        .map(|p| p + 1)
        .unwrap_or(0);
    // 프사 자리(start..head_start)는 비워 둔다 — 원문 "› Message from @…" 잔재가
    // 프사 뒤로 비쳐 보이면 안 된다.
    for c in rows[r][start..head_start.min(cols)].iter_mut() {
        *c = GridCell::blank();
    }
    let mut w = put_line(&mut rows[r], head_start, &header, true);
    if let Some(first) = lines.first() {
        w = put_line(&mut rows[r], w, first, false);
    }
    if lines.len() == 1 && truncated {
        w = ellipsis(&mut rows[r], w);
    }
    // 새 텍스트가 원문("› Message from @…")보다 짧으면 잔재를 지운다.
    for c in rows[r][w..old_end.max(w)].iter_mut() {
        *c = GridCell::blank();
    }
    for (i, line) in lines.iter().enumerate().skip(1) {
        let row = &mut rows[r + i];
        let w = put_line(row, indent, line, false);
        if i == lines.len() - 1 && truncated {
            ellipsis(row, w);
        }
    }
    face_slug.map(|_| start)
}

/// 사용자 override 학생 애셋의 최대 변 길이. 렌더가 슬롯에 contain-fit 하므로
/// 정확한 규격 강제는 불필요 — 사용자가 넣은 초고해상도 원본이 VRAM 을 잡아먹는
/// 것만 방어적으로 막는다(번들 기본 도트는 이미 이 아래라 무영향).
const MAX_STUDENT_EDGE: u32 = 512;

/// 과대 이미지만 contain 다운스케일(종횡비 유지). 그 외엔 원본 그대로.
fn downscale_student(img: image::DynamicImage) -> image::DynamicImage {
    if img.width() > MAX_STUDENT_EDGE || img.height() > MAX_STUDENT_EDGE {
        img.resize(
            MAX_STUDENT_EDGE,
            MAX_STUDENT_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    }
}

/// `~/.config/kasaterm/students/<filename>` 을 RGBA 로 읽는다(프사·로고처럼
/// 단일 이미지용). 파일/디렉토리가 없으면 None → 호출측이 번들 기본으로 폴백.
fn user_asset_rgba(filename: &str) -> Option<(Vec<u8>, u32, u32)> {
    user_asset_rgba_in(&crate::socket::students_dir()?, filename)
}

/// dir 주입 버전(테스트용) — students_dir 해석과 분리해 env 없이 검증한다.
fn user_asset_rgba_in(dir: &std::path::Path, filename: &str) -> Option<(Vec<u8>, u32, u32)> {
    let img = downscale_student(image::open(dir.join(filename)).ok()?);
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

/// 한 캐릭터·모션의 사용자 override 스프라이트 프레임 전부를 RgbaImage 로 연다.
/// 프레임이 **하나라도** 없으면 None — 부분 교체(일부만 사용자·일부는 번들)는
/// 애니가 튀므로 all-or-nothing 으로 전체 폴백시킨다.
fn user_sprite_images(slug: &str, motion: &str) -> Option<Vec<image::RgbaImage>> {
    let dir = crate::socket::students_dir()?;
    let n = if motion == "walk" {
        STUDENT_WALK_FRAMES
    } else {
        STUDENT_IDLE_FRAMES
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // 번들 파일명 규약과 동일: idle 만 무접미(`slug-N`), 나머지 모션은
        // `slug-<motion>-N`. walk 외 모션이 idle 파일명으로 새면 override 시
        // wave/cheer 가 idle 프레임으로 둔갑한다.
        let fname = if motion == "idle" {
            format!("{slug}-{i}.png")
        } else {
            format!("{slug}-{motion}-{i}.png")
        };
        let img = downscale_student(image::open(dir.join(&fname)).ok()?);
        out.push(img.to_rgba8());
    }
    Some(out)
}

/// 모션 이름 → GPU 텍스처 캐시 키 접두. idle 은 "f"(기존 배너 캐시와 호환),
/// 나머지는 모션 이름 그대로. 프레임 세트는 캐릭터×접두당 1회만 업로드된다.
fn sprite_key_prefix(motion: &str) -> &'static str {
    match motion {
        "idle" => "f",
        "wave" => "wave",
        "cheer" => "cheer",
        "walk" => "walk",
        _ => "f",
    }
}

/// 캐릭터 슬러그 + 모션 → 컴파일타임 내장 도트 프레임(arona-ui 스프라이트의
/// idle/wave/cheer 0..3 · walk-east 0..5). idle=대기, wave=승인 대기(한 팔
/// 인사), cheer=턴 완료(양팔 만세), walk=working(제자리 걸음).
fn student_sprite_png(slug: &str, motion: &str) -> Option<&'static [&'static [u8]]> {
    // idle/wave/cheer 공통 4프레임 — 파일명 접미사(""·"-wave"·"-cheer")만 다르다.
    macro_rules! frames4 {
        ($n:literal, $m:literal) => {{
            const F: [&[u8]; STUDENT_IDLE_FRAMES] = [
                include_bytes!(concat!("../assets/students/", $n, $m, "-0.png")),
                include_bytes!(concat!("../assets/students/", $n, $m, "-1.png")),
                include_bytes!(concat!("../assets/students/", $n, $m, "-2.png")),
                include_bytes!(concat!("../assets/students/", $n, $m, "-3.png")),
            ];
            &F[..]
        }};
    }
    macro_rules! walk {
        ($n:literal) => {{
            const F: [&[u8]; STUDENT_WALK_FRAMES] = [
                include_bytes!(concat!("../assets/students/", $n, "-walk-0.png")),
                include_bytes!(concat!("../assets/students/", $n, "-walk-1.png")),
                include_bytes!(concat!("../assets/students/", $n, "-walk-2.png")),
                include_bytes!(concat!("../assets/students/", $n, "-walk-3.png")),
                include_bytes!(concat!("../assets/students/", $n, "-walk-4.png")),
                include_bytes!(concat!("../assets/students/", $n, "-walk-5.png")),
            ];
            &F[..]
        }};
    }
    macro_rules! student {
        ($n:literal) => {
            match motion {
                "idle" => frames4!($n, ""),
                "wave" => frames4!($n, "-wave"),
                "cheer" => frames4!($n, "-cheer"),
                "walk" => walk!($n),
                _ => return None,
            }
        };
    }
    Some(match slug {
        "arona" => student!("arona"),
        "prana" => student!("prana"),
        "midori" => student!("midori"),
        "momoi" => student!("momoi"),
        "yuzu" => student!("yuzu"),
        "arisu" => student!("arisu"),
        "yuuka" => student!("yuuka"),
        "shiroko" => student!("shiroko"),
        "hoshino" => student!("hoshino"),
        "koharu" => student!("koharu"),
        "himari" => student!("himari"),
        "aru" => student!("aru"),
        _ => return None,
    })
}

/// 모션 프레임들을 RGBA로 디코딩하고 투명 여백을 잘라낸다. 크롭은 전 프레임
/// **합집합** 알파 bbox 하나로 — 프레임별 bbox로 자르면 애니의 미세한
/// 키 차이가 contain-fit 배율 차이로 증폭돼 캐릭터가 들썩인다.
/// GPU 텍스처 캐시(`has_image`) 미스 시에만 호출되므로 (캐릭터,모션)당 1회.
fn student_sprite_frames(slug: &str, motion: &str) -> Option<Vec<(Vec<u8>, u32, u32)>> {
    // 사용자 override(students_dir) 전 프레임이 있으면 그걸, 없으면 번들 내장.
    let decoded: Vec<image::RgbaImage> = match user_sprite_images(slug, motion) {
        Some(imgs) => imgs,
        None => {
            let frames = student_sprite_png(slug, motion)?;
            let d: Vec<_> = frames
                .iter()
                .filter_map(|b| image::load_from_memory(b).ok().map(|i| i.to_rgba8()))
                .collect();
            if d.len() != frames.len() {
                return None;
            }
            d
        }
    };
    let (w, h) = decoded[0].dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for img in &decoded {
        if img.dimensions() != (w, h) {
            return None;
        }
        for (x, y, p) in img.enumerate_pixels() {
            if p[3] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return None; // 전부 투명한 이미지
    }
    Some(
        decoded
            .iter()
            .map(|img| {
                let c = image::imageops::crop_imm(img, x0, y0, x1 - x0 + 1, y1 - y0 + 1)
                    .to_image();
                let (cw, ch) = c.dimensions();
                (c.into_raw(), cw, ch)
            })
            .collect(),
    )
}

/// 캐릭터 슬러그 → statusline 프사 PNG(웹뷰 bust 를 96×96 contain-리사이즈한
/// 정사각 상반신, 컴파일타임 내장).
fn student_profile_png(slug: &str) -> Option<&'static [u8]> {
    Some(match slug {
        "arona" => include_bytes!("../assets/students/arona-profile.png"),
        "prana" => include_bytes!("../assets/students/prana-profile.png"),
        "midori" => include_bytes!("../assets/students/midori-profile.png"),
        "momoi" => include_bytes!("../assets/students/momoi-profile.png"),
        "yuzu" => include_bytes!("../assets/students/yuzu-profile.png"),
        "arisu" => include_bytes!("../assets/students/arisu-profile.png"),
        "yuuka" => include_bytes!("../assets/students/yuuka-profile.png"),
        "shiroko" => include_bytes!("../assets/students/shiroko-profile.png"),
        "hoshino" => include_bytes!("../assets/students/hoshino-profile.png"),
        "koharu" => include_bytes!("../assets/students/koharu-profile.png"),
        "himari" => include_bytes!("../assets/students/himari-profile.png"),
        "aru" => include_bytes!("../assets/students/aru-profile.png"),
        _ => return None,
    })
}

/// 프사 PNG → RGBA. GPU 텍스처 캐시(`has_image`) 미스 시에만 호출되므로
/// 캐릭터당 1회 디코딩. 이미 얼굴에 맞춰 잘린 에셋이라 bbox 크롭은 불필요.
/// 프사 hover 확대 팝업의 좌상단 위치 — 프사 가로 중심 위로 띄우되 창 좌우/상단
/// 경계 안으로 클램프한다. statusline 프사는 창 하단이라 팝업을 위로(음의 y) 낸다.
/// 좁은 창(팝업 변보다 폭이 작을 때)에서도 x 가 6px 밑으로 안 내려가게 한다.
fn face_popup_pos(fx: f32, fw: f32, fy: f32, pop: f32, win_w: f32, title_h: f32) -> (f32, f32) {
    let px = (fx + fw / 2.0 - pop / 2.0).clamp(6.0, (win_w - pop - 6.0).max(6.0));
    let py = (fy - pop - 8.0).max(title_h + 6.0);
    (px, py)
}

fn student_profile_rgba(slug: &str) -> Option<(Vec<u8>, u32, u32)> {
    if let Some(r) = user_asset_rgba(&format!("{slug}-profile.png")) {
        return Some(r);
    }
    let img = image::load_from_memory(student_profile_png(slug)?).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// 테마 전환이 걷히는 데 걸리는 시간. 눈이 "무엇이 바뀌었나"를 읽을 만큼은 길고,
/// 다음 클릭을 기다리게 할 만큼 길지는 않은 자리.
pub(crate) const THEME_FX_SECS: f32 = 0.34;

/// 픽셀 블록 한 변(논리 px). 셀보다 크게 잡아야 "큰 픽셀"로 읽힌다 — 셀 크기에
/// 맞추면 그냥 부드러운 페이드처럼 보이고, 블록 수도 네 배가 된다.
const FX_BLOCK: f32 = 26.0;

/// 테마 전환 디졸브 — 옛 배경색 블록이 가운데서 바깥으로 걷힌다.
///
/// 블록마다 사라질 시점을 「중심에서의 거리」와 「좌표 해시」로 섞어 정한다. 거리만
/// 쓰면 매끈한 원이 퍼져 픽셀이라는 느낌이 안 나고, 해시만 쓰면 방향 없이 지글거리는
/// TV 노이즈가 된다. 둘을 섞어야 퍼지는 물결의 **가장자리가 픽셀로 부서진다**.
fn paint_theme_dissolve(g: &mut gpu::GpuRenderer, t: f32, old_bg: [u8; 4], w: f32, h: f32) {
    let (cx, cy) = (w * 0.5, h * 0.5);
    let max_d = (cx * cx + cy * cy).sqrt().max(1.0);
    let cols = (w / FX_BLOCK).ceil() as i32;
    let rows = (h / FX_BLOCK).ceil() as i32;
    for j in 0..rows {
        for i in 0..cols {
            let (bx, by) = (i as f32 * FX_BLOCK, j as f32 * FX_BLOCK);
            let d = ((bx + FX_BLOCK * 0.5 - cx).powi(2) + (by + FX_BLOCK * 0.5 - cy).powi(2))
                .sqrt()
                / max_d;
            // 좌표를 섞어 0..1 로 흩는 값. 난수를 안 쓰는 건 프레임마다 같은 답이
            // 나와야 블록이 한 번 사라진 뒤 다시 나타나지 않기 때문이다.
            let hash = {
                let n = (i.wrapping_mul(73_856_093) ^ j.wrapping_mul(19_349_663)) as u32;
                (n >> 8 & 0xFFFF) as f32 / 65535.0
            };
            // 거리 70% + 흩뿌림 30%. 앞의 +0.2 는 **첫 프레임에 이미 중앙이 뚫려
            // 있게** 한다 — 0 에서 시작하면 한 프레임 동안 화면 전체가 옛 배경
            // 단색이라 "퍼진다"가 아니라 "깜빡 꺼졌다 켜진다"로 읽힌다.
            if t * 1.1 + 0.2 > d * 0.7 + hash * 0.3 {
                continue;
            }
            round_rect(g, bx, by, FX_BLOCK, FX_BLOCK, 0.0, old_bg);
        }
    }
}

/// 캐릭터 이름 자리에 그 학생의 얼굴을 그린다 — 없는 캐릭터면 아무것도 안 그리고
/// `false` 를 돌려 부르는 쪽이 색 점으로 되돌아가게 한다.
///
/// 업로드는 캐릭터당 한 번(`has_image` 미스일 때만)이라 프레임마다 불러도 싸다.
/// statusline·Info·tell 렌더가 같은 키를 공유하니 어디서 처음 그리든 나머지는
/// 캐시를 탄다.
pub(crate) fn draw_student_face(
    g: &mut gpu::GpuRenderer,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
) -> bool {
    let Some(slug) = theme::character_slug(name) else {
        return false;
    };
    let key = format!("student:{slug}:profile");
    if !g.has_image(&key) {
        let Some((rgba, w, h)) = student_profile_rgba(slug) else {
            return false;
        };
        g.upload_image(&key, &rgba, w, h);
    }
    g.queue_image_above(&key, x, y, size, size);
    true
}

/// 0..1 을 오가는 부드러운 호흡 — `period` 초에 한 번 왕복한다.
///
/// 켰다 끄는 깜빡임과 갈리는 건 **가장자리 시야**에서다. 밝기가 뚝 끊기면 눈이
/// 그쪽으로 끌려가 하던 일을 놓치는데, 이어지면 있다는 것만 알고 지나칠 수 있다.
fn breathe(t: f32, period: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::TAU * t / period.max(0.001)).cos()
}

/// 프로세스 시작 기준 단조증가 초 — 시간으로 도는 그림(로딩바 스윕, idle gif)이
/// 전부 같은 시계를 본다. 펌프가 도는 동안 매 프레임 갱신된다.
pub(crate) fn anim_phase_secs() -> f32 {
    static EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    EPOCH.elapsed().as_secs_f32()
}

fn student_idle_gif(slug: &str) -> Option<&'static [u8]> {
    Some(match slug {
        "arona" => include_bytes!("../assets/students/arona-idle.gif"),
        "prana" => include_bytes!("../assets/students/prana-idle.gif"),
        "midori" => include_bytes!("../assets/students/midori-idle.gif"),
        "momoi" => include_bytes!("../assets/students/momoi-idle.gif"),
        "yuzu" => include_bytes!("../assets/students/yuzu-idle.gif"),
        "arisu" => include_bytes!("../assets/students/arisu-idle.gif"),
        "yuuka" => include_bytes!("../assets/students/yuuka-idle.gif"),
        "shiroko" => include_bytes!("../assets/students/shiroko-idle.gif"),
        "hoshino" => include_bytes!("../assets/students/hoshino-idle.gif"),
        "koharu" => include_bytes!("../assets/students/koharu-idle.gif"),
        "himari" => include_bytes!("../assets/students/himari-idle.gif"),
        "aru" => include_bytes!("../assets/students/aru-idle.gif"),
        _ => return None,
    })
}

/// 투명하지 않은 픽셀이 차지하는 사각 `(x, y, w, h)`. 빈 그림이면 전체.
fn alpha_bbox(img: &image::RgbaImage) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[3] > 8 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == u32::MAX {
        return (0, 0, img.width(), img.height());
    }
    (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

/// 한 캐릭터의 idle 애니메이션 — 프레임 RGBA 와 각 프레임이 머무는 ms.
struct IdleAnim {
    frames: Vec<(Vec<u8>, u32, u32)>,
    delays_ms: Vec<u32>,
    total_ms: u32,
}

/// idle.gif → 프레임 배열. 캐릭터당 **한 번만** 디코딩해 캐시한다.
///
/// 캔버스는 256² 인데 캐릭터는 그 안에 94×208 짜리 전신 도트로 서 있다. 그걸
/// 통째로 16px 칸에 넣으면 캐릭터 폭이 6px 이 되어 누구인지 못 알아본다 — 그래서
/// 알파 bbox 를 잡아 **어깨 위 정사각**만 도려낸다. 정적 프사가 이미 얼굴에 맞춰
/// 잘린 에셋인 것과 같은 이유고, 덕분에 두 경로가 같은 크기로 읽힌다.
///
/// 픽셀 아트라 축소는 Nearest 로 — 보간을 쓰면 도트가 뭉개져 흐려진다.
fn student_idle_anim(slug: &str) -> Option<std::sync::Arc<IdleAnim>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, Option<Arc<IdleAnim>>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(slug).cloned()) {
        return hit;
    }
    let built = (|| {
        use image::AnimationDecoder;
        let bytes = student_idle_gif(slug)?;
        let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
        let mut frames = Vec::new();
        let mut delays_ms = Vec::new();
        let mut crop: Option<(u32, u32, u32)> = None;
        for f in dec.into_frames().collect_frames().ok()? {
            let (num, den) = f.delay().numer_denom_ms();
            // 0ms 프레임은 브라우저 관행대로 100ms 로 — 그대로 두면 루프 길이가
            // 0 이 되어 나눗셈이 터진다.
            let ms = if den == 0 { 100 } else { (num / den.max(1)).max(20) };
            let mut buf = f.into_buffer();
            // 자를 자리는 **첫 프레임에서 한 번만** 잡는다. 프레임마다 다시 재면
            // 팔이 오르내릴 때 사각이 따라 흔들려 얼굴이 칸 안에서 덜컹거린다.
            let (cx, cy, cs) = *crop.get_or_insert_with(|| {
                let (bx, by, bw, _bh) = alpha_bbox(&buf);
                (bx, by, bw.max(1))
            });
            let cs = cs.min(buf.width().saturating_sub(cx)).min(buf.height().saturating_sub(cy));
            if cs == 0 {
                return None;
            }
            let face = image::imageops::crop(&mut buf, cx, cy, cs, cs).to_image();
            // 쓰는 자리가 16~22px 이라 32² 면 충분하고, 프레임 수만큼 곱해도 가볍다.
            const OUT: u32 = 32;
            let small =
                image::imageops::resize(&face, OUT, OUT, image::imageops::FilterType::Nearest);
            frames.push((small.into_raw(), OUT, OUT));
            delays_ms.push(ms);
        }
        (!frames.is_empty()).then(|| {
            let total_ms = delays_ms.iter().sum::<u32>().max(1);
            Arc::new(IdleAnim { frames, delays_ms, total_ms })
        })
    })();
    if let Ok(mut c) = cache.lock() {
        c.insert(slug.to_string(), built.clone());
    }
    built
}

/// 캐릭터 자리에 **움직이는** 얼굴을 그린다 — `phase` 는 앱이 켜진 뒤 흐른 초.
///
/// gif 가 없는 캐릭터는 정지 프사로 되돌아간다(`draw_student_face`). 프레임마다
/// 텍스처 키가 갈리므로 업로드는 프레임당 한 번뿐이고, 이후로는 큐잉만 한다.
pub(crate) fn draw_student_face_anim(
    g: &mut gpu::GpuRenderer,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    phase: f32,
) -> bool {
    let Some(slug) = theme::character_slug(name) else {
        return false;
    };
    let Some(anim) = student_idle_anim(slug) else {
        return draw_student_face(g, name, x, y, size);
    };
    let mut at = ((phase * 1000.0) as u32) % anim.total_ms;
    let mut idx = 0;
    for (i, d) in anim.delays_ms.iter().enumerate() {
        if at < *d {
            idx = i;
            break;
        }
        at -= d;
    }
    let key = format!("student:{slug}:idle:{idx}");
    if !g.has_image(&key) {
        let (rgba, w, h) = &anim.frames[idx];
        g.upload_image(&key, rgba, *w, *h);
    }
    g.queue_image_above(&key, x, y, size, size);
    true
}

/// SCHALE 로고 PNG → RGBA. agents 뷰 캐시 미스 시 1회 디코딩. 사용자
/// override(students_dir/schale-logo.png) 우선, 없으면 include_bytes 번들.
fn schale_logo_rgba() -> Option<(Vec<u8>, u32, u32)> {
    if let Some(r) = user_asset_rgba("schale-logo.png") {
        return Some(r);
    }
    let img = image::load_from_memory(include_bytes!("../assets/students/schale-logo.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// agents/resume 피커 배경(교실). 셀 뒤에 깔리므로 텍스트 대비 확보를 위해 로드
/// 시 밝기를 낮춘다 — 원본 에셋은 보존, 여기서만 RGB × DIM. user override
/// (students_dir/schale-classroom.png) 우선, 없으면 include_bytes 번들.
fn schale_classroom_rgba() -> Option<(Vec<u8>, u32, u32)> {
    const DIM: f32 = 0.40;
    let mut img = user_asset_rgba("schale-classroom.png")
        .and_then(|(rgba, w, h)| image::RgbaImage::from_raw(w, h, rgba))
        .or_else(|| {
            image::load_from_memory(include_bytes!("../assets/schale-classroom.png"))
                .ok()
                .map(|i| i.to_rgba8())
        })?;
    for px in img.pixels_mut() {
        px[0] = (px[0] as f32 * DIM) as u8;
        px[1] = (px[1] as f32 * DIM) as u8;
        px[2] = (px[2] as f32 * DIM) as u8;
    }
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// agents 목록 뷰에서 SCHALE 로고를 얹을 위치 — "Claude Code" 헤더 행을 찾아 그
/// 왼쪽 여백(logo_cols + 2칸 갭 앞)의 top-left (row, col)을 돌려준다. Clawd 블록아트가
/// 없는 목록 뷰에서 startup 배너의 Clawd 자리와 같은 쪽(헤더 왼쪽)에 앵커한다.
fn find_agents_header_anchor(rows: &[Vec<GridCell>], logo_cols: usize) -> Option<(usize, usize)> {
    for (r, row) in rows.iter().enumerate() {
        let line: String = row.iter().map(|c| c.ch).collect();
        if let Some(idx) = line.find("Claude Code") {
            return Some((r, idx.saturating_sub(logo_cols + 2)));
        }
    }
    None
}

/// claude /resume 피커 행의 학생 태그(` · #학생이름`) 탐지 — (태그 '#' col,
/// 이름 끝 col, 학생 slug). resume_visibility 스위퍼가 세션 설명줄 끝에 스탬프한
/// 태그가 앵커다. 요건 3중: ① '#' 바로 앞이 " ·"(피커 구분자) ② 그 앞 어딘가
/// 또 다른 '·'(설명줄은 "날짜 · 크기 · #태그" 꼴로 '·' 2개 이상) ③ '#' 뒤 연속
/// 텍스트가 로스터 이름 — 이라 일반 터미널 출력 오탐은 사실상 없다. PR 번호
/// (`repo#12`) 같은 다른 '#' 는 이름 검증에서 떨어지므로 행의 모든 '#' 후보를
/// 순서대로 시도한다.
/// 그리드 행 → (스페이서를 흡수한 실제 텍스트, 각 텍스트 char 의 셀 col). 와이드
/// 문자(한글, ≥U+1100) 다음의 스페이서 셀('\0', 또는 alacritty composed 의 직후
/// ' ')을 소비해, 캐시된 세션 name 을 셀 텍스트에서 그대로 substring 검색할 수 있게
/// 한다(picker_student_tag 와 동일한 wide 스페이서 규칙). agents 뷰 세션 행 칩용.
fn row_text_cells(row: &[GridCell]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut cols = Vec::new();
    let mut spacer_pending = false;
    for (i, cell) in row.iter().enumerate() {
        match cell.ch {
            '\0' => spacer_pending = false,
            ' ' if spacer_pending => spacer_pending = false,
            ch => {
                text.push(ch);
                cols.push(i);
                spacer_pending = (ch as u32) >= 0x1100;
            }
        }
    }
    (text, cols)
}

fn picker_student_tag(row: &[GridCell]) -> Option<(usize, usize, &'static str)> {
    for (c0, _) in row.iter().enumerate().filter(|(_, c)| c.ch == '#') {
        if c0 < 2 || row[c0 - 1].ch != ' ' || row[c0 - 2].ch != '·' {
            continue;
        }
        if !row[..c0 - 2].iter().any(|c| c.ch == '·') {
            continue;
        }
        let mut name = String::new();
        let mut end = c0;
        // 와이드 문자(한글) 다음 한 칸은 스페이서 셀 — 그리드 경로에 따라 '\0'
        // 또는 ' ' 로 온다(alacritty composed 는 ' ', 실측). 직전 문자가 와이드일
        // 때만 스페이서로 소비하고, 그 외 공백은 이름 종료.
        let mut spacer_pending = false;
        for (i, cell) in row.iter().enumerate().skip(c0 + 1) {
            match cell.ch {
                ' ' | '\0' if spacer_pending => {
                    end = i;
                    spacer_pending = false;
                }
                ' ' | '\0' => break,
                ch => {
                    name.push(ch);
                    end = i;
                    spacer_pending = (ch as u32) >= 0x1100;
                    if name.chars().count() > 6 {
                        break; // 로스터 이름은 최장 3자 — 과도하면 태그 아님
                    }
                }
            }
        }
        if let Some(slug) = theme::character_slug(&name) {
            return Some((c0, end, slug));
        }
    }
    None
}

/// claude 입력박스 위 "── 세션명 ──" 구분선의 이름 구간 위치(거노: rename 아웃라인).
/// 하단 10행에서 대시가 지배적이고 비-대시 텍스트 섬이 있는 rule 행을 찾아, **좌우 대시
/// 런 사이**(양옆 공백 포함)의 (row, c0, c1)을 돌려준다. 이름 글자 셀이 아니라 대시 경계로
/// 잡아야 한글 같은 와이드(2셀) 문자의 둘째 셀까지 박스 안에 정확히 들어온다(거노: 칸 안맞음).
/// 순수 '─' rule·statusline·입력행은 걸러진다.
fn find_titled_rule(rows: &[Vec<GridCell>]) -> Option<(usize, usize, usize)> {
    let n = rows.len();
    for r in (n.saturating_sub(10)..n).rev() {
        let row = &rows[r];
        let dashes = row.iter().filter(|c| c.ch == '─').count();
        if dashes < row.len() / 2 {
            continue;
        }
        // 이름 섬이 없는 순수 '─' rule(입력박스 바닥 테두리 등)은 건너뛴다 — `?` 로 함수를
        // 끝내면 그 아래 순수 rule 이 세션명 줄보다 먼저 걸려 아웃라인이 통째 사라진다(거노).
        // box-drawing 문자(╭╮╰╯│…, U+2500-257F) 전체를 이름에서 제외 — 둥근 입력박스
        // 테두리 행(╭────╮)의 모서리가 이름 섬으로 오탐되어 행 전체에 사각형이 그려졌다(거노).
        let is_name = |c: &GridCell| {
            !matches!(c.ch, ' ' | '\0') && !('\u{2500}'..='\u{257F}').contains(&c.ch)
        };
        let Some(first) = row.iter().position(&is_name) else { continue };
        let Some(last) = row.iter().rposition(&is_name) else { continue };
        // teammate 칩(`──── @이름 ──`)은 claude 네이티브가 그리는 agent 배지지
        // 세션명이 아니다 — 아웃라인을 두르면 칩에 네모칸이 생긴다(거노 2026-07-27).
        if row[first].ch == '@' {
            continue;
        }
        // 이름 왼쪽의 마지막 '─' 다음 셀 = c0(선행 공백 포함), 오른쪽 첫 '─' 이전 셀 = c1
        // (와이드 문자 둘째 셀·후행 공백 포함). 대시 런이 없으면 이름 셀로 폴백.
        let c0 = row[..first].iter().rposition(|c| c.ch == '─').map_or(first, |i| i + 1);
        let c1 = row[last + 1..]
            .iter()
            .position(|c| c.ch == '─')
            .map_or(last, |i| (last + 1 + i).saturating_sub(1));
        return Some((r, c0, c1));
    }
    None
}

/// Clawd 시작 배너 감지. 결정행(몸통 2행째)의 9글리프 시퀀스를 찾고 바로
/// 윗행의 머리 7글리프로 확정한다 — 이 조합은 일반 텍스트에서 사실상
/// 나올 수 없다. 스크롤로 배너가 뷰포트 가장자리에 걸치면 보이는 행만으로
/// 감지한다(거노: 스크롤 살짝 내리면 Clawd 원본이 노출) — 위로 잘리면
/// top_row 가 음수, 아래로 잘리면 박스가 화면 밖까지 이어진다. 호출측은
/// blank 범위를 스냅샷 안으로 클램프하고 스프라이트를 pane 세로로 클립할 것.
/// 반환: 배너 박스의 (top_row, left_col) 목록.
/// 행 스캔은 첫 글리프 비교로 즉시 탈락하므로 프레임당 비용 미미.
/// 감지된 Claude Code 스크롤 sticky prompt 한 건(셀 좌표 + 보이는 텍스트).
pub(crate) struct StickyPrompt {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize, // exclusive
    pub text: String,
}

thread_local! {
    /// 이번 프레임에 그린 sticky pill 들의 (소속 pane id, 클릭 히트 rect(logical
    /// px), 보이는 텍스트). render 가 매 프레임 새로 채우고, mouse handler 는
    /// 클릭 판정에, seek 진행은 "지금 그 pane 의 sticky 텍스트"를 읽는 데 쓴다.
    /// struct App 무접촉(병렬 작업 규칙) — GUI 단일 스레드라 thread_local 로 충분.
    pub(crate) static STICKY_PILLS:
        std::cell::RefCell<Vec<(String, (f32, f32, f32, f32), String)>> =
        std::cell::RefCell::new(Vec::new());

    /// 진행 중인 sticky 클릭 seek. 클릭이 target(그 프롬프트 첫 줄 텍스트)을 잡고,
    /// about_to_wait 가 매 틱 wheel-up 한 노치씩 보내 화면을 관찰한다 — target 이
    /// 뷰포트로 들어와 sticky 텍스트가 바뀌거나(또는 최상단 도달로 사라지면) 멈춘다.
    pub(crate) static STICKY_SEEK: std::cell::RefCell<Option<StickySeek>> =
        std::cell::RefCell::new(None);
}

/// 클릭한 sticky 프롬프트를 화면으로 끌어오는 seek 상태(struct App 밖 — 무접촉).
pub(crate) struct StickySeek {
    pub pane_id: String,
    pub target: String,
    /// wheel SGR 를 쏠 pane-local 셀(클릭 지점) — 노치마다 재사용.
    pub cell: (u16, u16),
    pub last_send: std::time::Instant,
    pub sent: u32,
}

/// 노치 간 최소 간격 — 33ms 펌프 틱보다 짧게 잡아 틱마다 한 노치가 나가되,
/// PTY 리페인트가 반영될 시간은 준다(로컬 리페인트는 보통 이보다 빠름).
const STICKY_SEEK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
/// 폭주 방지 상한(정상 종료가 먼저 걸린다). 500 노치면 어떤 화면이든 최상단 도달.
const STICKY_SEEK_MAX: u32 = 500;

/// seek 이 진행 중인가 — about_to_wait 의 30fps 펌프 게이트.
pub(crate) fn sticky_seek_active() -> bool {
    STICKY_SEEK.with(|s| s.borrow().is_some())
}

/// 이번 프레임 그 pane 에 그려진 sticky pill 텍스트(없으면 None = sticky 사라짐).
fn sticky_text_for(pane_id: &str) -> Option<String> {
    STICKY_PILLS.with(|s| {
        s.borrow()
            .iter()
            .find(|(id, _, _)| id == pane_id)
            .map(|(_, _, t)| t.clone())
    })
}

/// sticky 클릭 → seek 시작. target 은 클릭한 pill 텍스트, cell 은 wheel 을 쏠 위치.
pub(crate) fn begin_sticky_seek(pane_id: String, target: String, cell: (u16, u16)) {
    let now = std::time::Instant::now();
    STICKY_SEEK.with(|s| {
        *s.borrow_mut() = Some(StickySeek {
            pane_id,
            target,
            cell,
            // 첫 틱에 바로 한 노치 나가게 간격만큼 과거로.
            last_send: now.checked_sub(STICKY_SEEK_INTERVAL).unwrap_or(now),
            sent: 0,
        });
    });
}

/// seek 한 스텝. 다음 노치를 보내야 하면 (pane_id, col, row) 반환, 아니면 None
/// (대기 중이거나 종료). 종료 판정: 현재 sticky 텍스트가 target 과 다르면(타깃이
/// 뷰포트로 들어옴) 또는 없으면(최상단) 완료로 보고 상태를 지운다.
pub(crate) fn sticky_seek_step() -> Option<(String, u16, u16)> {
    let now = std::time::Instant::now();
    STICKY_SEEK.with(|s| {
        let mut b = s.borrow_mut();
        let seek = b.as_mut()?;
        let reached = match sticky_text_for(&seek.pane_id) {
            None => true,
            Some(t) => t != seek.target,
        };
        if reached || seek.sent >= STICKY_SEEK_MAX {
            *b = None;
            return None;
        }
        if now.duration_since(seek.last_send) < STICKY_SEEK_INTERVAL {
            return None; // 직전 노치의 리페인트 대기
        }
        seek.last_send = now;
        seek.sent += 1;
        let (col, row) = seek.cell;
        Some((seek.pane_id.clone(), col, row))
    })
}

/// 저채도·중간 밝기 = "흐릿한 회색" fg. Claude Code 가 dim SGR(2) 대신 회색
/// 전경색으로 sticky 를 흐리게 줄 때를 위한 폴백 판정(dim 플래그와 OR).
fn is_grayish_fg(fg: &kasa_bridge::screen::Color) -> bool {
    use kasa_bridge::screen::Color;
    match fg {
        Color::Idx(8) | Color::Idx(7) => true, // bright black / white-gray
        Color::Rgb(r, g, b) => {
            let (r, g, b) = (*r as i32, *g as i32, *b as i32);
            let mx = r.max(g).max(b);
            let mn = r.min(g).min(b);
            (mx - mn) < 36 && (56..=190).contains(&mx) // 저채도 + 중간 밝기
        }
        _ => false,
    }
}

/// 한 행의 보이는 텍스트 구간 요약: (text, first_col, last_col_excl, 글자수,
/// 흐릿한 글자수). 후행 공백은 텍스트에서 트림한다.
fn sticky_row_span(row: &[GridCell]) -> (String, usize, usize, usize, usize) {
    let mut text = String::new();
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut glyphs = 0usize;
    let mut dim = 0usize;
    for (i, c) in row.iter().enumerate() {
        let visible = c.ch != ' ' && c.ch != '\0';
        if visible {
            if first.is_none() {
                first = Some(i);
            }
            last = i + 1;
            glyphs += 1;
            if c.dim || is_grayish_fg(&c.fg) {
                dim += 1;
            }
        }
        if first.is_some() {
            text.push(if c.ch == '\0' { ' ' } else { c.ch });
        }
    }
    let first = first.unwrap_or(0);
    (text.trim_end().to_string(), first, last, glyphs, dim)
}

/// Claude Code 의 스크롤 sticky prompt 감지. mouse-tracking TUI 라 kasaterm 은
/// 뷰포트 스크롤 여부를 직접 못 안다 — 화면에 "Jump to bottom" 힌트(=위로
/// 스크롤된 상태)가 있을 때만, 최상단의 흐릿한 프롬프트 행을 sticky 로 본다.
/// 이 게이트가 평상시(맨 아래) 오탐을 막는다. `KASATERM_STICKY_DEBUG=1` 이면
/// 게이트 결과와 상단 행 스캔을 stderr 로 흘려 실측 튜닝을 돕는다.
pub(crate) fn find_sticky_prompt(rows: &[Vec<GridCell>]) -> Option<StickyPrompt> {
    let dbg = std::env::var_os("KASATERM_STICKY_DEBUG").is_some();
    // 스크롤 게이트: "jump to bottom" / ("bottom" & "click") 관대 매치.
    let scrolled = rows.iter().any(|r| {
        let s: String = r.iter().map(|c| c.ch).collect::<String>().to_lowercase();
        s.contains("jump to bottom") || (s.contains("bottom") && s.contains("click"))
    });
    if dbg {
        eprintln!("[sticky] scrolled_gate={scrolled} rows={}", rows.len());
    }
    if !scrolled {
        return None;
    }
    // 최상단 몇 행에서 "흐릿한 글자가 우세하고 실제 텍스트가 있는" 행.
    for ri in 0..rows.len().min(3) {
        let (text, first, last, glyphs, dim) = sticky_row_span(&rows[ri]);
        if dbg {
            eprintln!(
                "[sticky] row{ri} glyphs={glyphs} dim={dim} cols={first}..{last} text={:?}",
                text.chars().take(48).collect::<String>()
            );
        }
        if glyphs >= 2 && dim * 2 >= glyphs {
            return Some(StickyPrompt {
                row: ri,
                col_start: first,
                col_end: last,
                text,
            });
        }
    }
    None
}

pub(crate) fn find_clawd_banners(rows: &[Vec<GridCell>]) -> Vec<(isize, usize)> {
    const BODY: [char; 9] = ['▝', '▜', '█', '█', '█', '█', '█', '▛', '▘'];
    const HEAD: [char; 7] = ['▐', '▛', '█', '█', '█', '▜', '▌'];
    // 발 행: 배너 좌단 기준 2칸 들여쓰기 `▘▘ ▝▝`, 양옆은 공백(2.1.212 실측).
    const FEET: [char; 5] = ['▘', '▘', ' ', '▝', '▝'];
    let blank = |cell: &GridCell| matches!(cell.ch, ' ' | '\0');
    let matches_at = |row: &[GridCell], at: usize, pat: &[char]| {
        at + pat.len() <= row.len()
            && pat.iter().enumerate().all(|(i, &p)| row[at + i].ch == p)
    };
    let mut out = Vec::new();
    let n = rows.len();
    for r in 0..n {
        let row = &rows[r];
        let mut c = 0usize;
        while c + BODY.len() <= row.len() {
            if matches_at(row, c, &BODY) {
                if r == 0 {
                    // 몸통이 최상단 행 = 머리가 위로 잘림. 몸통 9글리프
                    // 단독으로도 일반 텍스트 오탐 여지가 사실상 없다.
                    out.push((-1, c));
                    c += BODY.len();
                    continue;
                }
                if matches_at(&rows[r - 1], c + 1, &HEAD) {
                    out.push((r as isize - 1, c));
                    c += BODY.len();
                    continue;
                }
            }
            c += 1;
        }
    }
    // 위로 2행 잘림: 최상단에 발만 남은 경우. 발 글리프는 짧아 양옆
    // 공백(배너 폭 9칸 확보)까지 요구해 오탐을 줄인다.
    if let Some(row) = rows.first() {
        let mut p = 2usize;
        while p + FEET.len() + 2 <= row.len() {
            if matches_at(row, p, &FEET)
                && blank(&row[p - 2])
                && blank(&row[p - 1])
                && blank(&row[p + 5])
                && blank(&row[p + 6])
            {
                out.push((-2, p - 2));
                p += FEET.len();
            } else {
                p += 1;
            }
        }
    }
    // 아래에서 진입: 최하단에 머리만 보이는 경우(몸통·발은 화면 밖).
    // 머리 7글리프 + 양옆 공백. 몸통행이 화면 안에 있으면 위 몸통 스캔이
    // 이미 잡으므로 마지막 행만 본다.
    if let Some(row) = rows.last().filter(|_| n >= 2) {
        let mut p = 1usize;
        while p + HEAD.len() + 1 <= row.len() {
            if matches_at(row, p, &HEAD) && blank(&row[p - 1]) && blank(&row[p + 7]) {
                out.push((n as isize - 1, p - 1));
                p += HEAD.len();
            } else {
                p += 1;
            }
        }
    }
    out
}

/// Clawd 배너 옆 타이틀의 "Claude Code" → pane 학생 이름 — 스냅샷 전용, 원본
/// 그리드 무손상(도트 교체와 같은 원칙). 배너 세로 범위에서 art 오른쪽의
/// "Claude Code" 글자 시퀀스를 찾아 한글 이름(와이드 글리프 + ' ' 스페이서)으로
/// 갈아끼우고, 뒤따르는 버전 텍스트를 이름 바로 뒤로 당긴다. 당겨서 남는 칸은
/// blank — 연속 공백 2칸 너머는 박스형 웰컴 변형의 오른쪽 테두리 영역이라
/// 건드리지 않는다(테두리 열이 밀리면 박스가 깨진다).
fn replace_banner_title(
    rows: &mut [Vec<GridCell>],
    br: isize,
    bc: usize,
    name: &str,
    accent: Option<[u8; 4]>,
) {
    const TITLE: [char; 11] = ['C', 'l', 'a', 'u', 'd', 'e', ' ', 'C', 'o', 'd', 'e'];
    let r0 = br.max(0) as usize;
    let r1 = (br + CLAWD_ROWS as isize).clamp(0, rows.len() as isize) as usize;
    for row in rows[r0..r1].iter_mut() {
        let start = bc + CLAWD_COLS;
        if start >= row.len() {
            continue;
        }
        let Some(tc) = (start..row.len().saturating_sub(TITLE.len() - 1))
            .find(|&c| TITLE.iter().enumerate().all(|(i, &p)| row[c + i].ch == p))
        else {
            continue;
        };
        // 이름 셀: 원 타이틀 스타일(bold 등) 승계, 색만 학생 accent 로 —
        // 테두리·스피너 텍스트와 같은 "이 pane 의 학생" 색 언어.
        let mut style = row[tc].clone();
        if let Some([r, g, b, _]) = accent {
            style.fg = kasa_bridge::screen::Color::Rgb(r, g, b);
        }
        let mut repl: Vec<GridCell> = Vec::with_capacity(TITLE.len());
        for ch in name.chars() {
            let mut cell = style.clone();
            cell.ch = ch;
            repl.push(cell);
            // 와이드 글리프 다음 칸은 스페이서 — composed 경로 실측은 ' '.
            let mut sp = style.clone();
            sp.ch = ' ';
            repl.push(sp);
        }
        if repl.len() > TITLE.len() {
            return; // 로스터 이름은 최대 3자(6칸) — 넘치면 원문 유지
        }
        let mut end = tc + TITLE.len();
        let mut probe = end;
        while probe < row.len() {
            if matches!(row[probe].ch, ' ' | '\0') {
                if probe + 1 >= row.len() || matches!(row[probe + 1].ch, ' ' | '\0') {
                    break;
                }
            } else {
                end = probe + 1;
            }
            probe += 1;
        }
        let tail: Vec<GridCell> = row[tc + TITLE.len()..end].to_vec();
        let mut w = tc;
        for cell in repl.into_iter().chain(tail) {
            row[w] = cell;
            w += 1;
        }
        for cell in row[w..end].iter_mut() {
            *cell = GridCell::blank();
        }
        return; // 타이틀은 배너당 한 줄
    }
}

/// claude 웰컴 배너("Welcome back <user>!") → 배정 학생 인사말. Clawd 아트(=학생
/// 도트로 치환된 자리) 위쪽 박스 안의 "Welcome back " 행을 찾아, 사용자 이름을
/// 추출하고 그 행을 페르소나 인사말로 갈아끼운다(원 볼드 스타일 승계 + 학생 accent
/// 색, 박스 우측 보더 전까지 클립·초과 시 말줄임). async agents launcher 등 웰컴
/// 행이 없는 화면에선 자연 no-op(패시브). 스냅샷 전용 — 원본 그리드 무손상.
fn replace_welcome_greeting(
    rows: &mut [Vec<GridCell>],
    br: isize,
    name: &str,
    accent: Option<[u8; 4]>,
) {
    const PREFIX: [char; 13] =
        ['W', 'e', 'l', 'c', 'o', 'm', 'e', ' ', 'b', 'a', 'c', 'k', ' '];
    // "Welcome back" 은 아트(도트) 바로 위 박스 안, 중앙정렬. 아트 top(br) 기준
    // 위로 최대 4행만 본다. 아트가 위로 잘려(br<0) 웰컴 행이 화면 밖이면 자연 skip.
    let hi = br.clamp(0, rows.len() as isize) as usize;
    let lo = (br - 4).max(0) as usize;
    for r in lo..hi {
        // 불변 스캔: "Welcome back " 위치·이름·박스 우측 한계·원 스타일을 값으로.
        let (wc, excl, user, limit, mut style) = {
            let row = &rows[r];
            let Some(wc) = (0..row.len().saturating_sub(PREFIX.len()))
                .find(|&c| PREFIX.iter().enumerate().all(|(i, &p)| row[c + i].ch == p))
            else {
                continue;
            };
            let name_start = wc + PREFIX.len();
            let Some(excl_rel) = row[name_start..].iter().position(|c| c.ch == '!')
            else {
                continue;
            };
            let excl = name_start + excl_rel;
            if excl <= name_start {
                continue; // 빈 이름("Welcome back !") — 원문 유지
            }
            // 이름 추출: 와이드 글리프 바로 뒤 스페이서(' '/'\0') 셀 1칸을 흡수한다
            // — 단 실제 composed 는 스페이서가 있지만, 스페이서 없이 붙은 그리드
            // (테스트·비정상)에서도 다음 글자를 삼키지 않도록 "다음이 스페이서일
            // 때만" 건너뛴다.
            let mut user = String::new();
            let mut i = name_start;
            while i < excl {
                let ch = row[i].ch;
                if ch != '\0' {
                    user.push(ch);
                }
                i += 1;
                if crate::gpu::is_wide_char(ch)
                    && i < excl
                    && matches!(row[i].ch, ' ' | '\0')
                {
                    i += 1;
                }
            }
            // 우측 한계 = "!" 뒤 공백 구간이 끝나는 지점(=오른쪽 Tips 컬럼 또는 박스
            // 세로 보더의 시작). 2컬럼 배너는 같은 행 오른쪽에 Tips 가 있으므로 첫
            // 보더만 찾으면 그 사이 Tips 를 덮는다 — 다음 non-blank 전까지만 그린다.
            let limit = (excl + 1..row.len())
                .find(|&c| !matches!(row[c].ch, ' ' | '\0'))
                .unwrap_or(row.len());
            (wc, excl, user.trim().to_string(), limit, row[wc].clone())
        };
        let Some(greet) = crate::theme::character_welcome(name, &user) else {
            return; // 로스터 밖 이름 — 배너당 한 번, 원문 유지
        };
        if let Some([rr, gg, bb, _]) = accent {
            style.fg = kasa_bridge::screen::Color::Rgb(rr, gg, bb);
        }
        // 인사말 → 셀(한글은 글리프+스페이서 2칸).
        let mut cells: Vec<GridCell> = Vec::new();
        for ch in greet.chars() {
            let mut cell = style.clone();
            cell.ch = ch;
            cells.push(cell);
            if crate::gpu::is_wide_char(ch) {
                let mut sp = style.clone();
                sp.ch = ' ';
                cells.push(sp);
            }
        }
        let avail = limit.saturating_sub(wc);
        if avail == 0 {
            return;
        }
        if cells.len() > avail {
            cells.truncate(avail);
            if let Some(last) = cells.last_mut() {
                last.ch = '…';
            }
        }
        // 가변 쓰기: 인사말 그린 뒤 원문 잔여("!"까지)를 blank.
        {
            let row = &mut rows[r];
            for (k, cell) in cells.iter().enumerate() {
                row[wc + k] = cell.clone();
            }
            let written = wc + cells.len();
            let tail_end = excl.min(row.len().saturating_sub(1));
            for c in written..=tail_end {
                row[c] = GridCell::blank();
            }
        }
        // 배너 박스 보더(╭─╮│╰╯)도 학생색 — 타이틀·인사말과 색 언어 통일
        // (거노: "색상도 학생색상으로", 배너 전체가 학생색인 게 의도).
        if let Some(acc) = accent {
            let art_bottom = (br + CLAWD_ROWS as isize).max(0) as usize;
            tint_welcome_box(rows, r, art_bottom, acc);
        }
        return; // 웰컴 인사말은 배너당 한 줄
    }
}

/// 웰컴 배너 박스의 보더 문자(box-drawing U+2500~257F) fg 를 학생 accent 로 —
/// "Welcome back" 행 위 상단 코너(╭╮/┌┐)부터 아트 아래 하단 코너(╰╯/└┘)까지의
/// 박스에 한해서만 칠한다(다른 박스 오염 방지 — 코너로 이 배너 박스 범위 특정).
fn tint_welcome_box(
    rows: &mut [Vec<GridCell>],
    welcome_row: usize,
    art_bottom: usize,
    accent: [u8; 4],
) {
    let is_box = |ch: char| (0x2500u32..=0x257F).contains(&(ch as u32));
    let top = (0..welcome_row)
        .rev()
        .find(|&rr| rows[rr].iter().any(|c| matches!(c.ch, '╭' | '╮' | '┌' | '┐')));
    let bottom = (art_bottom.min(rows.len())..rows.len())
        .find(|&rr| rows[rr].iter().any(|c| matches!(c.ch, '╰' | '╯' | '└' | '┘')));
    let (Some(top), Some(bottom)) = (top, bottom) else {
        return;
    };
    let [r, g, b, _] = accent;
    let col = kasa_bridge::screen::Color::Rgb(r, g, b);
    for row in rows[top..=bottom].iter_mut() {
        for cell in row.iter_mut() {
            if is_box(cell.ch) {
                cell.fg = col.clone();
            }
        }
    }
}

/// claude agents 목록 화면인지 화면 텍스트로 감지. argv(`is_claude_agents`)는 `claude
/// agents` **명령**만 잡고, 세션 안에서 "← for agents"로 여는 목록 뷰는 같은 프로세스라
/// argv 가 안 바뀌어 못 잡는다(거노: agents view 로고 안 뜸). 목록 상단 통계줄
/// "N awaiting input · N working · N completed" 의 고유 문구를 신호로 쓴다 — 일반
/// 대화엔 statusline(U+FFFC)이 있어 호출부에서 `!has_profile_slot` 로 이미 걸러진다.
fn screen_is_agents_list(rows: &[Vec<GridCell>]) -> bool {
    let full: String = rows.iter().flat_map(|r| r.iter().map(|c| c.ch)).collect();
    full.contains("awaiting input") && full.contains("completed")
}

/// claude `--resume` 세션 피커 화면인지 감지. "Resume session (N of M)" 헤더가
/// 뜨는 시스템 UI라, 학생 pane 후처리(prompt box accent·세션 제목 인레이)를 여기서
/// 오발동하면 안 된다 — Search 박스(`╭─╮ ⌕ Search… ╰─╯`)가 pane 입력박스로 오인돼
/// 빈 초록 사각형이 그려졌다(거노). 일반 대화엔 statusline(U+FFFC)이 있어 호출부에서
/// !has_profile_slot 로 이미 걸러진다.
fn screen_is_resume_picker(rows: &[Vec<GridCell>]) -> bool {
    let full: String = rows.iter().flat_map(|r| r.iter().map(|c| c.ch)).collect();
    // "Resume session (N of M)" 헤더가 피커 고유 — 단순 "Resume session" 은
    // 대화 본문에 우연히 나올 수 있어 여는 괄호까지 확인한다. 피커도 맨 아래
    // statusline(U+FFFC) 한 줄이 남아 has_profile_slot 으로는 못 거른다
    // (거노: Search 아래 핑크 사각형 잔재 — accent 후처리 오발동).
    //
    // 좁은 창에선 "Resume session" 과 "(N of M)" 이 다른 셀 행으로 wrap 되며
    // 사이에 행끝 패딩(스페이스·U+0000)이 껴 "Resume session (" 직접 매칭이
    // 깨진다(거노: 특정 창 크기에서만 사각형 잔상 재발). 공백류·null 을 한 칸
    // 으로 접어 wrap 여부와 무관하게 매칭한다.
    let squashed: String = full
        .split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    squashed.contains("Resume session (")
}

/// AskUserQuestion picker 감지 — `❯ 1. …` 옵션 목록 + 하단 힌트 박스가 학생
/// 입력박스로 오인돼 accent 사각형이 남던 화면(거노: "question 이나 resume").
/// 고유 시그니처: 항상 마지막 옵션인 "Chat about this" + 하단 네비 힌트
/// ("Esc to cancel" 또는 "Enter to select"). resume 피커엔 없는 조합이라
/// 대화 본문 우연 등장을 힌트 AND 로 한 번 더 거른다. resume 와 같은 squash
/// 정규화로 wrap·다중 공백에 강건하게 매칭한다.
fn screen_is_ask_picker(rows: &[Vec<GridCell>]) -> bool {
    let full: String = rows.iter().flat_map(|r| r.iter().map(|c| c.ch)).collect();
    let squashed: String = full
        .split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    squashed.contains("Chat about this")
        && (squashed.contains("Esc to cancel") || squashed.contains("Enter to select"))
}

/// 한 창이 이번 프레임에 얹을 학생 스프라이트 자리들.
///
/// 셀을 훑어 모으는 쪽(메인 그리드 / 별도창)과 그리는 쪽을 가르는 경계다 —
/// 좌표계는 창마다 다르지만 **이미지 키와 업로드 규칙은 한 벌**이어야 한다.
/// 두 벌이 되면 한쪽만 프레임을 올리거나 캐시 키가 갈려, 같은 학생이 창마다
/// 다른 그림으로 뜬다.
#[derive(Default)]
pub(crate) struct StudentOverlays {
    /// Clawd 배너 자리 — `(slug, rect, (클립 위, 클립 아래))`. 스크롤로 잘리게 클립.
    pub(crate) banner: Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))>,
    /// working 스피너 자리 — 제자리 걸음(walk).
    pub(crate) spinner: Vec<(&'static str, (f32, f32, f32, f32))>,
    /// 승인 대기 — 한 팔 인사(wave).
    pub(crate) waiting: Vec<(&'static str, (f32, f32, f32, f32))>,
    /// 입력박스 위 standing — `(slug, motion, rect)`.
    pub(crate) standing: Vec<(&'static str, &'static str, (f32, f32, f32, f32))>,
    /// statusline 프사(bust) 자리.
    pub(crate) profile: Vec<(&'static str, (f32, f32, f32, f32))>,
    /// 그중 hover 확대가 걸리는 것 — `(학생 이름, slug, rect)`. 별도창만 채운다:
    /// 메인 창은 프사 rect 를 클릭 히트(`face_hit_rects`)로도 재활용하느라 자체
    /// 목록을 따로 쥐고 있다. 이름이 slug 와 별개인 건 팝업 이름표 때문(slug 는 에셋 키).
    pub(crate) faces: Vec<(String, &'static str, (f32, f32, f32, f32))>,
}

impl StudentOverlays {
    /// 다음 프레임을 스스로 불러야 하나 — 움직이는 스프라이트가 하나라도 있으면.
    /// 프사만 있는 창은 정적 1프레임이라 깨울 필요가 없다.
    pub(crate) fn animating(&self) -> bool {
        !self.banner.is_empty()
            || !self.spinner.is_empty()
            || !self.waiting.is_empty()
            || !self.standing.is_empty()
    }
}

/// 모은 자리에 스프라이트를 얹는다. `anim_ms` 는 창이 공유하는 애니 시계
/// (`version_anim_start` 경과). 프레임 업로드는 캐릭터×모션당 1회고, 창마다
/// GpuRenderer 가 달라 창별로 한 번씩 올라간다(같은 키, 같은 픽셀).
///
/// 전부 `queue_image_above` — 셀 **위** 패스다. 아래 패스로 내리면 statusline
/// 테두리 글리프가 얼굴을 가로지르고 blank 처리한 자리에 셀 배경이 덮인다.
pub(crate) fn paint_student_overlays(
    g: &mut gpu::GpuRenderer,
    slots: &StudentOverlays,
    anim_ms: u64,
) {
    let anim_idx = (anim_ms / STUDENT_ANIM_FRAME_MS) as usize % STUDENT_IDLE_FRAMES;
    let walk_idx = (anim_ms as f32 / STUDENT_WALK_FRAME_MS) as usize % STUDENT_WALK_FRAMES;
    let ensure_anim = |g: &mut gpu::GpuRenderer, slug: &str, motion: &str| {
        let pfx = sprite_key_prefix(motion);
        if !g.has_image(&format!("student:{slug}:{pfx}0")) {
            if let Some(frames) = student_sprite_frames(slug, motion) {
                for (i, (rgba, w, h)) in frames.iter().enumerate() {
                    g.upload_image(&format!("student:{slug}:{pfx}{i}"), rgba, *w, *h);
                }
            }
        }
    };
    for (slug, (bx, by, bw, bh), (clip_y0, clip_y1)) in &slots.banner {
        ensure_anim(g, slug, "idle");
        g.queue_image_clipped(
            &format!("student:{slug}:f{anim_idx}"),
            *bx, *by, *bw, *bh, *clip_y0, *clip_y1,
        );
    }
    for (slug, (bx, by, bw, bh)) in &slots.spinner {
        ensure_anim(g, slug, "walk");
        g.queue_image_above(&format!("student:{slug}:walk{walk_idx}"), *bx, *by, *bw, *bh);
    }
    for (slug, (bx, by, bw, bh)) in &slots.waiting {
        ensure_anim(g, slug, "wave");
        g.queue_image_above(&format!("student:{slug}:wave{anim_idx}"), *bx, *by, *bw, *bh);
    }
    for (slug, motion, (bx, by, bw, bh)) in &slots.standing {
        ensure_anim(g, slug, motion);
        let pfx = sprite_key_prefix(motion);
        g.queue_image_above(&format!("student:{slug}:{pfx}{anim_idx}"), *bx, *by, *bw, *bh);
    }
    for (slug, (bx, by, bw, bh)) in &slots.profile {
        let key = format!("student:{slug}:profile");
        if !g.has_image(&key) {
            if let Some((rgba, w, h)) = student_profile_rgba(slug) {
                g.upload_image(&key, &rgba, w, h);
            }
        }
        g.queue_image_above(&key, *bx, *by, *bw, *bh);
    }
}

/// 프사 팝업 한 변 = 셀 높이의 몇 배인가.
///
/// 프사 원본이 96×96 뿐이라(12명 전부, 더 큰 바스트업 원본은 레포에 없다) 이
/// 배수가 곧 확대율을 정한다 — 8배면 344px 라 3.6배 확대가 되어 총열·머리카락
/// 윤곽에 계단이 보였다. 6배(2.7배 확대)가 "얼굴 크기 vs 뭉갬"의 절충점으로
/// 거노가 고른 값이다. 256 전신 프레임으로 갈아타면 선명해지지만 캐릭터가 원본
/// 100×208 이라 같은 박스에서 반토막이 나고 바스트업→전신으로 성격이 바뀐다.
const FACE_POPUP_CELLS: f32 = 6.0;

/// statusline 프사 위에 커서가 있을 때 뜨는 큰 bust 팝업.
///
/// `face` 는 그 프사 자리(논리 px), `cell_h` 는 셀 높이(팝업 크기 산출용),
/// `title_h` 는 팝업이 넘어가면 안 되는 위쪽 크롬 높이. statusline 은 늘 창
/// 아래쪽이라 위로 띄운다.
///
/// 팝업 크기를 호출자가 넘기지 않고 여기서 정하는 건, 메인 창과 별도창 두 곳이
/// 부르기 때문이다 — 상수를 밖에 두면 한쪽만 고쳐진다.
///
/// bust 는 누끼(투명 배경)라 캐릭터만 뜨고 뒤가 비친다 — 배경 박스는 없다
/// (거노: "배경색 아예 없애고"). 이름표만 얇은 pill 로 가독성을 확보한다.
///
/// 매 프레임 hover 를 다시 판정하는 쪽이 호출자다 — 커서가 벗어나면 다음 프레임에
/// 저절로 사라진다(애니 없음).
pub(crate) fn paint_face_popup(
    g: &mut gpu::GpuRenderer,
    cname: &str,
    slug: &str,
    face: (f32, f32, f32, f32),
    cell_h: f32,
    win_w: f32,
    title_h: f32,
) {
    let pop = FACE_POPUP_CELLS * cell_h;
    let key = format!("student:{slug}:profile");
    if !g.has_image(&key) {
        if let Some((rgba, w, h)) = student_profile_rgba(slug) {
            g.upload_image(&key, &rgba, w, h);
        }
    }
    let (fx, fy, fw, _) = face;
    let (px, py) = face_popup_pos(fx, fw, fy, pop, win_w, title_h);
    g.queue_image_above(&key, px, py, pop, pop);
    let accent = theme::character_accent(cname).unwrap_or_else(theme::accent);
    let fs = 13.0;
    let tw = g.measure_chrome_text(cname, fs, true);
    let tx = px + (pop - tw) / 2.0;
    let ty = py + pop + 4.0;
    round_rect(
        g, tx - 7.0, ty - 3.0, tw + 14.0, fs + 8.0,
        theme::radius_sm(), theme::with_alpha(theme::bg(), 0xE6),
    );
    g.draw_text(
        tx, ty, cname,
        gpu::DrawOpts { font_size: fs, color: accent, bold: true, italic: false },
    );
}

/// statusline 프사 자리표시자(U+FFFC 연속 셀) 위치 — `(행, 시작열, 칸수)`.
///
/// statusline.py 가 학생 이름 대신 이 문자를 내보낸다. **아래→위 스캔**인 것이
/// 중요하다: statusline 은 늘 화면 바닥 쪽인데, 대화 출력에 U+FFFC 원문이 섞이면
/// (statusline 디버그 출력 등) 위쪽 행이 앵커를 가로채 얼굴이 엉뚱한 데 붙는다
/// (실사고). 메인 창과 별도창이 같은 자리를 찍도록 한 곳에 둔다.
pub(crate) fn find_statusline_face(rows: &[Vec<GridCell>]) -> Option<(usize, usize, usize)> {
    rows.iter().enumerate().rev().find_map(|(r, row)| {
        row.iter().position(|c| c.ch == '\u{fffc}').map(|c0| {
            let n = row[c0..].iter().take_while(|c| c.ch == '\u{fffc}').count();
            (r, c0, n)
        })
    })
}

/// 입력박스 위 standing 학생의 앵커 — `(앵커 행, 학생 왼쪽 열)`.
///
/// `face_row` 는 statusline 행. 그 바로 위가 아래 테두리(순수 '─')면, 거기서
/// 위로 첫 rule 행이 입력박스 윗 테두리다 — ❯ 영역이 여러 줄로 자라도 스캔이라
/// 따라간다. 학생은 그 윗 테두리 줄에 발이 닿게 서고, 칩(effort·context 경고)이
/// 떠 있으면 그 왼쪽으로 비켜선다.
///
/// 윗 테두리는 `/rename` 세션명이 "── 학생 ──" 로 박힐 수 있어 짧은 텍스트 섬을
/// 인정한다(max_label 24) — 순수 rule 만 보면 이름 지은 세션에서 standing 이
/// 통째로 사라진다(거노 실사고). 아래 테두리는 항상 순수 '─'(0).
pub(crate) fn find_standing_anchor(
    rows: &[Vec<GridCell>],
    face_row: usize,
    cols: usize,
) -> Option<(usize, f32)> {
    // 다수 판정을 **격자 전체 폭이 아니라 내용 폭**(마지막 non-blank 까지)으로 한다.
    // 전체 폭으로 재면 pane 이 claude 의 입력박스보다 넓을 때 테두리가 소수가 되어
    // `is_rule` 이 거짓이 되고, standing 이 통째로 사라진다 — 155칸 pane 에 60칸
    // 테두리로 실측(dash=60/155 → anchor=None). 내용 폭 기준이면 박스가 pane 보다
    // 좁아도 성립한다. 대신 짧은 구분선 조각을 테두리로 오인하지 않게 최소 길이를 둔다.
    let is_rule = |row: &[GridCell], max_label: usize| {
        let mut dashes = 0usize;
        let mut label = 0usize;
        let mut content_w = 0usize;
        for (i, c) in row.iter().enumerate() {
            match c.ch {
                '─' => {
                    dashes += 1;
                    content_w = i + 1;
                }
                ' ' | '\0' => {}
                _ => {
                    label += 1;
                    content_w = i + 1;
                    if label > max_label {
                        return false;
                    }
                }
            }
        }
        dashes >= 8 && dashes > content_w / 2
    };
    if face_row < 4 || !is_rule(&rows[face_row - 1], 0) {
        return None;
    }
    let tr = (face_row.saturating_sub(16)..face_row - 1)
        .rev()
        .find(|&r| is_rule(&rows[r], 24))
        .filter(|&tr| tr >= 1)?;
    let anchor = tr - 1;
    Some((anchor, stand_left_col(rows, anchor, cols)?))
}

/// 앵커 행이 정해진 뒤의 가로 자리 — 그 행에 이미 뭐가 떠 있으면(effort 칩·
/// context 경고) 그 왼쪽으로 비켜선다. 하네스마다 세로 앵커를 찾는 법은 다르지만
/// 가로 규칙은 같아서 여기 한 곳에만 둔다.
fn stand_left_col(rows: &[Vec<GridCell>], anchor: usize, cols: usize) -> Option<f32> {
    let first = rows[anchor].iter().position(|c| !matches!(c.ch, ' ' | '\0'));
    let right_c = match first {
        Some(f) => f as f32 - 1.5,
        None => cols as f32 - 1.0,
    };
    let left_c = right_c - STAND_CELLS;
    (left_c > 2.0).then_some(left_c)
}

/// 테두리 없는 입력창(`PromptBox::Filled`, codex) 위 standing 앵커.
///
/// claude 는 statusline 자리표시자(U+FFFC)에서 아래 테두리를 짚고 위로 스캔하지만
/// **codex 엔 자리표시자를 심을 데가 없다** — `[tui] status_line` 은 정해진 세그먼트
/// 이름 배열이고 모르는 항목은 `⚠ Ignored invalid status line item` 으로 버려진다
/// (0.146.0 실측). 커맨드 훅도 없다. 대신 입력행 자체는 `prompt_box` 가 배경 채움으로
/// 이미 정확히 집어내므로 그 바로 윗행을 앵커로 쓴다 — 테두리 스캔이 통째로 없어
/// claude 쪽이 밟았던 함정(dash 비율 오판)에서 자유롭다.
pub(crate) fn find_filled_standing_anchor(
    rows: &[Vec<GridCell>],
    cols: usize,
) -> Option<(usize, f32)> {
    let PromptBox::Filled { rows: r } = prompt_box(rows)? else {
        return None;
    };
    let anchor = r.start.checked_sub(1)?;
    Some((anchor, stand_left_col(rows, anchor, cols)?))
}

/// standing 학생이 차지하는 가로 칸수 — 앵커 계산과 그리기가 같은 값을 써야 한다.
pub(crate) const STAND_CELLS: f32 = 4.0;

/// Claude Code 라이브 스피너("✻ Verbing…" 별 dingbat, 또는 braille) 위치 감지 —
/// `rows_show_working`(input.rs)과 같은 신호를 행·열 좌표로 돌려준다. 마지막
/// non-blank 30행, 행 앞머리(col<8)만 본다(본문 인용 별표 오탐 방지). 스피너
/// 셀은 blank 처리하고 그 자리에 학생 working 도트를 얹는 용도.
pub(crate) fn find_claude_spinner(rows: &[Vec<GridCell>]) -> Option<(usize, usize)> {
    let last = rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    // todo 트리가 뜨면 스피너 행이 statusline(=last)에서 멀어진다: todo ~7행 +
    // 입력박스(테두리·❯·테두리) ~4행이 사이에 껴 10행 창 밖으로 밀려나 walk
    // 도트가 사라졌다(거노). 앞머리 글리프(별/점/점자 col<8) + '…'/"esc to
    // interrupt" 라는 강한 시그니처라 30행으로 넓혀도 본문 오탐은 사실상 없다.
    let start = (last + 1).saturating_sub(30);
    // 스피너 애니메이션은 별(U+2720~274F)·점자(U+2800~28FF)·가운뎃점(·) 등
    // 여러 글리프를 순환한다. 특정 글리프만 잡으면 점 프레임에서 감지가 끊겨
    // 학생 도트가 프레임마다 깜빡인다 → `rows_show_working` 과 같은 문맥 기준
    // (별+…/점자/"esc to interrupt")으로 working 행을 찾고, 그 행 첫 글리프
    // (=스피너 자리) col 을 돌려준다. 스피너가 어떤 프레임이든 위치가 고정된다.
    for r in (start..=last).rev() {
        let row = &rows[r];
        let line: String = row
            .iter()
            .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
            .collect();
        let has_star = row
            .iter()
            .take(8)
            .any(|cell| (0x2720..=0x274F).contains(&(cell.ch as u32)));
        let has_braille = row
            .iter()
            .take(8)
            .any(|cell| (0x2800..=0x28FF).contains(&(cell.ch as u32)));
        // 최근 claude code(2.1.207 실측)는 스피너 행에 "esc to interrupt" 를
        // 안 넣는다("· Verbing… (3m · ↓ 9k tokens)") — 점(·) 프레임을 문맥
        // 폴백이 못 받아 감지가 프레임마다 끊겼다. 점도 앞머리 글리프로 인정.
        let has_dot = row.iter().take(8).any(|cell| cell.ch == '·');
        let working_row = ((has_star || has_dot) && line.contains('…'))
            || has_braille
            || line.contains("esc to interrupt");
        if working_row {
            if let Some(c) = row
                .iter()
                .take(8)
                .position(|cell| !matches!(cell.ch, ' ' | '\0'))
            {
                return Some((r, c));
            }
        }
    }
    None
}

/// 승인 대기 도트가 설 자리 — 질문 헤더 행("Do you want to proceed", 없으면 첫
/// ❯ 행, 그것도 없으면 마지막 non-blank 행)과 그 행의 텍스트 끝 col. pane
/// 우상단 고정은 윈도우 우상단의 collab 승인 토스트와 겹쳐서(거부 버튼 가림)
/// 프롬프트 자체에 앵커한다. 스캔 범위는 `rows_show_approval_prompt` 와 동일.
fn approval_anchor(rows: &[Vec<GridCell>]) -> Option<(usize, usize)> {
    let last = rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    let start = (last + 1).saturating_sub(14);
    let end_col = |r: usize| {
        rows[r]
            .iter()
            .rposition(|cell| !matches!(cell.ch, ' ' | '\0'))
            .unwrap_or(0)
    };
    let mut chevron: Option<usize> = None;
    for r in start..=last {
        let line: String = rows[r]
            .iter()
            .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
            .collect();
        if line.to_lowercase().contains("do you want to proceed") {
            return Some((r, end_col(r)));
        }
        if chevron.is_none() && line.contains('❯') {
            chevron = Some(r);
        }
    }
    let r = chevron.unwrap_or(last);
    Some((r, end_col(r)))
}

/// Truncate a label to a *pixel* budget using the shaper's real metrics.
/// `clip_display_width` approximates with a fixed px-per-column constant, which
/// only holds for the CJK/ASCII mix it was tuned against — an all-ASCII title
/// measures far narrower than its column count implies, and an all-Hangul one
/// wider. Where a label sits next to another element, measure instead of guess.
pub(crate) fn clip_px(
    g: &mut gpu::GpuRenderer,
    s: &str,
    font_size: f32,
    bold: bool,
    budget: f32,
) -> String {
    if budget <= 0.0 {
        return String::new();
    }
    if g.measure_chrome_text(s, font_size, bold) <= budget {
        return s.to_string();
    }
    let mut out = s.to_string();
    while out.chars().count() > 1 {
        out.pop();
        if g.measure_chrome_text(&format!("{out}…"), font_size, bold) <= budget {
            break;
        }
    }
    out.push('…');
    out
}

/// Truncate a label to a *display-width* budget (CJK glyphs are double-width)
/// with a trailing ellipsis, so long Hangul/CJK titles never bleed past the
/// tab into neighboring chrome. Shared by the side strip and the top tab bar.
fn clip_display_width(s: &str, budget: usize) -> String {
    let total: usize = s.chars().map(cjk_display_w).sum();
    if total <= budget {
        return s.to_string();
    }
    let mut used = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let w = cjk_display_w(c);
        if used + w > budget.saturating_sub(1) {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod picker_tag_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    /// 실제 그리드처럼 한글(와이드) 문자 뒤에 스페이서 셀을 끼운 행 — alacritty
    /// composed 경로는 ' '(실측), kasa-bridge 경로는 '\0' 이라 둘 다 만든다.
    fn row_wide(s: &str, spacer: char) -> Vec<GridCell> {
        let mut out = Vec::new();
        for c in s.chars() {
            let mut cell = GridCell::blank();
            cell.ch = c;
            out.push(cell);
            if (c as u32) >= 0x1100 {
                let mut sp = GridCell::blank();
                sp.ch = spacer;
                out.push(sp);
            }
        }
        out
    }

    // 실측 /resume 피커 설명줄: "    14 minutes ago · main · 23KB · #프라나"
    #[test]
    fn picker_row_detected_plain_and_wide() {
        for row in [
            row_from("    14 minutes ago · main · 23KB · #프라나"),
            row_wide("    14 minutes ago · main · 23KB · #프라나", ' '),
            row_wide("    14 minutes ago · main · 23KB · #프라나", '\0'),
        ] {
            let (c0, end, slug) = picker_student_tag(&row).expect("tag");
            assert_eq!(slug, "prana");
            assert_eq!(row[c0].ch, '#');
            assert!(end > c0 && end < row.len());
            // 이름 마지막 셀까지 범위에 포함(블랭크 처리 범위).
            assert!(row[c0..=end].iter().any(|c| c.ch == '나'));
        }
    }

    // 좁은 창서 헤더가 wrap 되면 "Resume session" 과 "(N of M)" 사이에 행끝
    // 패딩(스페이스·\0)이 껴 예전엔 감지가 끊겨 accent 사각형이 남았다
    // (거노: 특정 창 크기에서만 재발). 공백류를 접어 wrap 무관하게 잡는다.
    #[test]
    fn resume_picker_survives_wrapped_header() {
        // 한 행 안 다수 공백(직접 매칭이면 "Resume session   (" 로 깨짐)
        assert!(screen_is_resume_picker(&[row_from(
            "Resume session      (2 of 5)"
        )]));
        // 두 행으로 wrap + 행끝 패딩(스페이스/\0)
        let wrapped_sp = vec![row_from("Resume session   "), row_from("(2 of 5)")];
        assert!(screen_is_resume_picker(&wrapped_sp));
        let wrapped_null = vec![row_from("Resume session\0\0\0"), row_from("(2 of 5)")];
        assert!(screen_is_resume_picker(&wrapped_null));
        // 정상 한 칸 케이스 유지
        assert!(screen_is_resume_picker(&[row_from("Resume session (3 of 9)")]));
        // 본문 산문은 여전히 무시(여는 괄호 시퀀스 없음)
        assert!(!screen_is_resume_picker(&[row_from(
            "let's Resume session tomorrow"
        )]));
    }

    // AskUserQuestion picker 는 "Chat about this"(항상 마지막 옵션) + 하단
    // 네비 힌트로 감지한다 — 정상 입력박스(미도리 세션제목 보더든 @칩이든)는
    // 건드리지 않고 picker 만 accent 배제(거노: question 도 사각형 잔상).
    #[test]
    fn ask_picker_detected_by_signature() {
        // 실측 시그니처: 옵션 목록 + "Chat about this" + "Enter to select"·"Esc to cancel"
        let full = vec![
            row_from("❯ 1. 기존과 동일 스윕"),
            row_from("  2. 은은한 정적 바"),
            row_from("  3. Type something."),
            row_from("──────────────"),
            row_from("  4. Chat about this"),
            row_from("Enter to select · ↑/↓ to navigate · Esc to cancel"),
        ];
        assert!(screen_is_ask_picker(&full));
        // "Tab/Arrow keys to navigate" 변형(Enter to select 문구 없이 Esc 만)
        let variant = vec![
            row_from("  N. Chat about this"),
            row_from("Tab/Arrow keys to navigate · Esc to cancel"),
        ];
        assert!(screen_is_ask_picker(&variant));
        // resume 피커는 ask 아님("Chat about this" 없음)
        assert!(!screen_is_ask_picker(&[row_from("Resume session (2 of 5)")]));
        // 본문에 "Chat about this" 가 우연히 있어도 힌트 없으면 무시(AND 게이트)
        assert!(!screen_is_ask_picker(&[row_from(
            "We could Chat about this later"
        )]));
    }

    // PR 번호(`repo#12`)의 '#' 는 이름 검증에서 떨어지고, 뒤의 진짜 태그가 잡힌다.
    #[test]
    fn pr_number_hash_skipped() {
        let row = row_from("    2 days ago · main · 1MB · repo#12 · #시로코");
        let (_, _, slug) = picker_student_tag(&row).expect("tag");
        assert_eq!(slug, "shiroko");
    }

    // 오탐 방어: '·' 1개뿐(태그 구분자만) / 구분자 없는 해시태그 / 로스터 밖 이름.
    #[test]
    fn non_picker_rows_ignored() {
        assert!(picker_student_tag(&row_from(" · #시로코")).is_none());
        assert!(picker_student_tag(&row_from("echo #시로코 · done")).is_none());
        assert!(picker_student_tag(&row_from("  1 day ago · main · 2KB · #낯선이")).is_none());
        assert!(picker_student_tag(&row_from("plain text without tags")).is_none());
    }
}

#[cfg(test)]
mod clawd_banner_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    // 실측 배너(claude code 2.1.212): 머리 1칸·발 2칸 들여쓰기.
    const HEAD: &str = " ▐▛███▜▌  Claude Code v2.1.212";
    const BODY: &str = "▝▜█████▛▘ Fable 5 · ~/Desktop";
    const FEET: &str = "  ▘▘ ▝▝   0 awaiting input";

    #[test]
    fn full_banner_detected() {
        let rows = vec![row_from(""), row_from(HEAD), row_from(BODY), row_from(FEET)];
        assert_eq!(find_clawd_banners(&rows), vec![(1, 0)]);
    }

    // 스크롤로 머리 행이 위로 잘림 — 몸통이 최상단 행. top_row = -1 로
    // 잡혀야 몸통·발이 blank 되고 스프라이트가 클립돼 그려진다(거노:
    // 스크롤 살짝 내리면 Clawd 원본 노출 회귀 방지).
    #[test]
    fn body_at_top_row_detected_as_cropped() {
        let rows = vec![row_from(BODY), row_from(FEET), row_from("")];
        assert_eq!(find_clawd_banners(&rows), vec![(-1, 0)]);
    }

    // 머리·몸통까지 잘리고 발만 최상단에 남은 경우.
    #[test]
    fn feet_only_at_top_row_detected() {
        let rows = vec![row_from(FEET), row_from(""), row_from("")];
        assert_eq!(find_clawd_banners(&rows), vec![(-2, 0)]);
    }

    // 아래에서 진입: 최하단 행에 머리만 보임 — top_row = 마지막 행.
    #[test]
    fn head_only_at_bottom_row_detected() {
        let rows = vec![row_from(""), row_from(""), row_from(HEAD)];
        assert_eq!(find_clawd_banners(&rows), vec![(2, 0)]);
    }

    // 몸통이 최하단 행(발만 화면 밖) — 머리+몸통 조합으로 잡힌다.
    #[test]
    fn body_at_bottom_row_detected() {
        let rows = vec![row_from(""), row_from(HEAD), row_from(BODY)];
        assert_eq!(find_clawd_banners(&rows), vec![(1, 0)]);
    }

    // 일반 텍스트·비슷한 블록 글리프는 오탐하지 않는다.
    #[test]
    fn plain_text_not_detected() {
        let rows = vec![
            row_from("normal output line"),
            row_from("▝▜███▛▘ short art"),
            row_from("▘▘▝▝ no gap feet"),
        ];
        assert_eq!(find_clawd_banners(&rows), Vec::<(isize, usize)>::new());
    }

    // 발 패턴이 최상단이라도 양옆에 다른 글자가 붙어 있으면 배너가 아니다.
    #[test]
    fn feet_without_flanking_blanks_not_detected() {
        let rows = vec![row_from("ab▘▘ ▝▝cd"), row_from("")];
        assert_eq!(find_clawd_banners(&rows), Vec::<(isize, usize)>::new());
    }

    fn dim_row(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell.dim = true;
                cell
            })
            .collect()
    }

    // 스크롤 게이트("Jump to bottom") 가 없으면 상단이 흐릿해도 sticky 아님 —
    // 평상시(맨 아래) 오탐 방지의 핵심.
    #[test]
    fn sticky_needs_scroll_gate() {
        let rows = vec![dim_row("> 이전 프롬프트"), row_from("본문 라인")];
        assert!(find_sticky_prompt(&rows).is_none());
    }

    // 게이트 + 최상단 흐릿한 프롬프트 → 그 행·텍스트로 감지.
    #[test]
    fn sticky_gated_dim_top_detected() {
        let rows = vec![
            dim_row("> 이전 프롬프트 미리보기"),
            row_from("작업 결과 라인"),
            row_from("  Jump to bottom (click) ↓"),
        ];
        let s = find_sticky_prompt(&rows).expect("sticky detected");
        assert_eq!(s.row, 0);
        assert!(s.text.contains("이전 프롬프트"));
        assert_eq!(s.col_start, 0);
    }

    // 게이트가 있어도 상단이 흐릿하지 않으면(일반 밝은 텍스트) 감지 안 함.
    #[test]
    fn sticky_gated_but_bright_ignored() {
        let rows = vec![
            row_from("밝은 일반 출력 라인"),
            row_from("more output"),
            row_from("Jump to bottom (click)"),
        ];
        assert!(find_sticky_prompt(&rows).is_none());
    }

    // 타이틀 치환: "Claude Code" → 학생 이름(와이드+스페이서 셀), 버전 텍스트는
    // 이름 바로 뒤로 당겨지고 남는 칸은 blank, 행 길이는 불변.
    #[test]
    fn banner_title_replaced_with_student_name() {
        let mut rows = vec![row_from(""), row_from(HEAD), row_from(BODY), row_from(FEET)];
        replace_banner_title(&mut rows, 1, 0, "아루", Some([255, 128, 0, 255]));
        // HEAD 에서 "Claude Code" 는 col 10 부터 — 이름이 그 자리에 앉는다.
        assert_eq!(rows[1][10].ch, '아');
        assert_eq!(rows[1][11].ch, ' '); // 와이드 스페이서
        assert_eq!(rows[1][12].ch, '루');
        assert_eq!(
            rows[1][10].fg,
            kasa_bridge::screen::Color::Rgb(255, 128, 0)
        );
        let tail: String = rows[1][14..23].iter().map(|c| c.ch).collect();
        assert_eq!(tail, " v2.1.212");
        assert!(rows[1][23..].iter().all(|c| c.ch == ' '));
        assert_eq!(rows[1].len(), row_from(HEAD).len());
        // 몸통·발 행은 그대로.
        assert_eq!(rows[2], row_from(BODY));
    }

    // 박스형 웰컴 변형: 버전 뒤 연속 공백 너머의 오른쪽 테두리는 밀리지 않는다.
    #[test]
    fn boxed_variant_right_border_untouched() {
        let head_box = "│  ▐▛███▜▌  Claude Code v2.1.212    │";
        let mut rows = vec![row_from(""), row_from(head_box), row_from(""), row_from("")];
        let border = head_box.chars().count() - 1;
        replace_banner_title(&mut rows, 1, 1, "시로코", None);
        assert_eq!(rows[1][border].ch, '│');
        assert_eq!(rows[1][12].ch, '시');
        // accent 없으면 원 타이틀 fg(blank 기본 = Default) 유지.
        assert_eq!(rows[1][12].fg, kasa_bridge::screen::Color::Default);
    }

    // 머리 행이 스크롤로 잘려 타이틀이 화면 밖이면 아무것도 안 바꾼다.
    #[test]
    fn cropped_banner_leaves_rows_unchanged() {
        let mut rows = vec![row_from(BODY), row_from(FEET), row_from("")];
        let before = rows.clone();
        replace_banner_title(&mut rows, -1, 0, "아루", None);
        assert_eq!(rows, before);
    }

    const ALL_SLUGS: [&str; 12] = [
        "arona", "prana", "midori", "momoi", "yuzu", "arisu", "yuuka", "shiroko", "hoshino",
        "koharu", "himari", "aru",
    ];

    // 상태 모션 3종(idle/wave/cheer)이 12명 전원에 배선돼 있고 프레임 수가 맞다.
    // (include_bytes 라 파일 부재면 컴파일 실패 — 여긴 arm 매칭 오타를 잡는다.)
    #[test]
    fn every_student_has_state_motions() {
        for slug in ALL_SLUGS {
            for motion in ["idle", "wave", "cheer"] {
                let f = student_sprite_png(slug, motion)
                    .unwrap_or_else(|| panic!("{slug} {motion} 프레임 미배선"));
                assert_eq!(f.len(), STUDENT_IDLE_FRAMES, "{slug} {motion} 프레임 수");
            }
            assert_eq!(
                student_sprite_png(slug, "walk").map(|f| f.len()),
                Some(STUDENT_WALK_FRAMES),
                "{slug} walk 프레임 수",
            );
        }
    }

    // 로스터 밖 슬러그·미지원 모션은 None.
    #[test]
    fn unknown_slug_or_motion_is_none() {
        assert!(student_sprite_png("nobody", "idle").is_none());
        assert!(student_sprite_png("koharu", "sob").is_none());
    }

    // 텍스처 캐시 키 접두: idle 만 "f"(기존 배너 캐시 호환), 나머지는 이름 그대로.
    #[test]
    fn motion_key_prefixes() {
        assert_eq!(sprite_key_prefix("idle"), "f");
        assert_eq!(sprite_key_prefix("wave"), "wave");
        assert_eq!(sprite_key_prefix("cheer"), "cheer");
        assert_eq!(sprite_key_prefix("walk"), "walk");
        // 미지의 모션은 idle 프레임 접두로 폴백(빈 화면보다 낫다).
        assert_eq!(sprite_key_prefix("???"), "f");
    }

    // 한글은 실제 composed 처럼 글리프+스페이서 2칸으로 배치하는 헬퍼.
    fn wide_row(s: &str) -> Vec<GridCell> {
        let mut row = Vec::new();
        for c in s.chars() {
            let mut cell = GridCell::blank();
            cell.ch = c;
            row.push(cell);
            if crate::gpu::is_wide_char(c) {
                let mut sp = GridCell::blank();
                sp.ch = ' ';
                row.push(sp);
            }
        }
        row
    }

    // 셀 행 → 텍스트(와이드 스페이서 제거) — 검증용.
    fn row_text(row: &[GridCell]) -> String {
        let mut s = String::new();
        let mut i = 0;
        while i < row.len() {
            let ch = row[i].ch;
            if ch != '\0' {
                s.push(ch);
            }
            i += 1;
            if crate::gpu::is_wide_char(ch) && i < row.len() && matches!(row[i].ch, ' ' | '\0') {
                i += 1;
            }
        }
        s
    }

    // 1컬럼(좁은 폭): 도트 위 "Welcome back 건호!" → 학생 인사말 + accent 색,
    // 이름("건호")은 그리드에서 추출해 인사말에 삽입된다. 폭은 넉넉히(클립 없음).
    #[test]
    fn welcome_greeting_single_column() {
        let pad = " ".repeat(50);
        let mut rows = vec![
            wide_row(&format!("  Welcome back 건호!{pad}")),
            row_from("   ▐▛███▜▌"),
            row_from("  ▝▜█████▛▘"),
            row_from("    ▘▘ ▝▝"),
        ];
        replace_welcome_greeting(&mut rows, 1, "코하루", Some([200, 50, 50, 255]));
        let line = row_text(&rows[0]);
        assert!(line.contains("어서오세요"), "인사말 치환됨: {line}");
        assert!(line.contains("건호"), "이름 추출·삽입: {line}");
        assert!(!line.contains("Welcome back"), "원문 제거: {line}");
        let first = rows[0].iter().find(|c| !matches!(c.ch, ' ' | '\0')).unwrap();
        assert_eq!(first.fg, kasa_bridge::screen::Color::Rgb(200, 50, 50));
    }

    // 2컬럼(넓은 폭): "Welcome back" 뒤 박스 세로 보더 │ 는 인사말이 길어도 안 밀린다.
    #[test]
    fn welcome_greeting_clipped_at_border() {
        let mut rows = vec![
            wide_row("│  Welcome back 건호!    │"),
            row_from(" ▐▛███▜▌"),
            row_from("▝▜█████▛▘"),
            row_from("  ▘▘ ▝▝"),
        ];
        let border = rows[0].iter().rposition(|c| c.ch == '│').unwrap();
        replace_welcome_greeting(&mut rows, 1, "아루", None); // 아루 인사말 = 긴 편
        assert_eq!(rows[0][border].ch, '│', "우측 보더 보존");
        assert!(
            rows[0][border + 1..].iter().all(|c| matches!(c.ch, ' ' | '\0')),
            "보더 너머 무변화",
        );
    }

    // 2컬럼: 같은 행 오른쪽 Tips 컬럼은 인사말이 길어도 침범하지 않는다.
    #[test]
    fn welcome_greeting_preserves_right_column() {
        let mut rows = vec![
            wide_row("  Welcome back 건호!      Tips for getting started"),
            row_from(" ▐▛███▜▌"),
            row_from("▝▜█████▛▘"),
        ];
        let tips_col = rows[0].iter().position(|c| c.ch == 'T').unwrap();
        let tips_before: Vec<char> = rows[0][tips_col..].iter().map(|c| c.ch).collect();
        replace_welcome_greeting(&mut rows, 1, "아루", None);
        let tips_after: Vec<char> = rows[0][tips_col..].iter().map(|c| c.ch).collect();
        assert_eq!(tips_before, tips_after, "오른쪽 Tips 컬럼 보존");
    }

    // launcher 등 "Welcome back" 행이 없으면 no-op(원본 그리드 무변경).
    #[test]
    fn welcome_greeting_noop_without_welcome() {
        let mut rows = vec![
            row_from("Claude Code v2.1.215"),
            row_from(" ▐▛███▜▌"),
            row_from("▝▜█████▛▘"),
        ];
        let before = rows.clone();
        replace_welcome_greeting(&mut rows, 1, "코하루", None);
        assert_eq!(rows, before, "웰컴 행 없으면 무변경");
    }

    // 로스터 밖 이름이면 배너 원문 유지.
    #[test]
    fn welcome_greeting_unknown_character_noop() {
        let mut rows = vec![wide_row("Welcome back 건호!"), row_from(" ▐▛███▜▌")];
        let before = rows.clone();
        replace_welcome_greeting(&mut rows, 1, "없는이름", None);
        assert_eq!(rows, before);
    }

    // 배너 박스 보더가 학생 accent 로 틴트되고, 범위 밖 다른 박스는 오염 안 된다.
    #[test]
    fn welcome_box_border_tinted() {
        let acc = [80, 160, 240, 255];
        let mut rows = vec![
            row_from("╭─ Claude Code ─╮"),
            wide_row("│ Welcome back 건호! │"),
            row_from("│   ▐▛███▜▌   │"),
            row_from("│  ▝▜█████▛▘  │"),
            row_from("│    ▘▘ ▝▝    │"),
            row_from("╰───────────────╯"),
            row_from("╭─ other box ─╮"),
        ];
        replace_welcome_greeting(&mut rows, 2, "코하루", Some(acc));
        let want = kasa_bridge::screen::Color::Rgb(80, 160, 240);
        assert_eq!(rows[0][0].fg, want, "상단 보더 ╭ 틴트");
        assert_eq!(rows[5][0].fg, want, "하단 보더 ╰ 틴트");
        assert_eq!(rows[1][0].fg, want, "welcome 행 좌 │ 틴트");
        assert_eq!(
            rows[6][0].fg,
            kasa_bridge::screen::Color::Default,
            "범위 밖 다른 박스 미오염",
        );
    }
}

#[cfg(test)]
mod spinner_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    // 점(·) 프레임 회귀 방지: 예전엔 별/점자 글리프만 잡아 점 프레임에서
    // None 을 반환 → 학생 도트가 프레임마다 깜빡였다.
    #[test]
    fn spinner_detects_dot_frame() {
        let rows = vec![
            row_from(""),
            row_from("· Cerebrating… (esc to interrupt)"),
        ];
        assert_eq!(find_claude_spinner(&rows), Some((1, 0)));
    }

    // 라이브 실측(claude code 2.1.207): 스피너 행에 "esc to interrupt" 힌트가
    // 없다 — 점 프레임은 점+… 문맥만으로 잡혀야 한다.
    #[test]
    fn spinner_detects_dot_frame_without_esc_hint() {
        let rows = vec![row_from("· Caramelizing… (3m 39s · ↓ 9.7k tokens)")];
        assert_eq!(find_claude_spinner(&rows), Some((0, 0)));
    }

    #[test]
    fn spinner_detects_star_and_braille() {
        let star = vec![row_from("✻ Working… (esc to interrupt)")];
        assert!(find_claude_spinner(&star).is_some());
        let braille = vec![row_from("⠹ Loading")];
        assert!(find_claude_spinner(&braille).is_some());
    }

    #[test]
    fn spinner_ignores_plain_text() {
        let rows = vec![row_from("just some normal output line")];
        assert_eq!(find_claude_spinner(&rows), None);
    }
}

#[cfg(test)]
mod teammate_msg_tests {
    use super::*;

    fn row_from(s: &str, cols: usize) -> Vec<GridCell> {
        let mut row = vec![GridCell::blank(); cols];
        for (i, c) in s.chars().enumerate() {
            row[i].ch = c;
        }
        row
    }

    fn row_text(row: &[GridCell]) -> String {
        row.iter()
            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn detects_collapsed_line() {
        let row = row_from("  › Message from @aru-9c88", 80);
        assert_eq!(
            teammate_collapsed_line(&row),
            Some((2, 1, "aru-9c88".to_string()))
        );
        // v2.1.216+: 이름 뒤 "(ctrl+o to expand)" 단축키 힌트가 붙어도 접힌 줄.
        let hinted = row_from("  › Message from @aru-9c88 (ctrl+o to expand)", 80);
        assert_eq!(
            teammate_collapsed_line(&hinted),
            Some((2, 1, "aru-9c88".to_string()))
        );
        let plural_hinted = row_from("› 3 messages from @yuzu-1ba1 (ctrl+o to expand)", 80);
        assert_eq!(
            teammate_collapsed_line(&plural_hinted),
            Some((0, 3, "yuzu-1ba1".to_string()))
        );
    }

    // 복수형 "› N messages from @이름" → count=N, 이름 추출(여러 자릿수 포함).
    #[test]
    fn detects_plural_collapsed_line() {
        let row = row_from("  › 3 messages from @aru-9c88", 80);
        assert_eq!(
            teammate_collapsed_line(&row),
            Some((2, 3, "aru-9c88".to_string()))
        );
        let row2 = row_from("› 12 messages from @yuzu-1ba1", 80);
        assert_eq!(
            teammate_collapsed_line(&row2),
            Some((0, 12, "yuzu-1ba1".to_string()))
        );
    }

    // 이름 뒤에 다른 글자가 있으면 본문 안 인용 — 실출력 덮어쓰기 오탐 방지.
    // 단수·복수 양쪽 다.
    #[test]
    fn rejects_trailing_text_and_plain_lines() {
        let quoted = row_from("› Message from @aru-9c88 라고 떴다", 80);
        assert_eq!(teammate_collapsed_line(&quoted), None);
        let plain = row_from("Message from @aru-9c88", 80);
        assert_eq!(teammate_collapsed_line(&plain), None);
        // 복수형도 이름 뒤 텍스트면 거부.
        let plural_quoted = row_from("› 3 messages from @aru-9c88 이라고", 80);
        assert_eq!(teammate_collapsed_line(&plural_quoted), None);
    }

    #[test]
    fn extract_tag_attrs_and_body() {
        let text = "<teammate-message teammate_id=\"aru-9c88\" color=\"orange\" \
                    summary=\"확인 통지\">아루다. 확인했다.</teammate-message>";
        let m = extract_teammate_msg(text, "aru-9c88").unwrap();
        assert_eq!(m.body, "아루다. 확인했다.");
        assert_eq!(m.color.as_deref(), Some("orange"));
        // 다른 보낸이의 태그는 건너뛰고 일치하는 태그만.
        assert!(extract_teammate_msg(text, "yuzu-1ba1").is_none());
    }

    // 세션명 rule 검출: 진짜 "── 이름 ──" 만. 둥근 입력박스 테두리(╭────╮·╰────╯)의
    // 모서리는 box-drawing 이라 이름 섬이 아니다 — 행 전체 사각형 오탐 회귀 방지(거노).
    #[test]
    fn titled_rule_ignores_box_border_rows() {
        let dash = |n: usize| "─".repeat(n);
        let title = row_from(&format!("{} 세션명 {}", dash(30), dash(30)), 80);
        let (r, c0, c1) = find_titled_rule(&[title]).expect("이름 섬 있는 rule 인정");
        assert_eq!(r, 0);
        assert!(c0 > 0 && c1 < 79, "이름 구간만, 행 전체 아님");
        let top = row_from(&format!("╭{}╮", dash(70)), 80);
        let bottom = row_from(&format!("╰{}╯", dash(70)), 80);
        let plain = row_from(&dash(72), 80);
        assert!(find_titled_rule(&[top]).is_none(), "둥근 상단 테두리 무시");
        assert!(find_titled_rule(&[bottom]).is_none(), "둥근 하단 테두리 무시");
        assert!(find_titled_rule(&[plain]).is_none(), "순수 rule 무시");
    }

    // teammate 칩 행(`──── @이름 ──`)은 세션명이 아니다 — claude 네이티브가 그리는
    // agent 이름 배지라 아웃라인(사각 테두리)을 두르면 안 된다(거노 2026-07-27:
    // "칩 네모칸"). 세션명 rule 은 계속 인정.
    #[test]
    fn titled_rule_ignores_agent_chip_row() {
        let dash = |n: usize| "─".repeat(n);
        let chip = row_from(&format!("{} @model-check {}", dash(50), dash(4)), 80);
        assert!(find_titled_rule(&[chip]).is_none(), "@칩 행은 세션명 아님");
        // 칩과 무관한 진짜 세션명은 그대로 인정.
        let title = row_from(&format!("{} 세션명 {}", dash(30), dash(30)), 80);
        assert!(find_titled_rule(&[title]).is_some(), "세션명 rule 은 유지");
    }

    // 크로스-방 tell 마커: 유효 캐릭터 `⟦이름⟧` 만 인정, 거노 직접 입력(마커 없음)·
    // 오탐(`⟦…⟧` 이지만 캐릭터 아님)은 무시 = 무색.
    #[test]
    fn tell_marker_parsed_and_guarded() {
        let row = row_from("⟦미도리⟧ 안녕하세요", 80);
        let (start, _, name) = tell_marker_line(&row).expect("유효 캐릭터 마커");
        assert_eq!((start, name.as_str()), (0, "미도리"));
        // claude TUI 실화면: 제출된 user 턴은 `❯ ` 프롬프트 마커 뒤에 온다.
        let prompted = row_from("❯ ⟦프라나⟧ 검증 메시지", 80);
        let (start, _, name) = tell_marker_line(&prompted).expect("❯ 뒤 마커도 인정");
        assert_eq!((start, name.as_str()), (2, "프라나"), "마커 시작 = ❯ + 공백 뒤");
        assert!(tell_marker_line(&row_from("그냥 내 입력", 80)).is_none());
        assert!(tell_marker_line(&row_from("❯ 마커 없는 제출", 80)).is_none());
        assert!(tell_marker_line(&row_from("⟦없는캐릭⟧ x", 80)).is_none());
    }

    // 프사 캐릭터: 아바타는 본문 위 행으로 올라가고(반환 col 이 그 x 기준) 본문은
    // 마커 자리(= `❯ ` 뒤 col 2)로 당겨져 wrap 연속 행과 좌측이 맞는다. `❯` 자리엔
    // 인용 마커 `›` 만 남고 이름 텍스트는 프사가 대신한다.
    #[test]
    fn restyle_tell_lifts_face_and_aligns_body() {
        let mut row = row_from("❯ ⟦미도리⟧ 본문", 80);
        let (marker_start, marker_end, name) = tell_marker_line(&row).unwrap();
        let face_col =
            restyle_tell_line(&mut row, marker_start, marker_end, &name, [107, 207, 127, 255]);
        assert_eq!(face_col, Some(0), "프사 x = ❯ 가 있던 왼쪽 여백");
        assert_eq!(row[0].ch, ' ', "여백은 프사 자리로 비운다");
        assert_eq!(row[2].ch, '본', "본문은 col 2 = wrap 연속 행과 동일");
        assert!(!row_text(&row).contains("미도리"), "이름은 프사가 대신");
    }

    // 실제 화면은 한글이 2셀이라 마커 `⟦이름⟧ ` 폭이 이름 길이에 따라 가변이다 —
    // 본문 시작 col 이 그 폭에 휘둘리면 wrap 연속 행과 계단이 진다(거노 2026-07-27).
    #[test]
    fn tell_body_col_independent_of_name_width() {
        // wide 셀 재현: 한글 뒤에 스페이서 한 칸(composed 경로와 동일).
        let mut row = row_from("❯ ⟦호 시 노 ⟧ 본문", 80);
        let (marker_start, marker_end, name) = tell_marker_line(&row).expect("마커 인식");
        assert_eq!(name, "호시노");
        let face_col = restyle_tell_line(&mut row, marker_start, marker_end, &name, [107, 207, 127, 255])
            .expect("프사");
        assert_eq!(face_col, 0, "이름이 길어도 프사는 왼쪽 여백 고정");
        assert_eq!(row[2].ch, '본', "{}", row_text(&row));
    }

    // wrap 연속 행: 2칸 들여쓰기 본문만 연속, TUI 구조 글리프(⎿·⏺)·빈 행에서 끊김.
    #[test]
    fn tell_wrap_continuation_bounds() {
        assert!(tell_wrap_continuation(&row_from("  짧게 답해줘.", 80)));
        assert!(!tell_wrap_continuation(&row_from("  ⎿  4 skills available", 80)));
        assert!(!tell_wrap_continuation(&row_from("⏺ 확인", 80)));
        assert!(!tell_wrap_continuation(&row_from("", 80)));
        assert!(!tell_wrap_continuation(&row_from("   들여쓰기 3", 80)));
    }

    // 인라인 재작성(학생 발신): 프사는 본문 위 행(호출측 이미지 패스)이고 첫 줄은
    // `› ` + 본문 — 그 폭이 이어 쓰는 줄의 들여쓰기와 같아 좌측이 한 줄로 선다.
    // 이름 텍스트는 프사가 대신하고 원문 "› Message from @…" 잔재는 지워진다.
    #[test]
    fn restyle_writes_inline_body_with_face_for_student() {
        let mut rows = vec![row_from("› Message from @aru-9c88", 60)];
        let face =
            expand_teammate_message(&mut rows, 0, 0, "aru-9c88", Some("아루다 확인"), [255, 128, 0, 255]);
        assert_eq!(face, Some(0), "프사 col 반환");
        assert_eq!(rows[0][0].ch, ' ', "첫 두 칸은 프사 자리");
        assert_eq!(rows[0][2].ch, '아', "본문은 이어 쓰는 줄과 같은 col 2");
        assert_eq!(
            rows[0][2].fg,
            kasa_bridge::screen::Color::Rgb(255, 128, 0),
            "학생 accent 로 도색"
        );
        let text = row_text(&rows[0]);
        assert!(text.contains("아 루 다"), "본문 와이드 스페이서 유지: {text}");
        assert!(!text.contains("aru-9c88"), "이름은 프사가 대신: {text}");
    }

    // 학생이 아닌 발신자(team-lead 등)는 프사가 없어 기존 `@이름❯` 헤더 유지.
    #[test]
    fn restyle_keeps_name_header_for_non_student() {
        let mut rows = vec![row_from("› Message from @team-lead", 60)];
        let face =
            expand_teammate_message(&mut rows, 0, 0, "team-lead", Some("확인"), [255, 128, 0, 255]);
        assert_eq!(face, None, "프사 없음");
        assert!(row_text(&rows[0]).starts_with("@ team-lead❯"), "{}", row_text(&rows[0]));
    }

    // 이어 쓸 blank 행이 없으면 말줄임으로 끝난다 — 다음 항목 침범 없음.
    #[test]
    fn restyle_truncates_with_ellipsis() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("다음 항목", 24),
        ];
        expand_teammate_message(
            &mut rows, 0, 0, "aru-9c88",
            Some("긴 본문이 한 줄을 넘겨 잘린다"),
            [255, 0, 0, 255],
        );
        let text = row_text(&rows[0]);
        assert!(text.ends_with('…'), "{text}");
        assert_eq!(row_text(&rows[1]), "다음 항목", "다음 항목 무손상");
    }

    // 아래 blank 행이 있으면 줄바꿈으로 이어 쓴다(거노) — 다음 항목과의
    // 구분 blank 1행은 남긴다.
    #[test]
    fn expands_into_blank_rows_keeping_separator() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("", 24),
            row_from("", 24),
            row_from("", 24),
            row_from("다음 항목", 24),
        ];
        expand_teammate_message(
            &mut rows, 0, 0, "aru-9c88",
            Some("긴 본문이 여러 줄에 걸쳐 이어진다"),
            [255, 0, 0, 255],
        );
        assert!(row_text(&rows[0]).contains('긴'), "{}", row_text(&rows[0]));
        // 이어 쓴 줄은 2칸 들여쓰기 + 학생색.
        assert!(row_text(&rows[1]).starts_with("  "), "{}", row_text(&rows[1]));
        assert!(!row_is_blank(&rows[1]));
        assert_eq!(
            rows[1].iter().find(|c| c.ch != ' ').unwrap().fg,
            kasa_bridge::screen::Color::Rgb(255, 0, 0)
        );
        // usable = blank_run(3) - 1 → 마지막 blank 는 구분용으로 남는다.
        assert!(row_is_blank(&rows[3]), "구분 blank 유지");
        assert_eq!(row_text(&rows[4]), "다음 항목", "다음 항목 무손상");
    }

    // 뷰포트 바닥까지 전부 빈 경우엔 구분행 없이 끝까지 쓴다.
    #[test]
    fn expands_to_viewport_bottom_without_separator() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("", 24),
            row_from("", 24),
        ];
        expand_teammate_message(
            &mut rows, 0, 0, "aru-9c88",
            Some("본문이 바닥까지 이어져 내려간다 아주 길게 계속"),
            [255, 0, 0, 255],
        );
        assert!(!row_is_blank(&rows[1]));
        assert!(!row_is_blank(&rows[2]), "바닥 행까지 사용");
    }

    // 본문 회수 실패 시엔 원문 유지 + 색만.
    #[test]
    fn restyle_without_body_recolors_only() {
        let mut rows = vec![row_from("› Message from @aru-9c88", 40)];
        expand_teammate_message(&mut rows, 0, 0, "aru-9c88", None, [0, 255, 0, 255]);
        assert_eq!(row_text(&rows[0]), "› Message from @aru-9c88");
        assert_eq!(rows[0][0].fg, kasa_bridge::screen::Color::Rgb(0, 255, 0));
    }

    // 셀 폭 word-wrap: 첫 줄/이후 줄 폭 분리, 와이드 2칸, 긴 단어 글자 분할.
    #[test]
    fn wrap_body_cells_widths_and_split() {
        let (lines, trunc) = wrap_body_cells("가나 다라 마바", 6, 6, 10);
        // "가나"(4)+" "+"다라" = 9 > 6 → 줄마다 한 단어.
        assert_eq!(lines, vec!["가나", "다라", "마바"]);
        assert!(!trunc);
        let (lines, trunc) = wrap_body_cells("가나다라", 4, 4, 10);
        assert_eq!(lines, vec!["가나", "다라"], "긴 단어 글자 분할");
        assert!(!trunc);
        let (lines, trunc) = wrap_body_cells("하나 둘 셋 넷", 4, 4, 2);
        assert_eq!(lines.len(), 2);
        assert!(trunc, "max_lines 초과분은 잘림 표시");
    }

    // agent 이름 로마자 → 로스터 역매핑, 로스터 밖은 태그 color 폴백.
    #[test]
    fn sender_accent_roster_and_fallback() {
        assert_eq!(theme::slug_character("aru"), Some("아루"));
        assert_eq!(
            teammate_sender_accent("aru-9c88", None),
            theme::character_accent("아루").unwrap()
        );
        assert_eq!(
            teammate_sender_accent("team-lead", Some("orange")),
            [228, 140, 60, 255]
        );
    }
}

#[cfg(test)]
mod student_asset_tests {
    use super::*;

    /// 번들 내장 프레임이 **모든 학생 × 모든 모션**에서 실제로 디코딩되는지.
    ///
    /// `student_sprite_frames` 는 프레임 크기가 하나라도 다르거나 PNG 하나가
    /// 안 풀리면 그 모션을 통째로 `None` 으로 돌려주고, 호출측은 업로드를 건너뛴
    /// 뒤 없는 키로 `queue_image_above` 를 부른다 — **아무것도 안 그려지고 에러도
    /// 없다**. 그래서 "프사(정적 로더)는 뜨는데 애니만 안 뜬다" 가 되면 원인을
    /// 밖에서 가릴 수 없다(2026-08-05 거노 신고 추적에 하루가 들었다).
    #[test]
    fn bundled_sprite_frames_decode_for_every_student_and_motion() {
        let mut checked = 0;
        for (_, slug) in crate::theme::CHARACTER_SLUGS {
            for motion in ["idle", "wave", "cheer", "walk"] {
                let frames = student_sprite_frames(slug, motion)
                    .unwrap_or_else(|| panic!("{slug}/{motion}: 프레임이 None — 애니가 통째로 안 그려진다"));
                let want = if motion == "walk" {
                    STUDENT_WALK_FRAMES
                } else {
                    STUDENT_IDLE_FRAMES
                };
                assert_eq!(frames.len(), want, "{slug}/{motion}: 프레임 수");
                for (i, (rgba, w, h)) in frames.iter().enumerate() {
                    assert!(*w > 0 && *h > 0, "{slug}/{motion}[{i}]: 0 크기");
                    assert_eq!(
                        rgba.len(),
                        (*w as usize) * (*h as usize) * 4,
                        "{slug}/{motion}[{i}]: RGBA 길이가 w*h*4 와 안 맞는다"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked >= 4, "모션을 하나도 못 셌다 — 로스터가 비었나");
    }

    // 사용자 override 파일이 없으면 None → 호출측이 번들 include_bytes 로 폴백.
    #[test]
    fn user_asset_missing_falls_back() {
        let dir = std::env::temp_dir().join(format!("kt-noassets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(user_asset_rgba_in(&dir, "yuuka-profile.png").is_none());
    }

    // override 파일이 있으면 그걸 읽고, 과대 이미지는 MAX_STUDENT_EDGE 로 종횡비
    // 유지 다운스케일(640×480 → 512×384).
    #[test]
    fn user_asset_read_and_downscale() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kt-assets-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(640, 480, image::Rgba([10, 20, 30, 255]))
            .save(dir.join("yuuka-profile.png"))
            .unwrap();
        let (rgba, w, h) =
            user_asset_rgba_in(&dir, "yuuka-profile.png").expect("override read");
        assert_eq!((w, h), (MAX_STUDENT_EDGE, 384));
        assert_eq!(rgba.len() as u32, w * h * 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 규격 이하 이미지는 원본 크기 그대로(불필요한 리샘플 방지).
    #[test]
    fn user_asset_small_kept_verbatim() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kt-small-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(96, 96, image::Rgba([1, 2, 3, 255]))
            .save(dir.join("schale-logo.png"))
            .unwrap();
        let (_, w, h) = user_asset_rgba_in(&dir, "schale-logo.png").expect("override read");
        assert_eq!((w, h), (96, 96));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod face_popup_tests {
    use super::face_popup_pos;

    #[test]
    fn clamps_within_window() {
        let pop = 160.0;
        let (win_w, title_h) = (1440.0, 30.0);
        // 창 오른쪽 끝 프사 → 팝업이 오른쪽 밖으로 넘치지 않는다.
        let (px, _) = face_popup_pos(1400.0, 40.0, 1200.0, pop, win_w, title_h);
        assert!(px + pop <= win_w - 6.0 + 0.01, "px={px} overflows right edge");
        // 창 왼쪽 끝 프사 → 팝업 x 가 6px 밑으로 안 내려간다.
        let (px, _) = face_popup_pos(0.0, 40.0, 1200.0, pop, win_w, title_h);
        assert!(px >= 6.0 - 0.01, "px={px} underflows left edge");
    }

    #[test]
    fn floats_above_and_clamps_to_titlebar() {
        let pop = 160.0;
        // 프사가 창 상단 근처면 팝업 top 이 타이틀바 아래로 클램프된다.
        let (_, py) = face_popup_pos(100.0, 40.0, 40.0, pop, 1440.0, 30.0);
        assert!(py >= 30.0 + 6.0 - 0.01, "py={py} intrudes into titlebar");
        // 프사가 창 하단이면 팝업은 그 위로(음의 방향) 뜬다 = 프사 top 보다 위.
        let (_, py) = face_popup_pos(100.0, 40.0, 1200.0, pop, 1440.0, 30.0);
        assert!(py < 1200.0, "py={py} should float above the face");
    }

    #[test]
    fn narrow_window_never_goes_negative() {
        // 팝업 변보다 좁은 창에서도 x 가 6px 아래로 안 간다(max 방어).
        let (px, _) = face_popup_pos(10.0, 20.0, 200.0, 160.0, 100.0, 30.0);
        assert!(px >= 6.0 - 0.01, "px={px} negative on narrow window");
    }
}

#[cfg(test)]
mod sticky_seek_tests {
    use super::*;

    fn set_pills(entries: &[(&str, &str)]) {
        STICKY_PILLS.with(|s| {
            *s.borrow_mut() = entries
                .iter()
                .map(|(id, text)| (id.to_string(), (0.0, 0.0, 10.0, 10.0), text.to_string()))
                .collect();
        });
    }

    // 클릭 → seek 시작, 첫 스텝은 그 pane 에 wheel-up 노치(클릭 셀)를 쏜다.
    #[test]
    fn first_step_emits_notch() {
        set_pills(&[("%1", "이전 프롬프트")]);
        begin_sticky_seek("%1".into(), "이전 프롬프트".into(), (5, 7));
        assert!(sticky_seek_active());
        assert_eq!(sticky_seek_step(), Some(("%1".to_string(), 5, 7)));
        assert!(sticky_seek_active()); // 아직 진행 중
    }

    // 스로틀: 방금 노치 직후 재호출은 대기(None)하되 seek 은 살아있다(리페인트 대기).
    #[test]
    fn throttled_between_notches() {
        set_pills(&[("%1", "T")]);
        begin_sticky_seek("%1".into(), "T".into(), (1, 1));
        assert!(sticky_seek_step().is_some()); // 첫 노치
        assert_eq!(sticky_seek_step(), None); // 간격 내 재호출 → 대기
        assert!(sticky_seek_active());
    }

    // sticky 텍스트가 target 과 달라지면(타깃이 뷰포트로 들어옴) 종료·상태 클리어.
    #[test]
    fn stops_when_target_enters_view() {
        set_pills(&[("%1", "타깃")]);
        begin_sticky_seek("%1".into(), "타깃".into(), (1, 1));
        set_pills(&[("%1", "더 이전 프롬프트")]); // sticky 가 이전 프롬프트로 교체됨
        assert_eq!(sticky_seek_step(), None);
        assert!(!sticky_seek_active());
    }

    // sticky 가 사라지면(최상단 도달) 종료.
    #[test]
    fn stops_when_sticky_gone() {
        set_pills(&[("%1", "타깃")]);
        begin_sticky_seek("%1".into(), "타깃".into(), (1, 1));
        set_pills(&[]); // 최상단 — pill 없음
        assert_eq!(sticky_seek_step(), None);
        assert!(!sticky_seek_active());
    }
}

#[cfg(test)]
mod prompt_box_tests {
    use super::*;

    fn row_from(s: &str) -> Vec<GridCell> {
        s.chars()
            .map(|c| {
                let mut cell = GridCell::blank();
                cell.ch = c;
                cell
            })
            .collect()
    }

    // 진짜 claude 입력박스: 대시줄 사이 ❯ 마커행 → 감지된다(실제 composed 는
    // 모서리·세로선 없는 순수 대시줄이라 box_rows 와 동일 형태).
    #[test]
    fn real_prompt_box_detected() {
        let rows = vec![
            row_from("some output above"),
            row_from(&"─".repeat(28)),
            row_from(&format!("❯ hello{}", " ".repeat(21))),
            row_from(&"─".repeat(28)),
        ];
        assert!(matches!(
            prompt_box(&rows),
            Some(PromptBox::Bordered { ref rows, top: 1, bottom: 3 }) if *rows == (2..3)
        ));
    }

    // codex 입력줄: 보더가 없고 **줄 전체가 명시 배경색**이다(실측 bg=Rgb(63,69,77)).
    // 배경 없이 `›` 만 보면 인용문을 입력창으로 오인하므로 둘을 함께 요구한다.
    #[test]
    fn codex_filled_prompt_row_detected() {
        let filled = |s: &str| {
            let mut r = row_from(s);
            for c in r.iter_mut() {
                c.bg = kasa_bridge::screen::Color::Rgb(63, 69, 77);
            }
            r
        };
        let rows = vec![
            row_from("⚠ MCP startup incomplete"),
            filled("› Use /skills to list available skills"),
            row_from("gpt-5.5 medium · tmuxify · main · Context 0% used"),
        ];
        assert!(matches!(prompt_box(&rows), Some(PromptBox::Filled { ref rows }) if *rows == (1..2)));

        // 실제 codex 는 마커 행 위아래에 같은 채움색 여백 행을 둔다(실측 3줄).
        // 마커 행만 잡으면 가운데 한 줄만 칠해져 상자가 아니라 밑줄이 된다(거노).
        let boxed = vec![
            row_from("⚠ MCP startup incomplete"),
            filled(&" ".repeat(50)),
            filled("› Use /skills to list available skills"),
            filled(&" ".repeat(50)),
            row_from("gpt-5.5 medium · tmuxify · main · Context 0% used"),
        ];
        assert!(
            matches!(prompt_box(&boxed), Some(PromptBox::Filled { ref rows }) if *rows == (1..4)),
            "여백 행까지 한 상자로"
        );
        // 같은 줄이라도 배경이 없으면 입력창이 아니다 — 인용문 오인 방지.
        let plain = vec![row_from("› quoted line, not an input box at all")];
        assert!(prompt_box(&plain).is_none());
    }

    /// codex 학생은 입력행 **바로 위**에 선다. claude 처럼 statusline 자리표시자
    /// (U+FFFC)에서 출발할 수 없어서다 — `[tui] status_line` 은 정해진 세그먼트
    /// 이름 배열이라 모르는 항목을 넣으면 `Ignored invalid status line item` 으로
    /// 버려진다(0.146.0 실측). 앵커가 입력행에 직접 매이는지 못박는다.
    #[test]
    fn codex_student_stands_on_the_row_above_the_input() {
        let filled = |s: &str| {
            let mut r = row_from(s);
            for c in r.iter_mut() {
                c.bg = kasa_bridge::screen::Color::Rgb(63, 69, 77);
            }
            r
        };
        let rows = vec![
            row_from("⚠ MCP startup incomplete"),
            row_from(""),
            filled(&" ".repeat(50)),
            filled("› Write tests for @filename"),
            filled(&" ".repeat(50)),
            row_from("gpt-5.5 medium · tmuxify · main · Context 0% used"),
        ];
        let (anchor, left_c) = find_filled_standing_anchor(&rows, 80).expect("앵커");
        assert_eq!(anchor, 1, "여백 행까지 포함한 상자(2..5) 바로 위");
        // 앵커 행이 비어 있으면 오른쪽 끝에 선다.
        assert!((left_c - (80.0 - 1.0 - STAND_CELLS)).abs() < f32::EPSILON);

        // 입력창을 못 찾으면 아무 데도 안 세운다 — 빈 화면에 학생이 뜨는 회귀 방지.
        assert!(find_filled_standing_anchor(&[row_from("just text")], 80).is_none());

        // 입력행이 첫 줄이면(스크롤로 위가 잘림) 설 자리가 없다.
        let top = vec![filled("› Write tests for @filename")];
        assert!(find_filled_standing_anchor(&top, 80).is_none());
    }

    // diff·git·노트 TUI 의 대시 구분선 쌍은 사이에 ASCII '>'(인용·프롬프트)가
    // 있어도 입력박스로 오인하지 않는다 — 거노 2026-07-22: 뜬금없는 빈 초록
    // 사각형(style_prompt_box 오발동) 회귀 방지.
    #[test]
    fn plain_dash_rules_ignored() {
        let rows = vec![
            row_from("web/public/cast/ += 캐릭터 12장"),
            row_from(&"─".repeat(30)),
            row_from(" > some diff line here"),
            row_from(&"─".repeat(30)),
            row_from("Notes: press n to add notes"),
        ];
        assert!(prompt_box(&rows).is_none());
    }
}

#[cfg(test)]
mod cross_session_msg_tests {
    use super::*;

    /// 실제 transcript 에 박히는 원문 그대로(2026-08-09 채집).
    const REAL: &str = "Another Claude session sent a message:\n\
<cross-session-message from=\"uds:/tmp/cc-socks/27516.sock\" from-name=\"타이틀 생성 푸시\" from-mode=\"bypass\">\n\
ROUNDTRIP-OK\n\
</cross-session-message>\n\
This came from another Claude session";

    #[test]
    fn peer_label_picks_up_cross_session_body() {
        // 화면엔 `@peer` 로 뜨므로 sender 는 그 라벨이다 — 이름 대조로는 절대 안 걸린다.
        let m = extract_teammate_msg(REAL, PEER_LABEL).expect("본문을 못 뽑았다");
        assert_eq!(m.body, "ROUNDTRIP-OK");
        // color 는 cross-session 태그에 아예 없다.
        assert!(m.color.is_none());
    }

    #[test]
    fn real_sender_comes_from_socket_pid_not_from_name() {
        // from-name 은 세션 이름이라 자동 제목에 덮인다 — 이 검체가 그 실물이다
        // (진짜 발신자는 aru-p107-a2x 인데 「타이틀 생성 푸시」로 실려 왔다).
        // 그래서 되찾기는 소켓 경로의 pid 로만 한다.
        assert_eq!(socket_pid("uds:/tmp/cc-socks/27516.sock"), Some("27516"));
        assert_eq!(socket_pid("uds:/tmp/cc-socks/abc.sock"), None);
        assert_eq!(socket_pid("bridge:whatever"), None);
    }

    #[test]
    fn teammate_tag_still_matches_by_name() {
        // 옛 형식은 이름 대조 그대로 — 쌓인 transcript 를 거슬러 읽을 때 만난다.
        let t = "<teammate-message teammate_id=\"momoi\" color=\"red\">안녕</teammate-message>";
        let m = extract_teammate_msg(t, "momoi").expect("옛 형식이 깨졌다");
        assert_eq!(m.body, "안녕");
        assert_eq!(m.color.as_deref(), Some("red"));
        assert!(m.sender.is_none());
        assert!(extract_teammate_msg(t, "다른사람").is_none());
    }
}
