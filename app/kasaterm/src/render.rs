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
        ) = snap.unwrap_or((0, 0, false, 80, 0, 0, preedit_text.clone(), 0, 0));
        // When split OR any pane is multi-tab, every pane body is pushed
        // down by its header band. The cursor / preedit / selection
        // overlays anchor off the same origin as the cells, so they must
        // apply the identical shift — otherwise the cursor floats up into
        // the header row (which is exactly what made it appear one line
        // above the actual prompt after a cross-pane tab drop).
        let show_headers = self
            .pty_layout
            .as_ref()
            .is_some_and(|t| t.leaves().len() > 1)
            || self
                .ws
                .lock()
                .ok()
                .map(|ws| ws.panes.values().any(|p| p.tabs.len() > 1))
                .unwrap_or(false);
        let header_shift = if show_headers { PANE_HEADER_HEIGHT } else { 0.0 };
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
        if self.file_tree_visible {
            self.refresh_file_tree();
        }
        // Git column follows the same active-pane cwd; publish it so the
        // off-thread poller refreshes the right repo.
        self.publish_git_col_cwd();
        // Every pane's status bar wants its own repo badge — feed all pane cwds
        // to the same poller.
        self.publish_pane_git_cwds();
        let Some(window) = self.window.as_ref() else { return };
        // Snapshot for the launch banner before the &mut self.gpu borrow
        // below (which rules out re-borrowing &self inside that block).
        let win_size = window.inner_size();
        let win_px = (win_size.width as f32, win_size.height as f32);
        let version_alpha = self.version_alpha();
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
            .git_col_data
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
        // Markdown panes: (id, doc, body box, scroll px, raw_mode, edit lines,
        // cursor, h_scroll px, syntax lang). Render mode draws blocks; Raw mode
        // draws the editor buffer.
        #[allow(clippy::type_complexity)]
        let mut md_slots: Vec<(
            String,
            Arc<MarkdownDoc>,
            (f32, f32, f32, f32),
            f32,
            bool,
            Option<Vec<String>>,
            (usize, usize),
            f32,
            &'static str,
        )> = Vec::new();
        // Per-pane body rect (header-excluded) in logical px, collected for
        // every pane so in-pane WebViews and other overlays can be snapped
        // to their pane after the borrow scope ends.
        let mut body_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let (slots, headers, footer_slots): (
            Vec<PaneSlot>,
            Vec<HeaderInfo>,
            Vec<(String, f32, f32, f32, f32)>,
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
            let any_multitab = leaves
                .iter()
                .any(|(id, _, _, _, _)| ws.panes.get(id).map_or(false, |p| p.tabs.len() > 1));
            // A zoomed pane keeps its header even though it's the only visible
            // leaf — otherwise there's no double-click target to un-zoom.
            let show_headers = leaves.len() > 1 || any_multitab || self.zoomed_pane.is_some();
            let header_shift_px = if show_headers {
                PANE_HEADER_HEIGHT * scale
            } else {
                0.0
            };
            let mut slots = Vec::new();
            let mut headers = Vec::new();
            // Box geometry per leaf (id, x, y, w, h) in logical px — collected
            // for EVERY pane, headered or not, so the per-pane status bar can
            // anchor to the box bottom even on a lone unsplit pane.
            let mut footer_slots: Vec<(String, f32, f32, f32, f32)> = Vec::new();
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
                    let header_px_now = if show_headers { PANE_HEADER_HEIGHT } else { 0.0 };
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
                // raw, cursor, scroll px, h_scroll px, syntax lang).
                let md: Option<(
                    Arc<MarkdownDoc>,
                    bool,
                    Option<Vec<String>>,
                    (usize, usize),
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
                        m.scroll as f32,
                        m.h_scroll,
                        code_lang_for_path(std::path::Path::new(&m.doc.path)),
                    )
                });
                let composed: Vec<Vec<GridCell>> = match pane.term() {
                    Some(t) => t.cells.iter().take(rows_now).map(normalise).collect(),
                    None => Vec::new(),
                };
                // Cells start below the header band when split, and are
                // inset inside the pane box so text never jams the divider
                // or window edge.
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
                let header_shift_logical = if show_headers {
                    PANE_HEADER_HEIGHT
                } else {
                    0.0
                };
                let body_left = WINDOW_PADDING
                    + sidebar_w
                    + x_cells as f32 * self.cell.w
                    + PANE_INNER_X;
                let body_top = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
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
                let pane_font_scale = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                slots.push(PaneSlot {
                    rows: composed,
                    origin_px,
                    // Unfocused panes dim their text only (no box veil). Single
                    // un-split pane is never dimmed.
                    dim: show_headers && active_id.as_deref() != Some(id.as_str()),
                    font_scale: pane_font_scale,
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
                let base_w = w_cells as f32 * self.cell.w;
                let full_w = if x_cells + w_cells >= grid_cols {
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
                let right_inset = if x_cells + w_cells >= grid_cols { 0.0 } else { PANE_INNER_X };
                let bw = (full_w - PANE_INNER_X - right_inset).max(1.0);
                let base_h = h_cells as f32 * self.cell.h;
                let full_h = if y_cells + h_cells >= grid_rows {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lh = w.inner_size().height as f32 / s;
                        let dock = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
                        (raw_lh - dock - (TITLE_HEIGHT + grid_rows as f32 * self.cell.h)).max(0.0)
                    });
                    base_h + extra
                } else {
                    base_h
                };
                let bottom_inset = if y_cells + h_cells >= grid_rows { 0.0 } else { PANE_INNER_Y };
                let bh = (full_h - header_shift_logical - PANE_INNER_Y - bottom_inset).max(1.0);
                body_rects.push((id.clone(), (bx, by, bw, bh)));
                if let Some(image) = img {
                    image_slots.push((id.clone(), image, (bx, by, bw, bh), img_zoom, img_rot, img_pan));
                }
                if let Some((doc, raw_mode, lines, cursor, scroll, h_scroll, lang)) = md {
                    md_slots.push((
                        id.clone(),
                        doc,
                        (bx, by, bw, bh),
                        scroll,
                        raw_mode,
                        lines,
                        cursor,
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
                if show_headers {
                    // Custom title (rename / OSC) wins; otherwise show the
                    // live foreground process (vim, claude, zsh …); only
                    // fall back to the raw "%N" pane id if both are empty.
                    let smart = self.pty.get(&id).and_then(|p| Self::smart_pane_label(p));
                    let label = pane
                        .title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .or(smart)
                        .unwrap_or_else(|| id.clone());
                    // Prefix the pane id so the user can always see which pane
                    // is which (for `tell %N`, etc.). Skip when the label has
                    // already fallen back to the id — no "%18 · %18".
                    let label = if label == id {
                        label
                    } else {
                        format!("{id} · {label}")
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
                        label,
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
                        tabs: pane
                            .tabs
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                t.title
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
                                    })
                            })
                            .collect(),
                        active_tab: pane.active_tab,
                    });
                }
            }
            // Fallback: if nothing is marked active (e.g. active_pane not yet
            // set right after a split), make the first header active so the
            // focused-tab box/accent always shows on exactly one pane.
            if !headers.is_empty() && !headers.iter().any(|h| h.is_active) {
                headers[0].is_active = true;
            }
            (slots, headers, footer_slots)
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
        let collab_toast_msg = self.collab_toast.as_ref().map(|(m, _)| m.clone());
        let slot_views: Vec<gpu::PaneSlot<'_>> = slots
            .iter()
            .map(|s| gpu::PaneSlot {
                rows: &s.rows,
                origin_px: s.origin_px,
                dim: s.dim,
                font_scale: s.font_scale,
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
        let show_drop_zone = header_drag_active || tab_drag_active;
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
        // Floating drag-ghost data (label + cursor), captured before the gpu
        // borrow below so the paint pass draws it without re-borrowing self.
        let drag_ghost: Option<(String, (f32, f32))> = if header_drag_active {
            self.header_drag
                .as_ref()
                .map(|hd| (hd.pane.clone(), self.cursor_px))
        } else {
            None
        };
        // Ghostty-style split seams: one 1px hairline per interior split
        // boundary instead of a 4-side border around every pane (which
        // doubled up into a thick seam between abutting panes). Coords match
        // divider_at_px so drag hit-testing lines up with the drawn line.
        let pane_seams: Vec<(f32, f32, f32, f32)> = self
            .pty_layout
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
            .unwrap_or_default();
        // Left window-tab sidebar geometry. Cache the hit rects for the
        // mouse handler; the gpu block below paints from the same numbers so
        // a click always lands on what the user sees.
        let sb_win_h = win_px.1 / scale;
        self.refresh_window_labels();
        let sb_labels = self.window_labels.clone();
        let (sb_tabs, sb_closes, sb_plus) = self.sidebar_layout(sb_win_h);
        // Only register hit-rects when the tab strip is actually painted. A
        // hidden sidebar (file-tree-only / collapsed) must not leave stale tab
        // rects that a header-drag would false-hit as a cross-window drop.
        let sidebar_shown = self.tab_strip_w() > 0.0;
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
        let mut settings_rects_out: Vec<(SettingsAction, (f32, f32, f32, f32))> = Vec::new();
        // Caret blink for the commit-modal message box, computed before `g`
        // borrows `self.gpu` (the blink helper takes `&self`).
        let commit_caret_on = self.cursor_blink_on(std::time::Instant::now());
        // Per-header completion-flash strength, sampled before `g` borrows
        // `self.gpu` (the header loop can't call `&self` while `g` is live).
        let header_flash: Vec<Option<f32>> =
            headers.iter().map(|h| self.notify_flash_factor(&h.id)).collect();
        if let Some(g) = self.gpu.as_mut() {
            g.clear_chrome();
            // Upload any image pane's pixels once, then queue each for this
            // frame. The image pass (in g.render) paints under the chrome so
            // pane headers / focus ring / dim overlay land on top.
            for (id, image, _, _, rot, _) in &image_slots {
                // Per-rotation cache key — rotated pixels uploaded once per
                // (pane, rotation) pair so toggling between rotations doesn't
                // re-rotate every frame.
                let key = format!("{id}-r{rot}");
                if !g.has_image(&key) {
                    let (rgba, w, h) = rotate_rgba_cw(&image.rgba, image.w, image.h, *rot);
                    g.upload_image(&key, &rgba, w, h);
                }
            }
            g.draw_cells(&slot_views);
            for (id, _, (bx, by, bw, bh), zoom, rot, (pan_x, pan_y)) in &image_slots {
                let key = format!("{id}-r{rot}");
                g.queue_image(&key, *bx, *by, *bw, *bh, *zoom, *pan_x, *pan_y);
            }
            // Markdown is laid out into chrome glyphs/rects here — after the
            // (empty) cell pass, before pane headers/borders so those land on
            // top. The returned content height feeds scroll clamping.
            // Rebuilt fresh each frame so a pane toggled out of raw mode (or
            // closed) drops its caret hit box.
            self.md_body_rects.clear();
            for (id, doc, (bx, by, bw, bh), scroll, raw_mode, lines, cursor, h_scroll, lang) in &md_slots {
                let content_h = if *raw_mode {
                    let lines = lines.as_deref().unwrap_or(&[]);
                    // Stash the body box so a mouse click can hit-test to a caret
                    // position (md_click_caret reads this).
                    self.md_body_rects.insert(id.clone(), (*bx, *by, *bw, *bh));
                    g.draw_raw_editor(
                        lines, *cursor, *bx, *by, *bw, *bh, *scroll, *h_scroll, lang, &md_preedit,
                        raw_cursor_on,
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
            {
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
                let (bx, by, bw, bh) = Self::file_tree_toggle_rect();
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
            // the file-tree toggle (Warp-style location chip).
            {
                let (tbx, _, tbw, _) = Self::file_tree_toggle_rect();
                let px0 = tbx + tbw + 12.0;
                let isz = theme::ICON_SIZE;
                let iy = (TITLE_HEIGHT - isz) / 2.0;
                let ty = (TITLE_HEIGHT - chrome_font) / 2.0;
                g.queue_icon("folder", px0, iy, isz, theme::text_dim());
                let after = px0 + isz;
                // Title-bar cwd chip follows the FOCUSED pane's shell cwd —
                // resolved via the shell's pid + /proc-style lookup. Falls
                // back to kasaterm's own cwd when the pane has no PTY (image
                // / markdown) or the pid couldn't be sniffed.
                let cwd_str = {
                    let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
                    active
                        .and_then(|id| self.pty.get(&id).and_then(|p| p.shell_pid()))
                        .and_then(socket::pid_cwd)
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
                let title_text: String = {
                    let ws = self.ws.lock().unwrap();
                    let active = ws.active_pane.clone();
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
                    // Append the pane's real OS tty (ghostty-style) — daemon
                    // cache first (the daemon owns the PTY in daemon mode), else
                    // the local pty session.
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
                };
                if !title_text.is_empty() {
                    let tw = g.measure_chrome_text(&title_text, chrome_font, true);
                    let win_w_logical = win_px.0 / scale;
                    let center_x = (win_w_logical / 2.0) - tw / 2.0;
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
                            color: theme::text(),
                            bold: true,
                            italic: false,
                        },
                    );
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
                // Truncate a label to a *display-width* budget (CJK glyphs are
                // double-width) with a trailing ellipsis, so long Hangul/CJK
                // titles never bleed past the tab into the cell grid.
                let clip = |s: &str, budget: usize| -> String {
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
                };
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
                        &clip(&name, name_max),
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
                            &clip(&cwd, ((self.sidebar_w_logical - 60.0).max(0.0) / 6.5).floor().max(4.0) as usize),
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
                // Shell picker popup, stacked under the "+" button. Layout
                // (shell_menu_layout) and hit rects were computed before the
                // GPU borrow so clicks land on the same boxes we paint.
                if menu_open && !shell_menu_layout.is_empty() {
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
                    for (_, label, _icon, (ix, iy, iw, ih)) in &shell_menu_layout {
                        let hov = sb_cursor.0 >= *ix
                            && sb_cursor.0 <= *ix + *iw
                            && sb_cursor.1 >= *iy
                            && sb_cursor.1 <= *iy + *ih;
                        if hov {
                            round_rect(g, *ix, *iy, *iw, *ih, theme::RADIUS_MD, theme::surface_hover());
                        }
                        g.queue_icon(
                            "terminal",
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
                }
                // Settings entry — same tab-box style as the session tabs, so it
                // reads as the last item in the list. Active (selected) box
                // while the screen is open, faint hover box otherwise.
                {
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
            if tree_col_w > 0.0 {
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
                    let active = self.file_tree_search_active;
                    let fill = if active { theme::surface_active() } else { theme::surface() };
                    round_rect(g, row_x, sbx_y, search_w, search_box_h, theme::RADIUS_SM, theme::border());
                    round_rect(g, row_x + 1.0, sbx_y + 1.0, search_w - 2.0, search_box_h - 2.0, theme::RADIUS_SM - 1.0, fill);
                    let ic = if active { theme::text() } else { theme::text_dim() };
                    g.queue_icon("folder-tree", row_x + 8.0, sbx_y + (search_box_h - 14.0) / 2.0, 14.0, ic);
                    let mut shown = self.file_tree_search_query.clone();
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
                    self.file_tree_search_rect = (row_x, sbx_y, search_w, search_box_h);
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
                    self.file_tree_new_folder_rect = (nf_x, bty, btn_sz, btn_sz);
                    self.file_tree_new_file_rect = (nfile_x, bty, btn_sz, btn_sz);
                }
                // Inline "new file/folder" naming row, pinned above the tree.
                let mut tree_top = sbx_y + search_box_h + 8.0;
                if let Some((is_dir, buf)) = self.file_tree_new.clone() {
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
                    self.file_tree_new_row_rect = (row_x, iy, row_w, item_h);
                    tree_top += item_h;
                } else {
                    self.file_tree_new_row_rect = (0.0, 0.0, 0.0, 0.0);
                }
                let start_y = tree_top;
                let win_h = win_px.1 / scale;
                let step = 14.0_f32; // per-depth indent width
                let mut rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))> = Vec::new();
                // `file_tree_nodes` already holds the right set: a query swaps it
                // for whole-tree search hits (file_tree_search_collect), empty
                // restores the expanded tree. So just render it as-is.
                let vis_nodes: Vec<&FileNode> = self.file_tree_nodes.iter().collect();
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
                    let y = start_y - self.file_tree_scroll + idx as f32 * item_h;
                    if y + item_h < start_y || y > win_h {
                        continue; // off-screen → clip (and don't cache a hit rect)
                    }
                    let hovered =
                        self.file_tree_hover.as_deref() == Some(node.path.as_path());
                    let expanded =
                        node.is_dir && self.file_tree_expanded.contains(&node.path);
                    let is_open = active_file.as_deref() == Some(node.path.as_path());
                    let is_selected =
                        self.file_tree_selected.as_deref() == Some(node.path.as_path());
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
                    // Single-color outline icon (cursor/VSCode style): a plain
                    // folder or file glyph, tinted by row state. Ignored/dotfile
                    // rows render dim + italic so build output (target, dist) and
                    // config (.git, .claude) recede from real source.
                    let icon_color = if node.ignored {
                        theme::with_alpha(theme::text_dim(), 0x99)
                    } else if hovered || is_open {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    g.queue_icon(if node.is_dir { "folder" } else { "file" }, icon_x, iy, isz, icon_color);
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
                    g.draw_text(
                        text_x,
                        y + (item_h - font) / 2.0,
                        &label,
                        gpu::DrawOpts { font_size: font, color: fg, bold: false, italic: node.ignored },
                    );
                    rects.push((node.path.clone(), (row_x, y, row_w, item_h)));
                }
                self.file_tree_rects = rects;

                // Overflow affordances: a soft fade at whichever edge still has
                // hidden rows, plus a hover-only scrollbar thumb. The viewport
                // runs from the first row (`start_y`, already below the search
                // box / inline new-row) to the column bottom, so the fade never
                // eats the chrome above it.
                let view_top = start_y;
                let view_bottom = TITLE_HEIGHT + col_h;
                let viewport_h = (view_bottom - view_top).max(0.0);
                let content_h = self.file_tree_nodes.len() as f32 * item_h;
                if content_h > viewport_h + 0.5 {
                    let overflow = content_h - viewport_h;
                    let scroll = self.file_tree_scroll;
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
            }
            // ── Git column ── right-hand chrome mirroring the file-tree column
            // on the left, but native instead of the old floating webview: the
            // poller fills `git_view` off-thread and this paints branch +
            // change list + Commit/Push, caching file-row / button hit rects
            // for the mouse handler. window_cells already reserved its width so
            // no pane overlaps it; it stops above the dock so the dock bar and
            // the action buttons never fight for the same strip.
            self.git_col_file_rects.clear();
            self.git_col_btn_rects.clear();
            self.git_path_hdr_rect = None;
            self.git_branch_hdr_rect = None;
            self.git_path_menu_rects.clear();
            self.git_branch_menu_rects.clear();
            if git_col_w > 0.0 {
                let dock_h = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
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
                    self.git_col_expand_rect = Some((expand_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
                    self.git_col_close_rect = Some((close_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
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
                    let pcol = if self.git_col_pinned_cwd.is_some() { theme::accent() } else { theme::text_dim() };
                    let px = g.draw_text(gcx0, y, &path_disp, gpu::DrawOpts { font_size: 12.0, color: pcol, bold: false, italic: false });
                    self.git_path_hdr_rect = Some((gcx0 - 3.0, y - 3.0, (px - gcx0) + 6.0, 19.0));
                    if !git_view.no_repo {
                        let branch = if git_view.branch.is_empty() { "—" } else { git_view.branch.as_str() };
                        let cx2 = g.draw_text(px, y, " : ", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                        let bend = g.draw_text(cx2, y, branch, gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false });
                        self.git_branch_hdr_rect = Some((cx2 - 3.0, y - 3.0, (bend - cx2) + 6.0, 19.0));
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
                let input_top = bottom;
                if git_view.no_repo {
                    g.draw_text(gcx0, y, "git 저장소가 아닙니다", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
                    self.git_commit_btn_rect = None;
                    self.git_commit_caret_rect = None;
                    list_top = y + 8.0;
                } else {
                    g.queue_icon("git-branch", gcx0, y + 1.0, 13.0, theme::text_mute());
                    g.draw_text(gcx0 + 18.0, y, "Uncommitted changes", gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false });
                    let bh = 24.0_f32;
                    let by = y - 4.0;
                    let caret_w = 20.0_f32;
                    let lw = g.measure_chrome_text("Commit", 12.0, true);
                    let main_w = 24.0 + lw + 10.0;
                    let total_w = main_w + caret_w;
                    let bx = git_col_x + git_col_w - 12.0 - total_w;
                    let can_commit = !git_view.staged.is_empty() || !git_view.unstaged.is_empty();
                    let mhov = self.cursor_px.0 >= bx && self.cursor_px.0 <= bx + main_w && self.cursor_px.1 >= by && self.cursor_px.1 <= by + bh;
                    let chov = self.cursor_px.0 >= bx + main_w && self.cursor_px.0 <= bx + total_w && self.cursor_px.1 >= by && self.cursor_px.1 <= by + bh;
                    let base = if can_commit { theme::surface_active() } else { theme::with_alpha(theme::surface_hover(), 0x66) };
                    round_rect(g, bx, by, total_w, bh, theme::RADIUS_SM, base);
                    if can_commit && mhov { round_rect(g, bx, by, main_w, bh, theme::RADIUS_SM, theme::accent()); }
                    if can_commit && chov { round_rect(g, bx + main_w, by, caret_w, bh, theme::RADIUS_SM, theme::accent()); }
                    g.rect(bx + main_w, by + 5.0, 1.0, bh - 10.0, theme::with_alpha(theme::bg(), 0x99));
                    let fg = if can_commit { theme::text() } else { theme::text_mute() };
                    g.queue_icon("git-commit-horizontal", bx + 8.0, by + (bh - 13.0) / 2.0, 13.0, fg);
                    g.draw_text(bx + 24.0, by + (bh - 12.0) / 2.0, "Commit", gpu::DrawOpts { font_size: 12.0, color: fg, bold: true, italic: false });
                    g.draw_text(bx + main_w + (caret_w - 7.0) / 2.0, by + (bh - 11.0) / 2.0, "▾", gpu::DrawOpts { font_size: 11.0, color: fg, bold: false, italic: false });
                    self.git_commit_btn_rect = Some((bx, by, main_w, bh));
                    self.git_commit_caret_rect = Some((bx + main_w, by, caret_w, bh));
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
                        let mut y_cur = list_top - self.git_col_scroll;
                        for (title, staged, files) in [
                            ("Staged Changes", true, &git_view.staged),
                            ("Changes", false, &git_view.unstaged),
                        ] {
                            if files.is_empty() {
                                continue;
                            }
                            // Section header (count) — clipped to the list zone.
                            if y_cur + header_h > list_top && y_cur < input_top {
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
                                let expanded = self.git_col_expanded.contains(&(staged, path.clone()));
                                let row_visible = !(ry + item_h < list_top || ry > input_top);
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
                                    if let Some(rows_d) = self.git_col_diff_cache.get(&(staged, path.clone())) {
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
                        self.git_col_file_rects = rects;
                        self.git_col_stage_rects = stage_rects;
                        self.git_col_discard_rects = discard_rects;
                        self.git_col_open_rects = open_rects;
                    }
                // Dropdowns (path picker / branch switcher) paint last so they
                // overlay the list + buttons. Built from the precomputed repo
                // list and the poller's branch list.
                git_paint_dropdowns(
                    g,
                    git_col_x,
                    git_col_w,
                    TITLE_HEIGHT,
                    self.git_path_hdr_rect,
                    self.git_branch_hdr_rect,
                    self.git_path_menu_open,
                    self.git_branch_menu_open,
                    &git_repo_list,
                    &self.git_col_pinned_cwd,
                    &git_view.branches,
                    &git_view.branch,
                    &mut self.git_path_menu_rects,
                    &mut self.git_branch_menu_rects,
                );
                // ── Commit-button dropdown (Commit / Push / Create PR)
                self.git_commit_menu_rects.clear();
                if self.git_commit_menu_open {
                    if let Some((ccx, ccy, ccw, cch)) = self.git_commit_caret_rect {
                        let items = [
                            ("git-commit-horizontal", "Commit", GitCommitAction::Commit),
                            ("arrow-up", "Push", GitCommitAction::Push),
                            ("github", "Create PR", GitCommitAction::CreatePr),
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
                            g.draw_text(mx + 38.0, iy + (ih - 13.0) / 2.0, label, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                            self.git_commit_menu_rects.push((act, (mx, iy, iw, ih)));
                            iy += ih;
                        }
                    }
                }
                // ── Commit modal (screenshot #5): dim + centered card.
                self.git_commit_modal_rects.clear();
                if self.git_commit_modal_open {
                    g.rect(git_col_x, top, git_col_w, bottom - top, theme::with_alpha([0, 0, 0, 255], 0x99));
                    let bw = (git_col_w - 40.0).min(640.0).max(0.0);
                    let bx = git_col_x + (git_col_w - bw) / 2.0;
                    let bh = (bottom - top - 50.0).min(660.0).max(0.0);
                    let bxy = top + (bottom - top - bh) / 2.0;
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
                    self.git_commit_modal_rects.push((GitModalBtn::Close, (xx - 5.0, my, 26.0, 26.0)));
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
                    let on = self.git_commit_modal_include_unstaged;
                    round_rect(g, tx, my - 2.0, tw, th, th / 2.0, if on { theme::accent() } else { theme::surface_active() });
                    let knob = th - 6.0;
                    let kx = if on { tx + tw - knob - 3.0 } else { tx + 3.0 };
                    round_rect(g, kx, my - 2.0 + 3.0, knob, knob, knob / 2.0, [255, 255, 255, 255]);
                    self.git_commit_modal_rects.push((GitModalBtn::IncludeUnstaged, (tx - 4.0, my - 5.0, tw + 8.0, th + 8.0)));
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
                    if self.git_commit_focused {
                        round_rect(g, cx - 1.0, my - 1.0, cw + 2.0, inh + 2.0, theme::RADIUS_SM, theme::accent());
                    }
                    round_rect(g, cx, my, cw, inh, theme::RADIUS_SM, theme::surface());
                    let itx = cx + 10.0;
                    let ity = my + 9.0;
                    let preedit = if self.git_commit_focused { self.preedit.as_str() } else { "" };
                    if self.git_commit_msg.is_empty() && preedit.is_empty() {
                        g.draw_text(itx, ity, "변경 사항 설명…", gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: false, italic: false });
                    }
                    let cur = self.git_commit_cursor.min(self.git_commit_msg.chars().count());
                    let before: String = self.git_commit_msg.chars().take(cur).collect();
                    let after: String = self.git_commit_msg.chars().skip(cur).collect();
                    let mut px = g.draw_text(itx, ity, &before, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                    let caret_x = px;
                    if !preedit.is_empty() {
                        px = g.draw_text(px, ity, preedit, gpu::DrawOpts { font_size: 13.0, color: theme::accent(), bold: false, italic: false });
                    }
                    if !after.is_empty() {
                        g.draw_text(px, ity, &after, gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
                    }
                    if self.git_commit_focused && preedit.is_empty() && commit_caret_on {
                        g.rect(caret_x, ity, 1.5, 14.0, theme::text());
                    }
                    self.git_commit_input_rect = Some((cx, my, cw, inh));
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
                        self.git_commit_modal_rects.push((btn, (cx, my, cw, bbh)));
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
                    self.git_commit_modal_rects.push((GitModalBtn::Cancel, (canc_x, cby, cancel_w, 34.0)));
                    round_rect(g, conf_x, cby, confirm_w, 34.0, theme::RADIUS_SM, if conf_hov { theme::accent() } else { theme::surface_active() });
                    let wconf = g.measure_chrome_text("Confirm", 13.0, true);
                    g.draw_text(conf_x + (confirm_w - wconf) / 2.0, cby + 10.0, "Confirm", gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false });
                    self.git_commit_modal_rects.push((GitModalBtn::Confirm, (conf_x, cby, confirm_w, 34.0)));
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
                let per_tab = if tab_list.len() == 1 {
                    tabs_area
                } else {
                    (tabs_area / tab_list.len() as f32).clamp(56.0, 320.0)
                };
                // Left edge of each tab's pill, for the drag insertion bar.
                let mut tab_edges: Vec<f32> = Vec::with_capacity(tab_list.len());
                // Geometry for the post-loop structural border pass.
                let mut tabs_left: Option<f32> = None;
                let mut tabs_right_edge: f32 = 0.0;
                let mut inter_boundaries: Vec<f32> = Vec::new();
                let mut active_tab_box: Option<(f32, f32)> = None;
                let gap = 6.0_f32;
                let mut tx = h.x + 8.0;
                for (i, tab) in tab_list.iter().enumerate() {
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
                    let t_fg = if bright {
                        theme::with_alpha(theme::text(), combine(0xFF))
                    } else {
                        theme::with_alpha(theme::text(), combine(0x82))
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
                    // no visible gap.
                    let box_x = if i == 0 { h.x } else { tab_x0 - 6.0 };
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
                        let bar_x = tab_edges.get(target).copied().unwrap_or(tx - gap);
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
                    // itself signals the current state.
                    let sb_icon = if self.statusbar_hidden.contains(&h.id) {
                        "panel-bottom-dashed"
                    } else {
                        "panel-bottom"
                    };
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
            // Per-pane status bar at the foot of each pane box: cwd + branch
            // chips (click → cd / checkout dropdowns) on the left, ± diff on
            // the right. The gpu borrow rules out &self method calls in here,
            // so visibility / cwd / badge all read the fields directly.
            self.statusbar_path_rects.clear();
            self.statusbar_branch_rects.clear();
            self.statusbar_toggle_rects.clear();
            self.statusbar_diff_rects.clear();
            let (sb_mx, sb_my) = self.cursor_px;
            let sb_home = std::env::var("HOME").ok();
            for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                if self.statusbar_hidden.contains(fid) || *fbox_h < PANE_FOOTER_HEIGHT + 4.0 {
                    continue;
                }
                let bar_y = fy + fbox_h - PANE_FOOTER_HEIGHT;
                g.rect(*fx, bar_y, *fw, PANE_FOOTER_HEIGHT, theme::bg());
                g.rect(*fx, bar_y, *fw, 1.0, theme::border());
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
                    self.statusbar_path_rects.push((fid.clone(), (cx, pill_y, pw, pill_h)));
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
                        self.statusbar_branch_rects.push((fid.clone(), (cx, pill_y, pw, pill_h)));
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
                        self.statusbar_diff_rects.push((fid.clone(), (cx, pill_y, pw, pill_h)));
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
                    self.statusbar_toggle_rects
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
            // Status-bar dropdown (directory picker / branch switcher), drawn
            // last so it overlays the cell grid + every bar. Anchored to the
            // chip that opened it and expanded UPWARD — the bar lives at the
            // pane's bottom, so a downward menu would fall off the edge.
            self.statusbar_menu_dir_rects.clear();
            self.statusbar_menu_branch_rects.clear();
            if let Some((menu_pid, kind)) = self.statusbar_menu.clone() {
                let anchor = match kind {
                    StatusbarMenu::Path => self
                        .statusbar_path_rects
                        .iter()
                        .find(|(p, _)| *p == menu_pid)
                        .map(|(_, r)| *r),
                    StatusbarMenu::Branch => self
                        .statusbar_branch_rects
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
                            .statusbar_menu_dirs
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
                        StatusbarMenu::Branch => self.statusbar_menu_branches.clone(),
                    };
                    // Live-search filter (path picker only). Inlined as field
                    // reads — the gpu borrow (`g`) rules out &self method calls.
                    let q = self.statusbar_menu_search.to_lowercase();
                    let fidx: Vec<usize> = if is_path {
                        self.statusbar_menu_dirs
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
                    let scroll = self.statusbar_menu_scroll.clamp(0.0, overflow as f32 * item_h);
                    self.statusbar_menu_scroll = scroll;
                    let first = ((scroll / item_h).round() as usize).min(overflow);
                    self.statusbar_menu_rect = Some((menu_x, menu_y, menu_w, menu_h));
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
                        let mut shown = self.statusbar_menu_search.clone();
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
                                || self.statusbar_menu_dirs.get(i).map(|p| p.is_dir()).unwrap_or(false);
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
                                .statusbar_menu_dir_rects
                                .push((self.statusbar_menu_dirs[i].clone(), row)),
                            StatusbarMenu::Branch => self
                                .statusbar_menu_branch_rects
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
                    self.statusbar_menu_rect = None;
                }
            } else {
                self.statusbar_menu_rect = None;
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
            self.collab_toast_rect = None;
            if collab_toast_alpha > 0.0 {
                if let Some(msg) = collab_toast_msg.as_ref() {
                    let t_font = 13.0_f32;
                    let win_w = win_px.0 / scale;
                    let text_w = g.measure_chrome_text(msg, t_font, true);
                    let (px, py) = (14.0_f32, 8.0_f32);
                    let box_w = text_w + px * 2.0;
                    let box_h = t_font + py * 2.0;
                    let bx = win_w - box_w - 16.0;
                    let by = TITLE_HEIGHT + 12.0;
                    self.collab_toast_rect = Some((bx, by, box_w, box_h));
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
            }
            // Alt/Option held → tmux "display-panes": each pane shows its %N
            // big + centered on an accent pill, so the user can read the id
            // (for `tell %N`, focus, etc.) without it crowding the header.
            // Works in single-pane too — body_rects covers every pane.
            if self.show_pane_numbers {
                for (id, rect) in &body_rects {
                    let (rx, ry, rw, rh) = *rect;
                    if rw < 24.0 || rh < 24.0 {
                        continue;
                    }
                    let font = (rh * 0.4).clamp(24.0, 72.0);
                    let tw = g.measure_chrome_text(id, font, true);
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
                        id,
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
            if !self.docked.is_empty() {
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
                for d in &self.docked {
                    let label: &str =
                        if d.label.is_empty() { "shell" } else { d.label.as_str() };
                    let lw = g.measure_chrome_text(label, chrome_font, false);
                    let chip_w = lw + icon + 24.0;
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
                    let close_x = cx + chip_w - icon - 6.0;
                    g.queue_icon("x", close_x, cy + (chip_h - icon) / 2.0, icon, theme::text_dim());
                    chip_close_hits.push((d.id.clone(), (close_x - 2.0, cy, icon + 6.0, chip_h)));
                    chip_hits.push((d.id.clone(), (cx, cy, chip_w - icon - 8.0, chip_h)));
                    cx += chip_w + 6.0;
                }
                self.dock_chip_rects = chip_hits;
                self.dock_chip_close_rects = chip_close_hits;
            } else {
                self.dock_chip_rects.clear();
                self.dock_chip_close_rects.clear();
            }
            // Drop-zone highlight sits on top of everything during a drag.
            if let Some((zx, zy, zw, zh)) = drop_zone_rect {
                g.rect(zx, zy, zw, zh, theme::with_alpha(theme::accent(), 90));
            }
            // Floating drag ghost under the cursor: the pane being carried is
            // visibly "lifted out" so dragging it onto a sidebar window for a
            // cross-window move reads as physically picking the pane up.
            if let Some((label, (cgx, cgy))) = &drag_ghost {
                let gw = 140.0_f32;
                let gh = 26.0_f32;
                let gx = *cgx - gw / 2.0;
                let gy = *cgy - gh / 2.0;
                g.round_rect_fill(gx, gy, gw, gh, 6.0, theme::with_alpha(theme::surface_active(), 230));
                g.rect(gx, gy, gw, 2.0, theme::accent());
                g.draw_text(
                    gx + 10.0,
                    gy + (gh - 12.0) / 2.0,
                    label,
                    gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: false, italic: false },
                );
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
            if let Some(drag) = self.file_tree_drag.as_ref() {
                if drag.active {
                    let name = drag
                        .path
                        .file_name()
                        .map(|n| nfc_hangul(&n.to_string_lossy()))
                        .unwrap_or_default();
                    let is_dir = self
                        .file_tree_nodes
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
                settings_rects_out = settings::paint_settings(g, ctx);
            }
            if let Err(e) = g.render(&slot_views, scale, time_secs, true) {
                eprintln!("[gpu] render error: {e:?}");
            }
        }
        self.settings_rects = settings_rects_out;
        self.confirm_btn_rects = confirm_btn_hits;
        self.pane_tab_rects = tab_hits;
        self.pane_tab_close_rects = tab_close_hits;
        self.pane_plus_rects = plus_hits;
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
        let rebuild = pty_dirty
            || self.chrome_dirty
            || blink_changed
            || version_animating
            || toast_animating;
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
