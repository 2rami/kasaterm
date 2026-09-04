//! pane 레이아웃 조작 — split/move/close/focus/swap/drop/divider/zoom/tab + 좌표·resize. daemon-authoritative.
use super::*;

/// 본문 중앙의 "안에 넣기" 존 반경 — pane 반폭·반높이를 1.0 으로 본 정규화 좌표.
/// 0.42 면 중앙 사각형이 pane 의 42%×42% 를 먹고, 네 쐐기는 여전히 각 변을
/// 통째로 낀다. 더 좁히면(0.25) 조준이 어려워 "안 붙는다"로 읽히고, 더 넓히면
/// 가장자리 split 을 노릴 때 중앙이 걸린다.
const DROP_CENTER_R: f32 = 0.42;

/// pane 중심 기준 정규화 offset → 드롭 존. 순수 함수(단위테스트 대상).
///
/// 4방향 판정은 raw 픽셀 거리가 아니라 정규화 offset 으로 한다 — 픽셀 거리를
/// 비교하면 가로로 넓은 pane 은 Up/Down 쐐기가 좁아져 "끝까지 가야" 방향이
/// 나왔다. half-w/half-h 로 나눠 정사각형 공간에서 비교하면 네 방향이 공평한
/// 90° 쐐기를 받는다.
///
/// 중앙은 어느 쪽으로 가를지 정할 수 없는 자리이므로 split 이 아니라 병합
/// (`Center` = 이 pane 의 탭으로 들어가기)이다. 이 존이 없으면 pane 을 통째로
/// 끌어 놓을 때 헤더 띠(28px)를 정확히 맞히지 않는 한 무조건 split 이 됐다.
/// 방향을 안 준 split 이 고를 축 — **긴 쪽을 쪼갠다.**
///
/// 거노 2026-08-05: "너무 가로로나 세로로 안 길게". 늘 같은 방향으로 쪼개면 네 번째
/// pane 쯤에서 종잇장이 된다. 긴 축을 자르면 정사각에 가까워지고, 다음 split 은 자연히
/// 반대 축을 골라 격자가 된다.
///
/// 판정은 **셀이 아니라 픽셀**로 한다 — 셀은 세로로 2.5배 길어(7×17.5) 80×24 pane 은
/// 셀로는 "가로가 3배"지만 화면에선 거의 정사각이다. 눈에 보이는 모양이 기준이다.
///
/// 다만 가로로 쪼개 **80칸을 못 지키면** 세로로 돌린다 — 코드·로그가 접히는 건 좀
/// 길쭉한 것보다 나쁘다. 반대로 세로로 쪼개 16줄을 못 지키면 가로로. 둘 다 못 지키면
/// 긴 축 규칙 그대로(더 나은 선택이 없다).
///
/// 최소 줄수가 16인 이유: claude 입력박스만 5줄이라 12줄짜리 pane 은 대화가 두 줄
/// 보인다. 「좁은 것보다 짧은 게 낫다」가 성립하려면 짧은 쪽이 실제로 쓸 만해야 한다 —
/// 12로 뒀다가 80×24 pane(표준 크기)이 12줄 두 장으로 갈렸다.
pub(crate) fn pick_split_axis(px_w: f32, px_h: f32, cols: u16, rows: u16) -> kasa_pty::SplitDir {
    use kasa_pty::SplitDir::{Horizontal, Vertical};
    let long_axis = if px_w >= px_h { Horizontal } else { Vertical };
    match long_axis {
        Horizontal if cols / 2 < MIN_PANE_COLS && rows / 2 >= MIN_PANE_ROWS => Vertical,
        Vertical if rows / 2 < MIN_PANE_ROWS && cols / 2 >= MIN_PANE_COLS => Horizontal,
        d => d,
    }
}

/// 쓸 만한 pane 의 하한. `pick_split_axis` 의 축 선택과 `split_fleet` 의 인원 상한이
/// **같은 숫자**를 봐야 한다 — 축은 「80칸을 못 지키니 세로로」라고 판단했는데 인원
/// 상한이 다른 기준이면, 축 판단이 피하려던 좁은 pane 을 인원이 다시 만든다.
///
/// 16줄인 이유는 claude 입력박스만 5줄이라 12줄 pane 은 대화가 두 줄 보인다는 것이다
/// (12로 뒀다가 표준 80×24 pane 이 12줄 두 장으로 갈렸다).
pub(crate) const MIN_PANE_COLS: u16 = 80;
pub(crate) const MIN_PANE_ROWS: u16 = 16;

pub(crate) fn drop_zone_for_offsets(nx: f32, ny: f32) -> DropZone {
    if nx.abs() < DROP_CENTER_R && ny.abs() < DROP_CENTER_R {
        return DropZone::Center;
    }
    if nx.abs() > ny.abs() {
        if nx < 0.0 {
            DropZone::Left
        } else {
            DropZone::Right
        }
    } else if ny < 0.0 {
        DropZone::Up
    } else {
        DropZone::Down
    }
}

impl App {
    /// 지금 화면에 보이는 pane id — 활성 방(`active_window`)의 BSP leaf 와, 그 leaf
    /// 자리에 사는 보조 탭들.
    ///
    /// damage gate 와 애니메이션 펌프가 **모든 방의 모든 pane** 을 세고 있었다. 방마다
    /// claude 를 띄우면 그 pane 들이 죄 「working」이라, **보이지도 않는 헤더 바** 하나
    /// 때문에 앱이 상시 30fps 를 갈았다(2026-08-13 실측: pane 12개·방 4개). 빈
    /// 인스턴스는 claude 가 없어 재현되지 않아서, 이 증상이 오래 「스크롤이 버벅인다」로
    /// 보였다 — 정작 스크롤 경로는 멀쩡했다.
    ///
    /// 안 보이는 pane 은 상태가 바뀌어도 그릴 그림이 없으니 판정에서 뺀다. 방을 전환하는
    /// 순간엔 `chrome_dirty` 가 서고, 그릴 때 셀은 항상 PTY 의 최신 상태를 다시 읽으므로
    /// 「전환했더니 옛 화면」이 되지는 않는다.
    pub(crate) fn visible_pane_ids(&self) -> std::collections::HashSet<String> {
        let mut set: std::collections::HashSet<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().into_iter().map(String::from).collect())
            .unwrap_or_default();
        // 보조 탭의 pty 는 leaf 가 아니다 — 화면을 든 건 그 탭이 사는 바깥 pane 이라,
        // 바깥이 보이면 안쪽도 보이는 것으로 센다(`window_of_pane` 과 같은 접기 규칙).
        if let Ok(ws) = self.ws.lock() {
            let inner: Vec<String> = ws
                .panes
                .keys()
                .filter(|id| {
                    ws.outer_for_pty(id.as_str())
                        .is_some_and(|outer| set.contains(&outer))
                })
                .cloned()
                .collect();
            set.extend(inner);
        }
        set
    }

    /// Convert logical-pixel position into a (pane_id, col, row) cell
    /// inside the pane the click landed in. Multi-pane aware: walks the
    /// parsed Layout to find the pane whose rect contains the click,
    /// then translates the pixel into that pane's cell-local coords.
    /// Returns None when the workspace has no panes or the click missed
    /// every pane (gutter between split borders, padding, etc).
    pub(crate) fn px_to_pane_cell(&self, px: f32, py: f32) -> Option<(String, u16, u16)> {
        let sb = self.effective_sidebar_w();
        let ws = self.ws.lock().unwrap();
        // 줌 중엔 그 pane 하나가 작업영역을 통째로 채운다(effective_leaf_rects) —
        // 원본 split 트리로 판정하면 클릭이 **숨어 있는** pane 으로 가고, 셀 좌표도
        // 그 pane 의 옛 박스 원점으로 계산돼 화면과 어긋난다. Claude 프롬프트가
        // 아래쪽에 있으니 위 절반만 대충 맞고 아래는 통째로 빗나갔다(거노:
        // "최대화하고 위치 매핑이 이상해"). 박스가 하나뿐이라 단일 pane 경로와
        // 계산이 같다. 렌더와 같은 조건(트리에 살아 있는 leaf)일 때만 타서,
        // 닫힌 pane 의 유령 줌이 클릭을 삼키지 않는다.
        if let Some(z) = self.zoomed_pane.as_deref() {
            let live = self
                .pty_layout
                .as_ref()
                .is_some_and(|t| t.leaves().iter().any(|l| *l == z));
            if live {
                let pane = ws.panes.get(z)?;
                let t = pane.term()?;
                if t.cols == 0 || t.rows == 0 {
                    return None;
                }
                let fs = self
                    .pane_font_scales
                    .get(z)
                    .copied()
                    .unwrap_or(1.0)
                    .max(0.1);
                // 줌 pane 은 「떠 있는 카드」라 가장자리에서 zoom_inset_cells 만큼
                // 들여 그려진다(render_frame_gpu·effective_leaf_rects 와 같은 함수).
                // 그 원점을 안 빼면 클릭·드래그 선택이 inset 셀수(가로 ~2·세로 1)
                // 만큼 오른쪽 아래 글자를 집는다(거노: "확대하면 드래그 위치가
                // 정확히 안 맞아").
                let (gc, gr) = self.window_cells();
                let (ix, iy) = self.zoom_inset_cells(gc, gr);
                let box_left = sb + WINDOW_PADDING + ix as f32 * self.cell.w;
                let box_top = TITLE_HEIGHT + iy as f32 * self.cell.h;
                let lc =
                    ((px - box_left - PANE_INNER_X).max(0.0) / (self.cell.w * fs)).floor() as u16;
                let lr = ((py - box_top - pane.header_px() - PANE_INNER_Y).max(0.0)
                    / (self.cell.h * fs))
                    .floor() as u16;
                return Some((z.to_string(), lc.min(t.cols - 1), lr.min(t.rows - 1)));
            }
        }
        if let Some(layout) = ws.layout.as_ref() {
            // ghostty식: 헤더 띠 폐기 → 헤더만큼 셀을 밀던 보정 제거.
            let header_h = 0.0_f32;
            // Box hit-test runs in whole-grid cells (header included, no
            // inset) so a click anywhere in the pane box selects it.
            let gcol = ((px - sb - WINDOW_PADDING).max(0.0) / self.cell.w).floor() as i32;
            // Render shifts every split pane down by the header band (origin_y
            // += header_shift, see render_frame_gpu). The box hit-test must
            // subtract the same band, or the lower pane's rows map ~one header
            // above where they're actually drawn — clicks / scroll there miss
            // the pane entirely.
            let grow = ((py - TITLE_HEIGHT - header_h).max(0.0) / self.cell.h).floor() as i32;
            for leaf in layout.leaves() {
                if let Layout::Pane { id, x, y, w, h } = leaf {
                    let (bx, by, bw, bh) = (*x as i32, *y as i32, *w as i32, *h as i32);
                    if gcol >= bx && gcol < bx + bw && grow >= by && grow < by + bh {
                        // Local cell uses the body origin: box edge + header
                        // band + inner inset, matching the render origin.
                        let pid = format!("%{id}");
                        // Per-pane font zoom: glyphs render at cell × fs, so the
                        // pixel→cell divisor must use the same zoomed cell or a
                        // font-bumped pane maps the cursor to the wrong row/col
                        // (selection + mouse-report drift). The box origin stays
                        // on the shared grid — only the in-pane step scales.
                        let fs = self
                            .pane_font_scales
                            .get(&pid)
                            .copied()
                            .unwrap_or(1.0)
                            .max(0.1);
                        let box_left = sb + WINDOW_PADDING + bx as f32 * self.cell.w;
                        let box_top = TITLE_HEIGHT + by as f32 * self.cell.h;
                        // 본문(셀)은 헤더 띠 아래에서 시작 — 헤더 있는 pane은 그만큼
                        // 빼야 마우스가 실제 그려진 행에 맞는다(render origin과 동일).
                        // grow(박스 hit-test)는 헤더 포함 박스라 header_h=0 그대로.
                        let hdr = ws.panes.get(&pid).map(|p| p.header_px()).unwrap_or(0.0);
                        let lc = ((px - box_left - PANE_INNER_X).max(0.0) / (self.cell.w * fs))
                            .floor() as u16;
                        let lr = ((py - box_top - hdr - PANE_INNER_Y).max(0.0) / (self.cell.h * fs))
                            .floor() as u16;
                        let (mc, mr) =
                            ws.panes
                                .get(&pid)
                                .and_then(|p| p.term())
                                .map_or((lc, lr), |t| {
                                    (
                                        lc.min(t.cols.saturating_sub(1)),
                                        lr.min(t.rows.saturating_sub(1)),
                                    )
                                });
                        return Some((pid, mc, mr));
                    }
                }
            }
            return None;
        }
        // No layout yet — single pane fills the window (inset only).
        let id = ws
            .active_pane
            .clone()
            .or_else(|| ws.panes.keys().next().cloned())?;
        let pane = ws.panes.get(&id)?;
        let t = pane.term()?;
        if t.cols == 0 || t.rows == 0 {
            return None;
        }
        let fs = self
            .pane_font_scales
            .get(&id)
            .copied()
            .unwrap_or(1.0)
            .max(0.1);
        // 단일 pane(layout 없음)도 이미지/마크다운/2탭이면 헤더 띠가 있다 —
        // 본문 셀은 그 아래에서 시작하므로 multi-pane 경로와 동일하게 보정.
        let hdr = pane.header_px();
        let lc = ((px - sb - WINDOW_PADDING - PANE_INNER_X).max(0.0) / (self.cell.w * fs)).floor()
            as u16;
        let lr =
            ((py - TITLE_HEIGHT - hdr - PANE_INNER_Y).max(0.0) / (self.cell.h * fs)).floor() as u16;
        Some((id, lc.min(t.cols - 1), lr.min(t.rows - 1)))
    }
    /// Convenience wrapper that returns only the active pane's local
    /// cell coords. Most callers (wheel, selection drag) only care
    /// about the active pane.
    pub(crate) fn px_to_cell_active(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let (pane_id, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let active_match = ws.active_pane.as_deref() == Some(pane_id.as_str());
        active_match.then_some((col, row))
    }
    /// Target pane for outgoing key/text. When the workspace has an
    /// active pane, we name it explicitly so tmux doesn't fall back to
    /// "last-active" semantics that disagree with our UI.
    pub(crate) fn target_pane(&self) -> Option<String> {
        self.ws.lock().unwrap().active_pane.clone()
    }
    /// Surface id that should receive keyboard input — the active pane's
    /// *active tab*'s pid, not the outer pane id. `target_pane()` returns
    /// the layout key (== first tab's pid), so once the user switches tabs
    /// the daemon keeps routing keystrokes to the first tab. The daemon's
    /// PTY map is keyed by tab pid, so input must name the active tab
    /// explicitly. Falls back to the outer id for single-tab / tmux panes
    /// whose tabs carry no explicit pid (same fallback as `active_pty`).
    pub(crate) fn target_surface(&self) -> Option<String> {
        let ws = self.ws.lock().ok()?;
        let outer = ws.active_pane.clone()?;
        let pid = ws
            .panes
            .get(&outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or(outer);
        Some(pid)
    }
    /// The PtySession that currently has keyboard focus, if any. Used
    /// by every routing-by-active-pane code path in PTY mode.
    /// PtySession of a pane's currently-active tab. Use this instead of
    /// `self.pty.get(outer_id)` — after a cross-pane tab drag the layout
    /// id and the active tab's pid diverge, and the direct lookup misses.
    /// Drives wheel scroll / mouse-reporting / pane-targeted send_bytes.
    pub(crate) fn pty_for_pane(&self, outer_id: &str) -> Option<&Arc<kasa_pty::PtySession>> {
        let ws = self.ws.lock().ok()?;
        let pid = ws
            .panes
            .get(outer_id)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or_else(|| outer_id.to_string());
        drop(ws);
        self.pty.get(&pid)
    }
    pub(crate) fn active_pty(&self) -> Option<&Arc<kasa_pty::PtySession>> {
        // The active *tab*'s pid drives input/scroll/title — falling back
        // to the outer pane id (== first-tab pid) for single-tab panes
        // whose tabs haven't been initialised with an explicit pid yet
        // (e.g. tmux-mode panes, where the outer key is what `pty` keys on).
        let ws = self.ws.lock().unwrap();
        let outer = ws.active_pane.clone()?;
        let pid = ws
            .panes
            .get(&outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or(outer);
        drop(ws);
        self.pty.get(&pid)
    }
    /// Window size in cell coordinates. Source of truth for resize
    /// distribution + new-pane sizing. The grid lives inside
    /// `WINDOW_PADDING` on every side, so subtract 2× padding from the
    /// logical viewport before dividing — otherwise we tell the PTY it
    /// has N rows but only N-1 fit before clipping, and the last row
    /// (where most TUIs paint their statusline) gets cut in half.
    /// Falls back to (80, 24) when the window isn't ready yet.
    pub(crate) fn window_cells(&self) -> (u16, u16) {
        let Some(window) = self.window.as_ref() else {
            return (80, 24);
        };
        let size = window.inner_size();
        let scale = self.effective_scale();
        let raw_lw = size.width as f32 / scale;
        let raw_lh = size.height as f32 / scale;
        let lw = (raw_lw
            - self.effective_sidebar_w()
            - self.effective_right_chrome_w()
            - 2.0 * WINDOW_PADDING)
            .max(0.0);
        // Top: TITLE_HEIGHT (chrome strip). Bottom: WINDOW_PADDING. The
        // asymmetry is intentional — the strip replaces the top padding.
        // Reserve the dock bar from the grid only when it carries chips.
        let lh = (raw_lh - TITLE_HEIGHT - WINDOW_PADDING - self.bottom_reserve_h()).max(0.0);
        // 하한이 40 이던 시절엔 이 줄이 거짓말을 했다 — 쓸 폭이 160px 뿐인 창에서도
        // 40칸(340px)이라고 PTY 에 알려, 터미널이 우측 칼럼 밑으로 180px 파고들어
        // 그려졌다. 폭을 지키는 일은 `chrome_widths` 의 예산이 맡고, 여기는 0 칸을
        // 알리지 않을 만큼의 바닥만 맡는다.
        let cols = (lw / self.cell.w).floor().max(GRID_MIN_COLS) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        if std::env::var_os("KASATERM_LOG_LAYOUT").is_some() {
            eprintln!(
                "[layout] win=({raw_lw:.0}x{raw_lh:.0}) usable=({lw:.0}x{lh:.0}) cell=({:.1}x{:.1}) cells=({cols}x{rows})",
                self.cell.w, self.cell.h
            );
        }
        (cols, rows)
    }
    /// Push the current PtyLayout into `ws.layout` so the renderer
    /// (which only knows the tmux Layout shape) picks up the splits.
    /// A single-leaf tree leaves `ws.layout` empty — the render path's
    /// single-pane fallback handles that case.
    pub(crate) fn publish_pty_layout(&self) {
        if let Some(tree) = self.pty_layout.as_ref() {
            let (cols, rows) = self.window_cells();
            let mut ws = self.ws.lock().unwrap();
            if tree.leaves().len() <= 1 {
                ws.layout = None;
            } else {
                ws.layout = Some(tree.to_tmux_layout(cols, rows));
            }
            // 활성 방(윈도우)의 leaf pane 집합 — 일부 경로가 아직 참조.
            ws.active_window_panes = tree.leaves().iter().map(|l| l.to_string()).collect();
            // 전 윈도우(방) pane → window_idx — collab_board 가 전 방 학생을 방별로 그룹핑.
            // window_of_pane 과 같은 패턴(활성=pty_layout, 그 외=windows[i]). PtyBackend 가
            // App 의 windows 를 못 봐서 ws 로 미러한다(거노: 좌측 통합·전 방 영속).
            let mut pw: HashMap<String, usize> = HashMap::new();
            for i in 0..self.windows.len() {
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows[i].as_ref()
                };
                if let Some(l) = layout {
                    for leaf in l.leaves() {
                        pw.insert(leaf.to_string(), i);
                    }
                }
            }
            // 보조 탭 pid 도 화면 안이다 — 탭은 바깥 pane 자리에 살고 사용자가 탭바로
            // 언제든 본다. leaf 만 실으면 collab_board 가 탭 학생을 전부 detached
            // (화면밖)로 찍고, SendMessage 의 닫힌-pane 가드가 「사용자가 닫았거나
            // 숨긴 자리」라며 차단했다(거노 2026-08-18: 탭에 넣으면 인식을 못 한다).
            // 바깥 pane 이 pw 에 없으면(숨김·stash) 탭도 안 싣는다 — 그건 진짜 화면밖.
            for (pid, outer) in &ws.pid_to_pane {
                if let Some(i) = pw.get(outer).copied() {
                    pw.entry(pid.clone()).or_insert(i);
                }
            }
            ws.pane_window = pw;
        }
        // Keep the socket snapshot in lockstep with the renderer view —
        // every code path that adds/removes panes or moves focus goes
        // through publish_pty_layout, so this is the one spot we have
        // to wire the cmux mirror.
    }
    /// Resize every backend session so its grid matches the new window
    /// size. In tmux mode the daemon redistributes for us. In PTY mode
    /// we walk the BSP tree and SIGWINCH each leaf to its own rect.
    pub(crate) fn resize_backend(&self, cols: u16, rows: u16) {
        if let Some(tmux) = self.tmux.as_ref() {
            let _ = tmux.resize_client(cols, rows);
            return;
        }
        // The window is the single source of truth for size. Derive every
        // leaf's usable grid from the window cell box here, then push those
        // sizes to whoever owns the PTY — the daemon over RPC, or local
        // sessions. Panes themselves only carry BSP ratios, never absolute
        // rows/cols, so this one computation feeds both backends.
        let Some(_tree) = self.pty_layout.as_ref() else {
            return;
        };
        // image/md pane만 헤더 띠를 가지므로(pane_header_px) 그 pane의 usable
        // 높이에서만 헤더를 차감한다 — 루프 안에서 id별로 조회. 일반 터미널은 0.
        // Per-pane font scale shrinks/grows that pane's usable cells: bigger
        // glyphs ⇒ fewer cols/rows in the same box. 1.0 panes keep the exact
        // integer-cell math; scaled panes divide the base cell span by the
        // factor (the box stays on the base grid, matching the per-slot render
        // which sizes glyphs by the same factor). Keyed by pty/leaf id.
        let scale_of = self.pane_font_scales.clone();
        let cw = self.cell.w.max(1.0);
        let ch = self.cell.h.max(1.0);
        let mut leaf_cells: HashMap<String, (u16, u16)> = HashMap::new();
        for (id, _x, _y, w, h) in self.effective_leaf_rects(cols, rows) {
            let fs = scale_of.get(&id).copied().unwrap_or(1.0).max(0.1);
            // Work in logical px on the BASE grid — exactly the span the
            // renderer fills (origin + w·cell), then subtract the real px
            // insets/header and divide by the ZOOMED cell. The old path
            // rounded the inset to whole base cells and divided by fs, so a
            // shrunk pane (small fs) amplified that ceil error ∝ 1/fs and
            // told the PTY a grid that no longer matched the drawn area —
            // that's the "비율 안 맞음" past a certain zoom-out.
            let box_w_px = w as f32 * cw;
            let box_h_px = h as f32 * ch;
            let scaled_cw = cw * fs;
            let scaled_ch = ch * fs;
            // The status bar lives below the grid, so a pane that shows it
            // loses the same footer band off its usable height — otherwise the
            // shell paints its last rows behind the bar.
            let footer_px = self.statusbar_px(&id);
            let header_px = self.pane_header_px(&id);
            let usable_w = (box_w_px - 2.0 * PANE_INNER_X).max(scaled_cw);
            let usable_h = (box_h_px - header_px - footer_px - 2.0 * PANE_INNER_Y).max(scaled_ch);
            let pcols = (usable_w / scaled_cw).floor().max(1.0) as u16;
            let prows = (usable_h / scaled_ch).floor().max(1.0) as u16;
            leaf_cells.insert(id, (pcols, prows));
        }
        // Each leaf id IS its primary pane's pid, so resize that PTY directly
        // from leaf_cells — no dependency on ws.panes being populated. A
        // freshly split pane has no PaneState until its first output, so the
        // old ws.panes walk left it at 80×24 spawn size (화면 겹침/하단 잘림).
        for (id, (pc, pr)) in &leaf_cells {
            // 거울(뷰어) pane 은 원본 세션의 격자를 못 바꾼다 — 로컬 창을 줄였다고
            // 저쪽 기계 화면까지 쪼그라들면 안 된다(tmux 최소-클라이언트 문제).
            // 대신 렌더가 그 pane 의 글자 배율을 줄여 원본 격자를 통째로 담는다.
            if kasa_mcp::remote::is_view_pane(id) {
                continue;
            }
            if let Some(sess) = self.pty.get(id) {
                let _ = sess.resize(*pc, *pr);
            }
        }
        // In-pane secondary tabs (pid != outer) still resolve via ws.panes —
        // they share the outer leaf's rect but have their own PtySession.
        let snapshot: Vec<(String, Vec<String>)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .map(|(outer, p)| {
                    let pids: Vec<String> = p
                        .tabs
                        .iter()
                        .filter_map(|t| t.pid.clone())
                        .filter(|pid| pid != outer)
                        .collect();
                    (outer.clone(), pids)
                })
                .collect()
        };
        for (outer, pids) in snapshot {
            let Some(&(pc, pr)) = leaf_cells.get(&outer) else {
                continue;
            };
            for pid in pids {
                if kasa_mcp::remote::is_view_pane(&pid) {
                    continue;
                }
                if let Some(sess) = self.pty.get(&pid) {
                    let _ = sess.resize(pc, pr);
                }
            }
        }
        // Re-publish the layout because rect proportions may have
        // shifted (rounding) and the renderer caches the previous tree.
        self.publish_pty_layout();
    }
    /// If the cursor (logical px) rests on a split seam, return the BSP
    /// tree path of that split plus its axis. A few px of tolerance makes
    /// the thin seam easy to grab. None when not over any divider.
    pub(crate) fn divider_at_px(&self, x: f32, y: f32) -> Option<(Vec<u8>, kasa_pty::SplitDir)> {
        let tree = self.pty_layout.as_ref()?;
        // 줌 중엔 경계선을 아예 안 그린다(render 의 pane_seams 가 빈 벡터). 그런데도
        // 여기서 잡으면 아무것도 없는 자리에서 커서가 리사이즈로 바뀌고, 드래그가
        // 보이지 않는 분할비를 움직인다 — 보이지 않는 것은 클릭도 먹지 않아야 한다.
        if self.zoomed_pane.is_some() {
            return None;
        }
        if tree.leaves().len() <= 1 {
            return None;
        }
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let tol = 6.0_f32;
        for d in tree.dividers(cols, rows) {
            match d.dir {
                kasa_pty::SplitDir::Horizontal => {
                    let seam_x = pad + d.edge as f32 * self.cell.w;
                    let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                    let y1 = y0 + d.span_len as f32 * self.cell.h;
                    if (x - seam_x).abs() <= tol && y >= y0 && y <= y1 {
                        return Some((d.path, d.dir));
                    }
                }
                kasa_pty::SplitDir::Vertical => {
                    let seam_y = TITLE_HEIGHT + d.edge as f32 * self.cell.h;
                    let x0 = pad + d.span_start as f32 * self.cell.w;
                    let x1 = x0 + d.span_len as f32 * self.cell.w;
                    if (y - seam_y).abs() <= tol && x >= x0 && x <= x1 {
                        return Some((d.path, d.dir));
                    }
                }
            }
        }
        None
    }
    /// Split the focused pane in PTY mode. Spawns a new shell into a
    /// fresh PTY, inserts it into the BSP tree on the right (Horizontal)
    /// or bottom (Vertical) of the focused leaf, then resizes every
    /// session so each one matches its new rect. Becomes a no-op in
    /// tmux mode — splits there go through the cmux socket / tmux
    /// `split-window` instead.
    /// Splits the active pane and returns the new pane's id. The socket
    /// backend forwards this id back to the teammate launcher; an empty
    /// string means no pane was created (tmux backend / no active pane).
    /// 현재 방에 캐릭터 지정 학생 추가(StudentNav '+ 학생'). pending_character 를 세팅하고
    /// split — assign_character_env 가 그 캐릭터로 마커·persona env 를 입힌다(아로나/프라나
    /// 포함). 자동 빈슬롯 순환 대신 사용자가 고른 캐릭터.
    /// 새 pane 의 surface id 를 돌려준다(빈 문자열 = 실패) — 디스패처가 스폰 직후
    /// 그 학생에게 브리프를 주입할 주소로 쓴다.
    pub(crate) fn spawn_student(&mut self, character: &str) -> String {
        self.pending_character = Some(character.to_string());
        // 축을 고정하지 않는다(예전엔 `Horizontal` 이었다) — 디스패처가 학생을 여럿
        // 띄우면 매번 좌우로 갈라 얇은 세로 기둥이 된다. `None` 은 쪼갤 pane 의
        // 종횡비를 보고 긴 축을 고르므로, 이어 띄워도 칸이 정사각에 가깝게 수렴한다.
        let id = match self.split_pane_auto(None) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("[spawn_student] split failed: {e:#}");
                self.pending_character = None;
                return String::new();
            }
        };
        self.handoff_ime_to_active_surface();
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        id
    }

    /// pane 여러 개를 **한 번에** 배치한다 — 부른 pane 이 크게 남고 학생들이 균등하게.
    ///
    /// 옛 경로는 CLI 가 split 을 N 번 부르면서 **직전에 만든 pane 을 다음 대상으로**
    /// 삼았다. ⌘D 를 연달아 누른 것과 같은 모양이라 몫이 1/2 → 1/4 → 1/8 로
    /// 반감하고, 넷을 부르면 마지막 학생이 화면의 1/16 이다(거노 2026-08-13:
    /// "너네가 부르면 내가 드래그로 정렬하고"). 방향을 명시하면 더 나빴다 — 모든
    /// 회차가 같은 축이라 얇은 기둥이 된다.
    ///
    /// 그래서 pane 은 기존 경로로 낳고(cwd·방·학생 배정·이름이 다 거기 붙어 있다)
    /// **트리만 마지막에 한 번** 갈아 끼운다. 중간의 반감하는 모양은 화면에 안
    /// 나간다 — 이 함수가 끝날 때까지 프레임을 그리지 않기 때문이다.
    ///
    /// 창에 이미 다른 pane 이 있으면 그 자리는 건드리지 않는다(`replace_leaf`).
    ///
    /// 반환값은 **실제로 앉힌** pane id 들이다. `fleet_capacity` 가 하한(80칸·16줄)
    /// 으로 자르므로 요청보다 적을 수 있고, 부른 쪽이 그 차이를 사람에게 알려야
    /// 한다 — 조용히 적게 만들면 「5명 불렀는데 3명」이 또 사고가 된다.
    pub(crate) fn split_fleet(
        &mut self,
        count: usize,
        from: Option<&str>,
        host_ratio: f32,
    ) -> Result<Vec<String>> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 로컬 배치를 쓰지 않는다");
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let active = self.ws.lock().unwrap().active_pane.clone();
        let Some(host) = from.map(str::to_string).or(active) else {
            anyhow::bail!("배치할 기준 pane 이 없다");
        };
        // 탭을 지목받았으면 그 탭이 든 pane — 탭은 BSP leaf 가 아니라 트리에서 못 찾는다.
        let host = self.ws.lock().unwrap().outer_for_pty(&host).unwrap_or(host);
        let Some(owner) = self.window_of_pane(&host) else {
            anyhow::bail!("배치할 pane {host} 이 어느 window 트리에도 없다");
        };

        // 축은 **호스트 칸의 종횡비**로 고른다 — 창 전체가 아니라. 이미 쪼개진
        // 창에서 호스트가 세로로 긴 칸이면 좌우로 가르는 게 맞다.
        let (win_cols, win_rows) = self.window_cells();
        let host_rect = self
            .effective_leaf_rects(win_cols, win_rows)
            .into_iter()
            .find(|(id, ..)| *id == host)
            .map(|(_, _, _, w, h)| (w, h))
            .unwrap_or((win_cols, win_rows));
        let dir = pick_split_axis(
            host_rect.0 as f32 * self.cell.w.max(1.0),
            host_rect.1 as f32 * self.cell.h.max(1.0),
            host_rect.0,
            host_rect.1,
        );
        // 하한을 지키며 앉을 수 있는 만큼만. 0 이면 한 명도 못 앉힌다 —
        // 그때는 조용히 좁은 pane 을 만드는 대신 사유를 올린다.
        let room = kasa_pty::fleet_capacity(
            dir,
            host_ratio,
            host_rect.0,
            host_rect.1,
            MIN_PANE_COLS,
            MIN_PANE_ROWS,
        );
        if room == 0 {
            anyhow::bail!(
                "{host} 칸이 {}x{} 라 캐릭터 한 명도 못 앉힌다 — 창을 키우거나 탭으로 띄워라",
                host_rect.0,
                host_rect.1
            );
        }
        let want = count.min(room);

        let mut made: Vec<String> = Vec::new();
        for _ in 0..want {
            match self.spawn_split_session(&host) {
                Ok(id) => made.push(id),
                Err(e) => {
                    // 반쯤 만든 셸을 남기지 않는다 — 트리에 안 꽂힌 pane 은 화면에
                    // 없는데 셸은 계속 돈다.
                    for id in &made {
                        self.pty.remove(id);
                    }
                    return Err(e);
                }
            }
        }

        let tree = kasa_pty::fleet(&host, &made, dir, host_ratio);
        let layout = if owner == self.active_window {
            self.pty_layout.as_mut()
        } else {
            self.windows.get_mut(owner).and_then(|s| s.as_mut())
        };
        if !layout.is_some_and(|l| l.replace_leaf(&host, tree)) {
            for id in &made {
                self.pty.remove(id);
            }
            anyhow::bail!("pane {host} 자리를 못 찾았다 — 종료·재시작으로 사라졌는지 확인해라");
        }
        // 줌은 트리 밖 렌더 상태다 — 재배치하면 줌 대상이 화면과 어긋나므로 푼다.
        // 안 풀면 학생을 셋 띄웠는데 화면엔 옛 pane 하나만 크게 남는다.
        if self.zoomed_pane.is_some() {
            self.zoomed_pane = None;
        }
        if owner != self.active_window {
            // 안 보이는 방이라 포커스도 메인 그리드도 안 건드린다. 그 방 PTY 치수는
            // 그 창을 앞으로 가져올 때 다시 맞춰진다.
            return Ok(made);
        }
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(made)
    }

    /// split 이 얹을 새 셸을 띄우고 `self.pty` 에 등록한다 — **레이아웃은 안 건드린다**.
    /// 트리에 꽂는 일과 갈라 둔 건 비활성 방의 트리가 `pty_layout` 밖에 있기 때문이다.
    /// 트리에 못 꽂았으면 호출부가 `self.pty` 에서 지운다 — 번호는 거기 등록된 것으로만
    /// 판정하므로(`alloc_pane_id`) 그것으로 자동 회수된다.
    pub(crate) fn spawn_split_session(&mut self, active: &str) -> Result<String> {
        let new_id = self.alloc_pane_id();

        // Spawn the new session at a placeholder size — the resize
        // pass right after `split_leaf` puts every leaf at its real
        // rect, so the initial cols/rows here only matters for the
        // first bytes the shell prints before SIGWINCH lands.
        let (win_cols, win_rows) = self.window_cells();
        let cwd = self.spawn_cwd_from(Some(active));
        // split = room 만 상속, 학생은 새로 랜덤 배정(전역 유일). 07-13 의 "소스 학생 상속"
        // 설계는 모든 pane 이 루트 학생 하나로 수렴하는 부작용(거노 07-17: pane 열면 다
        // 프라나)으로 폐기 — 상속이 막으려던 "둔갑"(랜덤으로 떴다 뒤늦게 교정)은 배정이
        // spawn 시점 즉시(assign_character_env)가 된 지금은 재발하지 않는다. resume 은
        // shim 의 /character 교정이 세션 정본 캐릭터로 되돌리고, 사용자가 '+ 학생'·학생
        // 명령으로 명시 지정한 pending 은 그대로 존중(중복 허용, 색 변주로 구분).
        let room = self.ws.lock().unwrap().pane_room.get(active).cloned();
        let mut env = crate::proxy_env(&new_id);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
            self.ws
                .lock()
                .unwrap()
                .pane_room
                .insert(new_id.clone(), r.clone());
        }
        env.extend(self.assign_character_env(&new_id, cwd.as_deref(), room.as_deref()));
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols: win_cols,
            rows: win_rows,
            env,
            pane_id: new_id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        let session = Arc::new(session);
        self.pump_pty_screens(
            session.screens.clone(),
            new_id.clone(),
            std::sync::Arc::downgrade(&session),
        );
        self.insert_pty(new_id.clone(), session);
        Ok(new_id)
    }

    /// 포커스된 pane 을 쪼갠다. 실패는 **전부 `Err` + 사유**다.
    ///
    /// 예전엔 실패 셋을 `Ok(String::new())` 로 돌려줬는데, 소켓 경로가 그 빈
    /// 문자열을 `pane-new` 자리표시자로 바꿔 **`ok:true` 에 실어 보냈다** —
    /// 호출자가 성공으로 읽고 그 id 로 send 를 쏘면 조용히 사라진다. 거노가 학생
    /// 5명을 띄우려다 1명만 뜬 게 이것이다(2026-08-05). 사유 없는 실패는 호출자가
    /// `list surfaces` 를 다시 대조해야만 알 수 있어, 스크립트가 감지할 방법이 없었다.
    /// 방향을 안 준 split 이 고를 축 — **긴 쪽을 쪼갠다.**
    ///
    /// 거노 2026-08-05: "너무 가로로나 세로로 안 길게". 늘 같은 방향으로 쪼개면 네
    /// 번째 pane 쯤에서 종잇장이 된다. 긴 축을 자르면 정사각에 가까워지고, 다음
    /// split 은 자연히 반대 축을 골라 격자가 된다.
    ///
    /// 판정은 **셀이 아니라 픽셀**로 한다 — 셀은 세로로 2.5배 길어(7×17.5) 80×24
    /// pane 은 셀로는 "가로가 3배"지만 화면에선 거의 정사각이다. 눈에 보이는 모양을
    /// 기준으로 삼는 게 요구사항에 맞다.
    ///
    /// 다만 가로로 쪼개 **80칸을 못 지키면** 세로로 돌린다(코드·로그가 접히면 좁은
    /// 것보다 나쁘다). 반대로 세로로 쪼개 16줄을 못 지키면 가로로. 둘 다 못 지키면
    /// 긴 축 규칙 그대로 — 어차피 더 나은 선택이 없다.
    fn auto_split_dir(&mut self, pane: &str) -> kasa_pty::SplitDir {
        let (cols, rows) = self.window_cells();
        let Some((_, _, _, w, h)) = self
            .effective_leaf_rects(cols, rows)
            .into_iter()
            .find(|(id, _, _, _, _)| id == pane)
        else {
            // 트리에서 못 찾으면 가로 — 창이 대개 가로로 넓다.
            return kasa_pty::SplitDir::Horizontal;
        };
        pick_split_axis(
            w as f32 * self.cell.w.max(1.0),
            h as f32 * self.cell.h.max(1.0),
            w,
            h,
        )
    }

    /// `dir` 가 `None` 이면 종횡비로 고른다(`auto_split_dir`).
    pub(crate) fn split_pane_auto(&mut self, dir: Option<kasa_pty::SplitDir>) -> Result<String> {
        let dir = match dir {
            Some(d) => d,
            None => {
                let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
                    anyhow::bail!("활성 pane 이 없다");
                };
                self.auto_split_dir(&active)
            }
        };
        self.split_active_pane(dir)
    }

    pub(crate) fn split_active_pane(&mut self, dir: kasa_pty::SplitDir) -> Result<String> {
        if self.tmux.is_some() {
            anyhow::bail!("tmux 백엔드에선 로컬 split 을 쓰지 않는다");
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            anyhow::bail!("활성 pane 이 없다");
        };
        // 탭을 지목받았으면 **그 탭이 든 pane** 을 쪼갠다. 탭은 BSP leaf 가 아니라
        // `split_leaf` 가 못 찾고, 그러면 셸만 새로 띄운 채 통째로 실패한다.
        let active = self
            .ws
            .lock()
            .unwrap()
            .outer_for_pty(&active)
            .unwrap_or(active);
        // **그 pane 을 가진 트리**에 꽂는다 — 활성 window 트리가 아니라.
        // 예전엔 `pty_layout` 만 봐서, 거노가 다른 방을 보고 있으면 pane 이 자기
        // 자리를 쪼개려다 통째로 실패했다("pane %5 이 활성 window(1) 트리에 없다").
        // 스폰은 오케스트레이터가 배경에서 하는 일이라 **거노가 어느 방을 보고 있는지와
        // 무관해야** 한다.
        let owner = self.window_of_pane(&active);
        let new_id = self.spawn_split_session(&active)?;
        let (win_cols, win_rows) = self.window_cells();
        let foreign = owner.filter(|w| *w != self.active_window);
        let layout = match foreign {
            Some(w) => self.windows.get_mut(w).and_then(|s| s.as_mut()),
            None => self.pty_layout.as_mut(),
        };
        if !layout.is_some_and(|l| l.split_leaf(&active, dir, new_id.clone())) {
            // 샌 세션을 되감고 사유를 올린다.
            self.pty.remove(&new_id);
            anyhow::bail!(
                "pane {active} 을 어느 window 트리에서도 못 찾았다 — 종료·재시작으로 사라졌는지 확인해라"
            );
        }
        if foreign.is_some() {
            // 안 보이는 방을 쪼갠 것이라 포커스도 메인 그리드도 건드리지 않는다.
            // 그 방의 PTY 치수는 그 창을 앞으로 가져올 때(`aux_room_resize_pty`
            // ·window 전환) 어차피 다시 맞춰진다.
            return Ok(new_id);
        }
        self.ws.lock().unwrap().active_pane = Some(new_id.clone());
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(new_id)
    }

    /// User-facing split. SocketSplit keeps using `split_active_pane` because
    /// it temporarily rewrites `active_pane` while arranging background work;
    /// only a split that really leaves focus on the new pane hands off IME.
    pub(crate) fn split_active_pane_focused(
        &mut self,
        dir: kasa_pty::SplitDir,
    ) -> Result<String> {
        let new_id = self.split_active_pane(dir)?;
        self.handoff_ime_to_active_surface();
        Ok(new_id)
    }
    /// Stage-3 in-pane tab spawn. Creates a fresh PtySession with its own
    /// pid, registers it in `pid_to_pane` so output streams find the right
    /// (outer pane, tab) pair, and appends a `PaneTab` whose `pid` points at
    /// the new shell. Outer pane id and layout don't change — adding a tab
    /// never reshapes the BSP tree.
    /// 새 탭의 pane id 를 돌려준다 — 부른 쪽이 거기에 명령을 실어야 하기 때문이다.
    /// 예전엔 `()` 라 소켓으로 탭을 만들면 대상이 뭔지 알 방법이 없었다(split 이
    /// 자리표시자를 돌려주던 것과 같은 함정).
    ///
    /// `activate`: 사람이 탭바 + 를 누른 경우만 true. 소켓 스폰(false)이 활성탭을
    /// 뺏으면 오케스트레이터 pane 을 보던 사람 화면이 서브에이전트 부팅으로 덮인다.
    pub(crate) fn spawn_new_tab(&mut self, outer: &str, activate: bool) -> Result<String> {
        if self.tmux.is_some() {
            anyhow::bail!("in-pane tabs not supported on tmux backend");
        }
        // Outer pane must already exist in the layout (it's the user's focused
        // pane). Use its size for the initial pty so the shell starts at the
        // right cols/rows — `resize_backend` after re-applies it anyway, but a
        // sane initial size keeps the welcome banner from wrapping weird.
        let (cols, rows) = self
            .pane_cells(outer)
            .unwrap_or_else(|| self.window_cells());
        let cwd = self.spawn_cwd_from(Some(outer));
        let new_pid = self.alloc_pane_id();
        // 탭도 split 과 **같은 대접**이다: 방은 상속하고 학생은 새로 배정한다.
        // 이게 없던 동안 탭으로 띄운 학생은 캐릭터가 아예 없어서 보더색·프사·입력박스
        // 도색은 물론 페르소나 env 와 board 등재까지 통째로 빠졌다(거노 2026-08-07:
        // "탭안에서 생성하면 학생테마가안먹네"). split 이 깨져 학생들이 탭으로
        // 우회하던 참이라 더 눈에 띄었다.
        let room = self.ws.lock().unwrap().pane_room.get(outer).cloned();
        let mut env = crate::proxy_env(&new_pid);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
            self.ws
                .lock()
                .unwrap()
                .pane_room
                .insert(new_pid.clone(), r.clone());
        }
        env.extend(self.assign_character_env(&new_pid, cwd.as_deref(), room.as_deref()));
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env,
            pane_id: new_pid.clone(),
            initial_scrollback: Vec::new(),
        })?;
        let session = Arc::new(session);
        self.pump_pty_screens(
            session.screens.clone(),
            new_pid.clone(),
            std::sync::Arc::downgrade(&session),
        );
        self.insert_pty(new_pid.clone(), session);
        {
            let mut ws = self.ws.lock().unwrap();
            ws.pid_to_pane.insert(new_pid.clone(), outer.to_string());
            if let Some(pane) = ws.panes.get_mut(outer) {
                let mut tab = PaneTab::default();
                tab.pid = Some(new_pid.clone());
                pane.tabs.push(tab);
                if activate {
                    pane.active_tab = pane.tabs.len() - 1;
                }
                pane.dirty = true;
            }
        }
        if activate {
            self.handoff_ime_to_active_surface();
        }
        // 탭은 트리를 안 바꾸지만 pane_window 미러는 pid_to_pane 을 함께 싣는다 —
        // 안 밀어주면 다음 레이아웃 변경까지 board 가 이 학생을 화면밖으로 찍는다.
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(new_pid)
    }
    /// Cell extent of `outer` inside the current `pty_layout`. Used by
    /// `spawn_new_tab` to size a brand-new shell at the pane's real bounds.
    /// Returns `None` when the layout is in single-pane fallback or the id
    /// isn't a leaf.
    pub(crate) fn pane_cells(&self, outer: &str) -> Option<(u16, u16)> {
        let (cols, rows) = self.window_cells();
        let tree = self.pty_layout.as_ref()?;
        for (id, _x, _y, w, h) in tree.leaf_rects(cols, rows) {
            if id == outer {
                return Some((w.max(1), h.max(1)));
            }
        }
        None
    }
    /// Leaf rects for render / hit-test / resize, honoring a tmux-style zoom.
    /// When a pane is zoomed it fills the whole work area and the others are
    /// hidden; the daemon's layout tree is untouched (zoom is GUI-local render
    /// state). If the zoomed pane is gone (closed or moved out by a broadcast)
    /// this falls back to the real layout, so a stale zoom never paints a
    /// phantom pane.
    pub(crate) fn effective_leaf_rects(
        &self,
        cols: u16,
        rows: u16,
    ) -> Vec<(String, u16, u16, u16, u16)> {
        if let Some(z) = self.zoomed_pane.as_ref() {
            if let Some(tree) = self.pty_layout.as_ref() {
                if tree.leaves().iter().any(|l| *l == z.as_str()) {
                    let (ix, iy) = self.zoom_inset_cells(cols, rows);
                    return vec![(z.clone(), ix, iy, cols - ix * 2, rows - iy * 2)];
                }
            }
        }
        self.pty_layout
            .as_ref()
            .map(|t| t.leaf_rects(cols, rows))
            .unwrap_or_default()
    }
    /// 줌 pane 을 작업영역 가장자리에서 들이는 셀 수 `(가로, 세로)`.
    ///
    /// 통째로 채우면 줌 화면이 「pane 하나뿐인 평소 화면」과 픽셀 단위로 같아져,
    /// 최대화 중인지 아닌지 구분이 안 된다(거노). 여백 + 테두리(render.rs)가
    /// 「이것만 보고 있다」를 만든다. 셀은 세로로 긴 직사각형이라 사방 1셀씩
    /// 들이면 여백이 2배 어긋나므로 종횡비로 가로를 보정한다. 창이 좁으면 그
    /// 축은 들이지 않는다 — 여백보다 내용 칸이 먼저다.
    pub(crate) fn zoom_inset_cells(&self, cols: u16, rows: u16) -> (u16, u16) {
        let ratio = (self.cell.h / self.cell.w.max(1.0)).round().clamp(1.0, 4.0) as u16;
        let ix = if cols > ratio * 2 + 8 { ratio } else { 0 };
        let iy = if rows > 6 { 1 } else { 0 };
        (ix, iy)
    }
    /// Toggle tmux-style zoom on `pane`: zoom fills the work area with just that
    /// pane; toggling again (or the pane already being zoomed) restores the
    /// split. Reflows the backend so the PTY matches its new extent.
    pub(crate) fn toggle_pane_zoom(&mut self, pane: &str) {
        if self.zoomed_pane.as_deref() == Some(pane) {
            self.zoomed_pane = None;
        } else {
            self.zoomed_pane = Some(pane.to_string());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Drop a non-primary tab: kill its PTY, remove the pid map entry, drop
    /// the slot. The primary tab (index 0, pid == outer pane id) can't be
    /// closed this way — callers fall through to `remove_pane` for that.
    pub(crate) fn close_tab(&mut self, outer: &str, idx: usize) {
        // 마지막 탭이 나가면 `tabs` 가 비고, 그러면 `PaneState` 의 `Deref` 가
        // 가리킬 곳을 잃어 **다음 프레임에 앱이 통째로 죽는다**(2026-08-22 실측:
        // 탭 하나짜리 pane 의 알약을 휠 클릭 → 렌더 패닉). 위 독스가 「호출자가
        // remove_pane 으로 간다」고 적어 두고도 **코드가 강제하지 않아서** 새
        // 호출자가 생길 때마다 밟는 구조였다 — 가운데 클릭이 그 여섯 번째 길이다.
        // 기본값이 안전해야 하므로 판정을 호출부에서 이 함수 안으로 옮긴다.
        //
        // ⚠️ 위임은 **PTY 를 문 primary 탭일 때만**이다. `remove_pane` 은
        // `record_closed_pane` 첫 줄(`pty.contains_key`)에서 PTY 없는 pane 을
        // 통째로 건너뛰므로, 미리보기 전용 pane 을 그리로 보내면 아래 ⌘⇧T
        // 기록이 조용히 사라진다. 그쪽은 본체가 제 기록을 남기게 두고 **함수
        // 끝에서 빈 껍데기만** 걷는다.
        let last_primary = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(outer).is_some_and(|p| {
                p.tabs.len() <= 1
                    && p.tabs
                        .first()
                        .is_some_and(|t| t.pid.as_deref() == Some(outer))
            })
        };
        if last_primary {
            self.remove_pane(outer);
            return;
        }
        let (pid_opt, preview_opt, preview_path): (
            Option<String>,
            Option<String>,
            Option<std::path::PathBuf>,
        ) = {
            let ws = self.ws.lock().unwrap();
            let tab = ws.panes.get(outer).and_then(|p| p.tabs.get(idx));
            (
                tab.and_then(|t| t.pid.clone()),
                tab.and_then(|t| t.preview_id.clone()),
                tab.and_then(|t| t.preview_path.clone()),
            )
        };
        if let Some(pid) = pid_opt.as_deref() {
            if pid != outer {
                // Secondary tab — drop its session entry; reader thread sees
                // the channel close and pushes EOF to `dead_panes`, but with
                // the pid_to_pane entry gone the reap pass routes through
                // remove_pane(pid) which is a no-op (pty already gone). Fine.
                self.pty.remove(pid);
                // 탭도 학생을 담는다(서브에이전트 스폰) — pane 과 같은 마커 정리를
                // 안 하면 닫힌 탭의 캐릭터 바인딩이 남아 board 가 유령을 센다.
                let closed_cwd = self.pane_cwd_cache.get(pid).cloned();
                Self::cleanup_collab_markers(pid, closed_cwd.as_deref());
                let mut ws = self.ws.lock().unwrap();
                ws.pid_to_pane.remove(pid);
                ws.pane_character.remove(pid);
            }
        }
        // Preview tab removal is immediate via the ws.panes mutation below;
        // with no daemon there's no broadcast to resurrect it.
        let _ = preview_opt;
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(pane) = ws.panes.get_mut(outer) {
                if idx < pane.tabs.len() {
                    pane.tabs.remove(idx);
                }
                if idx < pane.active_tab {
                    pane.active_tab -= 1;
                }
                if pane.active_tab >= pane.tabs.len() {
                    // 빈 벡터에서 `len() - 1` 은 `usize::MAX` 다 — 위 가드가
                    // 막지만, 언더플로우 자체를 남겨 두면 다음에 또 샌다.
                    pane.active_tab = pane.tabs.len().saturating_sub(1);
                }
                pane.dirty = true;
            }
        }
        self.handoff_ime_to_active_surface();
        // 이미지·마크다운 미리보기 탭은 닫아도 ⌘⇧T 로 되살릴 수 있게 경로를
        // 남긴다 — pane 닫기와 같은 스택이라 인포의 닫힘 줄에도 함께 뜬다
        // (2026-08-20 지시). PTY 탭(pid 있음)은 종전대로 기록 없이 죽는다.
        if pid_opt.is_none() {
            if let Some(path) = preview_path {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("preview")
                    .to_string();
                let folder = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let window = self.window_of_pane(outer).unwrap_or(self.active_window);
                self.push_closed_pane(crate::ClosedPane {
                    rec: serde_json::Value::Null,
                    pane_id: name,
                    character: String::new(),
                    folder,
                    neighbor: None,
                    window,
                    alive: false,
                    stashed: false,
                    idle_since: None,
                    preview: Some((outer.to_string(), path)),
                });
                self.chrome_dirty = true;
            }
        }
        // 탭이 다 나갔으면 빈 껍데기를 남기지 않는다 — 그 pane 을 그리는 순간
        // `Deref` 가 갈 곳을 잃는다. `remove_pane` 이 아니라 레이아웃만 걷는
        // 쪽인 이유는 위 미리보기 기록과 같다(PTY 도 이미지 텍스처도 이 pane
        // 것이 아니거나 되살리기가 다시 쓴다).
        let emptied = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(outer).is_some_and(|p| p.tabs.is_empty())
        };
        if emptied {
            self.collapse_layout_only(outer);
        }
        // pane_window 미러에서 닫힌 탭 pid 를 걷는다(스폰 쪽과 대칭).
        self.publish_pty_layout();
    }
    /// Drain `dead_panes` and remove each from the BSP tree + pty map.
    /// Called on the main thread from `about_to_wait` so the mutation
    /// runs without competing with the per-session reader threads.
    /// If removing all panes empties the tree, exit the event loop.
    pub(crate) fn reap_dead_panes(&mut self, event_loop: &ActiveEventLoop) {
        let ids: Vec<String> = std::mem::take(&mut *self.dead_panes.lock().unwrap());
        let reaped = !ids.is_empty();
        for id in ids {
            // PTY 를 놓는 것과 트리에서 leaf 를 걷는 것은 경로가 갈려 있다 —
            // `swap_character` 는 PTY 를 **먼저** 놓고 같은 pane id 로 새로 띄우는데,
            // 그 spawn 이 실패하면 PTY 없이 leaf 만 남는다. 옛 PTY 의 EOF 는 그
            // 뒤에 도착하므로, pty 맵만 보고 건너뛰면 그 자리가 **빈 pane 으로
            // 영구히 남았다**(화면엔 빈 칸, board 엔 안 잡히고, 저장 때
            // `layout_to_json` 이 `{"leaf": null}` 을 흘린다 — 2026-08-11 실측).
            // 트리에 아직 leaf 가 있다는 건 정리가 안 끝났다는 뜻이므로 진행한다.
            if !self.pty.contains_key(&id) && !self.leaf_lingers_anywhere(&id) {
                continue;
            }
            self.remove_pane(&id);
        }
        // 자원이 사라진 자리를 매 턴 훑는다. 죽음 알림(`dead_panes`)이 오지 않은
        // 어긋남은 여기서만 잡히고, 그걸 놓치면 사용자가 손댈 수 없는 상태로 굳는다.
        self.sweep_orphan_leaves();
        self.sweep_lost_panes();
        self.sweep_empty_rooms();
        // Last pane closed (e.g. user typed `exit` in the only shell): shut the
        // window so kasaterm exits cleanly the way users expect from a regular
        // terminal.
        //
        // 실제로 걷은 턴에만 본다 — 스윕 때문에 매 턴 돌게 되면서, 부팅 직후처럼
        // pane 이 아직 없는 순간을 「마지막 pane 이 닫혔다」로 읽을 수 있다.
        if reaped && self.tmux.is_none() && self.pty.is_empty() {
            event_loop.exit();
        }
    }

    /// 이 pane 의 그리드가 **셸을 전제하는가** — 터미널 탭이 있는데 그 PTY 가 하나도
    /// 안 남았으면 참.
    ///
    /// 그리드 유무만으로는 죽음을 못 가른다. 웹·이미지·마크다운 pane 은 **정의상 PTY 가
    /// 없어서**(`session.rs`: 「웹 pane — PTY 를 안 띄운다. 그리드 자리만 앉히고…」)
    /// `!has_pty` 로 걷으면 멀쩡한 파일 pane 을 죽인다. 반대로 그리드가 있다고 살아
    /// 있다고 보면, 셸이 죽고 마지막 화면만 남은 터미널을 영영 못 걷는다. 그래서
    /// **pane 이 담은 것이 터미널인지**를 따로 본다.
    ///
    /// 탭이 여럿이면 **하나라도 살아 있으면 산 것**이다 — 보조 탭은 자기 pid 로 `pty` 에
    /// 들어가므로 바깥 pane id 만 조회하면 산 탭을 못 보고 걷게 된다.
    fn grid_needs_pty(&self, ws: &Workspace, id: &str) -> bool {
        let Some(pane) = ws.panes.get(id) else {
            return false;
        };
        let mut saw_terminal = false;
        for tab in &pane.tabs {
            if tab.term().is_none() {
                continue;
            }
            saw_terminal = true;
            // 첫 탭은 pid 를 안 들고 있을 수 있다 — 그때는 pane id 자체가 PTY 키다.
            let pid = tab.pid.as_deref().unwrap_or(id);
            if self.pty.contains_key(pid) {
                return false;
            }
        }
        saw_terminal
    }

    /// 트리에 자리는 있는데 자원(PTY·화면 그리드)이 없는 leaf 를 걷는다.
    ///
    /// `drop_pane_resources` 도 같은 검사를 하지만 그건 **자원을 놓는 그 순간**에만
    /// 돈다. 그 함수의 주석은 자원을 놓는 지점이 자기 하나뿐이라고 단언하는데 사실이
    /// 아니다 — `close_window`·`remove_pane_stashed`·`collapse_layout_only`·
    /// `close_tab` 이 `pty.remove`/`panes.remove` 를 직접 부른다. 그 넷 중 하나로
    /// 자원이 사라지면 검사가 영영 안 돈다.
    ///
    /// 그렇게 남은 자리는 `ws.panes` 에 없어 **클릭 판정에도 안 잡힌다** — 헤더도
    /// × 버튼도 우클릭 메뉴도 없는 검은 사각형이라 사용자가 치울 방법이 없다
    /// (2026-08-25 실측: 방 하나에 %8 이 그 상태로 남아 있었고, 앱조차 「그런 pane
    /// 없다」고 답하면서 화면에는 자리를 차지했다). 태어나는 자리를 하나씩 막는 대신
    /// 매 턴 훑는 이유는, 자원을 놓는 경로가 앞으로 더 생겨도 여기서 잡히기 때문이다.
    fn sweep_orphan_leaves(&mut self) {
        let mut orphans: Vec<String> = Vec::new();
        {
            let ws = self.ws.lock().unwrap();
            for tree in std::iter::once(self.pty_layout.as_ref())
                .chain(self.windows.iter().map(|w| w.as_ref()))
                .flatten()
            {
                for leaf in tree.leaves() {
                    if leaf_is_orphan(
                        self.pty.contains_key(leaf),
                        ws.panes.contains_key(leaf),
                        self.grid_needs_pty(&ws, leaf),
                    ) {
                        orphans.push(leaf.to_string());
                    }
                }
            }
        }
        if orphans.is_empty() {
            return;
        }
        orphans.sort();
        orphans.dedup();
        for id in orphans {
            self.collapse_orphan_leaf(&id);
        }
    }

    /// 자원은 살아 있는데 **어느 트리에도 없고 되살리기 목록에도 없는** pane 을
    /// 목록에 되돌린다.
    ///
    /// 닫아 둔 pane 은 `closed_panes` 가 유일한 손잡이다 — 트리에 없으니 화면에서
    /// 찾을 수 없고, 그 목록에서까지 빠지면 ⌘⇧T 로도 못 부른다. 그런데 개수 상한
    /// 정리(`push_closed_pane`)는 밀어낸 항목을 `kill_hidden_pane` 으로 죽이려 하고,
    /// 그 함수는 죽이기를 거부하는 가지가 있으며(트리에 leaf 가 남은 경우), `pty
    /// .remove` 가 프로세스를 끝낸다는 전제도 `Arc` 를 다른 곳에서 들고 있으면
    /// 깨진다. 어느 쪽이든 결과는 같다 — **목록에서는 빠졌는데 셸도 claude 도 계속
    /// 도는 pane.** 2026-08-25 실측으로 학생 하나가 그 상태로 6시간 48분 동안 화면
    /// 밖에서 일하고 있었고, 사용자는 그 pane 이 사라졌다고만 알고 있었다.
    ///
    /// 죽이지 않고 되돌리는 이유는, 여기서 알 수 있는 것이 「손잡이가 없다」뿐이고
    /// 그 pane 이 하던 일이 끝났는지는 알 수 없기 때문이다. 손잡이를 돌려주면
    /// 사용자가 보고 정한다. `stashed` 로 넣는 것도 같은 이유 — 되찾자마자 개수
    /// 상한에 다시 밀리면 헛일이다.
    fn sweep_lost_panes(&mut self) {
        use std::sync::{Mutex, OnceLock};
        use std::time::{Duration, Instant};
        // 「안 보인 지 얼마나 됐나」. **struct App 을 안 건드리는 함수-로컬**이다
        // (병렬 작업 규칙 — testkit 하네스와 같은 패턴).
        static LOST_SINCE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
        // 유예. 이 스윕은 매 이벤트 턴 도는데, pane 을 만들고 옮기는 조작은 PTY 를
        // 먼저 세우고 트리에 나중에 붙이는 순간을 지난다 — 하필 그 찰나에 돌면
        // 멀쩡히 태어나는 중인 pane 이 「잃어버린 것」으로 잡혀 **숨김으로 치워진다**
        // (2026-08-27 지적 「숨기지도 않았는데 막 숨어」, 미니맵에 숨김 표시). 한 턴
        // 어긋난 것은 다음 턴에 제자리로 오므로, 연속으로 안 보일 때만 걷는다.
        //
        // 진짜로 잃어버린 pane 은 3초 뒤에 그대로 잡힌다 — 안전망은 살아 있고,
        // 늦게 목록에 뜨는 것이 멀쩡한 pane 을 치우는 것보다 훨씬 낫다.
        const GRACE: Duration = Duration::from_secs(3);
        let lost: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            self.pty
                .keys()
                .filter(|id| {
                    // 보조 탭은 애초에 leaf 가 아니다 — 화면을 든 것은 그 탭이 사는
                    // 바깥 pane 이라 트리에 없는 게 정상이다.
                    let is_tab = ws.pid_to_pane.get(*id).is_some_and(|outer| outer != *id);
                    !is_tab && !self.leaf_lingers_anywhere(id) && self.stashed_record(id).is_none()
                })
                .cloned()
                .collect()
        };
        // 돌아온 것은 시계를 지운다 — 안 지우면 몇 분 전 한 번 어긋난 pane 이
        // 다음에 잠깐 안 보이는 순간 곧바로 유예를 지나 걷힌다.
        let now = Instant::now();
        let due: Vec<String> = {
            let mut seen = LOST_SINCE.get_or_init(Default::default).lock().unwrap();
            seen.retain(|id, _| lost.contains(id));
            lost.iter()
                .filter(|id| now.duration_since(*seen.entry((*id).clone()).or_insert(now)) >= GRACE)
                .cloned()
                .collect()
        };
        for id in due {
            // 사용자가 숨긴 적 없는 pane 을 앱이 치우는 것이라, 조용히 하면 안 된다 —
            // 다음에 이 일이 또 나면 무엇이 언제 걷혔는지가 유일한 단서다.
            eprintln!(
                "[sweep] pane {id} 이 {}초 넘게 어느 트리에도 없어 되살리기 목록으로 치웠다",
                GRACE.as_secs()
            );
            // 락 밖에서 — `record_closed_pane` 이 스스로 `ws` 를 잡는다.
            self.record_closed_pane(&id, true, true);
            LOST_SINCE
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .remove(&id);
        }
    }

    /// pane 이 하나도 없는 방 슬롯을 닫는다.
    ///
    /// `switch_window` 는 빈 슬롯이면 셸을 하나 띄워 되살리지만, 그건 사용자가 그
    /// 방을 눌렀을 때 얘기다. 아무도 안 누르면 껍데기가 사이드바에 계속 앉아 있고,
    /// 저장은 빈 방을 건너뛰므로 되살릴 기록조차 안 남는다. 방이 비는 것 자체는
    /// 막을 수 없다 — 그 방의 마지막 pane 이 스스로 끝나면 `remove_pane` 이 트리를
    /// 통째로 놓는다 — 그러니 빈 뒤에 걷는다.
    ///
    /// 활성 창은 제외한다. 그 슬롯이 `None` 인 것은 정상이고(트리가 `pty_layout` 에
    /// 있다), 새 방을 만드는 중에도 잠깐 그 상태를 지난다.
    fn sweep_empty_rooms(&mut self) {
        if self.windows.len() <= 1 {
            return;
        }
        let empty: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(i, slot)| slot.is_none() && *i != self.active_window)
            .map(|(i, _)| i)
            .collect();
        // 뒤에서부터 — `close_window` 가 슬롯을 배열에서 빼므로 앞부터 지우면
        // 남은 인덱스가 밀려 엉뚱한 방을 겨눈다.
        for i in empty.into_iter().rev() {
            let _ = self.close_window(i);
        }
    }
    /// Drag a single-tab pane onto its own body half. Spawns a fresh shell
    /// next to `source` on the side OPPOSITE the drop, so the original
    /// pane visually "lands" on the side the user threw it to. Distinct
    /// from `drop_tab_into_body` (which lifts a tab into a new pane on the
    /// drop side) — this one keeps the source intact and adds a sibling.
    pub(crate) fn split_pane_opposite(&mut self, source: &str, zone: DropZone) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("split via drag unsupported on tmux backend");
        }
        let (cols, rows) = self
            .pane_cells(source)
            .unwrap_or_else(|| self.window_cells());
        let cwd = self.spawn_cwd_from(Some(source));
        let new_id = self.alloc_pane_id();
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: crate::proxy_env(&new_id),
            pane_id: new_id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        let session = Arc::new(session);
        self.pump_pty_screens(
            session.screens.clone(),
            new_id.clone(),
            std::sync::Arc::downgrade(&session),
        );
        self.insert_pty(new_id.clone(), session);
        // `before=true` means the new leaf becomes the LEFT/TOP child, so
        // the source ends up on the RIGHT/BOTTOM. We want source on the
        // dropped side → new on the opposite side.
        let (dir, before) = match zone {
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, false),
            // Center is handled by the caller as a tab merge — splitting
            // would lose the "drop into this pane's tabs" intent.
            DropZone::Center => return Ok(()),
        };
        let inserted = self
            .pty_layout
            .as_mut()
            .map(|t| t.insert_beside(source, dir, before, new_id.clone()))
            .unwrap_or(false);
        if !inserted {
            // Source vanished mid-drag — bail and clean up the spawned shell.
            self.pty.remove(&new_id);
            return Ok(());
        }
        let (win_cols, win_rows) = self.window_cells();
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        // Focus the freshly-spawned pane so the user is typing into it.
        self.ws.lock().unwrap().active_pane = Some(new_id);
        self.handoff_ime_to_active_surface();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }
    /// Tab drag dropped onto another pane's BODY. Splits the target pane
    /// in the matching quadrant and makes the moved tab the new leaf — the
    /// dragged shell now lives in its own pane next to `target`. Unifies
    /// the old "drag pane header" semantics into the tab drag so there's
    /// one drop UX.
    pub(crate) fn drop_tab_into_body(&mut self, td: &TabDrag, target: &str, zone: DropZone) {
        // 1. Lift the tab out of source.
        let (moved, src_empty): (Option<PaneTab>, bool) = {
            let mut ws = self.ws.lock().unwrap();
            let Some(src) = ws.panes.get_mut(&td.pane) else {
                return;
            };
            if td.from >= src.tabs.len() {
                return;
            }
            let t = src.tabs.remove(td.from);
            if td.from < src.active_tab && src.active_tab > 0 {
                src.active_tab -= 1;
            }
            if src.active_tab >= src.tabs.len() && !src.tabs.is_empty() {
                src.active_tab = src.tabs.len() - 1;
            }
            src.dirty = true;
            let empty = src.tabs.is_empty();
            (Some(t), empty)
        };
        let Some(moved) = moved else { return };
        // 2. If source emptied, drop it from layout (PtySession survives —
        //    it's the very shell we're about to re-attach as a new leaf).
        if src_empty {
            self.ws.lock().unwrap().panes.remove(&td.pane);
            self.collapse_layout_only(&td.pane);
        }
        // 3. Allocate a fresh layout id for the new pane. Layout ids and
        //    pty ids decoupled from stage-3 onward, so this avoids any
        //    clash with the moved tab's pid (which may have been the old
        //    source's outer id).
        let new_outer = self.alloc_pane_id();
        let (dir, before) = match zone {
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
            // Caller routes Center to the cross-pane tab-merge path; if it
            // slips through here, abort the split so we don't double-spawn.
            DropZone::Center => return,
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            if !tree.insert_beside(target, dir, before, new_outer.clone()) {
                // Target gone — fall back to inserting at the first leaf.
                if let Some(anchor) = tree.leaves().first().map(|s| s.to_string()) {
                    tree.insert_beside(&anchor, dir, before, new_outer.clone());
                }
            }
        } else {
            self.pty_layout = Some(kasa_pty::PtyLayout::single(&new_outer));
        }
        // 4. Build the new PaneState with the moved tab as its only tab.
        let moved_pid = moved.pid.clone();
        {
            let mut ws = self.ws.lock().unwrap();
            let mut ps = PaneState::default();
            ps.tabs.clear();
            ps.tabs.push(moved);
            ps.active_tab = 0;
            ps.dirty = true;
            ws.panes.insert(new_outer.clone(), ps);
            if let Some(pid) = moved_pid {
                // Rebind the pid map so future ScreenUpdates / find_tab_by_pty
                // route to new_outer even when pid != new_outer.
                ws.pid_to_pane.insert(pid, new_outer.clone());
            }
            ws.active_pane = Some(new_outer.clone());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// pane 을 통째로 `dst` 의 탭 스트립 안에 넣는다 — 드래그를 헤더/본문 중앙에
    /// 놓았을 때(`DropZone::Center`). 소스의 탭 **전부**가 순서대로 dst 뒤에 붙고
    /// 소스는 레이아웃에서 사라진다. split 과 달리 화면이 더 쪼개지지 않는다.
    ///
    /// `remove_pane` 이 아니라 `collapse_layout_only` 로 걷어내는 게 급소다 —
    /// 옮긴 탭들의 PtySession·이미지 텍스처·마크다운 캐시는 이제 dst 소유인데,
    /// `remove_pane` 은 그것들을 소스 것으로 보고 죽여 버린다(빈 pane 만 남는다).
    ///
    /// 반환값 = 실제로 옮겼는지. 자기 자신·없는 pane·빈 소스면 false.
    pub(crate) fn merge_pane_into_tabs(&mut self, src: &str, dst: &str) -> bool {
        if src == dst {
            return false;
        }
        let moved: Vec<PaneTab> = {
            let mut ws = self.ws.lock().unwrap();
            if !ws.panes.contains_key(dst) {
                return false;
            }
            let Some(s) = ws.panes.get_mut(src) else {
                return false;
            };
            // 비었는지 **비우기 전에** 본다. 순서가 반대면 빈 소스를 만났을 때
            // 이미 `take` 로 비워 놓고 early return 으로 나가, 탭 없는 껍데기가
            // `ws.panes` 에 남는다 — 그 pane 을 그리는 순간 앱이 죽는다.
            if s.tabs.is_empty() {
                return false;
            }
            std::mem::take(&mut s.tabs)
        };
        {
            let mut ws = self.ws.lock().unwrap();
            // 옮긴 탭들의 pid → dst 재바인딩. 이걸 빼먹으면 앞으로 오는
            // ScreenUpdate 가 사라진 소스로 배달돼 화면이 얼어붙는다.
            for t in &moved {
                if let Some(pid) = t.pid.clone() {
                    ws.pid_to_pane.insert(pid, dst.to_string());
                }
            }
            if let Some(d) = ws.panes.get_mut(dst) {
                // 끌어온 것을 곧바로 보여 준다 — 사용자가 방금 옮긴 pane 이
                // 뒤에 숨어 있으면 "사라졌다"로 읽힌다.
                let first = d.tabs.len();
                d.tabs.extend(moved);
                d.active_tab = first;
                d.dirty = true;
            }
            ws.panes.remove(src);
        }
        self.collapse_layout_only(src);
        self.ws.lock().unwrap().active_pane = Some(dst.to_string());
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }
    /// Cross-pane drag aftermath. The source pane lost every tab to dest;
    /// we just need its layout slot gone — *not* the PtySession (now owned
    /// by dest under the same pid key) or the image / markdown caches the
    /// moved tabs depend on. Picks a survivor focus exactly like
    /// `remove_pane` so the chrome doesn't blink to "no active".
    pub(crate) fn collapse_layout_only(&mut self, target: &str) {
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => return,
        };
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            Some(if cur_idx + 1 < leaves.len() {
                leaves[cur_idx + 1].clone()
            } else {
                leaves[cur_idx - 1].clone()
            })
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            self.pty_layout = None;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            ws.rebuild_pid_map();
            if was_active && next_focus.is_some() {
                ws.active_pane = next_focus;
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        if was_active {
            self.handoff_ime_to_active_surface();
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Drop the closed pane's collab markers. A stale /tmp/kasaterm-bound-_N
    /// can pass a dead session off as live when the pane number is reused
    /// within the same socket generation — the recovery guard's inode-
    /// generation check can't see that case, so deleting at close time is
    /// the root fix. character-/god-nudged-(레거시) markers likewise leak a roster
    /// slot and re-arm a suppressed nudge.
    ///
    /// `character-<N>` 는 **이 pane 의 방(cwd slug)에서만** 지운다 — pane 번호는 방 간
    /// 유니크가 아니다(윈도우마다 %1 재사용). 모든 방을 쓸면 다른 방의 *살아있는* 같은
    /// 번호 pane 의 캐릭터 마커까지 삭제돼, board 가 char=None 으로 떠 프사가 사라지고
    /// 그 캐릭터가 "미사용"으로 재배정됐다(거노: 캐릭터 주입 안 됨). cwd 를 모르면(캐시
    /// 미스) 폴백으로 전체를 쓴다 — 닫힌 pane 마커가 새는 것보단 낫다.
    pub(crate) fn cleanup_collab_markers(target: &str, cwd: Option<&std::path::Path>) {
        // %3 → _3, mirroring the shell hooks' ${ID//[^A-Za-z0-9]/_}.
        let safe: String = target
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let _ = std::fs::remove_file(kasa_socket::bound_marker_path(&safe));
        let num = target.trim_start_matches('%');
        let slug = cwd.map(kasa_mcp::character::mode_slug);
        if let Ok(rooms) = std::fs::read_dir(kasa_socket::collab_root()) {
            for room in rooms.flatten() {
                // slug 를 알면 그 cwd 의 방(정확히 그 slug, 또는 __room_<id> 변형)만.
                if let Some(s) = &slug {
                    let name = room.file_name();
                    let Some(rn) = name.to_str() else { continue };
                    if rn != s.as_str() && !rn.starts_with(&format!("{s}__room_")) {
                        continue;
                    }
                }
                let p = room.path();
                let _ = std::fs::remove_file(p.join(format!("character-{num}")));
                let _ = std::fs::remove_file(p.join(format!("god-nudged-{target}")));
            }
        }
        Self::archive_roster_pane(target);
    }
    /// Mark a closed pane's roster entries `archived=true` (munder 차용, ②a) so
    /// `roster_recovery` stops offering a deliberately-closed worker for resume.
    /// Sweeps every cwd roster file (pane numbers are unique across rooms) and
    /// uses the same `.lock` flock discipline as the bind hook's Python RMW —
    /// so we reuse that exact path by shelling out (best-effort, detached: close
    /// is interactive and rare, and archiving is advisory). A resume re-binds
    /// the pane fresh, which drops `archived` again (④).
    fn archive_roster_pane(target: &str) {
        const SCRIPT: &str = r#"
import sys, os, json, glob
try:
    import fcntl
except ImportError:
    fcntl = None
pane = sys.argv[1]
d = os.path.expanduser('~/.config/kasaterm/agent-roster')
for p in glob.glob(os.path.join(d, '*.json')):
    lf = open(p + '.lock', 'w')
    if fcntl is not None:
        fcntl.flock(lf.fileno(), fcntl.LOCK_EX)
    try:
        try:
            roster = json.load(open(p))
            if not isinstance(roster, dict):
                continue
        except Exception:
            continue
        v = roster.get(pane)
        if not isinstance(v, dict) or v.get('archived'):
            continue
        v['archived'] = True
        tmp = p + '.tmp'
        json.dump(roster, open(tmp, 'w'), ensure_ascii=False)
        os.replace(tmp, p)
    finally:
        if fcntl is not None:
            fcntl.flock(lf.fileno(), fcntl.LOCK_UN)
        lf.close()
"#;
        let Some(py) = crate::python3_program() else {
            return;
        };
        let _ = crate::proc::command(py)
            .arg("-X")
            .arg("utf8")
            .arg("-c")
            .arg(SCRIPT)
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    /// stash 트리(Option 슬롯)에서 leaf 하나 제거. 마지막 leaf 면 트리째 비운다
    /// (`None`). 제거했으면 true.
    fn remove_stashed_leaf(slot: &mut Option<kasa_pty::PtyLayout>, target: &str) -> bool {
        let Some(tree) = slot.as_mut() else {
            return false;
        };
        let ls = tree.leaves();
        if !ls.iter().any(|l| *l == target) {
            return false;
        }
        if ls.len() > 1 {
            tree.remove_leaf(target);
        } else {
            *slot = None;
        }
        true
    }

    /// `remove_stashed_leaf` 의 읽기 전용 짝 — 이 슬롯이 그 leaf 를 담고 있는지.
    fn stashed_leaf_exists(slot: &Option<kasa_pty::PtyLayout>, target: &str) -> bool {
        slot.as_ref()
            .map(|t| t.leaves().iter().any(|l| *l == target))
            .unwrap_or(false)
    }

    /// PTY 가 이미 없는 pane 이 아직 어느 트리엔가 leaf 로 남아 있는지 — 남아 있으면
    /// 그 자리는 그릴 것이 없는 유령 pane 이다. 활성 창만 봐서는 안 된다: 다른
    /// 윈도우로 전환해 둔 pane 은 stash 슬롯에 있고, 백그라운드 세션은 자기 트리를
    /// 따로 쥔다. 실제로 빈 칸이 남은 자리가 활성 창이 아니라 **다른 윈도우**였다.
    fn leaf_lingers_anywhere(&self, target: &str) -> bool {
        if Self::stashed_leaf_exists(&self.pty_layout, target) {
            return true;
        }
        if self
            .windows
            .iter()
            .any(|w| Self::stashed_leaf_exists(w, target))
        {
            return true;
        }
        self.sessions.iter().flatten().any(|s| {
            Self::stashed_leaf_exists(&s.pty_layout, target)
                || s.windows
                    .iter()
                    .any(|w| Self::stashed_leaf_exists(w, target))
        })
    }

    /// active 트리 밖(현재 세션의 stash 윈도우 + 백그라운드 세션들)에서 pane 을
    /// 걷어낸다 — CLI close 가 다른 윈도우/세션 pane 을 겨눌 때 유령 leaf(빈
    /// pane)가 남는 것 방지. 마지막 leaf 로 윈도우가 비면 그 윈도우도 닫는다.
    fn remove_pane_stashed(&mut self, target: &str) {
        let mut emptied_win: Option<usize> = None;
        for (i, slot) in self.windows.iter_mut().enumerate() {
            if Self::remove_stashed_leaf(slot, target) && slot.is_none() {
                emptied_win = Some(i);
            }
        }
        if let Some(i) = emptied_win {
            // close_window 가 인덱스·탭 스트립 보정까지 처리. 마지막 윈도우면
            // bail 하니 빈 슬롯이 잠시 남을 뿐 상태는 안 깨진다.
            let _ = self.close_window(i);
        }
        for sess in self.sessions.iter_mut().flatten() {
            sess.pty.remove(target);
            if let Ok(mut ws) = sess.ws.lock() {
                ws.panes.remove(target);
                ws.rebuild_pid_map();
            }
            Self::remove_stashed_leaf(&mut sess.pty_layout, target);
            let mut emptied: Option<usize> = None;
            for (i, slot) in sess.windows.iter_mut().enumerate() {
                if Self::remove_stashed_leaf(slot, target) && slot.is_none() {
                    emptied = Some(i);
                }
            }
            // 백그라운드 세션엔 close_window 를 못 쓴다(&mut self 라이브 필드
            // 전제) — 빈 윈도우 슬롯 제거와 active_window 보정만 직접.
            if let Some(i) = emptied {
                if i != sess.active_window && sess.windows.len() > 1 {
                    sess.windows.remove(i);
                    if sess.active_window > i {
                        sess.active_window -= 1;
                    }
                }
            }
        }
    }

    /// Internal: drop a pane regardless of whether it's the active one.
    /// Used by both `close_pane` (Cmd+W / header ×) and `reap_dead_panes`
    /// (shell exit). Picks a survivor focus when removing the focused
    /// pane.
    /// pane 이 쥐고 있던 자원을 놓는다 — PTY(**여기서 셸과 claude 가 죽는다**)·보조
    /// 탭 셸·협업 마커·GPU 텍스처·마크다운 캐시·화면 상태. 트리는 건드리지 않으므로
    /// 트리에서 이미 빠진 pane(숨긴 것)에도 그대로 쓴다.
    pub(crate) fn drop_pane_resources(&mut self, target: &str) {
        let closed_cwd = self.pane_cwd_cache.get(target).cloned();
        // `Arc<PtySession>` 의 마지막 주인을 놓는 지점 — 이 한 줄이 프로세스의 생사다.
        self.pty.remove(target);
        Self::cleanup_collab_markers(target, closed_cwd.as_deref());
        // Free the GPU texture if this was an image pane (no-op otherwise).
        if let Some(g) = self.gpu.as_mut() {
            g.drop_image(target);
        }
        self.md_content_h.remove(target);
        self.md_block_ys.remove(target);
        self.md_scroll_anchor.remove(target);
        // Drop secondary-tab ptys hosted by this pane and prune the reverse
        // map. Without this, an in-pane tab's shell would linger past its
        // container pane and `find_tab_by_pty` would point at a dead outer.
        let secondary_pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            ws.pid_to_pane
                .iter()
                .filter_map(|(pid, outer)| (outer == target).then(|| pid.clone()))
                .collect()
        };
        for pid in &secondary_pids {
            self.pty.remove(pid);
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            ws.rebuild_pid_map();
        }
        // 자원을 놓았는데 트리에 leaf 가 남으면 그 자리는 **도달 불가능한 검은
        // 사각형**이 된다 — 셸도 그리드도 없어 클릭할 것이 없고, 사용자가 치울
        // 방법조차 없다. 어느 죽음 경로로 왔든 자원을 놓는 지점은 여기 하나이므로,
        // 유령이 태어날 수 있는 자리도 여기 하나다.
        self.collapse_orphan_leaf(target);
    }

    /// 자원이 이미 없는 leaf 를 **트리에서만** 걷는다. `remove_pane` 과 달리 자원을
    /// 안 건드리므로 `drop_pane_resources` 안에서 불러도 재귀가 나지 않는다.
    fn collapse_orphan_leaf(&mut self, target: &str) {
        let (has_grid, needs_pty) = {
            let ws = self.ws.lock().unwrap();
            (
                ws.panes.contains_key(target),
                self.grid_needs_pty(&ws, target),
            )
        };
        if !leaf_is_orphan(self.pty.contains_key(target), has_grid, needs_pty) {
            return;
        }
        let in_active = self
            .pty_layout
            .as_ref()
            .is_some_and(|t| t.leaves().iter().any(|l| *l == target));
        if in_active {
            self.collapse_layout_only(target);
        } else if self.leaf_lingers_anywhere(target) {
            self.remove_pane_stashed(target);
        } else {
            return;
        }
        // 소리 없이 사라지면 사용자는 학생이 어떻게 된 건지 알 방법이 없다
        // (거노가 두 번 물었다). 되살리기 목록엔 이미 레코드가 있으므로 그리로
        // 안내한다.
        self.set_toast(format!("{target} 이 끝나 자리를 접었다 — ⌘⇧T 로 되살린다"));
    }

    /// 사용자가 닫은 pane — **죽이지 않고 화면에서만 뗀다.** BSP 트리에서 leaf 를
    /// 빼는 것이 전부라 PTY 도 화면 상태도 남고, 그래서 그 안의 claude 는 하던 일을
    /// 계속한다(거노: resume 로 잇는 게 아니라 데몬처럼 돌기를 원함). 출력이 유실될
    /// 걱정은 없다 — 화면 갱신은 pane 마다 붙은 전용 스레드(`pump_pty_screens`)라
    /// 트리와 무관하게 계속 돈다. 리사이즈는 `leaf_cells` 기반이라 트리 밖 pane 을
    /// 건드리지 않아 마지막 크기가 그대로 유지된다.
    ///
    /// 되살리기는 `reopen_pane_record` 의 재부착 경로, 정말 끄는 것은 인포의 ×
    /// (`discard_closed_pane_at`)다.
    pub(crate) fn hide_pane(&mut self, target: &str) {
        self.tuck_pane(target, false);
    }

    /// 사이드바 「pane 숨기기」 — 닫기와 같은 자리에 넣되 **절대 정리하지 않는다.**
    ///
    /// 닫기(`hide_pane`)는 개수 상한과 15분 idle 로 언젠가 프로세스를 놓는다. 그런데
    /// 숨기기는 *작업이 도는 중에* 화면에서만 치우는 것이라(2026-08-11 지시), 돌아왔을
    /// 때 대화가 끊겨 있으면 쓸모가 없다. 그래서 같은 스택에 `stashed` 로 넣고 두 정리
    /// 루프가 건너뛰게 한다.
    pub(crate) fn stash_pane(&mut self, target: &str) {
        self.tuck_pane(target, true);
    }

    fn tuck_pane(&mut self, target: &str, stashed: bool) {
        let in_active = self
            .pty_layout
            .as_ref()
            .is_some_and(|t| t.leaves().iter().any(|l| *l == target));
        if !in_active {
            // 다른 윈도우·백그라운드 세션의 pane 은 숨김 대상이 아니다(그쪽 트리를
            // 여기서 조작하면 유령 leaf 가 남는다) — 기존 경로로 보낸다.
            //
            // ⚠️ 그 경로는 **죽인다.** 사이드바는 모든 방의 pane 을 보여주므로, 거기서
            // 부를 때는 부르는 쪽이 먼저 `switch_window` 로 그 방을 활성으로 만들어야
            // 한다. 안 그러면 「숨겼는데 학생이 사라졌다」가 된다.
            self.remove_pane(target);
            return;
        }
        self.record_closed_pane(target, true, stashed);
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let i = leaves.iter().position(|l| l == target).unwrap_or(0);
            Some(leaves[if i + 1 < leaves.len() { i + 1 } else { i - 1 }].clone())
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            self.pty_layout = None;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            if was_active {
                ws.active_pane = next_focus;
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        if was_active {
            self.handoff_ime_to_active_surface();
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 숨겨 둔 pane 을 정말 끈다 — 트리 밖이라 레이아웃 조작이 필요 없고, 자원만
    /// 놓으면 된다. `remove_pane` 을 쓰면 되살리기 기록이 한 번 더 쌓인다.
    pub(crate) fn kill_hidden_pane(&mut self, target: &str) {
        // 트리에 아직 leaf 가 있으면 그건 **숨긴 pane 이 아니다** — 화면에 떠 있는
        // 남의 pane 이고, 이 호출은 같은 번호를 물려받은 낡은 레코드에서 왔다.
        // 그대로 진행하면 그 pane 의 셸과 claude 를 끄면서 트리는 안 걷어, 클릭도
        // 안 되는 검은 사각형만 남는다. 번호 재사용은 `used_pane_ids` 가 이미
        // 막지만, 앱을 껐다 켜 레코드만 복원된 자리 같은 틈은 여기서 닫는다.
        if self.leaf_lingers_anywhere(target) {
            return;
        }
        // 원격 pane 의 「진짜 끄기」(되살리기 목록에서 지우기) — 원격 셸도 함께.
        // 위 lingers 가드 **뒤**여야 한다: 낡은 레코드가 살아 있는 pane 번호를
        // 가리킬 때 그 pane 의 원격 셸을 죽이면 안 된다.
        kasa_mcp::remote::kill_remote(target);
        self.drop_pane_resources(target);
        self.chrome_dirty = true;
    }

    pub(crate) fn remove_pane(&mut self, target: &str) {
        // 원격 pane 이면 원격 셸까지 죽인다 — 여기는 「진짜 끄기」 경로다.
        // detach(앱 종료·재시작)는 이 함수를 안 타고 Arc drop 만으로 끝난다.
        kasa_mcp::remote::kill_remote(target);
        // 숨겨 둔(alive) 레코드가 이 번호를 물고 있으면 먼저 걷는다 — 지금 이 PTY 가
        // 죽으므로 그 손잡이는 재부착할 것이 없는 거짓이 된다. 되살리기 재료는 바로
        // 아래에서 alive=false 로 새로 적으니 잃지 않는다.
        self.drop_live_closed_records(target);
        // 사라지기 전에 되살릴 재료를 챙긴다(⌘⇧T). 여기가 "정말 죽는" 경로의 길목이라
        // 한 줄이면 충분하다 — 셸이 스스로 끝난 pane(`reap_dead_panes`)도 지나므로,
        // 실수로 exit 한 학생도 되돌릴 수 있다. 다만 그건 프로세스가 이미 없으니
        // 되살리기가 재부착이 아니라 레코드로 새로 띄우는 쪽이다(`alive=false`).
        self.record_closed_pane(target, false, false);
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => {
                // active 트리가 없어도(단일 pane 폴백) stash 쪽 유령은 걷어낸다.
                self.remove_pane_stashed(target);
                return;
            }
        };
        // CLI close 는 stash 된 윈도우·백그라운드 세션의 pane 도 겨눈다 — 그때
        // active 트리를 조작하면 (a) 유령 leaf 가 그쪽 트리에 남아 빈 pane 으로
        // 렌더되고 (b) active 가 단일 pane 이면 멀쩡한 트리를 통째 드랍한다.
        let in_active = leaves.iter().any(|l| l == target);
        if !in_active {
            self.remove_pane_stashed(target);
        }
        let next_focus: Option<String> = if was_active && in_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            if cur_idx + 1 < leaves.len() {
                Some(leaves[cur_idx + 1].clone())
            } else {
                Some(leaves[cur_idx - 1].clone())
            }
        } else {
            None
        };
        if in_active {
            if leaves.len() > 1 {
                if let Some(tree) = self.pty_layout.as_mut() {
                    tree.remove_leaf(target);
                }
            } else {
                // Last leaf — drop the tree entirely so single-pane
                // fallback re-engages if a future split repopulates it.
                self.pty_layout = None;
            }
        }
        self.drop_pane_resources(target);
        {
            let mut ws = self.ws.lock().unwrap();
            if was_active {
                ws.active_pane = next_focus;
            }
            // Layout shrank — every survivor needs a repaint, else the render
            // loop sees pane.dirty=false and skips the GPU pass, leaving the
            // closed pane's slot blank until the next dirty signal.
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        if was_active {
            self.handoff_ime_to_active_surface();
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Close `pid`'s pane: remove it from the BSP tree and drop its PTY.
    /// Shared by Cmd+W and the header × button. When `pid` is the window's
    /// last pane we don't leave an empty window — we spawn a fresh shell in
    /// its place (거노: BA GUI 종료 버튼으로 마지막 claude pane 을 닫으면 빈 창이
    /// 되어 안 닫히던 것 → 새 tty 셸로 교체). Otherwise just remove it.
    pub(crate) fn close_pane(&mut self, pid: &str) {
        if self.tmux.is_some() {
            return;
        }
        // 보조 탭 pid(`surface.new_tab` 이 준 id)는 탭 닫기 경로로 — pane 경로는
        // 트리·stash 만 보므로 PTY 는 죽여도 바깥 pane 의 tabs 항목이 남아, 죽은
        // 셸을 문 유령 탭이 탭바에 계속 떠 있었다(dismiss 로 탭 학생을 걷을 때).
        let tab_slot: Option<(String, usize)> = {
            let ws = self.ws.lock().unwrap();
            ws.pid_to_pane
                .get(pid)
                .filter(|outer| outer.as_str() != pid)
                .and_then(|outer| {
                    ws.panes.get(outer).and_then(|p| {
                        p.tabs
                            .iter()
                            .position(|t| t.pid.as_deref() == Some(pid))
                            .map(|idx| (outer.clone(), idx))
                    })
                })
        };
        if let Some((outer, idx)) = tab_slot {
            self.close_tab(&outer, idx);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // stash 윈도우·백그라운드 세션 pane(CLI close)은 active 트리와 무관 —
        // 아래 "마지막 pane 대체 셸 스폰" 분기를 타면 active 윈도우에 불필요한
        // 새 셸이 생긴다. remove_pane 이 stash 쪽 정리까지 맡는다.
        let in_active = self
            .pty_layout
            .as_ref()
            .is_some_and(|t| t.leaves().iter().any(|l| *l == pid));
        if !in_active {
            self.remove_pane(pid);
            return;
        }
        let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
        if leaves <= 1 {
            // split a fresh shell next to it, then hide the original — the new
            // shell takes over the whole window. 원본은 죽지 않고 백그라운드로
            // 물러날 뿐이라 ⌘⇧T 로 그대로 데려올 수 있다.
            if let Ok(new_id) = self.split_active_pane_focused(kasa_pty::SplitDir::Horizontal) {
                if !new_id.is_empty() && new_id != pid {
                    self.hide_pane(pid);
                }
            }
            return;
        }
        self.hide_pane(pid);
    }
    /// Cmd+W: close the active *tab*. A pane with several tabs drops only the
    /// focused one — the rest stay alive (the "Cmd+W killed every bound tab /
    /// my claude pane" bug). Routes through `confirm_or_close_tab`, which both
    /// decides tab-vs-pane (last tab → pane, no-op on a single-pane window) and
    /// raises the "close while a job is running?" modal when needed.
    pub(crate) fn close_active_tab(&mut self) {
        let (pane, idx) = {
            let ws = self.ws.lock().unwrap();
            let Some(id) = ws.active_pane.clone() else {
                return;
            };
            let idx = ws.panes.get(&id).map(|p| p.active_tab).unwrap_or(0);
            (id, idx)
        };
        self.confirm_or_close_tab(&pane, idx);
    }
    /// Cycle focus to the previous (delta=-1) or next (delta=+1) pane
    /// in document order. No-op when there's only one pane.
    /// N번째 pane 으로 바로 점프(Ctrl+1..9). 순서는 `cycle_focus` 와 같은 문서
    /// 순서다 — 두 손이 다른 순서를 세면 「⌘] 로 두 번 = Ctrl+3」이 안 맞아
    /// 어느 쪽도 못 믿게 된다. 범위 밖 번호는 조용히 무시(마지막으로 접지 않는다 —
    /// 4번을 눌렀는데 3번으로 가면 오타가 이동으로 굳는다).
    pub(crate) fn focus_pane_at(&mut self, idx: usize) {
        let Some(tree) = self.pty_layout.as_ref() else {
            return;
        };
        let leaves: Vec<String> = tree.leaves().iter().map(|s| s.to_string()).collect();
        let Some(target) = leaves.get(idx) else {
            return;
        };
        if self.ws.lock().unwrap().active_pane.as_deref() == Some(target.as_str()) {
            return;
        }
        self.focus_pane(target);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    pub(crate) fn cycle_focus(&mut self, delta: i32) {
        let Some(tree) = self.pty_layout.as_ref() else {
            return;
        };
        let leaves: Vec<String> = tree.leaves().iter().map(|s| s.to_string()).collect();
        if leaves.len() < 2 {
            return;
        }
        let cur_idx = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .and_then(|id| leaves.iter().position(|l| l == id))
            .unwrap_or(0);
        let n = leaves.len() as i32;
        let new_idx = ((cur_idx as i32 + delta).rem_euclid(n)) as usize;
        let new_active = leaves[new_idx].clone();
        self.focus_pane(&new_active);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Pane whose rectangle lies immediately in `dir` of the active pane
    /// and overlaps it on the perpendicular axis. Picks the nearest by
    /// centre distance so a tall neighbour split into several panes still
    /// resolves to the one the user is pointing at. None when there is no
    /// pane on that side.
    pub(crate) fn adjacent_pane(&self, dir: FocusDir) -> Option<String> {
        let tree = self.pty_layout.as_ref()?;
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        if rects.len() < 2 {
            return None;
        }
        let active = self.ws.lock().unwrap().active_pane.clone()?;
        let cur = rects.iter().find(|(id, ..)| id == &active)?;
        let (cx, cy, cw, ch) = (cur.1 as f32, cur.2 as f32, cur.3 as f32, cur.4 as f32);
        let (acx, acy) = (cx + cw / 2.0, cy + ch / 2.0);
        let mut best: Option<(String, f32)> = None;
        for (id, x, y, w, h) in &rects {
            if id == &active {
                continue;
            }
            let (x, y, w, h) = (*x as f32, *y as f32, *w as f32, *h as f32);
            let overlap_y = y < cy + ch && y + h > cy;
            let overlap_x = x < cx + cw && x + w > cx;
            let ok = match dir {
                FocusDir::Left => x + w <= cx + 1.0 && overlap_y,
                FocusDir::Right => x >= cx + cw - 1.0 && overlap_y,
                FocusDir::Up => y + h <= cy + 1.0 && overlap_x,
                FocusDir::Down => y >= cy + ch - 1.0 && overlap_x,
            };
            if !ok {
                continue;
            }
            let dist = (x + w / 2.0 - acx).abs() + (y + h / 2.0 - acy).abs();
            if best.as_ref().is_none_or(|(_, d)| dist < *d) {
                best = Some((id.clone(), dist));
            }
        }
        best.map(|(id, _)| id)
    }
    /// Move keyboard focus to the adjacent pane in `dir`.
    pub(crate) fn focus_dir(&mut self, dir: FocusDir) {
        if let Some(id) = self.adjacent_pane(dir) {
            self.focus_pane(&id);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
    /// Swap the active pane with its neighbour in `dir`. The BSP tree
    /// exchanges the two leaves' ids, so each pane's content moves into
    /// the other's slot while the PTYs stay put; focus rides along with
    /// the active id into its new position.
    pub(crate) fn swap_dir(&mut self, dir: FocusDir) {
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return;
        };
        let Some(target) = self.adjacent_pane(dir) else {
            return;
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            tree.swap_leaves(&active, &target);
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Pane whose header band contains the cursor (logical px), or None.
    /// Headers only exist when the workspace is split.
    pub(crate) fn header_at_px(&self, x: f32, y: f32) -> Option<String> {
        let (cols, rows) = self.window_cells();
        let rects = self.effective_leaf_rects(cols, rows);
        // A zoomed pane is a single rect but still has a header (to un-zoom),
        // so only bail on a lone pane when nothing is zoomed.
        if rects.len() <= 1 && self.zoomed_pane.is_none() {
            return None;
        }
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        for (id, cx, cy, cw, _ch) in rects {
            let bx = pad + cx as f32 * self.cell.w;
            let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
            let bw = cw as f32 * self.cell.w;
            if x >= bx && x <= bx + bw && y >= by && y <= by + PANE_HEADER_HEIGHT {
                return Some(id);
            }
        }
        None
    }
    /// Pane + edge the cursor is over, for header drag-and-drop. The zone
    /// is the dominant axis from the pane box centre, so the cursor always
    /// resolves to one of the four edges. None when off every pane.
    pub(crate) fn drop_target_at(&self, x: f32, y: f32) -> Option<(String, DropZone)> {
        let tree = self.pty_layout.as_ref()?;
        let leaves_count = tree.leaves().len();
        let (cols, rows) = self.window_cells();
        let rects = self.effective_leaf_rects(cols, rows);
        self.drop_zone_in_rects(&rects, leaves_count, x, y)
    }
    /// Geometry core of `drop_target_at`, split out so a live drag can hit-test
    /// against an arbitrary tree's rects (e.g. the base tree with the carried
    /// pane removed) instead of the current `pty_layout`. `leaves_count` drives
    /// the header band (single-leaf panes have none).
    pub(crate) fn drop_zone_in_rects(
        &self,
        rects: &[(String, u16, u16, u16, u16)],
        leaves_count: usize,
        x: f32,
        y: f32,
    ) -> Option<(String, DropZone)> {
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        // When the layout has >1 leaf every pane gets a header band — including
        // single-tab panes — so a drop onto a single-tab header reads as Center
        // (tab-merge), not the body's Up zone (the "drag→merge gives split" bug).
        let header_band = if leaves_count > 1 {
            PANE_HEADER_HEIGHT
        } else {
            0.0
        };
        for (id, cx, cy, cw, ch) in rects {
            let bx = pad + *cx as f32 * self.cell.w;
            let pane_top = TITLE_HEIGHT + *cy as f32 * self.cell.h;
            let bw = (*cw as f32 * self.cell.w).max(1.0);
            let bh = (*ch as f32 * self.cell.h).max(1.0);
            let body_top = pane_top + header_band;
            if x >= bx && x <= bx + bw && y >= pane_top && y <= pane_top + bh {
                if y < body_top {
                    return Some((id.clone(), DropZone::Center));
                }
                let body_h = (pane_top + bh - body_top).max(1.0);
                let nx = (x - (bx + bw / 2.0)) / (bw / 2.0);
                let ny = (y - (body_top + body_h / 2.0)) / (body_h / 2.0);
                return Some((id.clone(), drop_zone_for_offsets(nx, ny)));
            }
        }
        None
    }
    /// Window chip in the left sidebar under the cursor, resolved to that
    /// window's anchor leaf — the drop target for a cross-window header drag.
    /// Returns None when off every chip or over the already-active window (its
    /// panes are on screen, so an in-window drop is `drop_target_at`'s job).
    /// The daemon's `move_surface` does the actual cross-window detach/insert.
    pub(crate) fn sidebar_window_drop_target(&self, x: f32, y: f32) -> Option<String> {
        let inside =
            |r: &(f32, f32, f32, f32)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        let idx = self
            .window_tab_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)?;
        if idx == self.active_window {
            return None;
        }
        self.windows
            .get(idx)
            .and_then(|w| w.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
    }
    /// Relocate `moving` next to `target` along the edge given by `zone`.
    /// Detaches the moving leaf (its PTY stays alive) and re-attaches it
    /// beside the target, then resizes every pane to its new rect. No-op
    /// when source and target are the same pane.
    pub(crate) fn move_pane(&mut self, moving: &str, target: &str, zone: DropZone) {
        if moving == target {
            return;
        }
        let (dir, before) = match zone {
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
            // Header drag onto a target's centre = ambiguous for a
            // whole-pane move; ignore rather than picking a random edge.
            DropZone::Center => return,
        };
        // Cross-window relocation: the sidebar chip drop hands us a target leaf
        // that lives in another (parked) window. The active pty_layout can't
        // insert beside a leaf it doesn't own, so move the leaf across trees
        // directly — what the daemon's move_surface used to do.
        let in_active = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().contains(&target))
            .unwrap_or(false);
        if !in_active {
            if let Some(dst_idx) = self.window_of_pane(target) {
                self.move_pane_cross_window(moving, target, dir, before, dst_idx);
            }
            return;
        }
        if let Some(tree) = self.pty_layout.as_mut() {
            if !tree.remove_leaf(moving) {
                return;
            }
            if !tree.insert_beside(target, dir, before, moving.to_string()) {
                // Target vanished (shouldn't happen) — re-attach beside
                // the first surviving leaf so the pane isn't orphaned.
                if let Some(anchor) = tree.leaves().first().map(|s| s.to_string()) {
                    tree.insert_beside(&anchor, dir, before, moving.to_string());
                }
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.ws.lock().unwrap().active_pane = Some(moving.to_string());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// 라이브 드래그 이동: 드래그 중인 pane 을 커서가 가리키는 target/zone 으로
    /// **실제 `pty_layout` 에 재배치**하고 PTY 를 reshape 한다 — 가짜 프리뷰 박스가
    /// 아니라 진짜 터미널 내용이 옮겨진 자리에서 reflow 된다. 매 mouse-move 마다
    /// 불려도 zone 이 바뀔 때만 reshape(throttle).
    ///
    /// 모델: 드롭 무효(빈 곳·Center·자기 자신)일 땐 carried pane 을 원위치로
    /// 되돌린다. hit-test 는 carried pane 을 *제거한* base 트리 기준으로 해서
    /// (라이브로 옮겨진 트리가 아니라) 커서→target 매핑이 흔들리지 않게 한다.
    /// 드래그 시작 시 `drag_orig_layout` 에 원본을 박제하고, 드롭/취소 때 정리한다.
    /// 라이브 드래그로 옮기는 중인 pane — header/handle 드래그는 통째로, tab
    /// 드래그는 단일탭 pane 일 때만(멀티탭은 탭 추출이라 라이브 미적용).
    pub(crate) fn live_drag_moving(&self) -> Option<String> {
        if let Some(hd) = self.header_drag.as_ref() {
            hd.active.then(|| hd.pane.clone())
        } else if let Some(td) = self.tab_drag.as_ref() {
            let single = self
                .ws
                .lock()
                .ok()
                .and_then(|w| w.panes.get(&td.pane).map(|p| p.tabs.len() <= 1))
                .unwrap_or(true);
            (td.active && single).then(|| td.pane.clone())
        } else {
            None
        }
    }
    /// 라이브 드래그가 조준하고 있는 드롭 후보. carried pane 을 **제거한** base
    /// 트리 기준이다 — 화면에서 형제들이 벌어져 빈자리를 채운 그 모습이 곧
    /// 사용자가 겨누는 과녁이라, 드롭 확정도 같은 트리로 판정해야 프리뷰와
    /// 결과가 어긋나지 않는다(원본 트리로 재판정하면 두 pane 사이 분할선이
    /// 커서 밑에 되살아나 중앙 병합이 가장자리 split 으로 뒤집혔다).
    pub(crate) fn live_drag_hit(&self, moving: &str) -> Option<(String, DropZone)> {
        let mut base = self
            .drag_orig_layout
            .clone()
            .or_else(|| self.pty_layout.clone())?;
        if !base.remove_leaf(moving) {
            return None;
        }
        let (cols, rows) = self.window_cells();
        let rects = base.leaf_rects(cols, rows);
        self.drop_zone_in_rects(
            &rects,
            base.leaves().len(),
            self.cursor_px.0,
            self.cursor_px.1,
        )
    }
    /// 드롭이 "안에 넣기"(중앙)면 소스 pane 을 타깃 탭 스트립으로 병합하고 true.
    /// 라이브로 옮겨 둔 자리를 원본으로 되돌린 뒤 병합한다 — 안 되돌리면 이미
    /// 재배치된 트리 위에 병합이 겹쳐 소스가 두 번 사라진 것처럼 보인다.
    pub(crate) fn take_center_drop(&mut self, moving: &str) -> bool {
        let Some((dst, DropZone::Center)) = self.live_drag_hit(moving) else {
            return false;
        };
        if dst == moving {
            return false;
        }
        if let Some(orig) = self.drag_orig_layout.take() {
            self.pty_layout = Some(orig);
        }
        self.drag_live_applied = None;
        self.merge_pane_into_tabs(moving, &dst)
    }
    pub(crate) fn update_live_drag(&mut self) {
        let Some(moving) = self.live_drag_moving() else {
            return;
        };
        // 첫 라이브 적용 — 원본 박제. base = 원본에서 carried pane 제거.
        if self.drag_orig_layout.is_none() {
            self.drag_orig_layout = self.pty_layout.clone();
        }
        let Some(orig) = self.drag_orig_layout.clone() else {
            return;
        };
        let mut base = orig.clone();
        if !base.remove_leaf(&moving) {
            // 단일 pane(형제 없음) → 라이브로 가를 게 없다. 드롭 때 split_opposite
            // 같은 기존 경로가 처리하므로 여기선 손대지 않는다.
            return;
        }
        let (cols, rows) = self.window_cells();
        let hit = self.live_drag_hit(&moving);
        // 유효 드롭이면 base 에 끼워 넣은 live 트리, 아니면 원본(원위치 복귀).
        let (next_layout, applied) = match hit {
            // 중앙 = "안에 넣기" 프리뷰. 소스를 그리드에서 **뺀 채로** 보여 준다 —
            // 병합 후의 모습이 정확히 이것(빈자리를 형제가 채우고, 소스 내용은
            // 타깃 탭으로 들어간다)이고, 커서를 따라다니는 pill 이 옮기는 중인
            // pane 을 대신 보여 준다. 덤으로 화면과 hit-test 트리가 같아져
            // 프리뷰와 드롭 결과가 어긋날 여지가 사라진다.
            Some((ref target, DropZone::Center)) if *target != moving => {
                (base.clone(), Some((target.clone(), DropZone::Center)))
            }
            Some((ref target, zone)) if zone != DropZone::Center && *target != moving => {
                let (dir, before) = match zone {
                    DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
                    DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
                    DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
                    DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
                    DropZone::Center => unreachable!(),
                };
                let mut live = base.clone();
                if live.insert_beside(target, dir, before, moving.clone()) {
                    (live, Some((target.clone(), zone)))
                } else {
                    (orig.clone(), None)
                }
            }
            _ => (orig.clone(), None),
        };
        // zone 안 바뀌었으면 reshape 생략(SIGWINCH throttle).
        if applied == self.drag_live_applied {
            return;
        }
        // 중앙 프리뷰 동안 소스는 트리에 없다 — 그걸 active_pane 으로 두면 활성
        // pane 이 leaf 가 아닌 상태가 되어 헤더 강조·좌표 조회가 헛돈다. 겨누고
        // 있는 타깃을 활성으로 둔다(어차피 병합 후 활성이 될 pane 이다).
        let focus = match applied.as_ref() {
            Some((target, DropZone::Center)) => target.clone(),
            _ => moving.clone(),
        };
        self.drag_live_applied = applied;
        self.pty_layout = Some(next_layout);
        self.resize_backend(cols, rows);
        if let Ok(mut ws) = self.ws.lock() {
            ws.active_pane = Some(focus);
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 라이브 드래그 종료: 현재 `pty_layout`(=마지막으로 라이브 적용된 상태)을 그대로
    /// 확정하고 백업/throttle 상태를 비운다. 유효 드롭이 한 번도 없었으면 원본이
    /// 이미 복원돼 있으니 정리만 한다. 반환값 = 라이브로 실제 이동이 적용됐는지.
    pub(crate) fn finish_live_drag(&mut self) -> bool {
        let applied = self.drag_live_applied.is_some();
        self.drag_orig_layout = None;
        self.drag_live_applied = None;
        applied
    }

    /// Detach `moving` from the active window and graft it beside `target`,
    /// which lives in window `dst_idx`'s parked tree. The PTY stays alive — only
    /// the BSP trees are rewired. If the active window held `moving` as its sole
    /// pane it empties out, so we fold that slot away and follow the pane into
    /// its new window.
    fn move_pane_cross_window(
        &mut self,
        moving: &str,
        target: &str,
        dir: kasa_pty::SplitDir,
        before: bool,
        dst_idx: usize,
    ) {
        // remove_leaf returns false for a root-level (single) leaf, so detect
        // the sole-pane case up front instead of relying on its return.
        let src_only = self
            .pty_layout
            .as_ref()
            .map(|t| {
                let l = t.leaves();
                l.len() == 1 && l[0] == moving
            })
            .unwrap_or(false);
        if !src_only {
            let removed = self
                .pty_layout
                .as_mut()
                .map(|t| t.remove_leaf(moving))
                .unwrap_or(false);
            if !removed {
                return;
            }
        }
        // Graft beside the target in the destination window's tree.
        let grafted = self
            .windows
            .get_mut(dst_idx)
            .and_then(|w| w.as_mut())
            .map(|t| t.insert_beside(target, dir, before, moving.to_string()))
            .unwrap_or(false);
        if !grafted {
            return;
        }
        if src_only {
            // The active window is now empty. Its slot is None (the tree lived
            // in pty_layout, which we discard), so drop it and shift the
            // destination index down if it sat above the removed slot.
            self.windows.remove(self.active_window);
            let dst = if dst_idx > self.active_window {
                dst_idx - 1
            } else {
                dst_idx
            };
            self.pty_layout = self.windows[dst].take();
            self.active_window = dst;
            self.window_alert.remove(&dst);
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            self.chrome_dirty = true;
            self.ws.lock().unwrap().active_pane = Some(moving.to_string());
            self.publish_pty_layout();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else {
            // Source window keeps other panes — resize it before parking, then
            // follow the moved pane into its new home (switch_window resizes and
            // repaints the destination).
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            self.switch_window(dst_idx);
            self.ws.lock().unwrap().active_pane = Some(moving.to_string());
        }
    }
}

/// leaf 가 유령인가 — **자원이 둘 다 없어야** 참이다.
///
/// 한쪽만 보고 판정하면 멀쩡한 pane 을 걷는다. PTY 만 있는 것은 갓 split 해 첫
/// 화면이 아직 안 온 pane 이고, 그리드만 있는 것은 이미지·마크다운·웹 pane 이라
/// 셸이 원래 없다(`resize_backend` 도 `self.pty` 미스로 그냥 건너뛴다).
fn leaf_is_orphan(has_pty: bool, has_grid: bool, grid_needs_pty: bool) -> bool {
    if has_pty {
        return false;
    }
    // 그리드조차 없으면 예전부터 걷던 자리. 그리드가 남아 있어도 **그것이 터미널의
    // 그리드라면** 셸이 사라진 순간 마지막 프레임을 박제한 사진일 뿐이다 — 2026-08-25
    // 에 그 자리가 30분 넘게 `Puzzling… (16m 29s)` 를 그대로 띄운 채 앉아 있었고,
    // board 에도 안 잡혀 사용자가 치울 방법이 없었다.
    !has_grid || grid_needs_pty
}

#[cfg(test)]
mod auto_split_tests {
    use super::*;
    use kasa_pty::SplitDir::{Horizontal, Vertical};

    // 실측 셀(logical 7×17.5). 셀과 픽셀의 모양이 다르다는 게 이 규칙의 핵심이라
    // 테스트도 그 비율로 재야 의미가 있다.
    const CW: f32 = 7.0;
    const CH: f32 = 17.5;
    fn pick(cols: u16, rows: u16) -> kasa_pty::SplitDir {
        pick_split_axis(cols as f32 * CW, rows as f32 * CH, cols, rows)
    }

    #[test]
    fn 화면에서_가로로_긴_pane_은_가로를_쪼갠다() {
        // 240×50 = 1680×875px — 눈에 가로로 넓다.
        assert_eq!(pick(240, 50), Horizontal);
    }

    #[test]
    fn 화면에서_세로로_긴_pane_은_세로를_쪼갠다() {
        // 90×90 = 630×1575px. 셀 수만 보면 정사각이라 판정이 갈리지 않는데,
        // 픽셀로 보면 세로가 2.5배다 — 셀로 재면 여기서 틀린다.
        assert_eq!(pick(90, 90), Vertical);
    }

    #[test]
    fn 가로로_쪼개면_80칸을_못_지킬_때는_세로로_돌린다() {
        // 150×40 = 1050×700px 라 긴 축은 가로지만, 반으로 자르면 75칸이라 글이
        // 접힌다. 세로로 자르면 20줄이 남아 쓸 만하다.
        assert_eq!(pick(150, 40), Vertical);
    }

    #[test]
    fn 세로로_쪼개면_16줄을_못_지킬_때는_가로로_돌린다() {
        // 200×20 = 1400×350px. 긴 축은 가로이므로 원래도 가로다 — 반대 방향의
        // 가드가 발동하는 조합을 따로 만든다: 세로로 긴데 줄이 모자란 경우.
        assert_eq!(pick_split_axis(300.0, 400.0, 200, 20), Horizontal);
    }

    #[test]
    fn 표준_80x24_는_가로로_쪼갠다() {
        // 680×516px — 아직 가로가 길다. 반 쪼개면 40칸이라 가드가 걸릴 것 같지만,
        // 세로로 돌리면 12줄짜리 두 장이 된다(claude 입력박스만 5줄). **대안이
        // 쓸 만할 때만** 돌리는 게 규칙이라 여기선 안 돌린다.
        // MIN_ROWS 를 12 로 뒀을 때 이게 세로로 갈렸다 — 그래서 16.
        assert_eq!(pick_split_axis(680.0, 516.0, 80, 24), Horizontal);
    }

    #[test]
    fn 둘_다_못_지키면_긴_축_규칙_그대로() {
        // 40×10 — 가로로 쪼개도 20칸, 세로로 쪼개도 5줄. 더 나은 선택이 없으니
        // 규칙을 뒤집지 않는다(뒤집으면 어느 쪽이 나은지 설명할 수 없다).
        assert_eq!(pick_split_axis(400.0, 200.0, 40, 10), Horizontal);
        assert_eq!(pick_split_axis(200.0, 400.0, 40, 10), Vertical);
    }
}

#[cfg(test)]
mod drop_zone_tests {
    use super::*;

    #[test]
    fn center_is_a_zone_of_its_own_not_a_split() {
        // 중앙과 그 근처는 병합이다 — 이게 없으면 pane 을 통째로 끌어 놓을 때
        // 헤더 띠(28px)를 정확히 맞히지 않는 한 무조건 split 이 됐다.
        assert_eq!(drop_zone_for_offsets(0.0, 0.0), DropZone::Center);
        assert_eq!(drop_zone_for_offsets(0.3, -0.3), DropZone::Center);
        assert_eq!(drop_zone_for_offsets(-0.41, 0.41), DropZone::Center);
    }

    #[test]
    fn edges_still_split_in_four_directions() {
        assert_eq!(drop_zone_for_offsets(-0.9, 0.0), DropZone::Left);
        assert_eq!(drop_zone_for_offsets(0.9, 0.0), DropZone::Right);
        assert_eq!(drop_zone_for_offsets(0.0, -0.9), DropZone::Up);
        assert_eq!(drop_zone_for_offsets(0.0, 0.9), DropZone::Down);
    }

    #[test]
    fn center_zone_never_swallows_a_whole_edge() {
        // 어느 변이든 가장자리까지 가면 반드시 split — 중앙 존이 pane 을 통째로
        // 먹어 split 이 불가능해지는 회귀를 막는다.
        for t in [-0.99_f32, -0.5, 0.0, 0.5, 0.99] {
            assert_ne!(drop_zone_for_offsets(-1.0, t), DropZone::Center);
            assert_ne!(drop_zone_for_offsets(1.0, t), DropZone::Center);
            assert_ne!(drop_zone_for_offsets(t, -1.0), DropZone::Center);
            assert_ne!(drop_zone_for_offsets(t, 1.0), DropZone::Center);
        }
    }

    #[test]
    fn diagonal_outside_center_picks_the_dominant_axis() {
        // 대각선은 지배 축을 따른다 — 정규화 좌표라 가로로 넓은 pane 에서도
        // Up/Down 쐐기가 좁아지지 않는다.
        assert_eq!(drop_zone_for_offsets(0.8, 0.5), DropZone::Right);
        assert_eq!(drop_zone_for_offsets(0.5, 0.8), DropZone::Down);
        assert_eq!(drop_zone_for_offsets(-0.8, -0.5), DropZone::Left);
        assert_eq!(drop_zone_for_offsets(-0.5, -0.8), DropZone::Up);
    }

    #[test]
    fn stashed_leaf_lookup_pairs_with_removal() {
        // reap 이 "유령 leaf 가 있다"고 판정하는 눈(`stashed_leaf_exists`)과 실제로
        // 걷어내는 손(`remove_stashed_leaf`)이 어긋나면, PTY 없는 자리가 빈 pane 으로
        // 화면에 그대로 남는다 — 2026-08-11 실측 버그. 이 짝을 못박는다.
        let leaf = |id: &str| kasa_pty::PtyLayout::Leaf {
            pane_id: id.to_string(),
        };
        let mut slot = Some(kasa_pty::PtyLayout::Split {
            dir: kasa_pty::SplitDir::Horizontal,
            ratio: 0.5,
            a: Box::new(leaf("%1")),
            b: Box::new(kasa_pty::PtyLayout::Split {
                dir: kasa_pty::SplitDir::Vertical,
                ratio: 0.5,
                a: Box::new(leaf("%2")),
                b: Box::new(leaf("%3")),
            }),
        });
        // 중첩 깊이와 무관하게 찾는다 — 실제로 빈 칸이 남았던 자리가 깊이 2였다.
        assert!(App::stashed_leaf_exists(&slot, "%3"));
        assert!(!App::stashed_leaf_exists(&slot, "%9"));
        // 찾았다면 반드시 걷어낼 수 있어야 한다. 어긋나면 reap 이 매 틱 같은 id 를
        // 유령으로 판정하고도 못 지운다.
        assert!(App::remove_stashed_leaf(&mut slot, "%3"));
        assert!(!App::stashed_leaf_exists(&slot, "%3"));
        assert!(App::stashed_leaf_exists(&slot, "%2"));
        // 마지막 leaf 까지 걷으면 슬롯째 비고, 빈 슬롯·없는 대상 조회도 안전하다.
        assert!(App::remove_stashed_leaf(&mut slot, "%1"));
        assert!(App::remove_stashed_leaf(&mut slot, "%2"));
        assert!(slot.is_none());
        assert!(!App::stashed_leaf_exists(&slot, "%1"));
        assert!(!App::remove_stashed_leaf(&mut slot, "%1"));
    }
}

#[cfg(test)]
mod orphan_leaf_tests {
    use super::leaf_is_orphan;

    /// 2026-08-24: 숨긴 pane 의 낡은 레코드가 같은 번호를 물려받은 **산 pane** 의
    /// 자원을 놓으면서 트리는 안 걷어, 클릭도 안 되는 검은 사각형이 남았다.
    /// 그 자리를 걷는 판정이 이것 — 한쪽만 보도록 되돌리면 여기서 깨진다.
    #[test]
    fn only_a_leaf_with_neither_shell_nor_grid_is_a_ghost() {
        assert!(
            leaf_is_orphan(false, false, false),
            "셸도 그리드도 없으면 유령이다"
        );
        // 갓 split 한 pane — PTY 는 붙었고 첫 ScreenUpdate 가 아직 안 왔다.
        assert!(
            !leaf_is_orphan(true, false, false),
            "갓 태어난 pane 을 걷으면 안 된다"
        );
        // 이미지·마크다운·웹 pane — 셸이 원래 없다.
        assert!(
            !leaf_is_orphan(false, true, false),
            "PTY 없는 파일 pane 을 걷으면 안 된다"
        );
        assert!(!leaf_is_orphan(true, true, true), "평범한 셸 pane");
    }

    /// 2026-08-25: 셸이 죽었는데 **그리드가 남아** 마지막 프레임을 30분 넘게 띄운 채
    /// 앉아 있던 자리(`Puzzling… (16m 29s)` 고정). 그리드가 있다는 이유로 산 것으로
    /// 보면 이 자리를 영영 못 걷고, board 에도 안 잡혀 사용자는 치울 방법이 없다.
    #[test]
    fn a_terminal_whose_shell_died_is_a_ghost_even_with_a_grid() {
        assert!(
            leaf_is_orphan(false, true, true),
            "터미널 그리드인데 셸이 없으면 마지막 프레임을 박제한 사진이다"
        );
    }

    /// 위 판정을 `!has_pty` 하나로 줄이면 웹·파일 pane 이 죽는다 — 그쪽은 PTY 가
    /// 원래 없는 것이 정상이라 `grid_needs_pty` 가 거짓이어야 한다.
    #[test]
    fn a_file_pane_never_becomes_a_ghost_just_for_lacking_a_shell() {
        assert!(
            !leaf_is_orphan(false, true, false),
            "웹·이미지·마크다운 pane 은 셸이 없어도 산 것"
        );
    }
}
