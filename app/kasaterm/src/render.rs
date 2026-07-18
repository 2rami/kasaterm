//! GPU 렌더 패스 — App 렌더 메서드(cell-renderer 파이프라인 + chrome 오버레이).
//! main.rs 의 impl App 에서 분리. struct App·자유함수·타입은 crate root 그대로 참조.
use super::*;

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
        let preedit_text = self.preedit.clone();
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
            let pane_origin = ws
                .active_pane
                .as_ref()
                .and_then(|aid| {
                    ws.layout.as_ref().and_then(|l| {
                        l.leaves().into_iter().find_map(|n| match n {
                            Layout::Pane { id, x, y, .. } if format!("%{id}") == *aid => {
                                Some((*x, *y))
                            }
                            _ => None,
                        })
                    })
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
                    let (display, prow, pcol) = match &commit_overlay {
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
        // Keep the header breadcrumb's cwd cache fresh (self-rate-limited).
        self.refresh_pane_cwds();
        // File-tree column follows the active pane's cwd (rebuild on change).
        if self.file_tree.visible {
            self.refresh_file_tree();
        }
        // Git column follows the same active-pane cwd; publish it so the
        // off-thread poller refreshes the right repo.
        self.publish_git_col_cwd();
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
        let mut copy_btns: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
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
        // /rename 세션명 아웃라인 (x,y,w,h,color) — 입력박스 위 구분선 이름을 사각 테두리로.
        let mut title_outline_slots: Vec<(f32, f32, f32, f32, [u8; 4])> = Vec::new();
        // working 스피너(✻/braille) 자리 학생 도트(제자리 걸음): 같은 형태.
        let mut spinner_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // 승인 대기(approval prompt) 학생 도트(폴짝 바운스): 같은 형태.
        let mut waiting_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // statusline 자리표시자(U+FFFC) → 학생 프사(bust, 정적 1프레임).
        let mut profile_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // 접힌 팀메시지 줄 hover 시 전문 말풍선 — 커서는 한 곳이라 최대 1개.
        let mut teammate_bubble: Option<TeammateBubbleSlot> = None;
        // 입력박스 위 스페이서 행(effort 칩 자리)에 서 있는 학생(idle 전신 애니).
        let mut standing_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
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
            Option<Vec<String>>,
            (usize, usize),
            Option<((usize, usize), (usize, usize))>,
            f32,
            &'static str,
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
                        Some(vec![(z.to_string(), 0, 0, grid_cols, grid_rows)])
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
                    Option<Vec<String>>,
                    (usize, usize),
                    Option<((usize, usize), (usize, usize))>,
                    f32,
                    f32,
                    &'static str,
                )> = pane.markdown().map(|m| {
                    (
                        m.doc.clone(),
                        m.raw_mode,
                        if m.raw_mode {
                            Some(m.edit_lines.clone())
                        } else {
                            None
                        },
                        (m.cur_line, m.cur_col),
                        m.sel_range(),
                        m.scroll as f32,
                        m.h_scroll,
                        code_lang_for_path(std::path::Path::new(&m.doc.path)),
                    )
                });
                let mut composed: Vec<Vec<GridCell>> = match pane.term() {
                    Some(t) => t.cells.iter().take(rows_now).map(normalise).collect(),
                    None => Vec::new(),
                };
                // statusline 세션 id 마커(⟦8hex⟧) 은닉 — SGR8(conceal)은 claude 의
                // statusline 파이프라인이 속성을 벗겨 텍스트만 남긴다(실측: 마커가
                // 그대로 보임, 거노). 렌더 그리드에서 패턴으로 지운다 — visible_text
                // (백엔드 세션 파싱)는 원본 grid 를 읽으므로 영향 없다. 스캔 창은
                // 하단 8행 — 입력힌트·여백 행이 statusline 을 4행 밖으로 밀어낼 수
                // 있어(백엔드 rebind 스캔창을 3→8 넓힌 것과 같은 이유) 폭을 맞춘다.
                for row in composed.iter_mut().rev().take(8) {
                    let n = row.len();
                    let mut i = 0;
                    while i < n {
                        if row[i].ch == '⟦'
                            && i + 9 < n
                            && (1..=8).all(|k| row[i + k].ch.is_ascii_hexdigit())
                            && row[i + 9].ch == '⟧'
                        {
                            for c in row[i..=i + 9].iter_mut() {
                                c.ch = ' ';
                            }
                            i += 10;
                        } else {
                            i += 1;
                        }
                    }
                }
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
                    .filter(|_| !agents_view)
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
                    // 아래→위 스캔: statusline 은 항상 화면 바닥 쪽이고, 대화
                    // 출력에 U+FFFC 원문이 섞이면(statusline 디버그 출력 등)
                    // 위쪽 행이 앵커를 가로채 얼굴이 엉뚱한 데 붙는다(실사고).
                    if let Some((sr, sc, len)) =
                        composed.iter().enumerate().rev().find_map(|(r, row)| {
                            row.iter().position(|c| c.ch == '\u{fffc}').map(|c0| {
                                let n = row[c0..]
                                    .iter()
                                    .take_while(|c| c.ch == '\u{fffc}')
                                    .count();
                                (r, c0, n)
                            })
                        })
                    {
                        for cell in composed[sr].iter_mut().skip(sc).take(len) {
                            *cell = GridCell::blank();
                        }
                        let face_h = STATUSLINE_FACE_ROWS as f32 * sch;
                        profile_slots.push((
                            slug,
                            (
                                body_left + sc as f32 * scw,
                                (body_top + (sr + 1) as f32 * sch - face_h).max(body_top),
                                len as f32 * scw,
                                face_h,
                            ),
                        ));
                        // 입력박스 위에 서 있는 학생(전신 idle) — 프롬프트 위
                        // 스페이서 행(effort 칩·context 경고가 뜨는 자리) 우측.
                        // statusline 바로 위 행이 아래 테두리(전폭 '─')면 그
                        // 위로 첫 '─' 행이 입력박스 윗 테두리다 — ❯ 영역이
                        // 여러 줄로 자라도 스캔이라 따라간다. 발은 윗 테두리
                        // 줄에 닿고, 칩이 떠 있으면 그 왼쪽으로 비켜 선다.
                        // working/승인대기 중엔 스피너 walk·바운스 도트가 이미
                        // 학생을 그리므로(pet_busy) 세우지 않는다.
                        // max_label: 테두리 줄에 허용하는 비-대시 글자 수.
                        // 아래 테두리는 항상 순수 '─'(0), 윗 테두리는 /rename
                        // 세션명이 "── 학생 ──" 로 박힐 수 있어 짧은 텍스트
                        // 섬을 인정한다 — 순수 rule 만 보면 이름 지은 세션에서
                        // standing 도트가 통째로 사라진다(거노 실사고).
                        let is_rule = |row: &[GridCell], max_label: usize| {
                            let mut dashes = 0usize;
                            let mut label = 0usize;
                            for c in row {
                                match c.ch {
                                    '─' => dashes += 1,
                                    ' ' | '\0' => {}
                                    _ => {
                                        label += 1;
                                        if label > max_label {
                                            return false;
                                        }
                                    }
                                }
                            }
                            dashes > row.len() / 2
                        };
                        if !pet_busy && sr >= 4 && is_rule(&composed[sr - 1], 0) {
                            if let Some(tr) = (sr.saturating_sub(16)..sr - 1)
                                .rev()
                                .find(|&r| is_rule(&composed[r], 24))
                                .filter(|&tr| tr >= 1)
                            {
                                let anchor = tr - 1;
                                let first = composed[anchor]
                                    .iter()
                                    .position(|c| !matches!(c.ch, ' ' | '\0'));
                                let right_c = match first {
                                    Some(f) => f as f32 - 1.5,
                                    None => cols_now as f32 - 1.0,
                                };
                                const STAND_CELLS: f32 = 4.0;
                                let left_c = right_c - STAND_CELLS;
                                let h = INPUT_STANDING_ROWS as f32 * sch;
                                if left_c > 2.0 {
                                    standing_slots.push((
                                        slug,
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
                // Code-block scan is O(cells × distinct-colours) and walks
                // the whole grid twice per frame. It only makes sense on the
                // normal screen — TUIs in alt-screen (claude code TUI mode,
                // vim, less, full-screen apps) get a copy chip per pseudo-
                // block which is never useful. Skipping there reclaims most
                // of render_frame_gpu's time at high update rates.
                let pane_alt = pane.term().map(|t| t.alt_screen).unwrap_or(false);
                if !pane_alt {
                    for block in detect_code_blocks(&composed) {
                        let text = extract_block(&composed, block);
                        if text.trim().is_empty() {
                            continue;
                        }
                        let (start, _end, _left, right) = block;
                        let block_top = body_top + start as f32 * self.cell.h;
                        let block_right = body_left + (right as f32 + 1.0) * self.cell.w;
                        let bx = (block_right - COPY_BTN_W - 4.0).max(body_left);
                        let by = block_top + 3.0;
                        copy_btns.push((text, (bx, by, COPY_BTN_W, COPY_BTN_H)));
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
                            pane.character.as_deref().and_then(|n| {
                                theme::character_accent_n(
                                    n,
                                    theme::character_ordinal(&ws.pane_character, &id),
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
                // 접힌 팀메시지("› Message from @이름", verbose OFF) — 보낸 학생
                // 색으로 "@ 이름❯ 본문…" 인라인 전개(거노: verbose 안 켜고도
                // 읽고 싶다. 클로드코드에 팀메시지만 펼치는 설정은 없음 —
                // verbosity 카테고리는 bash/agent/todo 뿐이라 그리드 재작성으로).
                // 본문은 이 pane transcript tail 의 <teammate-message> 태그에서.
                // 그리드는 reflow 가 안 되니 여러 줄 전개 대신 인라인 한 줄 +
                // 줄에 마우스를 올리면 전문 말풍선(TeammateBubbleSlot).
                {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let msg_path = self.pane_claude_sid.get(id.as_str()).and_then(|sid| {
                        let cwd = self
                            .pane_view_cwd
                            .get(id.as_str())
                            .or_else(|| self.pane_cwd_cache.get(id.as_str()))?;
                        crate::socket::project_jsonl(cwd, sid)
                    });
                    for r in 0..composed.len() {
                        let Some((c0, sender)) = teammate_collapsed_line(&composed[r])
                        else {
                            continue;
                        };
                        let msg = msg_path
                            .as_deref()
                            .and_then(|p| latest_teammate_msg(p, &sender));
                        let accent = teammate_sender_accent(
                            &sender,
                            msg.as_ref().and_then(|m| m.color.as_deref()),
                        );
                        if let Some(m) = &msg {
                            let (cx, cy) = self.cursor_px;
                            let (ry0, ry1) = (
                                body_top + r as f32 * sch,
                                body_top + (r + 1) as f32 * sch,
                            );
                            let row_w = composed[r].len() as f32 * scw;
                            if cy >= ry0
                                && cy < ry1
                                && cx >= body_left
                                && cx < body_left + row_w
                            {
                                teammate_bubble = Some(TeammateBubbleSlot {
                                    sender: sender.clone(),
                                    summary: m.summary.clone(),
                                    body: m.body.clone(),
                                    accent,
                                    anchor: (body_left + c0 as f32 * scw, ry0, ry1),
                                    pane: (
                                        body_left,
                                        body_top,
                                        row_w,
                                        composed.len() as f32 * sch,
                                    ),
                                });
                            }
                        }
                        expand_teammate_message(
                            &mut composed,
                            r,
                            c0,
                            &sender,
                            msg.as_ref().map(|m| m.body.as_str()),
                            accent,
                        );
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
                let prompt_accent = if agents_view {
                    None
                } else {
                    pane.character
                        .as_deref()
                        .and_then(|n| {
                            theme::character_accent_n(
                                n,
                                theme::character_ordinal(&ws.pane_character, &id),
                            )
                        })
                        .filter(|_| {
                            self.pty
                                .get(id.as_str())
                                .and_then(|p| p.active_process_name())
                                .is_some_and(|n| n == "claude")
                        })
                };
                if let Some(accent) = prompt_accent {
                    style_prompt_box(&mut composed, accent);
                    // 입력박스 상단 보더 왼쪽 '─' 구간에 세션 제목 인레이(거노:
                    // @이름칩만으론 이 pane 이 뭘 하는 중인지 안 보임). 라벨
                    // 규칙은 피커와 동일(custom-title > aiTitle > 첫 user) —
                    // Stop hook(title-sync)이 턴마다 custom-title 을 최신
                    // 프롬프트로 갱신해 "지금 하는 작업"이 이 자리에 흐른다.
                    let title = self
                        .pane_claude_sid
                        .get(id.as_str())
                        .and_then(|sid| {
                            let cwd = self
                                .pane_view_cwd
                                .get(id.as_str())
                                .or_else(|| self.pane_cwd_cache.get(id.as_str()))?;
                            crate::socket::project_jsonl(cwd, sid)
                        })
                        .and_then(|p| pane_session_label(&p));
                    if let Some(t) = title.as_deref() {
                        inlay_prompt_box_title(&mut composed, t);
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
                        let dock = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT };
                        (raw_lh - dock - (TITLE_HEIGHT + grid_rows as f32 * self.cell.h)).max(0.0)
                    });
                    base_h + extra
                } else {
                    base_h
                };
                let bottom_inset = if y_cells + eff_h_cells >= grid_rows { 0.0 } else { PANE_INNER_Y };
                let bh = (full_h - header_shift_logical - PANE_INNER_Y - bottom_inset).max(1.0);
                body_rects.push((id.clone(), (bx, by, bw, bh)));
                if let Some(image) = img {
                    image_slots.push((id.clone(), image, (bx, by, bw, bh), img_zoom, img_rot, img_pan));
                }
                if let Some((doc, raw_mode, lines, cursor, sel, scroll, h_scroll, lang)) = md {
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
                    } else if let Some(c) = pane.character.as_ref() {
                        match pane
                            .title
                            .clone()
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|t| !t.is_empty())
                        {
                            Some(t) => format!("{c} · {t}"),
                            None => c.clone(),
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
        // Publish copy-button hit rects for the mouse handler; snapshot the
        // bare rects (+ hover state) for the overlay draw below. Both read
        // from the same numbers so a click lands on what the user sees.
        self.copy_btn_rects = copy_btns;
        let copy_btns_draw: Vec<(f32, f32, f32, f32, bool)> = self
            .copy_btn_rects
            .iter()
            .map(|(_, r)| {
                let hover = self.cursor_px.0 >= r.0
                    && self.cursor_px.0 <= r.0 + r.2
                    && self.cursor_px.1 >= r.1
                    && self.cursor_px.1 <= r.1 + r.3;
                (r.0, r.1, r.2, r.3, hover)
            })
            .collect();
        let toast_alpha = self.copy_toast_alpha();
        // Collab completion toast (top-right). Pre-read here like toast_alpha so
        // the render block below never re-borrows self while g is held.
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
        let show_drop_zone = tab_drag_active && !live_drag;
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
            .filter(|_| !cursor_on_header)
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
                    DropZone::Center => return None,
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
                let (win_right, win_bottom) = self.window.as_ref().map_or(
                    (
                        pad + cols as f32 * self.cell.w,
                        TITLE_HEIGHT + rows as f32 * self.cell.h,
                    ),
                    |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        (
                            w.inner_size().width as f32 / s,
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
        let (sb_tabs, sb_closes, sb_plus) = self.sidebar_layout(sb_win_h);
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
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(i).and_then(|w| w.as_ref())
                };
                layout.map_or(false, |l| {
                    l.leaves().iter().any(|leaf| {
                        self.pane_activity
                            .get(&leaf.to_string())
                            .map_or(false, |a| a.status != "idle" && !a.status.is_empty())
                    })
                })
            })
            .collect();
        // Per-window "just finished" flag: any leaf with a live completion
        // flash. Lights a SUCCESS dot on the window's sidebar tab so a finish
        // in a window you aren't viewing is visible across the strip.
        let sb_done: Vec<bool> = (0..sb_labels.len())
            .map(|i| {
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(i).and_then(|w| w.as_ref())
                };
                layout.map_or(false, |l| {
                    l.leaves()
                        .iter()
                        .any(|leaf| self.notify_flash_factor(&leaf.to_string()).is_some())
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
        let md_preedit = self.preedit.clone();
        // Raw-editor cursor blink phase (shared with the terminal cursor), read
        // before the gpu borrow so the editor cursor blinks in step.
        let raw_cursor_on = self.cursor_blink_on(std::time::Instant::now());
        // In-pane tab hit rects, collected during the header paint (needs the
        // measured tab widths) and published to self after the gpu borrow.
        let mut tab_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        let mut tab_close_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
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
        // Settings screen: sidebar entry rect + (when open) the full-view paint
        // snapshot, both captured before the gpu borrow. The screen's clickable
        // rects come back from paint_settings inside the borrow.
        let win_h_logical = win_px.1 / scale;
        let settings_btn = self.settings_btn_rect(win_h_logical);
        self.settings_btn_rect = settings_btn;
        let settings_ctx = self.settings_open.then(|| self.settings_snapshot(win_px, scale));
        let settings_toggle = self.settings_toggle_rect();
        let file_tree_toggle = self.file_tree_toggle_rect();
        let arona_btn = self.arona_btn_rect();
        let mut settings_rects_out: Vec<(SettingsAction, (f32, f32, f32, f32))> = Vec::new();
        let mut settings_content_h_out: f32 = 0.0;
        // Caret blink for the commit-modal message box, computed before `g`
        // borrows `self.gpu` (the blink helper takes `&self`).
        let commit_caret_on = self.cursor_blink_on(std::time::Instant::now());
        // Per-header completion-flash strength, sampled before `g` borrows
        // `self.gpu` (the header loop can't call `&self` while `g` is live).
        let header_flash: Vec<Option<f32>> =
            headers.iter().map(|h| self.notify_flash_factor(&h.id)).collect();
        // SCHALE OS(아로나) 패널 열림 여부 — gpu 빌림 전에 스냅샷(타이틀바 ✨ 버튼 active 표시).
        let arona_open = self.arona_panel_window.is_some();
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
                // Per-(rotation, frame) cache key — rotated pixels uploaded once
                // per (pane, rotation, gif frame). Static images have one frame
                // so this collapses to the old (pane, rotation) behaviour.
                let cur = image.cur_idx();
                let key = format!("{id}-r{rot}-f{cur}");
                if !g.has_image(&key) {
                    let (rgba, w, h) = rotate_rgba_cw(image.cur_rgba(), image.w, image.h, *rot);
                    g.upload_image(&key, &rgba, w, h);
                }
            }
            g.draw_cells(&slot_views);
            for (id, image, (bx, by, bw, bh), zoom, rot, (pan_x, pan_y)) in &image_slots {
                let key = format!("{id}-r{rot}-f{}", image.cur_idx());
                g.queue_image(&key, *bx, *by, *bw, *bh, *zoom, *pan_x, *pan_y);
            }
            // 학생 도트 — Clawd 배너 자리. idle 4프레임을 캐릭터당 1회 일괄
            // 업로드해 모든 pane이 공유하고, 매 프레임 시간 기반으로 현재
            // 프레임만 queue한다(재렌더는 배너 애니 타이머가 깨워줌).
            // 디코딩 실패 시 queue_image가 조용히 skip.
            let anim_ms = self.version_anim_start.elapsed().as_millis() as u64;
            let anim_idx = (anim_ms / STUDENT_ANIM_FRAME_MS) as usize % STUDENT_IDLE_FRAMES;
            // idle 프레임을 (캐릭터당 1회) 업로드 — 배너와 승인대기 도트가 공유.
            let ensure_idle = |g: &mut gpu::GpuRenderer, slug: &str| {
                if !g.has_image(&format!("student:{slug}:f0")) {
                    if let Some(frames) = student_sprite_frames(slug, "idle") {
                        for (i, (rgba, w, h)) in frames.iter().enumerate() {
                            g.upload_image(&format!("student:{slug}:f{i}"), rgba, *w, *h);
                        }
                    }
                }
            };
            for (slug, (bx, by, bw, bh), (clip_y0, clip_y1)) in &banner_slots {
                ensure_idle(g, slug);
                let key = format!("student:{slug}:f{anim_idx}");
                g.queue_image_clipped(&key, *bx, *by, *bw, *bh, *clip_y0, *clip_y1);
            }
            // agents 뷰 SCHALE 로고 — Clawd 자리(또는 헤더 왼쪽 여백)에 정적 1프레임.
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
            // working 스피너 자리 — walk 프레임 제자리 걸음(분주하게 일하는 중).
            // 셀 위 icon 패스라 blank 처리한 스피너 자리 위에 또렷하게 뜬다.
            let walk_idx =
                (anim_ms as f32 / STUDENT_WALK_FRAME_MS) as usize % STUDENT_WALK_FRAMES;
            for (slug, (bx, by, bw, bh)) in &spinner_slots {
                if !g.has_image(&format!("student:{slug}:walk0")) {
                    if let Some(frames) = student_sprite_frames(slug, "walk") {
                        for (i, (rgba, w, h)) in frames.iter().enumerate() {
                            g.upload_image(&format!("student:{slug}:walk{i}"), rgba, *w, *h);
                        }
                    }
                }
                g.queue_image_above(
                    &format!("student:{slug}:walk{walk_idx}"),
                    *bx, *by, *bw, *bh,
                );
            }
            // 승인 대기 — pane 우상단에서 idle 도트가 폴짝폴짝("봐주세요!").
            // 바닥 정렬이므로 박스 y 를 사인 바운스만큼 올렸다 내린다.
            for (slug, (bx, by, bw, bh)) in &waiting_slots {
                ensure_idle(g, slug);
                let bounce =
                    (anim_ms as f32 / 1000.0 * std::f32::consts::TAU * 1.2).sin().abs() * 7.0;
                g.queue_image_above(
                    &format!("student:{slug}:f{anim_idx}"),
                    *bx, by - bounce, *bw, *bh,
                );
            }
            // 입력박스 위 standing — 전신 idle 애니, 발이 윗 테두리 줄에 닿게
            // 바닥 정렬. 스크롤백 꼬리 행 위에 뜨므로 icon 패스.
            for (slug, (bx, by, bw, bh)) in &standing_slots {
                ensure_idle(g, slug);
                g.queue_image_above(&format!("student:{slug}:f{anim_idx}"), *bx, *by, *bw, *bh);
            }
            // statusline 프사 — bust 96×96 정적 1프레임, 캐릭터당 1회 업로드.
            // 박스가 아래 테두리 행을 침범하는 2행 키라 icon 패스(셀 위) —
            // 이미지 패스(셀 아래)면 테두리 글리프가 얼굴을 가로지른다.
            for (slug, (bx, by, bw, bh)) in &profile_slots {
                let key = format!("student:{slug}:profile");
                if !g.has_image(&key) {
                    if let Some((rgba, w, h)) = student_profile_rgba(slug) {
                        g.upload_image(&key, &rgba, w, h);
                    }
                }
                g.queue_image_above(&key, *bx, *by, *bw, *bh);
            }
            // Markdown is laid out into chrome glyphs/rects here — after the
            // (empty) cell pass, before pane headers/borders so those land on
            // top. The returned content height feeds scroll clamping.
            // Rebuilt fresh each frame so a pane toggled out of raw mode (or
            // closed) drops its caret hit box.
            self.md_body_rects.clear();
            for (id, doc, (bx, by, bw, bh), scroll, raw_mode, lines, cursor, sel, h_scroll, lang) in
                &md_slots
            {
                let content_h = if *raw_mode {
                    let lines = lines.as_deref().unwrap_or(&[]);
                    // Stash the body box so a mouse click can hit-test to a caret
                    // position (md_click_caret reads this).
                    self.md_body_rects.insert(id.clone(), (*bx, *by, *bw, *bh));
                    g.draw_raw_editor(
                        lines, *cursor, *sel, *bx, *by, *bw, *bh, *scroll, *h_scroll, lang,
                        &md_preedit, raw_cursor_on,
                    )
                } else {
                    // Upload this doc's inline images once (keyed per block).
                    for im in &doc.images {
                        if !g.has_image(&im.key) {
                            g.upload_image(&im.key, &im.rgba, im.w, im.h);
                        }
                    }
                    g.draw_markdown(&doc.blocks, *bx, *by, *bw, *bh, *scroll)
                };
                self.md_content_h.insert(id.clone(), content_h);
            }
            // Title strip fill: the unified BG so the top bar reads as one
            // surface with the sidebar and terminal body (no depth seam).
            g.rect(0.0, 0.0, win_px.0 / scale, TITLE_HEIGHT, theme::bg());
            // Sidebar-toggle button, just right of the traffic lights.
            // VSCode / Warp-style glyph: an outlined panel with its left
            // column filled when the sidebar is shown, hollow when hidden.
            // With tabs on top there is no side strip to toggle, so skip it.
            if !self.tabs_on_top {
                let (bx, by, bw, bh) = Self::sidebar_toggle_rect();
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_hover());
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
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_hover());
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
            // SCHALE OS(아로나) 토글 — ✨ 버튼. 터미널↔SCHALE OS 진입점(메뉴 대신).
            // accent 틴트로 항상 눈에 띄게, 패널 열려있으면 active. 우측(설정 왼쪽).
            if let Some((bx, by, bw, bh)) = arona_btn {
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if arona_open {
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::accent());
                } else {
                    let soft = theme::lerp(theme::accent(), theme::bg(), 0.78);
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, soft);
                    if hover {
                        round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_hover());
                    }
                }
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "sparkles",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    if arona_open { theme::bg() } else { theme::accent() },
                );
            }
            // Settings toggle, left of the git-column toggle. Always painted so
            // the screen is reachable even with the sidebar collapsed.
            if let Some((bx, by, bw, bh)) = settings_toggle {
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                let active = self.settings_open;
                if active {
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_active());
                } else if hover {
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_hover());
                }
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "settings-2",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    if hover || active { theme::text() } else { theme::text_dim() },
                );
            }
            // Git-column toggle, parked at the right end of the title strip
            // (the column lives on the right). Hand-drawn "panel-right" glyph —
            // an outlined panel with its right column filled — so it needs no
            // new icon asset (and reads as "right panel" at a glance).
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
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_hover());
                }
                let active = git_col_w > 0.0;
                let fg = if hover || active { theme::text() } else { theme::text_dim() };
                let gs = 15.0_f32;
                let gx = bx + (bw - gs) / 2.0;
                let gy = by + (bh - gs) / 2.0;
                // Outline (fg square hollowed by a BG inset), then the right
                // third filled + a seam line so it reads as a side panel.
                round_rect(g, gx, gy, gs, gs, 3.0, fg);
                round_rect(g, gx + 1.3, gy + 1.3, gs - 2.6, gs - 2.6, 2.0, theme::bg());
                let split = gx + gs * 0.58;
                g.rect(split, gy + 1.3, gx + gs - 1.3 - split, gs - 2.6, fg);
                g.rect(split, gy + 1.3, 1.0, gs - 2.6, fg);
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
                        round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::surface_hover());
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
            // Top bar: folder icon + current working directory, just right of
            // the file-tree toggle (Warp-style location chip). Side-tabs mode
            // only — with top tabs the tabs themselves own this strip space
            // (the cwd still shows on each pane's footer).
            if !self.tabs_on_top {
                let (tbx, _, tbw, _) = file_tree_toggle;
                let px0 = tbx + tbw + 12.0;
                let isz = theme::ICON_SIZE;
                let iy = (TITLE_HEIGHT - isz) / 2.0;
                let ty = (TITLE_HEIGHT - chrome_font) / 2.0;
                g.queue_icon("folder", px0, iy, isz, theme::text_dim());
                let after = px0 + isz;
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
                g.draw_text(
                    after + 6.0,
                    ty,
                    &cwd_str,
                    gpu::DrawOpts {
                        font_size: chrome_font,
                        color: theme::text(),
                        bold: false,
                        italic: false,
                    },
                );
                // Active pane title (OSC 0/2 or shell process name) drawn
                // centered in the title strip — Terminal.app / iTerm UX
                // for single-pane mode. When the workspace is split, each
                // pane carries its own header, so the centered title is
                // redundant but still useful as "which pane has focus".
                // Active pane's accent (surface.set_color) recolors the centered
                // title text too, so single-pane mode matches the per-pane tabs.
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
                                .and_then(|p| p.active_process_name())
                                .is_some_and(|n| n == "claude")
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
                        match work {
                            Some(w) => format!("{c}  ·  {w}"),
                            None => c,
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
                if !title_text.is_empty() {
                    // 포크/백그라운드 세션이면 세션명 뒤에 dim 배지(⑂ = 분기 기호).
                    // 배지 폭까지 포함해 (제목+배지)를 창 중앙 정렬 → 제목만 그릴 때와
                    // 시각적 중심이 유지된다.
                    const BG_BADGE: &str = "  ⑂ bg";
                    let tw = g.measure_chrome_text(&title_text, chrome_font, true);
                    let bw = if title_is_bg {
                        g.measure_chrome_text(BG_BADGE, chrome_font, false)
                    } else {
                        0.0
                    };
                    let win_w_logical = win_px.0 / scale;
                    let center_x = (win_w_logical / 2.0) - (tw + bw) / 2.0;
                    // Don't collide with the left chip cluster.
                    let left_edge = after + 6.0
                        + g.measure_chrome_text(&cwd_str, chrome_font, false)
                        + 24.0;
                    let tx = center_x.max(left_edge);
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
                    theme::RADIUS_MD,
                    theme::surface_active(),
                );
                for (_, label, icon, (ix, iy, iw, ih)) in &shell_menu_layout {
                    let hov = sb_cursor.0 >= *ix
                        && sb_cursor.0 <= *ix + *iw
                        && sb_cursor.1 >= *iy
                        && sb_cursor.1 <= *iy + *ih;
                    if hov {
                        round_rect(g, *ix, *iy, *iw, *ih, theme::RADIUS_MD, theme::surface_hover());
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
                    if is_active {
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_SM, theme::surface_active());
                        g.rect(*tx + 5.0, *ty, *tw - 10.0, ACTIVE_ACCENT_STROKE, theme::accent());
                    } else if is_hover {
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_SM, theme::surface_hover());
                    } else {
                        // Faint resting fill so background tabs still read as
                        // tabs (Windows Terminal-style), not floating labels.
                        let resting = theme::lerp(theme::surface_hover(), theme::bg(), 0.55);
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_SM, resting);
                    }
                    // Unseen-notification pulse, same cadence as the side strip.
                    if sb_alert.get(*i).copied().unwrap_or(false) && raw_cursor_on {
                        let mut c = theme::accent();
                        c[3] = 64;
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_SM, c);
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
                        round_rect(g, icon_x + isz - 3.0, icon_y - 3.0, 6.0, 6.0, 3.0, theme::accent());
                    }
                    if sb_done.get(*i).copied().unwrap_or(false) {
                        round_rect(g, icon_x + isz - 3.0, icon_y + isz - 3.0, 6.0, 6.0, 3.0, theme::success());
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
                                round_rect(g, *cx, *cy, *cw, *ch, theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x22));
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
                    round_rect(g, px, py, pw, ph, theme::RADIUS_SM, theme::surface_hover());
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
            }
            // Window-tab sidebar, Warp-style. Painted first so per-pane
            // headers / rings layer on top at the seam.
            if tab_strip_w > 0.0 {
                // Strip background: the unified BG, set apart from the cell
                // grid only by the right hairline below — not a darker fill.
                g.rect(
                    0.0,
                    TITLE_HEIGHT,
                    tab_strip_w,
                    (sb_win_h - TITLE_HEIGHT).max(0.0),
                    theme::bg(),
                );
                // Right hairline so the strip reads as a distinct column.
                g.rect(
                    tab_strip_w - 1.0,
                    TITLE_HEIGHT,
                    1.0,
                    (sb_win_h - TITLE_HEIGHT).max(0.0),
                    theme::border(),
                );
                let multi = sb_tabs.len() > 1;
                for (i, (tx, ty, tw, th)) in &sb_tabs {
                    let is_active = *i == sb_active;
                    let is_hover = sb_hover == Some(*i);
                    // Selected tab: subtle rounded highlight box (no left
                    // accent bar). Non-selected: flat, only a faint box on
                    // hover. Warp-style.
                    if is_active {
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_MD, theme::surface_active());
                    } else if is_hover {
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_MD, theme::surface_hover());
                    }
                    // Unseen-notification pulse: an accent wash over the tab on
                    // the blink's "on" phase, so a finish/attention in a
                    // background window blinks until the user switches to it.
                    if sb_alert.get(*i).copied().unwrap_or(false) && raw_cursor_on {
                        let mut c = theme::accent();
                        c[3] = 64;
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_MD, c);
                    }
                    // Icon chip: small rounded square with a glyph.
                    let (name, cwd) = sb_labels
                        .get(*i)
                        .cloned()
                        .unwrap_or_else(|| (format!("win {}", i + 1), String::new()));
                    let icon = 30.0_f32;
                    // Icon nudged right to leave a left gutter for the window
                    // ordinal — the number lives in that gutter, not on the chip.
                    let icon_x = *tx + 24.0;
                    let icon_y = *ty + (*th - icon) / 2.0;
                    // Chip contrasts with its backdrop either way: lighter than
                    // the strip on a flat tab, a hair darker than the active box.
                    let chip_bg = if is_active {
                        theme::surface_hover()
                    } else {
                        theme::surface_active()
                    };
                    round_rect(g, icon_x, icon_y, icon, icon, icon / 2.0, chip_bg);
                    g.queue_icon(
                        tab_icon_glyph(&name),
                        icon_x + (icon - theme::ICON_SIZE) / 2.0,
                        icon_y + (icon - theme::ICON_SIZE) / 2.0,
                        theme::ICON_SIZE,
                        theme::text_dim(),
                    );
                    // Window ordinal — a plain bold number in the left gutter,
                    // vertically centered on the icon. No badge circle (that
                    // read as a notification alert) and no overlap with the
                    // chip. `kasaterm-cli windows` lists the same ordinals.
                    let num = format!("{}", *i + 1);
                    let nfs = 14.0_f32;
                    let num_fg = if is_active { theme::text() } else { theme::text_mute() };
                    let nw = g.measure_chrome_text(&num, nfs, true);
                    g.draw_text(
                        *tx + 12.0 - nw / 2.0,
                        icon_y + (icon - nfs) / 2.0,
                        &num,
                        gpu::DrawOpts { font_size: nfs, color: num_fg, bold: true, italic: false },
                    );
                    // Working dot: this window has a pane mid-task (cross-window
                    // collab). Top-right of the icon chip, opposite the number
                    // badge (top-left) so the two never overlap. Static accent
                    // dot — the flowing bar lives on the in-window pane header.
                    if sb_busy.get(*i).copied().unwrap_or(false) {
                        let dsz = 9.0_f32;
                        let dx = icon_x + icon - dsz + 3.0;
                        let dy = icon_y - 3.0;
                        round_rect(g, dx, dy, dsz, dsz, dsz / 2.0, theme::accent());
                    }
                    // Completion dot: a pane in this window just finished
                    // (notify_flash). SUCCESS green at the bottom-right corner so
                    // it never overlaps the working dot (top-right).
                    if sb_done.get(*i).copied().unwrap_or(false) {
                        let dsz = 9.0_f32;
                        let dx = icon_x + icon - dsz + 3.0;
                        let dy = icon_y + icon - dsz + 3.0;
                        round_rect(g, dx, dy, dsz, dsz, dsz / 2.0, theme::success());
                    }
                    // Two-line label to the right of the icon.
                    let text_x = icon_x + icon + 10.0;
                    let name_fg: [u8; 4] = if is_active {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    let cwd_fg: [u8; 4] = theme::text_mute();
                    let show_close = multi && (is_active || is_hover);
                    // Display-width budget derived from the live sidebar width:
                    // ~8.4 logical px per CJK glyph, minus icon (~50) and the
                    // close × slot (~26 when shown). Reflows on drag-resize.
                    let avail = (self.sidebar_w_logical - 60.0 - if show_close { 26.0 } else { 0.0 }).max(0.0);
                    let name_max = (avail / 8.4).floor().max(2.0) as usize;
                    g.draw_text(
                        text_x,
                        *ty + 11.0,
                        &clip_display_width(&name, name_max),
                        gpu::DrawOpts {
                            font_size: 13.5,
                            color: name_fg,
                            bold: is_active,
                            italic: false,
                        },
                    );
                    if !cwd.is_empty() {
                        g.draw_text(
                            text_x,
                            *ty + 30.0,
                            &clip_display_width(&cwd, ((self.sidebar_w_logical - 60.0).max(0.0) / 6.5).floor().max(4.0) as usize),
                            gpu::DrawOpts {
                                font_size: 11.0,
                                color: cwd_fg,
                                bold: false,
                                italic: false,
                            },
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
                                round_rect(g, *cx, *cy, *cw, *ch, theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x22));
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
                // "+" new-window button under the last tab: flat, faint box on
                // hover, centred glyph.
                let (px, py, pw, ph) = sb_plus;
                let plus_hover = sb_cursor.0 >= px
                    && sb_cursor.0 <= px + pw
                    && sb_cursor.1 >= py
                    && sb_cursor.1 <= py + ph;
                if plus_hover {
                    round_rect(g, px, py, pw, ph, theme::RADIUS_MD, theme::surface_hover());
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
                // Settings entry — same tab-box style as the session tabs, so it
                // reads as the last item in the list. Active (selected) box
                // while the screen is open, faint hover box otherwise.
                // "+" 피커가 열려 있으면 스킵 — 팝업이 이 자리를 덮는데 아이콘 글리프는
                // rect 위 레이어라 비쳐 올라온다([[glyph_dim_layer_trap]] 관례: 가려지는
                // chrome 은 안 그린다).
                if !menu_open {
                    let (bx, by, bw, bh) = settings_btn;
                    let active = self.settings_open;
                    let hover = sb_cursor.0 >= bx
                        && sb_cursor.0 <= bx + bw
                        && sb_cursor.1 >= by
                        && sb_cursor.1 <= by + bh;
                    if active {
                        round_rect(g, bx, by, bw, bh, theme::RADIUS_MD, theme::surface_active());
                    } else if hover {
                        round_rect(g, bx, by, bw, bh, theme::RADIUS_MD, theme::surface_hover());
                    }
                    // Icon chip, matching the session tabs' chip geometry.
                    let icon = 30.0_f32;
                    let icon_x = bx + 24.0;
                    let icon_y = by + (bh - icon) / 2.0;
                    let chip_bg = if active { theme::surface_hover() } else { theme::surface_active() };
                    round_rect(g, icon_x, icon_y, icon, icon, icon / 2.0, chip_bg);
                    g.queue_icon(
                        "settings-2",
                        icon_x + (icon - theme::ICON_SIZE) / 2.0,
                        icon_y + (icon - theme::ICON_SIZE) / 2.0,
                        theme::ICON_SIZE,
                        if active { theme::text() } else { theme::text_dim() },
                    );
                    g.draw_text(
                        icon_x + icon + 10.0,
                        by + (bh - 14.0) / 2.0,
                        "Settings",
                        gpu::DrawOpts {
                            font_size: 14.0,
                            color: if active { theme::text() } else { theme::text_dim() },
                            bold: active,
                            italic: false,
                        },
                    );
                }
            }
            // ── File-tree column ── independent of the tab strip, parked just
            // right of it (VSCode explorer). Root = active pane's cwd; folders
            // first — click a folder to expand, a file to preview. Rows laid
            // out + hit rects cached here (window-tab pattern); the read_dir
            // build lives in refresh_file_tree, never per-frame.
            // Settings screen covers the work area on top, but chrome glyphs
            // (the tree's icons/labels) sit above every rect — so a settings
            // bg can't mask them. Skip the whole column while settings is open
            // or the file-tree text bleeds through ([[glyph_dim_layer_trap]]).
            if tree_col_w > 0.0 && !self.settings_open {
                let col_h = (sb_win_h - TITLE_HEIGHT).max(0.0);
                // Own background + right hairline so the column reads as a
                // distinct pane between the tabs and the cell grid.
                g.rect(tree_col_x, TITLE_HEIGHT, tree_col_w, col_h, theme::bg());
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
                    round_rect(g, row_x, sbx_y, search_w, search_box_h, theme::RADIUS_SM, theme::border());
                    round_rect(g, row_x + 1.0, sbx_y + 1.0, search_w - 2.0, search_box_h - 2.0, theme::RADIUS_SM - 1.0, fill);
                    let ic = if active { theme::text() } else { theme::text_dim() };
                    g.queue_icon("folder-tree", row_x + 8.0, sbx_y + (search_box_h - 14.0) / 2.0, 14.0, ic);
                    let mut shown = self.file_tree.search_query.clone();
                    if active && self.in_preedit {
                        shown.push_str(&self.preedit);
                    }
                    let caret_w = g.measure_chrome_text(&shown, 13.0, false);
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
                            round_rect(g, bx, bty, btn_sz, btn_sz, theme::RADIUS_SM, theme::surface_hover());
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
                    round_rect(g, row_x, iy, row_w, item_h, theme::RADIUS_SM, theme::surface_active());
                    g.rect(row_x, iy + 2.0, 2.0, item_h - 4.0, theme::accent());
                    g.queue_icon(if is_dir { "folder" } else { "file" }, row_x + 18.0, iy + (item_h - 16.0) / 2.0, 16.0, theme::text());
                    let mut shown = buf.clone();
                    if self.in_preedit {
                        shown.push_str(&self.preedit);
                    }
                    let caret_w = g.measure_chrome_text(&shown, 13.0, false);
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
                let start_y = tree_top;
                let win_h = win_px.1 / scale;
                let step = 14.0_f32; // per-depth indent width
                let mut rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))> = Vec::new();
                // `file_tree_nodes` already holds the right set: a query swaps it
                // for whole-tree search hits (file_tree_search_collect), empty
                // restores the expanded tree. So just render it as-is.
                let vis_nodes: Vec<&FileNode> = self.file_tree.nodes.iter().collect();
                // File the focused pane is currently showing — its row gets an
                // active tint + accent bar so the sidebar tracks the open file.
                // Inlined (not the `active_preview_path` helper) so it borrows
                // only `self.ws`, disjoint from the `g` mutable borrow alive here.
                let active_file: Option<std::path::PathBuf> = self.ws.lock().ok().and_then(|ws| {
                    ws.active_pane
                        .as_ref()
                        .and_then(|id| ws.panes.get(id).and_then(|p| p.preview_path.clone()))
                });
                for (idx, node) in vis_nodes.iter().enumerate() {
                    let node = *node;
                    let y = start_y - self.file_tree.scroll + idx as f32 * item_h;
                    if y + item_h < start_y || y > win_h {
                        continue; // off-screen → clip (and don't cache a hit rect)
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
                        round_rect(g, row_x, y, row_w, item_h, theme::RADIUS_SM, theme::surface_hover());
                    } else if is_open || is_selected {
                        round_rect(g, row_x, y, row_w, item_h, theme::RADIUS_SM, theme::surface_active());
                    } else if expanded {
                        round_rect(g, row_x, y, row_w, item_h, theme::RADIUS_SM, theme::with_alpha(theme::surface_hover(), 0x33));
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
                        g.queue_icon("folder", icon_x, iy, isz, icon_color);
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
                    let fg = if node.ignored {
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
                    let avail = (row_x + row_w - text_x - 4.0).max(0.0);
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
                        let mut shown = name;
                        if self.in_preedit {
                            shown.push_str(&self.preedit);
                        }
                        let caret_w = g.measure_chrome_text(&shown, font, false);
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
                        round_rect(g, tree_col_x + tree_col_w - 6.0, thumb_y, 3.5, thumb_h, 1.75, theme::with_alpha(theme::text(), 0x66));
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
                    // (action, label, danger, separator-before)
                    let items: [(crate::FtMenuAction, &str, bool, bool); 6] = [
                        (crate::FtMenuAction::NewFile, "새 파일", false, false),
                        (crate::FtMenuAction::NewFolder, "새 폴더", false, false),
                        (crate::FtMenuAction::Rename, "이름 변경", false, true),
                        (crate::FtMenuAction::CopyPath, "경로 복사", false, false),
                        (crate::FtMenuAction::Reveal, reveal_label, false, false),
                        (crate::FtMenuAction::Delete, "", true, true),
                    ];
                    let mih = 28.0_f32;
                    let sep = 7.0_f32;
                    let pad = 6.0_f32;
                    let menu_w = 200.0_f32;
                    let nsep = items.iter().filter(|(_, _, _, s)| *s).count() as f32;
                    let menu_h = pad * 2.0 + items.len() as f32 * mih + nsep * sep;
                    let win_w = win_px.0 / scale;
                    let mx = rawx.min(win_w - menu_w - 6.0).max(tree_col_x + 2.0);
                    let my = rawy.min(win_h - menu_h - 6.0).max(TITLE_HEIGHT + 2.0);
                    round_rect(g, mx, my, menu_w, menu_h, theme::RADIUS_MD, theme::surface());
                    // Hairline border so it reads as a floating layer over the rows.
                    g.rect(mx, my, menu_w, 1.0, theme::with_alpha(theme::border(), 0xCC));
                    g.rect(mx, my + menu_h - 1.0, menu_w, 1.0, theme::with_alpha(theme::border(), 0xCC));
                    g.rect(mx, my, 1.0, menu_h, theme::with_alpha(theme::border(), 0xCC));
                    g.rect(mx + menu_w - 1.0, my, 1.0, menu_h, theme::with_alpha(theme::border(), 0xCC));
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
                            round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_SM, theme::surface_hover());
                        }
                        let lbl = if matches!(action, crate::FtMenuAction::Delete) {
                            del_label.as_str()
                        } else {
                            label
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
            if git_col_w > 0.0 {
                let dock_h = if self.docked.is_empty() && self.zoomed_pane.is_none() { 0.0 } else { DOCK_HEIGHT };
                let gcx0 = git_col_x + 14.0;
                let gcw = (git_col_w - 28.0).max(0.0);
                let top = TITLE_HEIGHT;
                let bottom = (win_px.1 / scale - dock_h).max(top);
                // Background + left hairline so the column reads as its own pane.
                g.rect(git_col_x, top, git_col_w, bottom - top, theme::bg());
                g.rect(git_col_x, top, 1.0, bottom - top, theme::border());
                let red = [229, 83, 75, 255];
                let mut y = top + 10.0;
                // ── Row 1: ~path : branch  ····  N · +ins -del   ⤢ ✕
                // Path click → repo picker, branch click → switcher (rects below).
                {
                    let bi = 15.0_f32;
                    let close_x = git_col_x + git_col_w - 12.0 - bi;
                    let expand_x = close_x - bi - 8.0;
                    let bhov = |bx: f32| {
                        self.cursor_px.0 >= bx - 3.0
                            && self.cursor_px.0 <= bx + bi + 3.0
                            && self.cursor_px.1 >= y - 3.0
                            && self.cursor_px.1 <= y + bi + 3.0
                    };
                    g.queue_icon("maximize", expand_x, y, bi, if bhov(expand_x) { theme::text() } else { theme::text_mute() });
                    g.queue_icon("x", close_x, y, bi, if bhov(close_x) { theme::text() } else { theme::text_mute() });
                    self.git.col_expand_rect = Some((expand_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
                    self.git.col_close_rect = Some((close_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
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
                        let sx0 = expand_x - 12.0 - total;
                        if sx0 > bend + 14.0 {
                            g.queue_icon("file-text", sx0, y, 12.0, theme::text_mute());
                            let mut sx = sx0 + 16.0;
                            sx = g.draw_text(sx, y, &fnum, gpu::DrawOpts { font_size: 12.0, color: theme::text_dim(), bold: false, italic: false });
                            sx = g.draw_text(sx + 4.0, y, "·", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                            sx = g.draw_text(sx + 4.0, y, &plus, gpu::DrawOpts { font_size: 12.0, color: theme::success(), bold: false, italic: false });
                            g.draw_text(sx + 4.0, y, &minus, gpu::DrawOpts { font_size: 12.0, color: red, bold: false, italic: false });
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
                    round_rect(g, bx, by, total_w, bh, theme::RADIUS_SM, base);
                    if main_active && mhov { round_rect(g, bx, by, main_w, bh, theme::RADIUS_SM, theme::accent()); }
                    if can_drop && chov { round_rect(g, bx + main_w, by, caret_w, bh, theme::RADIUS_SM, theme::accent()); }
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
                            round_rect(g, scx + ang.cos() * 5.5 - d, scy + ang.sin() * 5.5 - d, d * 2.0, d * 2.0, d, theme::with_alpha(theme::text(), 30 + (a * 220.0) as u8));
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
                        round_rect(g, gcx0, list_top + 4.0, 8.0, 8.0, 4.0, theme::success());
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
                                        round_rect(g, gcx0 - 5.0, ry, gcw + 10.0, item_h, theme::RADIUS_SM, theme::surface_hover());
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
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::RADIUS_SM, theme::surface_active());
                                        }
                                        g.queue_icon(if staged { "minus" } else { "plus" }, ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { theme::text() } else { icon_dim });
                                        stage_rects.push((!staged, path.clone(), (ax - 1.0, ry, aw + 2.0, item_h)));
                                        ax -= aw + agap;
                                    }
                                    {
                                        let bh = self.cursor_px.0 >= ax && self.cursor_px.0 <= ax + aw && self.cursor_px.1 >= ry && self.cursor_px.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::RADIUS_SM, theme::surface_active());
                                        }
                                        g.queue_icon("undo-2", ax + (aw - 13.0) / 2.0, ry + (item_h - 13.0) / 2.0, 13.0, if bh { red } else { icon_dim });
                                        discard_rects.push((path.clone(), untracked, (ax - 1.0, ry, aw + 2.0, item_h)));
                                        ax -= aw + agap;
                                    }
                                    {
                                        let bh = self.cursor_px.0 >= ax && self.cursor_px.0 <= ax + aw && self.cursor_px.1 >= ry && self.cursor_px.1 < ry + item_h;
                                        if bh {
                                            round_rect(g, ax, ry + 2.0, aw, 18.0, theme::RADIUS_SM, theme::surface_active());
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
                                                g.draw_text(rx, ty, &minus, gpu::DrawOpts { font_size: 11.0, color: red, bold: false, italic: false });
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
                                                K::Del => (theme::with_alpha(red, 0x22), "-", red),
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
                        round_rect(g, mx - 1.0, my - 1.0, iw + 2.0, mh + 2.0, theme::RADIUS_MD, theme::with_alpha(theme::border(), 0xFF));
                        round_rect(g, mx, my, iw, mh, theme::RADIUS_MD, theme::surface());
                        let mut iy = my + 4.0;
                        for (icon, label, act) in items {
                            let hov = self.cursor_px.0 >= mx && self.cursor_px.0 <= mx + iw && self.cursor_px.1 >= iy && self.cursor_px.1 <= iy + ih;
                            if hov {
                                round_rect(g, mx + 4.0, iy, iw - 8.0, ih, theme::RADIUS_SM, theme::surface_hover());
                            }
                            g.queue_icon(icon, mx + 14.0, iy + (ih - 15.0) / 2.0, 15.0, theme::text_dim());
                            g.draw_text(mx + 38.0, iy + (ih - 13.0) / 2.0, &label, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                            self.git.commit_menu_rects.push((act, (mx, iy, iw, ih)));
                            iy += ih;
                        }
                    }
                }
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
                    round_rect(g, bx - 1.0, bxy - 1.0, bw + 2.0, bh + 2.0, theme::RADIUS_MD, theme::with_alpha(theme::border(), 0xFF));
                    round_rect(g, bx, bxy, bw, bh, theme::RADIUS_MD, theme::bg());
                    let pad = 22.0_f32;
                    let cx = bx + pad;
                    let cw = bw - pad * 2.0;
                    let mut my = bxy + pad;
                    // Header: icon chip + X
                    round_rect(g, cx, my, 36.0, 36.0, theme::RADIUS_SM, theme::surface_active());
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
                    round_rect(g, tx, my - 2.0, tw, th, th / 2.0, if on { theme::accent() } else { theme::surface_active() });
                    let knob = th - 6.0;
                    let kx = if on { tx + tw - knob - 3.0 } else { tx + 3.0 };
                    round_rect(g, kx, my - 2.0 + 3.0, knob, knob, knob / 2.0, [255, 255, 255, 255]);
                    self.git.commit_modal_rects.push((GitModalBtn::IncludeUnstaged, (tx - 4.0, my - 5.0, tw + 8.0, th + 8.0)));
                    my += 28.0;
                    // File list box
                    let lh = (bh * 0.28).min(180.0).max(60.0);
                    round_rect(g, cx - 1.0, my - 1.0, cw + 2.0, lh + 2.0, theme::RADIUS_SM, theme::with_alpha(theme::border(), 0xFF));
                    round_rect(g, cx, my, cw, lh, theme::RADIUS_SM, theme::surface());
                    let nf = git_view.staged.len() + git_view.unstaged.len();
                    let mut fx = g.draw_text(cx + 12.0, my + 10.0, &format!("{} files", nf), gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false });
                    fx = g.draw_text(fx + 10.0, my + 10.0, &format!("+{}", git_view.insertions), gpu::DrawOpts { font_size: 13.0, color: theme::success(), bold: false, italic: false });
                    g.draw_text(fx + 8.0, my + 10.0, &format!("-{}", git_view.deletions), gpu::DrawOpts { font_size: 13.0, color: red, bold: false, italic: false });
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
                                g.draw_text(rx, ly, &minus, gpu::DrawOpts { font_size: 12.0, color: red, bold: false, italic: false });
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
                        round_rect(g, cx - 1.0, my - 1.0, cw + 2.0, inh + 2.0, theme::RADIUS_SM, theme::accent());
                    }
                    round_rect(g, cx, my, cw, inh, theme::RADIUS_SM, theme::surface());
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
                        round_rect(g, cx, my, cw, bbh, theme::RADIUS_SM, if hov { theme::surface_hover() } else { theme::surface_active() });
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
                    round_rect(g, conf_x, cby, confirm_w, 34.0, theme::RADIUS_SM, if conf_hov { theme::accent() } else { theme::surface_active() });
                    let wconf = g.measure_chrome_text("Confirm", 13.0, true);
                    g.draw_text(conf_x + (confirm_w - wconf) / 2.0, cby + 10.0, "Confirm", gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false });
                    self.git.commit_modal_rects.push((GitModalBtn::Confirm, (conf_x, cby, confirm_w, 34.0)));
                }
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
                    let x_reserve = if reserve_x { close_w + 8.0 } else { 0.0 };
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
                    if show_x {
                        let close_x = cx + 8.0;
                        // Hover chip behind the × — same lift the +button gets,
                        // so the close target reads as clickable on hover.
                        let chip = icon_size + 6.0;
                        let chip_x = close_x + (icon_size - chip) / 2.0;
                        let chip_y = h.y + (PANE_HEADER_HEIGHT - chip) / 2.0;
                        let (mx, my) = self.cursor_px;
                        let x_hover =
                            mx >= chip_x && mx <= chip_x + chip && my >= chip_y && my <= chip_y + chip;
                        if x_hover {
                            round_rect(g, chip_x, chip_y, chip, chip, theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x22));
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
                    round_rect(g, plus_rect.0, plus_rect.1, plus_rect.2, plus_rect.3,
                        theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x22));
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
                        round_rect(g, chip_x, chip_y, chip_size, chip_size,
                            theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x22));
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
                    round_rect(g, sx, seg_y, seg_w, seg_h, theme::RADIUS_SM, theme::surface());
                    let ty = seg_y + (seg_h - seg_font) / 2.0;
                    for (label, lw, raw) in
                        [("Rendered", md_rendered_w, false), ("Raw", md_raw_w, true)]
                    {
                        let cell_w = lw + seg_pad * 2.0;
                        let active = h.md_raw_mode == raw;
                        let hover = inside(sx, seg_y, cell_w, seg_h);
                        if active {
                            round_rect(g, sx, seg_y, cell_w, seg_h,
                                theme::RADIUS_SM, theme::surface_hover());
                        } else if hover {
                            round_rect(g, sx, seg_y, cell_w, seg_h,
                                theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x18));
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
            // active_pane + pane 별 캐릭터명을 한 lock 으로 스냅샷 — 아래 pane 테두리
            // 루프가 g(=&mut self.gpu) 안이라 self 재borrow 불가. character_accent 폴백용.
            let (active_pane, pane_chars) = self
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
                            self.pane_claude_sid
                                .get(id)
                                .and_then(|sid| {
                                    kasa_mcp::character::session_character(sid)
                                })
                                .or_else(|| {
                                    let view = self
                                        .pty
                                        .get(id)
                                        .map(|p| p.is_claude_agents())
                                        .unwrap_or(false);
                                    if view {
                                        None
                                    } else {
                                        w.pane_character.get(id).cloned()
                                    }
                                })
                                .map(|c| (id.clone(), c))
                        })
                        .collect();
                    (w.active_pane.clone(), chars)
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
                    self.pty
                        .get(id.as_str())
                        .and_then(|p| p.active_process_name())
                        .is_some_and(|n| n == "claude")
                })
                .map(|(id, ..)| id.clone())
                .collect();
            // 헤더를 실제로 그린 pane 집합 — 헤더 working bar 가 거기 뜨므로 footer 로딩바는
            // 이 pane 들을 건너뛴다. `ws.panes.has_header()` 가 아니라 방금 그린 `headers`
            // (pty_layout 기반)에서 뽑아야 ws.panes↔pty_layout 데싱크로 한 pane 에 헤더(위)·
            // footer(아래) 스윕바가 동시에 뜨는 "로딩바 두개" 버그가 안 난다(거노).
            let headered: std::collections::HashSet<String> =
                headers.iter().map(|h| h.id.clone()).collect();
            if self.settings_open {
                self.pane_handle_rects.clear();
                self.pane_top_zones.clear();
                self.handle_menu_hits.clear();
            } else {
                let (hmx, hmy) = self.cursor_px;
                let accent = theme::accent_color(theme::accent_name());
                // 로딩바 스윕 위상 — 프로세스 시작 기준 단조증가 초. working pane 이
                // 있으면 about_to_wait 가 ~30fps 펌프하므로 매 프레임 갱신된다.
                static ANIM_EPOCH: std::sync::LazyLock<Instant> =
                    std::sync::LazyLock::new(Instant::now);
                let anim_phase = ANIM_EPOCH.elapsed().as_secs_f32();
                let mut handle_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                let mut zones: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                let mut menu_hits: Vec<(ActionKind, (f32, f32, f32, f32))> = Vec::new();
                const HANDLE: f32 = 22.0;
                const HMARGIN: f32 = 5.0;
                for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                    // pane 테두리 — 포커스된(active) claude pane 만 자기 학생 고정색
                    // 테두리(지금 어느 pane 을 보고 있는지 한눈에). 비활성·순수 셸은
                    // 무테두리 — 여러 pane 이 동시에 테두리를 둘러 지저분하던 걸 정리(거노).
                    if is_split
                        && active_pane.as_deref() == Some(fid.as_str())
                        && claude_panes.contains(fid.as_str())
                    {
                        // agents 목록 뷰는 SCHALE 블루 고정, 그 외엔 배정 학생색.
                        let border_col = if agents_view_panes.contains(fid.as_str()) {
                            theme::character_accent("샬레")
                        } else {
                            pane_chars.get(fid.as_str()).and_then(|n| {
                                theme::character_accent_n(
                                    n,
                                    theme::character_ordinal(&pane_chars, fid),
                                )
                            })
                        };
                        if let Some(col) = border_col {
                            let t = 1.5_f32;
                            g.rect(*fx, *fy, *fw, t, col);
                            g.rect(*fx, fy + fbox_h - t, *fw, t, col);
                            g.rect(*fx, *fy, t, *fbox_h, col);
                            g.rect(fx + fw - t, *fy, t, *fbox_h, col);
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
                        let items = [
                            ("plus", ActionKind::NewTab),
                            // columns-2(세로선=좌우 2칸) → Horizontal(right),
                            // rows-2(가로선=상하 2칸) → Vertical(bottom). 아이콘이
                            // 곧 결과 배치다 — SplitDir 이름과는 반대 매핑.
                            ("columns-2", ActionKind::SplitH),
                            ("rows-2", ActionKind::SplitV),
                            (sb_icon, ActionKind::ToggleStatusbar),
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
                        // pane 가장자리 안으로 클램프(좌측/우측 끝 pane).
                        mx = mx.max(*fx + 2.0).min(*fx + *fw - mw - 2.0);
                        let my = hy + HANDLE + 3.0;
                        round_rect(g, mx, my, mw, mh, theme::RADIUS_SM, theme::border());
                        round_rect(g, mx + 1.0, my + 1.0, mw - 2.0, mh - 2.0,
                            theme::RADIUS_SM - 1.0, theme::surface_hover());
                        let mut bx2 = mx + pad;
                        let by2 = my + pad;
                        for (icon, act) in items {
                            let on = hmx >= bx2 && hmx <= bx2 + bw && hmy >= by2 && hmy <= by2 + bh;
                            if on {
                                round_rect(g, bx2, by2, bw, bh, theme::RADIUS_SM, theme::surface_active());
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
                // active pane 파란 보더(1.5px)를 footer 배경이 덮지 않게 좌우·하단을
                // 보더 두께만큼 안쪽으로 그린다 — 안 그러면 나중에 그려지는 footer bg 가
                // 보더의 하단·좌우 끝을 덮어 "파란선이 하단바를 제외하고 감싸는"것처럼
                // 보인다(거노). active 가 아니면 inset 0.
                let bt = if is_split && active_pane.as_deref() == Some(fid.as_str()) { 1.5_f32 } else { 0.0 };
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, PANE_FOOTER_HEIGHT - bt, theme::bg());
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, 1.0, theme::border());
                // Pill metrics shared by every chip.
                let pill_h = 18.0_f32;
                let pill_y = bar_y + (PANE_FOOTER_HEIGHT - pill_h) / 2.0;
                let icon_sz = 13.0_f32;
                let pad_x = 9.0_f32;
                let icon_gap = 6.0_f32;
                let chip_gap = 7.0_f32;
                let font = 12.0_f32;
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
                    round_rect(g, cx, pill_y, pw, pill_h, theme::RADIUS_SM, theme::border());
                    round_rect(g, cx + 1.0, pill_y + 1.0, pw - 2.0, pill_h - 2.0,
                        theme::RADIUS_SM - 1.0,
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
                        round_rect(g, cx, pill_y, pw, pill_h, theme::RADIUS_SM, theme::border());
                        round_rect(g, cx + 1.0, pill_y + 1.0, pw - 2.0, pill_h - 2.0,
                            theme::RADIUS_SM - 1.0,
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
                        round_rect(g, cx, pill_y, pw, pill_h, theme::RADIUS_SM, theme::border());
                        round_rect(g, cx + 1.0, pill_y + 1.0, pw - 2.0, pill_h - 2.0,
                            theme::RADIUS_SM - 1.0,
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
                        round_rect(g, h_x - 4.0, h_y - 2.0, h_sz + 8.0, h_sz + 4.0,
                            theme::RADIUS_SM, theme::with_alpha(theme::text(), 0x22));
                    }
                    g.queue_icon("chevrons-down-up", h_x, h_y, h_sz,
                        if h_hover { theme::text() } else { theme::text_mute() });
                    self.statusbar.toggle_rects
                        .push((fid.clone(), (h_x - 4.0, bar_y, h_sz + 12.0, PANE_FOOTER_HEIGHT)));
                }
            }
            Self::paint_gpu_overlays(g, &overlay);
            // Code-block copy buttons, painted on top of the inactive-pane
            // veil so they stay legible everywhere. The icon is two
            // overlapping squares drawn from rects (font glyphs map
            // unreliably in this renderer — see CLAUDE.md box-drawing note).
            for (bx, by, bw, bh, hover) in &copy_btns_draw {
                let (bx, by, bw, bh) = (*bx, *by, *bw, *bh);
                let bg = if *hover {
                    theme::surface_hover()
                } else {
                    theme::with_alpha(theme::surface_active(), 0xE0)
                };
                round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, bg);
                let fg = if *hover { theme::text() } else { theme::text_dim() };
                let s = 8.0; // square side
                let off = 2.5; // overlap offset
                let t = 1.3; // stroke
                let gx = bx + (bw - (s + off)) / 2.0;
                let gy = by + (bh - (s + off)) / 2.0;
                // Back square (up-right), outline only.
                let (r1x, r1y) = (gx + off, gy);
                g.rect(r1x, r1y, s, t, fg);
                g.rect(r1x, r1y + s - t, s, t, fg);
                g.rect(r1x, r1y, t, s, fg);
                g.rect(r1x + s - t, r1y, t, s, fg);
                // Front square (down-left): refill the chip bg first so it
                // reads as sitting on top, then outline.
                let (r2x, r2y) = (gx, gy + off);
                g.rect(r2x, r2y, s, s, bg);
                g.rect(r2x, r2y, s, t, fg);
                g.rect(r2x, r2y + s - t, s, t, fg);
                g.rect(r2x, r2y, t, s, fg);
                g.rect(r2x + s - t, r2y, t, s, fg);
            }
            // 접힌 팀메시지 hover 말풍선 — 전문을 보낸 학생색 링의 박스로.
            // 그리드는 reflow 불가라 항시 전개 대신 hover 전개(인라인 한 줄로
            // 못 다 보여준 본문을 여기서). pane 아래 공간이 모자라면 줄 위로.
            if let Some(b) = &teammate_bubble {
                let font = 12.5_f32;
                let lh = 18.0_f32;
                let pad = 12.0_f32;
                let max_w = (b.pane.2 - 40.0).clamp(160.0, 620.0);
                let header = if b.summary.is_empty() {
                    format!("@ {}", b.sender)
                } else {
                    format!("@ {}  {}", b.sender, b.summary)
                };
                let mut lines = wrap_chrome_text(g, &b.body, max_w, font);
                const MAX_LINES: usize = 18;
                if lines.len() > MAX_LINES {
                    lines.truncate(MAX_LINES);
                    if let Some(last) = lines.last_mut() {
                        last.push('…');
                    }
                }
                let hw = g.measure_chrome_text(&header, font, true);
                let text_w = lines
                    .iter()
                    .map(|l| g.measure_chrome_text(l, font, false))
                    .fold(hw, f32::max)
                    .min(max_w);
                let w = text_w + pad * 2.0;
                let h = pad * 2.0 + lh * (lines.len() as f32 + 1.0) + 6.0;
                let (ax, ay0, ay1) = b.anchor;
                let x = ax.min(b.pane.0 + b.pane.2 - w).max(b.pane.0);
                let mut y = ay1 + 4.0;
                if y + h > b.pane.1 + b.pane.3 {
                    y = (ay0 - 4.0 - h).max(b.pane.1);
                }
                round_rect(
                    g, x - 1.5, y - 1.5, w + 3.0, h + 3.0, theme::RADIUS_MD + 1.5,
                    [b.accent[0], b.accent[1], b.accent[2], 230],
                );
                round_rect(g, x, y, w, h, theme::RADIUS_MD, theme::surface());
                g.draw_text(
                    x + pad, y + pad, &header,
                    gpu::DrawOpts { font_size: font, color: b.accent, bold: true, italic: false },
                );
                for (i, l) in lines.iter().enumerate() {
                    g.draw_text(
                        x + pad, y + pad + 6.0 + lh * (i as f32 + 1.0), l,
                        gpu::DrawOpts { font_size: font, color: theme::text(), bold: false, italic: false },
                    );
                }
            }
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
                    round_rect(g, menu_x, menu_y, menu_w, menu_h, theme::RADIUS_MD, theme::surface());
                    round_rect(g, menu_x, menu_y, menu_w, menu_h, theme::RADIUS_MD, theme::with_alpha(theme::border(), 0xFF));
                    let rows_top = menu_y + 4.0 + search_h;
                    // Inset search field + live query (or dim placeholder). Typing
                    // anywhere while the picker is open feeds this (forward_key).
                    if is_path {
                        let fy = menu_y + 6.0;
                        let fh = search_h - 8.0;
                        round_rect(g, menu_x + 8.0, fy, menu_w - 16.0, fh, theme::RADIUS_SM, theme::bg());
                        g.queue_icon("folder-tree", menu_x + 16.0, fy + (fh - 14.0) / 2.0, 14.0, theme::text_dim());
                        let mut shown = self.statusbar.menu_search.clone();
                        if self.in_preedit {
                            shown.push_str(&self.preedit);
                        }
                        let (txt, col) = if shown.is_empty() {
                            ("디렉터리 검색…".to_string(), theme::text_mute())
                        } else {
                            (shown, theme::text())
                        };
                        g.draw_text(menu_x + 38.0, fy + (fh - 13.0) / 2.0, &txt,
                            gpu::DrawOpts { font_size: 13.0, color: col, bold: false, italic: false });
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
                            round_rect(g, row.0 + 2.0, row.1, row.2 - 4.0, row.3, theme::RADIUS_SM, theme::accent());
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
                        round_rect(g, track_x, thumb_y, 3.0, thumb_h, 1.5, theme::with_alpha(theme::text(), 0x55));
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
                // CJK glyphs ~1em wide; pad generously so the pill never clips.
                let text_w = msg.chars().count() as f32 * t_font;
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
                    theme::RADIUS_MD,
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
                        theme::RADIUS_MD,
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
                            theme::RADIUS_SM,
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
                            theme::RADIUS_SM,
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
                        theme::RADIUS_MD,
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
            let dock_items: Vec<(String, String, bool)> = if let Some(z) = self.zoomed_pane.clone() {
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
            if !dock_items.is_empty() {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let bar_y = win_h - DOCK_HEIGHT;
                // Confine the dock to the pane-grid band: it must not bleed under
                // the session-tab strip / file tree on the left or the git column
                // on the right. Same bounds the cell grid uses in window_cells().
                let grid_x = sidebar_w + WINDOW_PADDING;
                let grid_right = win_w - git_col_w - WINDOW_PADDING;
                let grid_w = (grid_right - grid_x).max(0.0);
                // BG (not SURFACE): SURFACE is the darkest code-block layer and
                // read as a black gap against the lighter main background. BG
                // matches the rest of the chrome; the top border + raised chips
                // still set the dock apart.
                g.rect(grid_x, bar_y, grid_w, DOCK_HEIGHT, theme::bg());
                g.rect(grid_x, bar_y, grid_w, 1.0, theme::border());
                let chip_h = DOCK_HEIGHT - 12.0;
                let cy = bar_y + 6.0;
                let icon = theme::ICON_SIZE;
                let (mx, my) = (self.cursor_px.0 / scale, self.cursor_px.1 / scale);
                let mut cx = grid_x + 8.0;
                let mut chip_hits = Vec::new();
                let mut chip_close_hits = Vec::new();
                for (id, label, killable) in &dock_items {
                    let lw = g.measure_chrome_text(label, chrome_font, false);
                    let chip_w = if *killable { lw + icon + 24.0 } else { lw + 20.0 };
                    let hover = mx >= cx && mx <= cx + chip_w && my >= cy && my <= cy + chip_h;
                    round_rect(
                        g,
                        cx,
                        cy,
                        chip_w,
                        chip_h,
                        theme::RADIUS_SM,
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
            let v_alpha = version_alpha;
            if v_alpha > 0.0 {
                let label = Self::version_label();
                let v_font = 11.0_f32;
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                // Proportional glyphs, so estimate the run width to right-
                // align: ~0.5em per char is a safe over-estimate for this
                // mono-ish label, padded so it never clips the edge.
                let text_w = label.chars().count() as f32 * v_font * 0.52;
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
            // Confirm-close modal: a dim scrim + centered card with 취소/닫기,
            // queued last so it sits over every pane, overlay and toast.
            if let Some(dlg) = self.confirm_close.clone() {
                // Icons draw after the chrome pass, so the scrim (a chrome rect)
                // can't cover the split/action glyphs — drop them so nothing
                // bleeds through the modal.
                g.clear_icons();
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(0.0, 0.0, win_w, win_h, theme::with_alpha([0, 0, 0, 255], 0xB0));
                let card_w = 360.0_f32;
                let card_h = 168.0_f32;
                let cx0 = ((win_w - card_w) / 2.0).round();
                let cy0 = ((win_h - card_h) / 2.0).round();
                round_rect(g, cx0, cy0, card_w, card_h, theme::RADIUS_MD, theme::surface_active());
                let title = format!("{} 실행 중이에요", dlg.proc);
                let subtitle = match dlg.action {
                    crate::PendingClose::Window => "앱을 닫을까요?",
                    _ => "이 탭을 닫을까요?",
                };
                g.draw_text(
                    cx0 + 24.0,
                    cy0 + 30.0,
                    &title,
                    gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
                );
                g.draw_text(
                    cx0 + 24.0,
                    cy0 + 60.0,
                    subtitle,
                    gpu::DrawOpts { font_size: 13.0, color: theme::text_dim(), bold: false, italic: false },
                );
                let (mx, my) = self.cursor_px;
                let bf = 13.0_f32;
                let bpad = 18.0_f32;
                let btn_h = 34.0_f32;
                let btn_y = cy0 + card_h - 20.0 - btn_h;
                // 닫기 (destructive), flush to the card's right edge.
                let close_w = "닫기".chars().count() as f32 * bf + bpad * 2.0;
                let close_x = cx0 + card_w - 24.0 - close_w;
                let close_hover = mx >= close_x
                    && mx <= close_x + close_w
                    && my >= btn_y
                    && my <= btn_y + btn_h;
                round_rect(
                    g,
                    close_x,
                    btn_y,
                    close_w,
                    btn_h,
                    theme::RADIUS_SM,
                    theme::with_alpha(theme::danger(), if close_hover { 0xFF } else { 0xDD }),
                );
                g.draw_text(
                    close_x + bpad,
                    btn_y + (btn_h - bf) / 2.0,
                    "닫기",
                    gpu::DrawOpts { font_size: bf, color: [0xFF, 0xFF, 0xFF, 0xFF], bold: true, italic: false },
                );
                confirm_btn_hits.push((crate::ConfirmBtn::Close, (close_x, btn_y, close_w, btn_h)));
                // 취소, to its left.
                let cancel_w = "취소".chars().count() as f32 * bf + bpad * 2.0;
                let cancel_x = close_x - 10.0 - cancel_w;
                let cancel_hover = mx >= cancel_x
                    && mx <= cancel_x + cancel_w
                    && my >= btn_y
                    && my <= btn_y + btn_h;
                round_rect(
                    g,
                    cancel_x,
                    btn_y,
                    cancel_w,
                    btn_h,
                    theme::RADIUS_SM,
                    if cancel_hover { theme::surface_hover() } else { theme::surface() },
                );
                g.draw_text(
                    cancel_x + bpad,
                    btn_y + (btn_h - bf) / 2.0,
                    "취소",
                    gpu::DrawOpts { font_size: bf, color: theme::text(), bold: false, italic: false },
                );
                confirm_btn_hits.push((crate::ConfirmBtn::Cancel, (cancel_x, btn_y, cancel_w, btn_h)));
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
                    round_rect(g, gx, gy, pill_w, pill_h, theme::RADIUS_SM, theme::accent());
                    round_rect(g, gx + 1.0, gy + 1.0, pill_w - 2.0, pill_h - 2.0,
                        theme::RADIUS_SM - 1.0, theme::with_alpha(theme::surface_active(), 0xF5));
                    g.queue_icon(if is_dir { "folder" } else { "file" },
                        gx + 6.0, gy + (pill_h - 14.0) / 2.0, 14.0, theme::text());
                    g.draw_text(gx + 24.0, gy + (pill_h - gf) / 2.0, &name,
                        gpu::DrawOpts { font_size: gf, color: theme::text(), bold: false, italic: false });
                }
            }
            // Settings screen on top of everything (covers the pane grid; the
            // sidebar to its left stays visible).
            if let Some(ctx) = &settings_ctx {
                (settings_rects_out, settings_content_h_out) = settings::paint_settings(g, ctx);
            }
            if let Err(e) = g.render(&slot_views, scale, time_secs, true) {
                eprintln!("[gpu] render error: {e:?}");
            }
        }
        self.settings_rects = settings_rects_out;
        // Scroll clamp for the wheel handler: content minus the visible form
        // band (area minus the 84px page header), plus a little bottom pad.
        // Re-clamp the live offset too — a window grow or category shrink can
        // strand it past the new max.
        if let Some(ctx) = &settings_ctx {
            let view_h = (ctx.area.3 - 84.0).max(0.0);
            self.settings_scroll_max = (settings_content_h_out - view_h + 24.0).max(0.0);
            if self.settings_scroll > self.settings_scroll_max {
                self.settings_scroll = self.settings_scroll_max;
            }
        }
        self.confirm_btn_rects = confirm_btn_hits;
        self.pane_tab_rects = tab_hits;
        self.pane_tab_close_rects = tab_close_hits;
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
        // Keep the frame loop alive while a git op spins, so the spinner
        // animates until GitOpDone clears it.
        if self.git.op.is_some() {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
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
const CLAWD_COLS: usize = 9;
const CLAWD_ROWS: usize = 3;

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

/// claude TUI 입력박스 행 탐지 — 화면 하단에서 위로 ─ 보더 두 줄을 찾아 그
/// 사이 행 범위를 돌려준다(양끝 보더 행 번호는 range.start-1 / range.end).
/// 사이에 ❯ 프롬프트 마커 행이 있어야 입력박스로 인정한다(권한 메뉴 등
/// 다른 풀폭 박스 오인 방지).
fn prompt_box_rows(rows: &[Vec<GridCell>]) -> Option<std::ops::Range<usize>> {
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
    let b2 = rows.iter().rposition(|r| is_border(r))?;
    let b1 = rows[..b2].iter().rposition(|r| is_border(r))?;
    let range = (b1 + 1)..b2;
    let has_marker = rows[range.clone()].iter().any(|r| {
        r.iter()
            .find(|c| c.ch != ' ' && c.ch != '\0')
            .is_some_and(|c| matches!(c.ch, '❯' | '›' | '>'))
    });
    (has_marker && !range.is_empty()).then_some(range)
}

/// 학생 pane 입력박스의 양끝 보더 행(─ 줄 + @배지)을 claude 가 /color·
/// --agent-color 로 그린 명시색을 **무시하고** 학생 accent 로 강제 도색한다 —
/// pane 정체성 색과 항상 일치. (본문 틴트가 있던 시절엔 사이 행의 입력 글자를
/// 틴트에서 빼는 처리도 여기 있었는데, 본문이 테마 기본 fg 로 돌아가며 폐기.)
fn style_prompt_box(rows: &mut [Vec<GridCell>], accent: [u8; 4]) {
    let Some(range) = prompt_box_rows(rows) else { return };
    let (b1, b2) = (range.start - 1, range.end);
    for i in [b1, b2] {
        for c in rows[i].iter_mut() {
            // 세션명/테두리 줄 배경(claude --agent-color 로 채운 accent 밴드)을
            // 터미널색으로 되돌린다 — 아웃라인(─ 대시·세션명 글자)만 accent 로
            // 두고 배경은 안 칠한다(거노: 배경까지 채우면 글자가 묻힌다).
            c.bg = kasa_bridge::screen::Color::Default;
            if c.ch != ' ' && c.ch != '\0' {
                c.fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
            }
        }
    }
}

/// pane transcript 의 세션 라벨(피커와 동일 규칙: custom-title > aiTitle > 첫
/// user 프롬프트) — 파일 길이가 그대로면 캐시 반환(프레임당 stat 1회), 대화가
/// 자라 길이가 변했을 때만 재파싱(latest_teammate_msg 와 같은 전략).
fn pane_session_label(path: &std::path::Path) -> Option<String> {
    type Cache = std::collections::HashMap<std::path::PathBuf, (u64, Option<String>)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(Default::default);
    let len = std::fs::metadata(path).ok()?.len();
    let mut map = CACHE.lock().ok()?;
    if let Some((l, t)) = map.get(path) {
        if *l == len {
            return t.clone();
        }
    }
    let found = kasa_socket::sessions::session_label_for(path);
    map.insert(path.to_path_buf(), (len, found.clone()));
    found
}

/// 입력박스 상단 보더의 왼쪽 '─' 연속 구간에 세션 제목을 인레이 — 스냅샷 전용,
/// 원본 그리드 무손상(배너 타이틀 치환과 같은 원칙). 오른쪽 @이름칩은 왼쪽
/// run 안에서만 쓰므로 건드리지 않는다. 스타일은 보더 셀 승계 —
/// style_prompt_box 가 학생 accent 로 칠한 뒤 호출되어 칩·보더와 색 언어가
/// 같다. 폭이 모자라면 '…' 말줄임, 대시 여유(양끝 대시+양옆 공백 4칸)조차
/// 없는 극단 폭은 그냥 포기한다.
fn inlay_prompt_box_title(rows: &mut [Vec<GridCell>], title: &str) {
    use unicode_width::UnicodeWidthChar;
    let Some(range) = prompt_box_rows(rows) else { return };
    let row = &mut rows[range.start - 1];
    let Some(l0) = row.iter().position(|c| c.ch == '─') else { return };
    let run = row[l0..].iter().take_while(|c| c.ch == '─').count();
    let Some(avail) = run.checked_sub(4).filter(|a| *a >= 2) else { return };
    let style = row[l0].clone();
    let mk = |ch: char| {
        let mut c = style.clone();
        c.ch = ch;
        c
    };
    let total: usize = title.chars().map(|c| c.width().unwrap_or(1).max(1)).sum();
    let mut cells: Vec<GridCell> = Vec::with_capacity(avail.min(total));
    for ch in title.chars() {
        let w = ch.width().unwrap_or(1).max(1);
        if total > avail && cells.len() + w + 1 > avail {
            cells.push(mk('…'));
            break;
        }
        if cells.len() + w > avail {
            break;
        }
        cells.push(mk(ch));
        // 와이드 글리프 다음 칸은 스페이서(composed 경로 실측은 ' ').
        if w == 2 {
            cells.push(mk(' '));
        }
    }
    if cells.is_empty() {
        return;
    }
    let mut w = l0 + 1;
    row[w] = mk(' ');
    w += 1;
    for cell in cells {
        row[w] = cell;
        w += 1;
    }
    row[w] = mk(' ');
}

/// verbose OFF 에서 접힌 팀메시지 행("› Message from @<이름>") 탐지 —
/// (첫 글리프 col, 보낸이 agent 이름). 이름 뒤에 다른 글자가 있으면(본문 안
/// 인용 등) 접힌 줄이 아니라고 본다 — 오탐이 실제 출력 텍스트를 덮어쓰면 안 된다.
fn teammate_collapsed_line(row: &[GridCell]) -> Option<(usize, String)> {
    let chars: Vec<char> = row
        .iter()
        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
        .collect();
    let first = chars.iter().position(|&c| c != ' ')?;
    if !matches!(chars[first], '›' | '>') {
        return None;
    }
    const LABEL: &[char] = &[
        ' ', 'M', 'e', 's', 's', 'a', 'g', 'e', ' ', 'f', 'r', 'o', 'm', ' ', '@',
    ];
    let ls = first + 1;
    if chars.len() < ls + LABEL.len() || chars[ls..ls + LABEL.len()] != *LABEL {
        return None;
    }
    let ns = ls + LABEL.len();
    let name: String = chars[ns..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '-' || **c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    chars[ns + name.len()..]
        .iter()
        .all(|&c| c == ' ')
        .then_some((first, name))
}

/// 팀원 agent 이름("aru-9c88")의 보낸 학생 accent — 로마자 앞부분(마지막 '-'
/// 앞)을 로스터로 역매핑. 로스터 밖(team-lead 등)은 transcript 태그의 color
/// 명 → 그것도 없으면 테마 accent.
fn teammate_sender_accent(name: &str, tag_color: Option<&str>) -> [u8; 4] {
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
    summary: String,
    body: String,
    color: Option<String>,
}

/// `<teammate-message …>본문</teammate-message>` 파싱 — teammate_id 가 sender 와
/// 일치하는 첫 태그. 속성은 key="value" 나열(순서 무관).
fn extract_teammate_msg(text: &str, sender: &str) -> Option<TeammateMsg> {
    let mut rest = text;
    loop {
        let s = rest.find("<teammate-message")?;
        let after = &rest[s + "<teammate-message".len()..];
        let close = after.find('>')?;
        let attrs = &after[..close];
        let tail = &after[close + 1..];
        let attr = |key: &str| -> Option<String> {
            let pat = format!("{key}=\"");
            let a = attrs.find(&pat)? + pat.len();
            let e = attrs[a..].find('"')?;
            Some(attrs[a..a + e].to_string())
        };
        if attr("teammate_id").as_deref() == Some(sender) {
            let end = tail.find("</teammate-message>").unwrap_or(tail.len());
            return Some(TeammateMsg {
                summary: attr("summary").unwrap_or_default(),
                body: tail[..end].trim().to_string(),
                color: attr("color"),
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
) {
    let fg = kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2]);
    let Some(body) = body else {
        for c in rows[r].iter_mut() {
            if c.ch != ' ' && c.ch != '\0' {
                c.fg = fg.clone();
            }
        }
        return;
    };
    let cols = rows[r].len();
    if start >= cols || cols == 0 {
        return;
    }
    let style = rows[r][start].clone();
    let blank_run = rows[r + 1..].iter().take_while(|w| row_is_blank(w)).count();
    let usable = if r + 1 + blank_run >= rows.len() {
        blank_run
    } else {
        blank_run.saturating_sub(1)
    };
    let header = format!("@ {sender}❯ ");
    let indent = start + 2;
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let (lines, truncated) = wrap_body_cells(
        &flat,
        cols.saturating_sub(start + cell_width(&header)),
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
    let mut w = put_line(&mut rows[r], start, &header, true);
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
}

/// 접힌 팀메시지 hover 말풍선 페이로드 — pane 루프에서 채워 overlay 패스가
/// 그린다(copy 버튼과 같은 슬롯 관례).
struct TeammateBubbleSlot {
    sender: String,
    summary: String,
    body: String,
    accent: [u8; 4],
    /// 접힌 줄의 (x, top, bottom) — 말풍선 앵커.
    anchor: (f32, f32, f32),
    /// pane 본문 rect (x, y, w, h) — 말풍선을 이 안으로 클램프.
    pane: (f32, f32, f32, f32),
}

/// 말풍선 본문 word-wrap — measure 기반, '\n' 존중, 공백 우선 분할(없으면
/// 글자 단위). 빈 문단은 빈 줄로 남아 문단 간격 역할을 한다.
fn wrap_chrome_text(
    g: &mut gpu::GpuRenderer,
    text: &str,
    max_w: f32,
    font: f32,
) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        let mut sp: Option<usize> = None; // 마지막 공백 '뒤' byte 오프셋
        for ch in para.chars() {
            let cw = g.measure_chrome_text(&ch.to_string(), font, false);
            if cur_w + cw > max_w && !cur.is_empty() {
                if let Some(b) = sp.filter(|b| *b < cur.len()) {
                    let tail = cur.split_off(b);
                    out.push(std::mem::take(&mut cur).trim_end().to_string());
                    cur = tail;
                } else {
                    out.push(std::mem::take(&mut cur));
                }
                cur_w = g.measure_chrome_text(&cur, font, false);
                sp = None;
            }
            cur.push(ch);
            cur_w += cw;
            if ch == ' ' {
                sp = Some(cur.len());
            }
        }
        out.push(cur.trim_end().to_string());
    }
    out
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
        let fname = if motion == "walk" {
            format!("{slug}-walk-{i}.png")
        } else {
            format!("{slug}-{i}.png")
        };
        let img = downscale_student(image::open(dir.join(&fname)).ok()?);
        out.push(img.to_rgba8());
    }
    Some(out)
}

/// 캐릭터 슬러그 + 모션 → 컴파일타임 내장 도트 프레임(arona-ui walk
/// 스프라이트의 idle 0..3 / walk-east 0..5).
fn student_sprite_png(slug: &str, motion: &str) -> Option<&'static [&'static [u8]]> {
    macro_rules! idle {
        ($n:literal) => {{
            const F: [&[u8]; STUDENT_IDLE_FRAMES] = [
                include_bytes!(concat!("../assets/students/", $n, "-0.png")),
                include_bytes!(concat!("../assets/students/", $n, "-1.png")),
                include_bytes!(concat!("../assets/students/", $n, "-2.png")),
                include_bytes!(concat!("../assets/students/", $n, "-3.png")),
            ];
            &F
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
            &F
        }};
    }
    Some(match (slug, motion) {
        ("arona", "idle") => idle!("arona"),
        ("prana", "idle") => idle!("prana"),
        ("midori", "idle") => idle!("midori"),
        ("momoi", "idle") => idle!("momoi"),
        ("yuzu", "idle") => idle!("yuzu"),
        ("arisu", "idle") => idle!("arisu"),
        ("yuuka", "idle") => idle!("yuuka"),
        ("shiroko", "idle") => idle!("shiroko"),
        ("hoshino", "idle") => idle!("hoshino"),
        ("koharu", "idle") => idle!("koharu"),
        ("himari", "idle") => idle!("himari"),
        ("aru", "idle") => idle!("aru"),
        ("arona", "walk") => walk!("arona"),
        ("prana", "walk") => walk!("prana"),
        ("midori", "walk") => walk!("midori"),
        ("momoi", "walk") => walk!("momoi"),
        ("yuzu", "walk") => walk!("yuzu"),
        ("arisu", "walk") => walk!("arisu"),
        ("yuuka", "walk") => walk!("yuuka"),
        ("shiroko", "walk") => walk!("shiroko"),
        ("hoshino", "walk") => walk!("hoshino"),
        ("koharu", "walk") => walk!("koharu"),
        ("himari", "walk") => walk!("himari"),
        ("aru", "walk") => walk!("aru"),
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
fn student_profile_rgba(slug: &str) -> Option<(Vec<u8>, u32, u32)> {
    if let Some(r) = user_asset_rgba(&format!("{slug}-profile.png")) {
        return Some(r);
    }
    let img = image::load_from_memory(student_profile_png(slug)?).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
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
        let is_name = |c: &GridCell| !matches!(c.ch, '─' | ' ' | '\0');
        let Some(first) = row.iter().position(&is_name) else { continue };
        let Some(last) = row.iter().rposition(&is_name) else { continue };
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
fn find_clawd_banners(rows: &[Vec<GridCell>]) -> Vec<(isize, usize)> {
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

/// claude agents 목록 화면인지 화면 텍스트로 감지. argv(`is_claude_agents`)는 `claude
/// agents` **명령**만 잡고, 세션 안에서 "← for agents"로 여는 목록 뷰는 같은 프로세스라
/// argv 가 안 바뀌어 못 잡는다(거노: agents view 로고 안 뜸). 목록 상단 통계줄
/// "N awaiting input · N working · N completed" 의 고유 문구를 신호로 쓴다 — 일반
/// 대화엔 statusline(U+FFFC)이 있어 호출부에서 `!has_profile_slot` 로 이미 걸러진다.
fn screen_is_agents_list(rows: &[Vec<GridCell>]) -> bool {
    let full: String = rows.iter().flat_map(|r| r.iter().map(|c| c.ch)).collect();
    full.contains("awaiting input") && full.contains("completed")
}

/// Claude Code 라이브 스피너("✻ Verbing…" 별 dingbat, 또는 braille) 위치 감지 —
/// `rows_show_working`(input.rs)과 같은 신호를 행·열 좌표로 돌려준다. 마지막
/// non-blank 10행, 행 앞머리(col<8)만 본다(본문 인용 별표 오탐 방지). 스피너
/// 셀은 blank 처리하고 그 자리에 학생 working 도트를 얹는 용도.
fn find_claude_spinner(rows: &[Vec<GridCell>]) -> Option<(usize, usize)> {
    let last = rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    let start = (last + 1).saturating_sub(10);
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
            Some((2, "aru-9c88".to_string()))
        );
    }

    // 이름 뒤에 다른 글자가 있으면 본문 안 인용 — 실출력 덮어쓰기 오탐 방지.
    #[test]
    fn rejects_trailing_text_and_plain_lines() {
        let quoted = row_from("› Message from @aru-9c88 라고 떴다", 80);
        assert_eq!(teammate_collapsed_line(&quoted), None);
        let plain = row_from("Message from @aru-9c88", 80);
        assert_eq!(teammate_collapsed_line(&plain), None);
    }

    #[test]
    fn extract_tag_attrs_and_body() {
        let text = "<teammate-message teammate_id=\"aru-9c88\" color=\"orange\" \
                    summary=\"확인 통지\">아루다. 확인했다.</teammate-message>";
        let m = extract_teammate_msg(text, "aru-9c88").unwrap();
        assert_eq!(m.summary, "확인 통지");
        assert_eq!(m.body, "아루다. 확인했다.");
        assert_eq!(m.color.as_deref(), Some("orange"));
        // 다른 보낸이의 태그는 건너뛰고 일치하는 태그만.
        assert!(extract_teammate_msg(text, "yuzu-1ba1").is_none());
    }

    // 인라인 재작성: 헤더 + 본문(한글 와이드 = 글자+스페이서), 원문 잔재 제거.
    #[test]
    fn restyle_writes_inline_body_with_wide_spacers() {
        let mut rows = vec![row_from("› Message from @aru-9c88", 60)];
        expand_teammate_message(&mut rows, 0, 0, "aru-9c88", Some("아루다 확인"), [255, 128, 0, 255]);
        assert_eq!(row_text(&rows[0]), "@ aru-9c88❯ 아 루 다  확 인");
        assert!(rows[0][0].bold, "헤더는 bold");
        assert_eq!(
            rows[0][0].fg,
            kasa_bridge::screen::Color::Rgb(255, 128, 0),
            "학생 accent 로 도색"
        );
    }

    // 이어 쓸 blank 행이 없으면 말줄임으로 끝난다 — 다음 항목 침범 없음.
    #[test]
    fn restyle_truncates_with_ellipsis() {
        let mut rows = vec![
            row_from("› Message from @aru-9c88", 24),
            row_from("다음 항목", 24),
        ];
        expand_teammate_message(&mut rows, 0, 0, "aru-9c88", Some("긴 본문이 들어간다"), [255, 0, 0, 255]);
        let text = row_text(&rows[0]);
        assert!(text.starts_with("@ aru-9c88❯"), "{text}");
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
        assert!(row_text(&rows[0]).starts_with("@ aru-9c88❯ 긴"), "{}", row_text(&rows[0]));
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

// 입력박스 상단 보더 세션 제목 인레이 — 왼쪽 '─' run 에만 쓰고 @이름칩은
// 무손상, 좁으면 '…', 극단 폭은 원문 유지.
#[cfg(test)]
mod prompt_title_inlay_tests {
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

    fn box_rows(width: usize) -> Vec<Vec<GridCell>> {
        vec![
            row_from(&"─".repeat(width)),
            row_from(&format!("❯{}", " ".repeat(width - 1))),
            row_from(&"─".repeat(width)),
        ]
    }

    fn row_text(row: &[GridCell]) -> String {
        row.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn title_inlaid_left_of_border() {
        let mut rows = box_rows(30);
        inlay_prompt_box_title(&mut rows, "제목");
        // 대시 1칸 유지 후 " 제(sp)목(sp) " — 나머지는 보더 그대로.
        assert_eq!(row_text(&rows[0]), format!("─ 제 목  {}", "─".repeat(23)));
        // 아래 보더·본문은 무손상.
        assert!(rows[2].iter().all(|c| c.ch == '─'));
    }

    #[test]
    fn long_title_truncated_with_ellipsis() {
        let mut rows = box_rows(14); // avail = 10
        inlay_prompt_box_title(&mut rows, "가나다라마바사");
        let text = row_text(&rows[0]);
        assert!(text.starts_with("─ 가 나 다 라 …"), "{text}");
        // 오른쪽 끝 대시는 남는다(칩 영역 침범 금지 원칙의 최소 보증).
        assert_eq!(rows[0].last().unwrap().ch, '─');
    }

    #[test]
    fn narrow_border_left_untouched() {
        let mut rows = box_rows(11); // run=11 → avail=7 이지만 최소폭 실험은 5로
        let mut tiny = vec![
            row_from(&"─".repeat(5)),
            row_from("❯    "),
            row_from(&"─".repeat(5)),
        ];
        inlay_prompt_box_title(&mut tiny, "제목");
        // run 5 는 prompt_box_rows 의 dash>=10 미달 — 인식 자체가 안 돼 원문 유지.
        assert!(tiny[0].iter().all(|c| c.ch == '─'));
        // 정상 인식되는 11칸: avail=7, "제목"(4칸)은 들어간다.
        inlay_prompt_box_title(&mut rows, "제목");
        assert!(row_text(&rows[0]).starts_with("─ 제 목 "));
    }

    #[test]
    fn non_input_box_untouched() {
        // ❯ 마커 없는 풀폭 박스(권한 메뉴 등)는 건드리지 않는다.
        let mut rows = vec![
            row_from(&"─".repeat(30)),
            row_from(&format!("no marker{}", " ".repeat(21))),
            row_from(&"─".repeat(30)),
        ];
        inlay_prompt_box_title(&mut rows, "제목");
        assert!(rows[0].iter().all(|c| c.ch == '─'));
    }
}
