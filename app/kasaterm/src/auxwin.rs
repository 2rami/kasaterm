//! wgpu 멀티윈도우 기반 — 편집기/파일뷰를 메인 창에서 떼어 별도 OS 창으로 띄운다.
//!
//! chrome.rs 의 보조 창들(session/board/arona 패널·preview)이 전부 wry webview 인 것과
//! 달리, 여기 별도창은 **자체 wgpu Surface(GpuRenderer)** 로 `draw_raw_editor` 를 직접
//! 그린다. 창 하나당 GpuRenderer 하나(자체 device·글리프 아틀라스) — v1 은 공유 device
//! 리팩토링 없이 창마다 새 인스턴스로 간다(아틀라스 중복 수십 MB 는 v1 트레이드오프).
//!
//! 렌더/입력 라우팅이 전부 `AuxWindowKind` match 한 군데를 지나가므로, 나중에
//! `Settings` variant 를 추가할 땐 각 match(render·key·mouse·title)의 새 팔만 채우면 된다.
//!
//! Drop 순서 주의: `AuxWindow.gpu` 는 `window` 보다 **먼저** 드롭돼야 한다 — surface 가
//! 창의 metal layer 를 참조하므로 창이 먼저 해제되면 surface drop 이 use-after-free 다.
//! 그래서 struct 필드 선언에서 `gpu` 를 앞에, `window` 를 맨 뒤에 둔다(필드는 선언 순서로
//! 드롭). GpuRenderer 내부는 `_window`(Arc clone)→`surface` 순이라, gpu drop 시 Arc
//! refcount 만 줄고 실제 Window 는 `aux.window` 가 아직 잡고 있어 살아있다.
use super::*;

/// 별도 창 헤더 높이. OS 타이틀바를 껐으므로 이 띠가 창의 유일한 손잡이다.
const AUX_HEADER_H: f32 = 30.0;

/// 별도창이 담는 내용물. 이 enum 을 match 하는 지점(render/key/mouse/title)의
/// 팔만 채우면 새 창 종류를 꽂을 수 있다. `Settings` 는 데이터를 안 들고 있다 —
/// 설정 상태(`settings_cat`/`set_*`/`students_*`)는 App 이 소유하고 이 창은 뷰라,
/// 렌더는 `aux_render` 가 App 스냅샷으로 `paint_settings` 를 재사용하고 이벤트는
/// `aux_window_event` 가 `settings_click`/`settings_key`/`settings_scroll` 로 위임한다.
pub(crate) enum AuxWindowKind {
    Editor(MarkdownPane),
    Settings,
    /// 터미널 pane 을 별도 OS 창으로 분리(undock). 데이터를 안 들고 `pane_id` 만 —
    /// 셀 그리드/커서는 App.ws 의 그 pane 이 소유하고 메인 루프 `pump_pty_screens` 가
    /// 계속 갱신하므로, 이 창은 `draw_cells` 로 그 스냅샷을 그리는 뷰다. `PtySession`
    /// 은 App.pty 에 그대로 살아 세션이 안 끊긴다(undock 은 레이아웃 트리에서 leaf 만
    /// 빼고 pty·ws.panes 는 유지). 렌더는 `aux_terminal_render`, 이벤트는
    /// `aux_terminal_event` 로 위임(Settings 가 paint_settings 를 재사용하는 것과 동형).
    /// pane 하나를 꺼낸 창. `window` 는 **어느 방에서 나왔는지** — 이게 없으면
    /// 되돌릴 때 원래 방이 아니라 그때 활성 pane 옆에 붙고, 헤더에 소속을 적을
    /// 수도 없다. 방 재배치를 따라 remap 된다(`reorder_window`).
    Terminal { pane_id: String, window: usize },
    /// 방(윈도우) 하나를 통째로 별도 OS 창으로 분리. `Terminal` 이 pane 하나를 보듯
    /// 이건 그 방의 **BSP 트리 전체**를 본다 — pane 여러 개가 자기 자리에 그려진다.
    ///
    /// 트리를 들고 오지 않고 `App.windows[window]` 에 그대로 둔 채 인덱스로 참조한다.
    /// 그래서 되돌리기가 `switch_window(window)` 한 줄이고, info 방 그룹핑과 세션
    /// 저장이 손대지 않아도 맞는다. 대가는 방 재배치 때 인덱스가 흔들리는 것인데,
    /// 그건 `reorder_window` 의 remap 이 이 필드까지 통과시켜 막는다.
    ///
    /// `focus` 는 이 창 안에서 키 입력을 받을 pane. `term_pane_id()` 가 이걸 내주므로
    /// 키·휠·IME 경로는 `Terminal` 것을 그대로 쓴다.
    Room { window: usize, focus: Option<String> },
}

/// 편집기 창의 OS 타이틀 — 파일명(+ dirty ●). doc.path 는 String 이라 Path 로 감싼다.
fn aux_editor_title(m: &MarkdownPane) -> String {
    let name = std::path::Path::new(m.doc.path.as_str())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    if m.modified {
        format!("● {name}")
    } else {
        name.to_string()
    }
}

pub(crate) struct AuxWindow {
    /// 자체 wgpu 렌더러. `window` 보다 먼저 드롭돼야 하므로 앞에 선언(모듈 doc 참조).
    pub(crate) gpu: gpu::GpuRenderer,
    pub(crate) kind: AuxWindowKind,
    /// 다음 프레임에 다시 그려야 함(입력/리사이즈 후 set). 메인 렌더 루프에 얹지 않고
    /// 이 창 자신의 이벤트에서만 소비한다.
    pub(crate) dirty: bool,
    /// 이 창의 마지막 커서 위치(logical px). 드래그 선택·클릭 캐럿 히트테스트용.
    pub(crate) cursor_px: (f32, f32),
    /// 마우스 드래그 선택 진행 중.
    pub(crate) selecting: bool,
    /// OS 포커스 여부. 캐럿 blink 는 포커스된 창만(불필요한 GPU 낭비 방지).
    pub(crate) focused: bool,
    /// 이 창 편집기의 한글 조합 프리에딧. App.hangul 이 조합하고 드라이버가 이 창 것으로
    /// 스탬프한다 — 창마다 자기 프리에딧을 오버레이(메인창 preedit 과 안 섞임).
    pub(crate) preedit: String,
    /// 마지막으로 OS 창 타이틀에 세팅한 문자열(중복 set_title 회피).
    pub(crate) last_title: String,
    /// 헤드리스 캡처 (deadline, png 경로). 메인 `pending_capture` 의 aux 판 — 자동캡처가
    /// 메인 창만 찍으므로 별도창은 자기 gpu 로 따로 readback 한다.
    pub(crate) pending_capture: Option<(Instant, String)>,
    /// 렌더 뷰(마크다운) 본문 높이. raw 편집기는 줄 수 × 줄높이로 미리 알 수 있지만
    /// 렌더 뷰는 **그려 봐야** 안다(`draw_markdown` 의 반환값) — 휠 clamp 가 한 프레임
    /// 전 값을 쓰는 건 그래서다. 0 이면 아직 안 그렸다는 뜻이라 clamp 를 걸지 않는다.
    pub(crate) md_content_h: f32,
    /// `window` 는 맨 뒤 — `gpu` 보다 나중에 드롭돼 surface 가 살아있는 창을 참조한다.
    pub(crate) window: Arc<Window>,
}

impl AuxWindow {
    pub(crate) fn editor(&self) -> Option<&MarkdownPane> {
        match &self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => None,
        }
    }
    pub(crate) fn editor_mut(&mut self) -> Option<&mut MarkdownPane> {
        match &mut self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => None,
        }
    }
    /// 키 입력이 갈 pane id. 터미널 창은 그 pane, 방 창은 지금 포커스된 pane.
    /// 둘을 한 값으로 내주는 덕에 키·휠·IME 라우팅이 한 벌로 끝난다.
    fn term_pane_id(&self) -> Option<&str> {
        match &self.kind {
            AuxWindowKind::Terminal { pane_id, .. } => Some(pane_id.as_str()),
            AuxWindowKind::Room { focus, .. } => focus.as_deref(),
            _ => None,
        }
    }
    /// 이 창이 통째로 들고 있는 방 인덱스(방 창일 때만).
    pub(crate) fn room_window(&self) -> Option<usize> {
        match &self.kind {
            AuxWindowKind::Room { window, .. } => Some(*window),
            _ => None,
        }
    }
    /// 창 client 영역의 논리 크기(px).
    fn logical_size(&self) -> (f32, f32) {
        let scale = self.gpu.scale();
        let phys = self.window.inner_size();
        ((phys.width.max(1) as f32) / scale, (phys.height.max(1) as f32) / scale)
    }
    /// 이 창이 표시할 OS 타이틀 — 파일명(+ dirty ●).
    fn title(&self) -> String {
        match &self.kind {
            AuxWindowKind::Editor(m) => aux_editor_title(m),
            AuxWindowKind::Settings => "Settings".to_string(),
            // v1 은 pane id — 프로세스명(vim/claude…) 인레이는 App 만 알아 aux_render 가
            // 더 나은 라벨로 덮어쓸 수 있다(현재는 id 그대로).
            AuxWindowKind::Terminal { pane_id, .. } => pane_id.clone(),
            // 방 이름(window_labels)은 App 만 알아 `aux_room_render` 가 덮어쓴다.
            AuxWindowKind::Room { window, .. } => format!("방 {}", window + 1),
        }
    }
    /// 한 프레임 렌더. 자체 gpu 로 배경 + `draw_raw_editor` 를 그리고 present.
    /// `cursor_on` = 캐럿을 그릴지(blink 위상, 포커스 상태 반영).
    pub(crate) fn render(&mut self, cursor_on: bool) {
        let scale = self.gpu.scale();
        let (w, h) = self.logical_size();
        self.gpu.clear_chrome();
        // 본문 배경 — draw_raw_editor 의 gutter bg(theme::bg)·surface clear 와 동일색이라
        // letterbox 없이 한 판. (clear 색과 겹쳐도 무해.)
        self.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let pe = self.preedit.clone();
        match &self.kind {
            // 렌더 뷰 — pane 과 같은 `raw_mode` 분기다. 별도창이 늘 raw 였던 건
            // 이 갈래가 없어서지 의도가 아니었다(거노: "별도창으로 보면 렌더뷰가
            // 안 된다"). 본문 높이는 그려 봐야 나오므로 휠이 쓰도록 받아 둔다.
            AuxWindowKind::Editor(m) if !m.raw_mode => {
                let blocks = m.doc.blocks.clone();
                let gen = m.doc.gen;
                let scroll = m.scroll;
                let ch = self.gpu.draw_markdown(&blocks, gen, 0.0, 0.0, w, h, scroll, None);
                self.md_content_h = ch;
            }
            AuxWindowKind::Editor(m) => {
                let lang = crate::code_lang_for_path(std::path::Path::new(&m.doc.path));
                let sel = m.sel_range();
                // self.gpu(mut) 와 self.kind(shared, m) 은 disjoint 필드라 동시 차용 OK.
                self.gpu.draw_raw_editor(
                    &m.edit_lines,
                    (m.cur_line, m.cur_col),
                    sel,
                    0.0,
                    0.0,
                    w,
                    h,
                    m.scroll,
                    m.h_scroll,
                    lang,
                    &pe,
                    cursor_on,
                    // 팝아웃 창엔 아직 찾기 바가 없다(Cmd+F 는 aux 단축키
                    // 경로라 열리지 않는다) — 하이라이트만 켜면 켤 방법이
                    // 없는 표시가 남는다.
                    None,
                    // 자동완성도 같은 이유로 아직 없다 — aux 창은 키를
                    // `aux_insert` 로 받아 팝업 키 경로를 안 지난다. 목록만
                    // 띄우면 고를 수 없는 유령이 된다.
                    None,
                    // 진단은 App 이 들고 있어(`App.lsp`) 이 창에서 못 읽는다.
                    &[],
                    // 접기 UI 는 본 창 거터에만 있다 — 여기선 늘 비어 있다.
                    &[],
                    m.wrap,
                    // 팝아웃 창엔 멀티커서 키 경로가 없다 — 커서를 더할 방법이
                    // 없는데 그리기만 하면 지울 수도 없는 표시가 남는다.
                    &[],
                );
            }
            // Settings/Terminal/Room 창은 App 스냅샷(설정 상태·ws 셀 그리드)이 필요해
            // `aux_render_settings`/`aux_terminal_render`/`aux_room_render` 가 직접
            // 페인트한다 — 이 편집기 전용 render 로는 오지 않는다.
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => {}
        }
        let _ = self.gpu.render(&[], scale, 0.0, true);
    }
    /// 캐럿이 보이게 스크롤 보정(가로/세로) — 메트릭은 gpu 레이어가 소유해
    /// draw_raw_editor 와 드리프트하지 않는다.
    fn ensure_caret_visible(&mut self) {
        let (w, h) = self.logical_size();
        let snap = match &self.kind {
            AuxWindowKind::Editor(m) => {
                if !m.raw_mode {
                    return;
                }
                let line = m.cur_line.min(m.edit_lines.len().saturating_sub(1));
                let prefix: String = m
                    .edit_lines
                    .get(line)
                    .map(|l| l.chars().take(m.cur_col).collect())
                    .unwrap_or_default();
                ((*m.edit_lines).clone(), line, prefix, m.scroll, m.h_scroll)
            }
            AuxWindowKind::Settings
            | AuxWindowKind::Terminal { .. }
            | AuxWindowKind::Room { .. } => return,
        };
        let (lines, cur_line, prefix, scroll, h_scroll) = snap;
        let line_count = lines.len();
        let (ns, nh) = self
            .gpu
            .raw_editor_ensure_visible(
                line_count, cur_line, &prefix, w, h, scroll, h_scroll, &[], 0, &lines,
            );
        if let Some(m) = self.editor_mut() {
            m.scroll = ns.max(0.0);
            m.h_scroll = nh.max(0.0);
        }
    }
    /// PageUp/Down 한 스텝의 줄 수(본문 높이 / 줄높이 - 1).
    fn page_lines(&mut self) -> usize {
        let (_, h) = self.logical_size();
        let lh = self.gpu.raw_editor_line_h();
        (((h / lh).floor() as usize).saturating_sub(1)).max(1)
    }
}

impl App {
    // ── 별도창 스폰 ──────────────────────────────────────────────────────────

    /// `md` 를 담은 새 편집기 별도창을 만든다. `near` 가 Some 이면 그 물리좌표에
    /// 띄운다(Phase 3 tear-off), None 이면 OS 가 위치를 정한다. 새 창 인덱스 반환.
    pub(crate) fn spawn_aux_editor(
        &mut self,
        mut md: MarkdownPane,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Option<usize> {
        // 버퍼만 미리 채운다 — 뷰 모드는 부르는 쪽이 정한다. 여기서 raw 로
        // 못박던 동안엔 `.md` 를 팝아웃하면 렌더 뷰가 통째로 사라졌다(거노).
        md.seed_edit_lines();
        let title = aux_editor_title(&md);
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(760.0, 560.0));
        if let Some(pos) = near {
            attrs = attrs.with_position(pos);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] window create failed: {e}");
                return None;
            }
        };
        // 메인 창과 동일한 IME 정책: macOS 는 첫 자모 유실 버그 때문에 OS IME 를 끄고
        // in-process hangul Composer(self.hangul) 로 조합, 그 외 플랫폼은 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] gpu init failed: {e}");
                return None;
            }
        };
        let aux = AuxWindow {
            gpu,
            kind: AuxWindowKind::Editor(md),
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
            md_content_h: 0.0,
            window,
        };
        self.aux_windows.push(aux);
        let idx = self.aux_windows.len() - 1;
        eprintln!("[auxwin] opened editor window #{idx}");
        self.aux_redraw(idx);
        Some(idx)
    }

    /// 파일트리 Opt+더블클릭 / 빠른파일 Opt+클릭 — 파일을 바로 별도창으로 연다.
    /// 이미지가 아닌 텍스트/코드/마크다운만(편집기 창이므로). 이미 별도창에 열려 있으면
    /// 그 창을 포커스한다.
    pub(crate) fn popout_file_window(
        &mut self,
        path: std::path::PathBuf,
        event_loop: &ActiveEventLoop,
    ) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        // 이미 별도창에 있으면 포커스만.
        if let Some(i) = self.aux_windows.iter().position(|a| {
            a.editor()
                .map(|m| std::path::Path::new(&m.doc.path) == path.as_path())
                .unwrap_or(false)
        }) {
            self.aux_windows[i].window.focus_window();
            return;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if crate::is_image_path(&path) {
            // 이미지는 별도 편집기 창의 범위 밖 — 기존 보조탭 경로로 폴백.
            self.open_file(path, None, true);
            return;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[auxwin] 파일 읽기 실패 {}: {e}", path.display());
                return;
            }
        };
        let is_md = matches!(ext.as_str(), "md" | "markdown");
        let doc = Arc::new(build_markdown_doc(&path, &raw));
        let edit_lines: Arc<Vec<String>> =
            Arc::new(raw.split('\n').map(|s| s.to_string()).collect());
        let md = MarkdownPane {
            doc,
            is_md_doc: is_md,
            // 마크다운은 읽으려고 여는 것이라 렌더 뷰로 시작한다 — 코드·텍스트는
            // 그럴 뷰가 없으니 그대로 raw. 편집은 글자를 치면 알아서 넘어간다.
            raw_mode: !is_md,
            edit_lines,
            cur_line: 0,
            cur_col: 0,
            scroll: 0.0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
            find: None,
            complete: None,
            longest_cache: None,
            edit_gen: 0,
            wrap: false,
            extra: Vec::new(),
            undo_locked: false,
            folds: Vec::new(),
            folds_gen: 0,
            edited_at: None,
        };
        self.spawn_aux_editor(md, event_loop, None);
    }

    /// 파일 탭(보조탭이든 전용 split pane 이든)을 그 MarkdownPane 째로 별도창에 옮긴다 —
    /// 원래 탭/pane 은 제거. 팝아웃 아이콘 클릭(near=None → OS 기본 위치) 과 드래그
    /// tear-off(near=Some(커서 스크린 물리좌표) → 커서 밑에 뜸) 의 공통 진입점.
    pub(crate) fn popout_pane_tab(
        &mut self,
        outer: &str,
        tab_idx: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        // 이 pane 의 유일한 탭인가? 그러면 leaf 째 접고, 아니면 그 탭만 뺀다.
        let only_tab = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(outer)
            .map(|p| p.tabs.len() == 1)
            .unwrap_or(false);
        // MarkdownPane 을 탭에서 꺼낸다(내용물을 터미널 기본값 husk 로 대체).
        let md = {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.panes.get_mut(outer) else { return };
            let Some(tab) = pane.tabs.get_mut(tab_idx) else { return };
            match std::mem::take(&mut tab.content) {
                PaneContent::Markdown(m) => m,
                other => {
                    tab.content = other;
                    return;
                }
            }
        };
        if only_tab {
            // 전용 pane — leaf 를 접는다(remove_pane 이 resize/publish/redraw 까지).
            self.remove_pane(outer);
        } else {
            // 보조탭 husk 만 제거(형제 탭은 유지). 레이아웃 불변 → chrome 만 dirty.
            self.close_tab(outer, tab_idx);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        self.spawn_aux_editor(md, event_loop, near);
    }

    /// 별도창을 닫는다(dirty 여도 그냥 — P1 철학, ● 가 경고였다).
    /// 편집기 별도창 닫기 요청 — 저장 안 한 편집분이 있으면 확인 모달을 띄우고
    /// 창은 그대로 둔다(모달이 답을 받으면 `PendingClose::AuxEditor` 로 돌아온다).
    pub(crate) fn close_editor_window(&mut self, idx: usize) {
        let Some(id) = self.aux_windows.get(idx).map(|a| a.window.id()) else { return };
        if self.guard_dirty(&crate::PendingClose::AuxEditor(id)) {
            return;
        }
        self.close_aux_window(idx);
    }

    pub(crate) fn close_aux_window(&mut self, idx: usize) {
        if idx < self.aux_windows.len() {
            let _ = self.aux_windows.remove(idx);
            eprintln!("[auxwin] closed window #{idx}");
        }
    }

    /// 별도창 redraw 요청(dirty + request_redraw). 메인 루프는 안 건드린다.
    pub(crate) fn aux_redraw(&mut self, idx: usize) {
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.dirty = true;
            a.window.request_redraw();
        }
    }

    /// 별도창 한 프레임 그리기(RedrawRequested / capture 시). OS 타이틀도 여기서 동기화.
    pub(crate) fn aux_render(&mut self, idx: usize) {
        let blink = self.cursor_blink_on(Instant::now());
        // 타이틀 동기화(변경 시에만) — 편집기/설정 공통.
        {
            let Some(a) = self.aux_windows.get_mut(idx) else { return };
            let want = a.title();
            if want != a.last_title {
                a.window.set_title(&want);
                a.last_title = want;
            }
        }
        if matches!(self.aux_windows.get(idx).map(|a| &a.kind), Some(AuxWindowKind::Settings)) {
            self.aux_render_settings(idx, blink);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Terminal { .. })
        ) {
            self.aux_terminal_render(idx, blink);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Room { .. })
        ) {
            self.aux_room_render(idx, blink);
            return;
        }
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        let on = a.focused && blink;
        a.render(on);
        a.dirty = false;
    }

    /// 설정 별도창 한 프레임 — App 상태를 스냅샷해 `paint_settings` 를 그대로
    /// 재사용한다(오버레이 코드와 동일 함수). area 는 창 client 전체, cursor 는
    /// 이 창의 로컬 좌표. rects·scroll clamp 는 App 에 되돌려 클릭·휠이 참조한다.
    fn aux_render_settings(&mut self, idx: usize, blink: bool) {
        let (w, h, scale, cursor, focused) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let (w, h) = a.logical_size();
            (w, h, a.gpu.scale(), a.cursor_px, a.focused)
        };
        let mut ctx = self.settings_snapshot((0.0, 0.0, w, h), cursor);
        // 캐럿 blink 는 포커스된 창만(메인창 last_blink_on 은 안 건드린다).
        ctx.caret_on = focused && blink;
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let (rects, content_h) = settings::paint_settings(&mut a.gpu, &ctx);
        let _ = a.gpu.render(&[], scale, 0.0, true);
        a.dirty = false;
        self.settings_rects = rects;
        // 휠 스크롤 상한: content 높이 − 보이는 폼 밴드(84px 페이지 헤더 제외) + 여유.
        let view_h = (h - 84.0).max(0.0);
        self.settings_scroll_max = (content_h - view_h + 24.0).max(0.0);
        if self.settings_scroll > self.settings_scroll_max {
            self.settings_scroll = self.settings_scroll_max;
        }
    }

    // ── 이벤트 라우팅 ────────────────────────────────────────────────────────

    /// window id 가 별도창일 때 handler.rs 가 위임하는 단일 진입점. 반환 없이 소비.
    pub(crate) fn aux_window_event(
        &mut self,
        idx: usize,
        event: WindowEvent,
        event_loop: &ActiveEventLoop,
    ) {
        // ModifiersChanged 는 포커스된 창으로만 오는데 self.modifiers 갱신은 메인 창
        // 이벤트 arm 에만 있었다 — 별도창에서 Ctrl/Cmd 판정(터미널 제어바이트·Cmd+W·
        // 에디터 단축키)이 메인 창의 마지막 상태로 고정되는 버그. 종류 무관 공통 갱신.
        if let WindowEvent::ModifiersChanged(mods) = &event {
            self.modifiers = mods.state();
        }
        if matches!(self.aux_windows.get(idx).map(|a| &a.kind), Some(AuxWindowKind::Settings)) {
            self.aux_settings_event(idx, event);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Terminal { .. })
        ) {
            self.aux_terminal_event(idx, event, event_loop);
            return;
        }
        if matches!(
            self.aux_windows.get(idx).map(|a| &a.kind),
            Some(AuxWindowKind::Room { .. })
        ) {
            self.aux_room_event(idx, event, event_loop);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.close_editor_window(idx);
            }
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self
                    .aux_windows
                    .get(idx)
                    .map(|a| a.gpu.scale())
                    .unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                }
                self.aux_mouse_drag(idx);
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => self.aux_mouse_press(idx),
                    ElementState::Released => {
                        if let Some(a) = self.aux_windows.get_mut(idx) {
                            a.selecting = false;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.aux_wheel(idx, delta);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.aux_editor_key(idx, &event, event_loop);
            }
            WindowEvent::Ime(ime) => {
                self.aux_editor_ime(idx, ime);
            }
            WindowEvent::RedrawRequested => {
                self.aux_render(idx);
            }
            _ => {}
        }
    }

    // ── 키보드 ───────────────────────────────────────────────────────────────

    fn aux_editor_key(
        &mut self,
        idx: usize,
        event: &KeyEvent,
        _event_loop: &ActiveEventLoop,
    ) {
        use winit::keyboard::{KeyCode, PhysicalKey};
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input_at = Instant::now();
        // Cmd+W: 이 별도창 닫기. 저장 안 한 편집분이 있으면 먼저 묻는다.
        if self.host_mod()
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW))
        {
            self.close_editor_window(idx);
            return;
        }
        // Cmd/Ctrl 조합 = 편집기 단축키(저장/복붙/undo/선택/커서점프). 그 외 조합은 삼킴.
        if self.host_mod() || self.modifiers.control_key() {
            if let PhysicalKey::Code(code) = event.physical_key {
                self.aux_editor_shortcut(idx, code);
            }
            self.aux_redraw(idx);
            return;
        }
        // 렌더 뷰에서 글자를 치면 삼키는 대신 raw 로 넘어가 그 글자를 살린다
        // (pane 편집기와 같은 규칙). 방향키·PageUp 같은 이동 키는 렌더 뷰에서
        // 할 일이 없으니 그대로 삼킨다 — 스크롤은 휠이다.
        if self.aux_windows.get(idx).and_then(|a| a.editor()).is_some_and(|m| !m.raw_mode) {
            if !crate::markdown::md_mutating_key(event) {
                return;
            }
            if let Some(a) = self.aux_windows.get_mut(idx) {
                // 두 뷰의 스크롤은 단위가 다르다 — 렌더는 본문 픽셀, raw 는 줄
                // 좌표다. 값을 그대로 넘기면 엉뚱한 줄로 튀고 0 으로 되돌리면
                // 읽던 자리를 잃으니, 본문 높이 대비 비율로 옮겨 대략 같은 곳에서
                // 편집이 시작되게 한다.
                let lh = a.gpu.raw_editor_line_h();
                let ratio = if a.md_content_h > 0.0 {
                    (a.editor().map_or(0.0, |m| m.scroll) / a.md_content_h).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if let Some(m) = a.editor_mut() {
                    m.ensure_raw_seeded();
                    let line = ((m.edit_lines.len() as f32 * ratio) as usize)
                        .min(m.edit_lines.len().saturating_sub(1));
                    m.cur_line = line;
                    m.cur_col = 0;
                    m.scroll = line as f32 * lh;
                }
            }
        }
        // 평문 키 — 한글 조합 경유 편집.
        self.aux_editor_input(idx, event);
        self.aux_redraw(idx);
    }

    /// Cmd/Ctrl 편집기 단축키. 반환값은 소비 여부(현재는 전부 true 계열이지만
    /// 확장 시 false 팔이 필요할 수 있어 유지).
    fn aux_editor_shortcut(&mut self, idx: usize, code: winit::keyboard::KeyCode) -> bool {
        use winit::keyboard::KeyCode;
        // 확정은 **모든** 팔 앞에서 한 번 — pane 편집기의 `md_flush_preedit` 과
        // 같은 규칙. 예전엔 C·화살표가 빠져 있어 조합 중 그 키를 누르면 음절이
        // 유실됐다(양쪽 테이블에 같은 버그가 복제돼 있었다).
        self.aux_flush_hangul(idx);
        match code {
            KeyCode::KeyS => {
                self.aux_editor_save(idx);
                true
            }
            KeyCode::KeyV => {
                self.aux_editor_paste(idx);
                true
            }
            KeyCode::KeyC => {
                self.aux_copy(idx, false);
                true
            }
            KeyCode::KeyX => {
                self.aux_copy(idx, true);
                true
            }
            KeyCode::KeyA => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.select_all_buf();
                }
                true
            }
            // pane 편집기와 같은 Cmd+D = 캐럿 단어 선택.
            KeyCode::KeyD if !self.modifiers.shift_key() => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.select_word_at();
                }
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.ensure_caret_visible();
                }
                true
            }
            KeyCode::KeyZ => {
                let redo = self.modifiers.shift_key();
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    if let Some(m) = a.editor_mut() {
                        if redo {
                            m.do_redo();
                        } else {
                            m.do_undo();
                        }
                    }
                    a.ensure_caret_visible();
                }
                true
            }
            KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown => {
                let shift = self.modifiers.shift_key();
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    if let Some(m) = a.editor_mut() {
                        m.apply_cmd_arrow(code, shift);
                    }
                    a.ensure_caret_visible();
                }
                true
            }
            _ => false,
        }
    }

    /// 별도창 편집기의 한글 조합 입력(md_editor_input 의 aux 판). 공유 composer
    /// self.hangul 를 쓰되 프리에딧은 이 창의 것으로 스탬프한다.
    fn aux_editor_input(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        if crate::input::is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::AuxEditor(idx));
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.aux_insert(idx, &commit);
                        }
                        let pe = self.hangul.preedit().unwrap_or_default();
                        if let Some(a) = self.aux_windows.get_mut(idx) {
                            a.preedit = pe;
                        }
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace))
            && self.hangul.backspace()
        {
            let pe = self.hangul.preedit().unwrap_or_default();
            if let Some(a) = self.aux_windows.get_mut(idx) {
                a.preedit = pe;
            }
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.aux_insert(idx, &flushed);
        }
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.preedit.clear();
        }
        // 평문 편집/모션 키.
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let page_lines = if matches!(
            event.logical_key,
            Key::Named(NamedKey::PageUp) | Key::Named(NamedKey::PageDown)
        ) {
            self.aux_windows
                .get_mut(idx)
                .map(|a| a.page_lines())
                .unwrap_or(1)
        } else {
            0
        };
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.apply_edit_key(event, shift, alt, page_lines);
            }
            a.ensure_caret_visible();
        }
    }

    /// 조합 중인 음절을 버퍼에 확정하고 프리에딧을 비운다(저장/복사/undo 전에 호출).
    fn aux_flush_hangul(&mut self, idx: usize) {
        if let Some(flushed) = self.hangul.flush() {
            self.aux_insert(idx, &flushed);
        }
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.preedit.clear();
        }
    }

    pub(crate) fn aux_insert(&mut self, idx: usize, text: &str) {
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.insert_at_caret(text);
            }
            a.ensure_caret_visible();
        }
    }

    /// 비-macOS(Windows/Linux) OS IME 경로 — Preedit 는 이 창 프리에딧, Commit 은 삽입.
    fn aux_editor_ime(&mut self, idx: usize, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
            }
            Ime::Preedit(text, _) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit = text;
                }
            }
            Ime::Commit(text) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
                self.aux_insert(idx, &text);
            }
        }
        self.aux_redraw(idx);
    }

    fn aux_editor_save(&mut self, idx: usize) {
        let job = self.aux_windows.get(idx).and_then(|a| {
            a.editor().map(|m| (m.edit_lines.join("\n"), m.doc.path.clone()))
        });
        let Some((text, path)) = job else { return };
        match crate::markdown::write_atomic(&path, &text) {
            Ok(()) => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.mark_saved();
                }
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                self.set_toast(format!("✓ {name} 저장됨"));
            }
            Err(e) => {
                eprintln!("[auxwin] 저장 실패 {path}: {e}");
                self.set_toast(format!("⚠ 저장 실패: {e}"));
            }
        }
    }

    fn aux_editor_paste(&mut self, idx: usize) {
        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(_) => return,
        };
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.paste_at_caret(&text);
            }
            a.ensure_caret_visible();
        }
    }

    fn aux_copy(&mut self, idx: usize, cut: bool) {
        let text = self
            .aux_windows
            .get_mut(idx)
            .and_then(|a| a.editor_mut())
            .and_then(|m| m.take_copy(cut));
        let Some(text) = text else { return };
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                let _ = cb.set_text(text);
            }
            Err(e) => eprintln!("[auxwin] clipboard open failed: {e}"),
        }
        if cut {
            if let Some(a) = self.aux_windows.get_mut(idx) {
                a.ensure_caret_visible();
            }
        }
    }

    // ── 마우스 ───────────────────────────────────────────────────────────────

    /// 현재 커서 위치를 (line, col) 캐럿으로 히트테스트.
    fn aux_caret_at_cursor(&mut self, idx: usize) -> (usize, usize) {
        let snap = {
            let Some(a) = self.aux_windows.get(idx) else { return (0, 0) };
            let Some(m) = a.editor() else { return (0, 0) };
            (
                m.edit_lines.clone(),
                m.scroll,
                m.h_scroll,
                a.cursor_px.0,
                a.cursor_px.1,
            )
        };
        let (lines, scroll, h_scroll, cx, cy) = snap;
        let Some(a) = self.aux_windows.get_mut(idx) else { return (0, 0) };
        a.gpu
            .raw_editor_caret_at(&lines, 0.0, 0.0, scroll, h_scroll, cx, cy, &[], 0)
    }

    fn aux_mouse_press(&mut self, idx: usize) {
        let (line, col) = self.aux_caret_at_cursor(idx);
        self.last_input_at = Instant::now();
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let Some(m) = a.editor_mut() {
                m.cur_line = line;
                m.cur_col = col;
                // anchor==cursor → 아직 무선택, 드래그하면 자란다.
                m.sel_anchor = Some((line, col));
                m.last_edit = EditKind::Break;
            }
            a.selecting = true;
        }
        self.aux_redraw(idx);
    }

    fn aux_mouse_drag(&mut self, idx: usize) {
        if !self.aux_windows.get(idx).map(|a| a.selecting).unwrap_or(false) {
            return;
        }
        let (line, col) = self.aux_caret_at_cursor(idx);
        if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
            m.cur_line = line;
            m.cur_col = col;
        }
        self.aux_redraw(idx);
    }

    fn aux_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        let lh = a.gpu.raw_editor_line_h();
        let (_, h) = a.logical_size();
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * lh * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        // 렌더 뷰는 줄 수로 높이를 못 구한다(블록마다 높이가 다르다) — 지난
        // 프레임이 남긴 실제 본문 높이를 쓴다. 아직 한 번도 안 그렸으면 0 이라
        // clamp 가 스크롤을 막아 버리므로, 그때만 줄 기준으로 물러선다.
        let rendered = a.editor().is_some_and(|m| !m.raw_mode);
        let lines_n = a.editor().map(|m| m.edit_lines.len()).unwrap_or(0);
        let content_h = match (rendered, a.md_content_h) {
            (true, ch) if ch > 0.0 => ch,
            _ => lines_n as f32 * lh,
        };
        // 본문 높이를 넘는 만큼만 스크롤 — 마지막 줄이 화면 안에 머물게 여유 2줄.
        let max_scroll = (content_h - h + lh * 2.0).max(0.0);
        if let Some(m) = a.editor_mut() {
            // 위로 스크롤(y>0) = scroll 감소.
            let ns = (m.scroll - dy).clamp(0.0, max_scroll);
            m.scroll = ns.max(0.0);
        }
        a.dirty = true;
        a.window.request_redraw();
    }

    // ── 설정 별도창 ────────────────────────────────────────────────────────

    /// 설정 별도창이 있으면 그 인덱스. `settings_open` 은 이 창의 존재와 동기화된
    /// 편의 플래그(spawn 시 true, 닫힐 때 false)라 chrome active 표시가 참조한다.
    pub(crate) fn settings_window_idx(&self) -> Option<usize> {
        self.aux_windows
            .iter()
            .position(|a| matches!(a.kind, AuxWindowKind::Settings))
    }

    /// 설정 별도창 진입점(기어·사이드바 항목·프사 클릭). 이미 열려 있으면 포커스만,
    /// `cat`/`student` 가 주어지면 그 페이지·학생으로 전환한다(딥링크).
    pub(crate) fn open_settings_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        cat: Option<SettingsCat>,
        student: Option<String>,
    ) {
        let Some(idx) = self
            .settings_window_idx()
            .or_else(|| self.spawn_aux_settings(event_loop))
        else {
            return;
        };
        if let Some(c) = cat {
            if self.settings_cat != c {
                self.flush_student_persona();
                self.settings_cat = c;
                self.settings_input = None;
                self.settings_scroll = 0.0;
            }
        }
        if let Some(name) = student {
            self.select_student_for_edit(name);
        }
        if let Some(a) = self.aux_windows.get(idx) {
            a.window.focus_window();
        }
        self.aux_redraw(idx);
    }

    fn spawn_aux_settings(&mut self, event_loop: &ActiveEventLoop) -> Option<usize> {
        let attrs = WindowAttributes::default()
            .with_title("Settings")
            .with_theme(Some(Theme::Dark))
            // Wide enough that the theme grid wraps to three columns instead of
            // four rows — at 720 the palette cards alone filled the viewport and
            // pushed shape/accent below the fold.
            .with_inner_size(LogicalSize::new(920.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] settings window create failed: {e}");
                return None;
            }
        };
        // 설정 폼의 텍스트 필드(경로·persona)에 한글이 필요하다. 편집기와 동일한
        // IME 정책: macOS 는 OS IME 끄고 in-process composer, 그 외는 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] settings gpu init failed: {e}");
                return None;
            }
        };
        let aux = AuxWindow {
            gpu,
            kind: AuxWindowKind::Settings,
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: "Settings".to_string(),
            pending_capture: None,
            md_content_h: 0.0,
            window,
        };
        self.aux_windows.push(aux);
        let idx = self.aux_windows.len() - 1;
        self.settings_open = true;
        self.settings_scroll = 0.0;
        self.settings_input = None;
        eprintln!("[auxwin] opened settings window #{idx}");
        self.aux_redraw(idx);
        Some(idx)
    }

    pub(crate) fn close_settings_window(&mut self, idx: usize) {
        self.flush_student_persona();
        self.settings_input = None;
        self.settings_open = false;
        self.close_aux_window(idx);
    }

    /// 설정 별도창 이벤트 라우팅 — 편집기와 다른 처리(폼 클릭·휠 스크롤·필드 키).
    fn aux_settings_event(&mut self, idx: usize, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.close_settings_window(idx),
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux_windows.get(idx).map(|a| a.gpu.scale()).unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                    // hover 피드백(카드·세그먼트·행)을 갱신하려면 매 이동에 재페인트.
                    a.dirty = true;
                    a.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.last_input_at = Instant::now();
                let (cx, cy) = self.aux_windows.get(idx).map(|a| a.cursor_px).unwrap_or((0.0, 0.0));
                // rects 는 area=(0,0,w,h) 좌표계라 창 로컬 커서를 그대로 넘긴다.
                self.settings_click(cx, cy);
                self.aux_redraw(idx);
            }
            WindowEvent::MouseWheel { delta, .. } => self.aux_settings_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_settings_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_settings_ime(idx, ime),
            WindowEvent::RedrawRequested => self.aux_render(idx),
            _ => {}
        }
    }

    fn aux_settings_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let dy_px = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 40.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        let next = (self.settings_scroll - dy_px).clamp(0.0, self.settings_scroll_max);
        if (next - self.settings_scroll).abs() > 0.1 {
            self.settings_scroll = next;
            self.aux_redraw(idx);
        }
    }

    fn aux_settings_key(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input_at = Instant::now();
        if crate::input::is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::Settings);
        // Cmd/Ctrl+W: 설정 창 닫기.
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.close_settings_window(idx);
            return;
        }
        // macOS 는 OS IME 를 껐으므로 한글 자모(U+3130..318F)는 in-process composer
        // 로 조합해 완성 음절만 포커스 필드에 넣는다.
        //
        // ⚠️ 자모는 `event.text` 로만 온다 — `logical_key` 는 같은 키의 **영문
        // 각인**(ㄱ→r, ㅖ→P)이라, 그걸 보고 판단하면 자모가 조합기를 그냥
        // 지나쳐 `settings_key` 에 낱자로 꽂힌다("계"가 "ㄱㅖ"로 남던 것).
        #[cfg(target_os = "macos")]
        if self.settings_input.is_some() {
            // text 가 비고 logical_key 만 자모로 오는 프레임이 있어 둘 다 본다 —
            // 한쪽만 보면 그 프레임의 자모가 조합기를 못 만나고 필드에 낱자로 꽂힌다.
            let one = |s: &str| {
                let mut it = s.chars();
                it.next().filter(|_| it.next().is_none())
            };
            let typed = event.text.as_ref().and_then(|t| one(t)).or_else(|| {
                match &event.logical_key {
                    Key::Character(s) => one(s),
                    _ => None,
                }
            });
            if let Some(c) = typed {
                if self.settings_hangul_char(c) {
                    self.aux_redraw(idx);
                    return;
                }
            }
            if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
                self.aux_redraw(idx);
                return;
            }
            self.settings_hangul_flush();
        }
        // 포커스 필드가 있으면 그 필드로(persona/단일라인 분기는 settings_key 내부).
        if self.settings_input.is_some() {
            self.settings_key(event);
            self.aux_redraw(idx);
            return;
        }
        // 포커스 필드가 없을 때 Esc = 창 닫기.
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.close_settings_window(idx);
            return;
        }
        self.aux_redraw(idx);
    }

    /// 비-macOS OS IME 경로 — 설정 폼엔 preedit 렌더가 없어 Commit 만 반영한다.
    fn aux_settings_ime(&mut self, idx: usize, ime: Ime) {
        if let Ime::Commit(text) = ime {
            self.settings_insert_text(&text);
            self.aux_redraw(idx);
        }
    }

    // ── 터미널 별도창(undock) ──────────────────────────────────────────────
    //
    // 일반 터미널 pane 을 별도 OS 창으로 분리한다. 편집기/설정 aux 와 달리 이 창은
    // 자체 데이터를 안 들고, App.ws 의 그 pane 이 소유한 셀 그리드/커서를 매 프레임
    // 스냅샷해 `draw_cells` 로 그리는 뷰다. PtySession 은 App.pty 에 그대로 살아
    // 세션이 안 끊긴다. 입력은 `self.pty[pane_id].send_bytes` 로 직접, resize 는
    // 창 크기를 셀수로 환산해 `pty.resize`.

    /// `pane_id` 터미널 pane 을 별도창으로 띄운다. `near` Some 이면 그 물리좌표에
    /// (tear-off), None 이면 OS 기본 위치. 새 창 인덱스 반환. 진입점(undock)은 이미
    /// 레이아웃 트리에서 leaf 를 빼고 pty·ws.panes 를 유지한 상태로 호출한다.
    pub(crate) fn spawn_aux_terminal(
        &mut self,
        pane_id: String,
        home_window: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Option<usize> {
        let title = pane_id.clone();
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            // TODO(다음): 헤더의 클릭·드래그 배선(되돌리기 버튼 + drag_window)이
            // 붙는 즉시 `.with_decorations(false)` 를 여기 되살린다. 헤더 그리기는
            // 이미 됐지만 배선 없이 타이틀바를 끄면 창을 옮길 손잡이도, 되돌릴
            // 수단도 없는 창이 남는다 — 거노 요구는 "우리 UI 로"지 "손잡이 없이"가
            // 아니다.
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(800.0, 520.0));
        if let Some(pos) = near {
            attrs = attrs.with_position(pos);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] terminal window create failed: {e}");
                return None;
            }
        };
        // 메인 창과 동일 IME 정책: macOS 는 OS IME 끄고 in-process hangul, 그 외는 OS IME.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] terminal gpu init failed: {e}");
                return None;
            }
        };
        let aux = AuxWindow {
            gpu,
            kind: AuxWindowKind::Terminal { pane_id: pane_id.clone(), window: home_window },
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
            md_content_h: 0.0,
            window,
        };
        self.aux_windows.push(aux);
        let idx = self.aux_windows.len() - 1;
        eprintln!("[auxwin] opened terminal window #{idx} for {pane_id}");
        // 창 client 크기에 맞춰 PTY 를 즉시 resize — 셸이 SIGWINCH 로 새 셀수에 리플로우.
        self.aux_terminal_resize_pty(idx);
        self.aux_redraw(idx);
        Some(idx)
    }

    /// 창 client 크기(logical)를 셀수로 환산해 이 창이 뷰하는 pane 의 PTY 를 resize.
    /// 본문 = 창 − 좌우/상하 PANE_INNER 여백. 셀 메트릭은 이 창 gpu 의 것(논리 px).
    fn aux_terminal_resize_pty(&mut self, idx: usize) {
        let (pane_id, w, h, cw, ch) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let Some(pid) = a.term_pane_id() else { return };
            let (w, h) = a.logical_size();
            (pid.to_string(), w, h, a.gpu.cell_w, a.gpu.cell_h)
        };
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let cols = (((w - PANE_INNER_X * 2.0) / cw).floor() as i32).max(1) as u16;
        let rows = (((h - PANE_INNER_Y * 2.0) / ch).floor() as i32).max(1) as u16;
        if let Some(pty) = self.pty.get(&pane_id) {
            let _ = pty.resize(cols, rows);
        }
    }

    /// 터미널 별도창 한 프레임 — ws 에서 그 pane 의 셀 그리드/커서를 스냅샷해
    /// `draw_cells` 로 본문을, blink 위상이면 커서 rect 를 그린다(단일 pane 이라
    /// 헤더/링크hover/선택 오버레이는 v1 제외 — paint_gpu_overlays 커서부만 복제).
    fn aux_terminal_render(&mut self, idx: usize, blink: bool) {
        let (pane_id, scale, w, h, focused, home_window) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let Some(pid) = a.term_pane_id() else { return };
            let (w, h) = a.logical_size();
            let home = match &a.kind {
                AuxWindowKind::Terminal { window, .. } => *window,
                _ => 0,
            };
            (pid.to_string(), a.gpu.scale(), w, h, a.focused, home)
        };
        // draw 중 lock 을 안 쥐도록 셀/커서를 복사해 스냅샷. 배정 학생도 같이 —
        // 꺼낸 pane 도 메인 그리드에 있을 때와 같은 학생색·이름을 달아야 한다
        // (거노: 별도창이 완전 다른 앱 같다). ordinal 은 동명이인 구분용이라
        // 전체 pane 맵이 필요하다.
        let (snap, student) = {
            let ws = self.ws.lock().unwrap();
            let cells = ws.panes.get(&pane_id).and_then(|p| {
                p.term()
                    .map(|t| (t.cells.clone(), t.cursor_row, t.cursor_col, t.cursor_visible))
            });
            let who = ws.pane_character.get(&pane_id).cloned().map(|name| {
                let ord = crate::theme::character_ordinal(&ws.pane_character, &pane_id);
                let col = crate::theme::character_accent_n(&name, ord);
                (name, col)
            });
            (cells, who)
        };
        let working = self
            .pane_activity
            .get(&pane_id)
            .is_some_and(|a| a.status == "working");
        let accent = student
            .as_ref()
            .and_then(|(_, c)| *c)
            .unwrap_or_else(|| crate::theme::accent());
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let Some((rows, cur_row, cur_col, cur_vis)) = snap else {
            // pane 이 사라졌으면(셸 종료 등) 빈 배경만 present.
            let _ = a.gpu.render(&[], scale, 0.0, true);
            a.dirty = false;
            return;
        };
        // OS 타이틀바를 껐으니 헤더는 우리가 그린다 — 여기가 창의 유일한 손잡이다.
        // 왼쪽에 「%3 · 2번 방」(꺼낸 pane 의 번호와 나온 방), 오른쪽에 되돌리기.
        // 빈 곳을 끌면 창이 움직인다(`aux_header_press` → `drag_window`).
        {
            // 학생이 있으면 그 이름을 앞에 세우고 학생색으로 — 메인 그리드에서
            // 테두리·헤더가 하던 "이 창에 누가 있나"를 여기서도 한눈에.
            let label = match &student {
                Some((name, _)) => format!("{name} · {pane_id} · {}번 방", home_window + 1),
                None => format!("{pane_id} · {}번 방", home_window + 1),
            };
            a.gpu.rect(0.0, 0.0, w, AUX_HEADER_H, crate::theme::surface());
            a.gpu.rect(0.0, AUX_HEADER_H - 1.0, w, 1.0, crate::theme::border());
            let label_col = if student.is_some() { accent } else { crate::theme::text_mute() };
            a.gpu.draw_text(
                12.0,
                (AUX_HEADER_H - 12.0) / 2.0,
                &label,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: crate::theme::with_alpha(label_col, if focused { 0xF0 } else { 0x99 }),
                    bold: student.is_some(),
                    italic: false,
                },
            );
            // working 스윕바 — 메인 pane 상단의 그것과 같은 신호다. 없으면 꺼낸
            // 창만 "멈춘 것처럼" 보인다.
            if working {
                const BAR_H: f32 = 2.5;
                let phase = crate::render::anim_phase_secs();
                a.gpu.rect(0.0, 0.0, w, BAR_H, crate::theme::with_alpha(accent, 0x2e));
                let seg = (w * 0.32).clamp(36.0, 160.0);
                let span = w + seg;
                let off = (phase * 0.5).fract() * span - seg;
                let sx = off.max(0.0);
                let ex = (off + seg).min(w);
                if ex > sx {
                    a.gpu.rect(sx, 0.0, ex - sx, BAR_H, accent);
                }
            }
            // 되돌리기 — ⌘W 와 같은 동작이지만, 그 단축키를 모르면 창에 갇힌다.
            a.gpu.queue_icon(
                "corner-down-left",
                w - AUX_HEADER_H + 6.0,
                (AUX_HEADER_H - 14.0) / 2.0,
                14.0,
                crate::theme::text_mute(),
            );
        }
        // origin_px 는 물리 px(draw_cells 규약), 커서 rect 은 논리 px(gpu.rect 규약).
        let origin_px = (PANE_INNER_X * scale, (PANE_INNER_Y + AUX_HEADER_H) * scale);
        let slot = gpu::PaneSlot {
            rows: &rows,
            origin_px,
            font_scale: 1.0,
            dim: false,
            links: Vec::new(),
            default_fg: crate::cells::default_fg(),
        };
        a.gpu.draw_cells(&[slot]);
        // 커서 자리(논리 px). 조합 중 한글이 있으면 그 프리에딧을, 없으면 blink 커서.
        let cw = a.gpu.cell_w;
        let ch = a.gpu.cell_h;
        let px = PANE_INNER_X + cur_col as f32 * cw;
        let py = PANE_INNER_Y + cur_row as f32 * ch;
        let pe = a.preedit.clone();
        if pe.is_empty() {
            if cur_vis && focused && blink {
                let mut c = crate::cells::iterm_cursor();
                c[3] = 140; // ~0.55 alpha (paint_gpu_overlays 와 동일)
                a.gpu.rect(px, py, cw, ch, c);
            }
        } else {
            // 조합 중 한글 — 커서 자리에 프리에딧(메인 render 와 동일 draw_preedit).
            a.gpu.draw_preedit(px, py, &pe, crate::cells::iterm_cursor(), 1.0);
        }
        // 학생색 외곽선 — 메인 그리드의 active pane 테두리와 같은 신호다. 셀 위에
        // 얹어야 가장자리 글자에 안 먹힌다. 포커스가 없을 땐 흐리게 남겨 어느 창이
        // 누구인지는 계속 보이게 한다.
        if student.is_some() {
            const T: f32 = 1.5;
            let col = crate::theme::with_alpha(accent, if focused { 0xFF } else { 0x66 });
            a.gpu.rect(0.0, 0.0, w, T, col);
            a.gpu.rect(0.0, h - T, w, T, col);
            a.gpu.rect(0.0, 0.0, T, h, col);
            a.gpu.rect(w - T, 0.0, T, h, col);
        }
        let _ = a.gpu.render(&[], scale, 0.0, true);
        a.dirty = false;
        // 스윕바는 애니메이션이라 다음 프레임을 스스로 불러야 한다 — PTY 출력이
        // 없으면 아무도 이 창을 다시 그리지 않아 바가 한 자리에 얼어붙는다.
        // working 이 끝나면 이 요청도 끊긴다.
        if working {
            a.dirty = true;
            a.window.request_redraw();
        }
    }

    /// 이 방이 지금 별도 창으로 나가 있나. 사이드바 탭 표시와 info 배지가 같은
    /// 판정을 쓰도록 한 곳에 둔다.
    pub(crate) fn window_is_undocked(&self, window: usize) -> bool {
        self.aux_windows.iter().any(|a| a.room_window() == Some(window))
    }

    /// 방 창이 그릴 leaf rect(셀 단위). 창 client 를 셀수로 환산해 그 방 트리를 편다.
    ///
    /// 꺼낸 방은 활성이 아니어서 트리가 `windows[i]` 에 있지만, 활성일 때도 되도록
    /// `pty_layout` 을 함께 본다 — 활성 판정이 한 틱 어긋나도 빈 창이 되지 않는다.
    pub(crate) fn room_leaf_rects(&self, idx: usize) -> Vec<(String, u16, u16, u16, u16)> {
        let Some(a) = self.aux_windows.get(idx) else { return Vec::new() };
        let Some(window) = a.room_window() else { return Vec::new() };
        let (w, h) = a.logical_size();
        let (cw, ch) = (a.gpu.cell_w, a.gpu.cell_h);
        if cw <= 0.0 || ch <= 0.0 {
            return Vec::new();
        }
        let cols = (((w - PANE_INNER_X * 2.0) / cw).floor() as i32).max(1) as u16;
        let rows = (((h - PANE_INNER_Y * 2.0) / ch).floor() as i32).max(1) as u16;
        let layout = if window == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(window).and_then(|s| s.as_ref())
        };
        layout.map(|l| l.leaf_rects(cols, rows)).unwrap_or_default()
    }

    /// 방 별도창 한 프레임 — 트리를 `leaf_rects` 로 펼쳐 pane 마다 셀 그리드를 그린다.
    /// `draw_cells` 가 슬라이스를 받으므로 pane 이 몇이든 한 번에 올라간다. 포커스
    /// pane 만 또렷하고(나머지 dim) 커서도 거기만 — 메인 창의 관례 그대로다.
    fn aux_room_render(&mut self, idx: usize, blink: bool) {
        let (window, focus, scale, w, h, focused, cw, ch) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let AuxWindowKind::Room { window, focus } = &a.kind else { return };
            let (w, h) = a.logical_size();
            (*window, focus.clone(), a.gpu.scale(), w, h, a.focused, a.gpu.cell_w, a.gpu.cell_h)
        };
        let rects = self.room_leaf_rects(idx);
        // 방 이름은 App 만 안다 — title() 이 붙인 「방 N」을 실제 라벨로 승격.
        let label = self
            .window_labels
            .get(window)
            .map(|(n, _)| n.clone())
            .filter(|n| !n.is_empty());
        // 셀은 draw 전에 복사해 둔다 — 그리는 동안 ws lock 을 쥐지 않기 위해서.
        let snaps: Vec<_> = {
            let ws = self.ws.lock().unwrap();
            rects
                .iter()
                .filter_map(|(pid, x, y, _, _)| {
                    ws.panes.get(pid).and_then(|p| {
                        p.term().map(|t| {
                            (
                                pid.clone(),
                                t.cells.clone(),
                                *x,
                                *y,
                                t.cursor_row,
                                t.cursor_col,
                                t.cursor_visible,
                            )
                        })
                    })
                })
                .collect()
        };
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        if let Some(name) = label {
            if a.last_title != name {
                a.window.set_title(&name);
                a.last_title = name;
            }
        }
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let slots: Vec<gpu::PaneSlot> = snaps
            .iter()
            .map(|(pid, cells, x, y, _, _, _)| gpu::PaneSlot {
                rows: cells,
                // origin_px 는 물리 px(draw_cells 규약) — 셀 좌표를 논리 px 로 편 뒤 스케일.
                origin_px: (
                    (PANE_INNER_X + *x as f32 * cw) * scale,
                    (PANE_INNER_Y + *y as f32 * ch) * scale,
                ),
                font_scale: 1.0,
                dim: focus.as_deref() != Some(pid.as_str()),
                links: Vec::new(),
                default_fg: crate::cells::default_fg(),
            })
            .collect();
        a.gpu.draw_cells(&slots);
        // pane 경계 — 나눠져 있다는 걸 보이게. 포커스만 accent 테두리.
        for (pid, x, y, pw, ph) in &rects {
            let rx = PANE_INNER_X + *x as f32 * cw;
            let ry = PANE_INNER_Y + *y as f32 * ch;
            let (rw, rh) = (*pw as f32 * cw, *ph as f32 * ch);
            let is_focus = focus.as_deref() == Some(pid.as_str());
            let col = if is_focus { crate::theme::accent() } else { crate::theme::border() };
            a.gpu.rect(rx, ry, rw, 1.0, col);
            a.gpu.rect(rx, ry + rh - 1.0, rw, 1.0, col);
            a.gpu.rect(rx, ry, 1.0, rh, col);
            a.gpu.rect(rx + rw - 1.0, ry, 1.0, rh, col);
        }
        // 커서/프리에딧은 포커스 pane 자리에만.
        if let Some((_, _, x, y, cur_row, cur_col, cur_vis)) = snaps
            .iter()
            .find(|(pid, ..)| focus.as_deref() == Some(pid.as_str()))
        {
            let px = PANE_INNER_X + (*x as f32 + *cur_col as f32) * cw;
            let py = PANE_INNER_Y + (*y as f32 + *cur_row as f32) * ch;
            let pe = a.preedit.clone();
            if pe.is_empty() {
                if *cur_vis && focused && blink {
                    let mut c = crate::cells::iterm_cursor();
                    c[3] = 140;
                    a.gpu.rect(px, py, cw, ch, c);
                }
            } else {
                a.gpu.draw_preedit(px, py, &pe, crate::cells::iterm_cursor(), 1.0);
            }
        }
        let _ = a.gpu.render(&[], scale, 0.0, true);
        a.dirty = false;
    }

    /// 이 창이 뷰하는 pane 의 PTY 로 바이트 전송(빈 입력 무시).
    fn aux_term_send(&self, pane_id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(pty) = self.pty.get(pane_id) {
            let _ = pty.send_bytes(bytes);
        }
    }

    /// 터미널 별도창 이벤트 라우팅. Resized/Scale 는 PTY resize 까지, Close 는 dock 복귀.
    fn aux_terminal_event(
        &mut self,
        idx: usize,
        event: WindowEvent,
        event_loop: &ActiveEventLoop,
    ) {
        let _ = event_loop;
        match event {
            WindowEvent::CloseRequested => self.dock_pane_terminal(idx),
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_terminal_resize_pty(idx);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_terminal_resize_pty(idx);
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux_windows.get(idx).map(|a| a.gpu.scale()).unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.aux_terminal_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_terminal_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_terminal_ime(idx, ime),
            WindowEvent::RedrawRequested => self.aux_render(idx),
            _ => {}
        }
    }

    /// 휠 → PTY 스크롤백(alacritty display_offset). 위로(y>0)=과거로.
    fn aux_terminal_wheel(&mut self, idx: usize, delta: MouseScrollDelta) {
        let pane_id = match self.aux_windows.get(idx).and_then(|a| a.term_pane_id()) {
            Some(p) => p.to_string(),
            None => return,
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
        };
        if lines.abs() < 0.01 {
            return;
        }
        let step = lines.abs().ceil() as i32;
        if let Some(pty) = self.pty.get(&pane_id) {
            pty.scroll(if lines > 0.0 { step } else { -step });
        }
        self.aux_redraw(idx);
    }

    /// 터미널 별도창 키 입력 → PTY 바이트. forward_key 의 셸 전송부만 축약 재현
    /// (git/파일트리/이미지 side effect 없음). 한글은 편집기 aux 와 동일한 in-process
    /// composer(self.hangul) 경로.
    fn aux_terminal_key(&mut self, idx: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input_at = Instant::now();
        let pane_id = match self.aux_windows.get(idx).and_then(|a| a.term_pane_id()) {
            Some(p) => p.to_string(),
            None => return,
        };
        self.ime_retarget(crate::ImeFocus::Pane(pane_id.clone()));
        // Cmd/Ctrl+W: 이 창 닫기 → dock 복귀. 방 창이면 방을, 아니면 pane 을 되돌린다.
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            if self.aux_windows.get(idx).and_then(|a| a.room_window()).is_some() {
                self.dock_window_room(idx);
            } else {
                self.dock_pane_terminal(idx);
            }
            return;
        }
        // macOS in-process 한글 조합: 자모(U+3130..318F)면 completer 로, 완성 음절만 PTY.
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.aux_term_send(&pane_id, commit.as_bytes());
                        }
                        let pe = self.hangul.preedit().unwrap_or_default();
                        if let Some(a) = self.aux_windows.get_mut(idx) {
                            a.preedit = pe;
                        }
                        self.aux_redraw(idx);
                        return;
                    }
                }
            }
        }
        // Ctrl(+letter) → 제어바이트. host_mod(Cmd)는 제외(위 Cmd+W 외엔 삼킴).
        if self.modifiers.control_key() && !self.host_mod() {
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(b) = ctrl_byte(code) {
                    self.aux_term_flush_hangul(idx, &pane_id);
                    self.aux_term_send(&pane_id, &[b]);
                    return;
                }
            }
        }
        // 특수키 → ANSI 시퀀스.
        let seq: Option<&[u8]> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => Some(b"\r"),
            Key::Named(NamedKey::Tab) => Some(b"\t"),
            Key::Named(NamedKey::Escape) => Some(b"\x1b"),
            Key::Named(NamedKey::Backspace) => {
                // 조합 중이면 자모를 하나 빼고(셸로 안 보냄), 아니면 DEL.
                if self.hangul.backspace() {
                    let pe = self.hangul.preedit().unwrap_or_default();
                    if let Some(a) = self.aux_windows.get_mut(idx) {
                        a.preedit = pe;
                    }
                    self.aux_redraw(idx);
                    return;
                }
                Some(b"\x7f")
            }
            Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A"),
            Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B"),
            Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C"),
            Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D"),
            Key::Named(NamedKey::Home) => Some(b"\x1b[H"),
            Key::Named(NamedKey::End) => Some(b"\x1b[F"),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~"),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~"),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~"),
            _ => None,
        };
        if let Some(bytes) = seq {
            self.aux_term_flush_hangul(idx, &pane_id);
            self.aux_term_send(&pane_id, bytes);
            return;
        }
        // 평문 텍스트(자모는 위에서 소비됨) — 조합 잔여를 먼저 확정하고 그대로 전송.
        if let Some(t) = &event.text {
            if !t.is_empty() {
                self.aux_term_flush_hangul(idx, &pane_id);
                self.aux_term_send(&pane_id, t.as_bytes());
                self.aux_redraw(idx);
            }
        }
    }

    /// 조합 중인 음절을 확정해 PTY 로 보내고 프리에딧을 비운다(제어/특수/평문 전에).
    fn aux_term_flush_hangul(&mut self, idx: usize, pane_id: &str) {
        if let Some(flushed) = self.hangul.flush() {
            self.aux_term_send(pane_id, flushed.as_bytes());
        }
        if let Some(a) = self.aux_windows.get_mut(idx) {
            a.preedit.clear();
        }
    }

    /// 비-macOS(Windows/Linux) OS IME — Preedit 는 이 창 프리에딧, Commit 은 PTY 전송.
    fn aux_terminal_ime(&mut self, idx: usize, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
            }
            Ime::Preedit(text, _) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit = text;
                }
            }
            Ime::Commit(text) => {
                let pane_id = self
                    .aux_windows
                    .get(idx)
                    .and_then(|a| a.term_pane_id())
                    .map(|s| s.to_string());
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.preedit.clear();
                }
                if let Some(pid) = pane_id {
                    self.aux_term_send(&pid, text.as_bytes());
                }
            }
        }
        self.aux_redraw(idx);
    }

    // ── undock / dock ────────────────────────────────────────────────────────

    /// `pane_id` 터미널 pane 을 별도창으로 undock. **핵심**: `remove_pane` 을 쓰면
    /// `self.pty.remove` 로 세션까지 죽으므로 쓰지 않는다 — 레이아웃 트리에서 leaf 만
    /// 빼고 `self.pty`·`ws.panes` 는 유지해, PtySession 이 살아있고 그 셀 그리드를
    /// 별도창이 계속 뷰한다. 진입점 = 헤더 pop-out 아이콘 클릭(near=None) +
    /// 탭을 창 밖으로 드래그(tear-off, near=커서 물리좌표 — 파일 탭과 동일 제스처).
    pub(crate) fn undock_pane_terminal(
        &mut self,
        pane_id: &str,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        // 이미 별도창이면 포커스만.
        if let Some(i) = self
            .aux_windows
            .iter()
            .position(|a| a.term_pane_id() == Some(pane_id))
        {
            self.aux_windows[i].window.focus_window();
            return;
        }
        // tmux 백엔드는 로컬 PTY 소유가 아니라 미지원. PTY 없는 pane(이미지/md)도 무시.
        if self.tmux.is_some() || !self.pty.contains_key(pane_id) {
            return;
        }
        // 나온 방을 지금 붙들어야 한다 — 아래에서 트리의 leaf 를 빼고 나면
        // `window_of_pane` 이 더는 못 찾는다.
        let home_window = self.window_of_pane(pane_id).unwrap_or(self.active_window);
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        // 활성 트리에 없는 pane(스태시/백그라운드)은 v1 범위 밖.
        if !leaves.iter().any(|l| l == pane_id) {
            return;
        }
        let was_active =
            self.ws.lock().unwrap().active_pane.as_deref() == Some(pane_id);
        // 제거 pane 이 active 였으면 형제 leaf 로 포커스 이동(remove_pane 과 동일 규칙).
        let next_focus = if was_active && leaves.len() > 1 {
            let i = leaves.iter().position(|l| l == pane_id).unwrap_or(0);
            Some(if i + 1 < leaves.len() {
                leaves[i + 1].clone()
            } else {
                leaves[i - 1].clone()
            })
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(pane_id);
            }
        } else {
            // 마지막 leaf — 트리 통째 드랍(단일 pane 폴백 재engage). 메인 창은 잠시 빈다;
            // dock 복귀나 새 split 이 다시 채운다.
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
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.spawn_aux_terminal(pane_id.to_string(), home_window, event_loop, near);
    }

    /// 터미널 별도창을 닫으며 그 pane 을 메인 레이아웃으로 되돌린다(dock). 창을 먼저
    /// 제거하고(idx 확보 후), 살아있는 세션이면 활성 pane 오른쪽(Horizontal)에
    /// `split_leaf` 로 기존 pane_id 를 재삽입한다 — 새 세션을 만드는 split_active_pane
    /// 과 달리 기존 PtySession 을 그대로 얹으므로 셸이 안 끊긴다.
    fn dock_pane_terminal(&mut self, idx: usize) {
        let pane_id = match self.aux_windows.get(idx).and_then(|a| a.term_pane_id()) {
            Some(p) => p.to_string(),
            None => {
                self.close_aux_window(idx);
                return;
            }
        };
        // 나온 방을 창이 들고 있다 — 닫기 전에 꺼내야 한다.
        let home = match self.aux_windows.get(idx).map(|a| &a.kind) {
            Some(AuxWindowKind::Terminal { window, .. }) => Some(*window),
            _ => None,
        };
        self.close_aux_window(idx);
        // 셸이 이미 종료돼 세션이 사라졌으면 되돌릴 게 없다.
        if !self.pty.contains_key(&pane_id) {
            return;
        }
        // 나왔던 방으로 돌아간다. 이게 없으면 그때 보고 있던 방에 남의 pane 이
        // 튀어나온다 — 꺼낼 때와 되돌릴 때 활성 방이 같으리란 보장이 없다.
        // 밖에 나가 있는 방으로는 보내지 않는다(그 방은 메인에 안 그려진다).
        if let Some(w) = home {
            if w < self.windows.len() && w != self.active_window && !self.window_is_undocked(w) {
                self.switch_window(w);
            }
        }
        let in_tree = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().any(|l| *l == pane_id))
            .unwrap_or(false);
        if !in_tree {
            let active = self.ws.lock().unwrap().active_pane.clone();
            let inserted = match (active, self.pty_layout.as_mut()) {
                (Some(active), Some(tree)) => {
                    tree.split_leaf(&active, kasa_pty::SplitDir::Horizontal, pane_id.clone())
                }
                _ => false,
            };
            if !inserted {
                // 트리가 비었거나 active 가 트리에 없음 — 이 pane 을 유일 leaf 로.
                self.pty_layout = Some(kasa_pty::PtyLayout::single(pane_id.as_str()));
            }
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(pane_id.clone());
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 방 별도창 이벤트. 키·휠·IME 는 터미널 창 경로를 그대로 쓴다 — `term_pane_id`
    /// 가 포커스 pane 을 내주므로 한 벌로 충분하다. 다른 건 셋뿐이다: 닫기는 방을
    /// 메인으로 되돌리고, 리사이즈는 leaf 마다 PTY 를 다시 재고, 클릭은 포커스를 옮긴다.
    fn aux_room_event(&mut self, idx: usize, event: WindowEvent, event_loop: &ActiveEventLoop) {
        let _ = event_loop;
        match event {
            WindowEvent::CloseRequested => self.dock_window_room(idx),
            WindowEvent::Resized(size) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.gpu.resize(size.width, size.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_room_resize_pty(idx);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    let sf = a.window.scale_factor() as f32;
                    a.gpu.set_scale(sf);
                    a.gpu.set_font_size(FONT_SIZE);
                    let sz = a.window.inner_size();
                    a.gpu.resize(sz.width, sz.height);
                    a.dirty = true;
                    a.window.request_redraw();
                }
                self.aux_room_resize_pty(idx);
            }
            WindowEvent::Focused(f) => {
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.focused = f;
                    a.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux_windows.get(idx).map(|a| a.gpu.scale()).unwrap_or(1.0);
                if let Some(a) = self.aux_windows.get_mut(idx) {
                    a.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.aux_room_click(idx),
            WindowEvent::MouseWheel { delta, .. } => self.aux_terminal_wheel(idx, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_terminal_key(idx, &event),
            WindowEvent::Ime(ime) => self.aux_terminal_ime(idx, ime),
            WindowEvent::RedrawRequested => self.aux_render(idx),
            _ => {}
        }
    }

    /// 방 창 크기 변화 → leaf 마다 제 몫으로 PTY resize. 메인 `resize_backend` 가 하는
    /// 일을 이 창의 셀 메트릭 기준으로 한 것.
    fn aux_room_resize_pty(&mut self, idx: usize) {
        for (pid, _, _, w, h) in self.room_leaf_rects(idx) {
            if let Some(pty) = self.pty.get(&pid) {
                let _ = pty.resize(w.max(1), h.max(1));
            }
        }
    }

    /// 방 창 클릭 → 커서 아래 pane 으로 포커스 이동(키 입력이 그리로 간다).
    fn aux_room_click(&mut self, idx: usize) {
        let (cx, cy, cw, ch) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            (a.cursor_px.0, a.cursor_px.1, a.gpu.cell_w, a.gpu.cell_h)
        };
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let hit = self.room_leaf_rects(idx).into_iter().find(|(_, x, y, w, h)| {
            let rx = PANE_INNER_X + *x as f32 * cw;
            let ry = PANE_INNER_Y + *y as f32 * ch;
            cx >= rx && cx < rx + *w as f32 * cw && cy >= ry && cy < ry + *h as f32 * ch
        });
        let Some((pid, ..)) = hit else { return };
        if let Some(a) = self.aux_windows.get_mut(idx) {
            if let AuxWindowKind::Room { focus, .. } = &mut a.kind {
                if focus.as_deref() == Some(pid.as_str()) {
                    return;
                }
                *focus = Some(pid);
            }
        }
        self.aux_redraw(idx);
    }

    /// 방(윈도우) 하나를 통째로 별도 창으로 꺼낸다 — 탭을 창 밖에 놓았을 때.
    ///
    /// 꺼낼 방은 드래그 press 가 이미 활성으로 만들어 뒀고, 활성 방의 트리는 슬롯이
    /// 아니라 `pty_layout` 에 얹혀 있다. 그래서 먼저 제자리에 park 하고 메인이 볼
    /// 다른 방으로 활성을 옮긴 뒤 창을 띄운다. **방이 하나뿐이면 거부** — 꺼내고 나면
    /// 메인 창이 빈 채로 남는다.
    pub(crate) fn undock_window_room(
        &mut self,
        window: usize,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        if let Some(i) = self.aux_windows.iter().position(|a| a.room_window() == Some(window)) {
            self.aux_windows[i].window.focus_window();
            return;
        }
        if self.tmux.is_some() || window >= self.windows.len() || self.windows.len() < 2 {
            return;
        }
        self.windows[self.active_window] = self.pty_layout.take();
        if self.active_window == window {
            self.active_window = if window + 1 < self.windows.len() {
                window + 1
            } else {
                window - 1
            };
        }
        self.pty_layout = self.windows[self.active_window].take();
        self.window_alert.remove(&self.active_window);
        let focus = self
            .windows
            .get(window)
            .and_then(|s| s.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()));
        // 메인의 활성 pane 이 꺼낸 방 소속이면 남은 방의 것으로 옮긴다 — 안 그러면
        // 화면에 없는 pane 이 선택된 채로 키 입력이 별도 창 pane 에 꽂힌다.
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        {
            let mut ws = self.ws.lock().unwrap();
            let stale = ws.active_pane.as_ref().map(|p| !leaves.contains(p)).unwrap_or(true);
            if stale {
                ws.active_pane = leaves.first().cloned();
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.window_labels_at = None;
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.spawn_aux_room(window, focus, event_loop, near);
    }

    /// 방 별도창을 닫으며 그 방을 메인으로 되돌린다. 트리는 처음부터 `windows` 에
    /// 그대로 있었으므로 창을 없애고 그 방으로 전환하면 끝이다 — 재삽입이 없다.
    /// (`switch_window` 는 밖에 나간 방이면 창을 앞으로 보내므로, 반드시 창을 먼저
    /// 없애고 전환한다.)
    pub(crate) fn dock_window_room(&mut self, idx: usize) {
        let window = self.aux_windows.get(idx).and_then(|a| a.room_window());
        self.close_aux_window(idx);
        let Some(w) = window else { return };
        if w < self.windows.len() {
            self.switch_window(w);
        }
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    /// 방 별도창 스폰. `spawn_aux_terminal` 과 같은 얼개지만 pane 이 여럿이라 기본
    /// 크기가 더 크다.
    fn spawn_aux_room(
        &mut self,
        window: usize,
        focus: Option<String>,
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Option<usize> {
        let title = self
            .window_labels
            .get(window)
            .map(|(n, _)| n.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("방 {}", window + 1));
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(1000.0, 660.0));
        if let Some(pos) = near {
            attrs = attrs.with_position(pos);
        }
        let window_handle = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[auxwin] room window create failed: {e}");
                return None;
            }
        };
        #[cfg(target_os = "macos")]
        window_handle.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window_handle.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window_handle.clone(), FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[auxwin] room gpu init failed: {e}");
                return None;
            }
        };
        self.aux_windows.push(AuxWindow {
            gpu,
            kind: AuxWindowKind::Room { window, focus },
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
            md_content_h: 0.0,
            window: window_handle,
        });
        let idx = self.aux_windows.len() - 1;
        eprintln!("[auxwin] opened room window #{idx} for window {window}");
        // 창 크기에 맞춰 leaf 마다 PTY 를 즉시 재운다(셸이 SIGWINCH 로 리플로우).
        self.aux_room_resize_pty(idx);
        self.aux_redraw(idx);
        Some(idx)
    }
}

/// Ctrl+글자 → 제어바이트(^A=0x01 … ^Z=0x1a). 터미널 별도창 전용 축약(메인
/// forward_key 의 동일 매핑을 winit KeyCode 기준으로 옮긴 것).
fn ctrl_byte(code: winit::keyboard::KeyCode) -> Option<u8> {
    use winit::keyboard::KeyCode;
    let b = match code {
        KeyCode::KeyA => 0x01,
        KeyCode::KeyB => 0x02,
        KeyCode::KeyC => 0x03,
        KeyCode::KeyD => 0x04,
        KeyCode::KeyE => 0x05,
        KeyCode::KeyF => 0x06,
        KeyCode::KeyG => 0x07,
        KeyCode::KeyH => 0x08,
        KeyCode::KeyI => 0x09,
        KeyCode::KeyJ => 0x0a,
        KeyCode::KeyK => 0x0b,
        KeyCode::KeyL => 0x0c,
        KeyCode::KeyM => 0x0d,
        KeyCode::KeyN => 0x0e,
        KeyCode::KeyO => 0x0f,
        KeyCode::KeyP => 0x10,
        KeyCode::KeyQ => 0x11,
        KeyCode::KeyR => 0x12,
        KeyCode::KeyS => 0x13,
        KeyCode::KeyT => 0x14,
        KeyCode::KeyU => 0x15,
        KeyCode::KeyV => 0x16,
        KeyCode::KeyW => 0x17,
        KeyCode::KeyX => 0x18,
        KeyCode::KeyY => 0x19,
        KeyCode::KeyZ => 0x1a,
        _ => return None,
    };
    Some(b)
}
