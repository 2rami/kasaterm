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
    Terminal { pane_id: String },
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
    /// `window` 는 맨 뒤 — `gpu` 보다 나중에 드롭돼 surface 가 살아있는 창을 참조한다.
    pub(crate) window: Arc<Window>,
}

impl AuxWindow {
    pub(crate) fn editor(&self) -> Option<&MarkdownPane> {
        match &self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings | AuxWindowKind::Terminal { .. } => None,
        }
    }
    pub(crate) fn editor_mut(&mut self) -> Option<&mut MarkdownPane> {
        match &mut self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings | AuxWindowKind::Terminal { .. } => None,
        }
    }
    /// 터미널 창이 뷰하는 pane id. 그 외 종류는 None.
    fn term_pane_id(&self) -> Option<&str> {
        match &self.kind {
            AuxWindowKind::Terminal { pane_id } => Some(pane_id.as_str()),
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
            AuxWindowKind::Terminal { pane_id } => pane_id.clone(),
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
            // Settings/Terminal 창은 App 스냅샷(설정 상태·ws 셀 그리드)이 필요해
            // `aux_render_settings`/`aux_terminal_render` 가 직접 페인트한다 — 이
            // 편집기 전용 render 로는 오지 않는다.
            AuxWindowKind::Settings | AuxWindowKind::Terminal { .. } => {}
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
            AuxWindowKind::Settings | AuxWindowKind::Terminal { .. } => return,
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
        // 별도창은 항상 raw 편집기 — .md 를 렌더뷰로 열어놨어도 편집 버퍼를 시드한다.
        md.ensure_raw_seeded();
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
            raw_mode: true,
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
        let lines_n = a.editor().map(|m| m.edit_lines.len()).unwrap_or(0);
        // 본문 높이를 넘는 만큼만 스크롤 — 마지막 줄이 화면 안에 머물게 여유 2줄.
        let content_h = lines_n as f32 * lh;
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
        self.ime_retarget(crate::ImeFocus::Settings);
        // Cmd/Ctrl+W: 설정 창 닫기.
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.close_settings_window(idx);
            return;
        }
        // macOS 는 OS IME 를 껐으므로 한글 자모(U+3130..318F)는 in-process composer
        // 로 조합해 완성 음절만 포커스 필드에 넣는다(설정 폼엔 preedit 렌더 없음).
        #[cfg(target_os = "macos")]
        if self.settings_input.is_some() {
            if let Some(t) = &event.text {
                if t.chars().count() == 1 {
                    if let Some(c) = t.chars().next() {
                        if (0x3130..=0x318F).contains(&(c as u32)) {
                            if let Some(commit) = self.hangul.feed(c) {
                                self.settings_insert_text(&commit);
                            }
                            self.aux_redraw(idx);
                            return;
                        }
                    }
                }
            }
            if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
                self.aux_redraw(idx);
                return;
            }
            if let Some(flushed) = self.hangul.flush() {
                self.settings_insert_text(&flushed);
            }
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
        event_loop: &ActiveEventLoop,
        near: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Option<usize> {
        let title = pane_id.clone();
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
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
            kind: AuxWindowKind::Terminal { pane_id: pane_id.clone() },
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: true,
            preedit: String::new(),
            last_title: title,
            pending_capture: None,
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
        let (pane_id, scale, w, h, focused) = {
            let Some(a) = self.aux_windows.get(idx) else { return };
            let Some(pid) = a.term_pane_id() else { return };
            let (w, h) = a.logical_size();
            (pid.to_string(), a.gpu.scale(), w, h, a.focused)
        };
        // draw 중 lock 을 안 쥐도록 셀/커서를 복사해 스냅샷.
        let snap = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(&pane_id).and_then(|p| {
                p.term()
                    .map(|t| (t.cells.clone(), t.cursor_row, t.cursor_col, t.cursor_visible))
            })
        };
        let Some(a) = self.aux_windows.get_mut(idx) else { return };
        a.gpu.clear_chrome();
        a.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        let Some((rows, cur_row, cur_col, cur_vis)) = snap else {
            // pane 이 사라졌으면(셸 종료 등) 빈 배경만 present.
            let _ = a.gpu.render(&[], scale, 0.0, true);
            a.dirty = false;
            return;
        };
        // origin_px 는 물리 px(draw_cells 규약), 커서 rect 은 논리 px(gpu.rect 규약).
        let origin_px = (PANE_INNER_X * scale, PANE_INNER_Y * scale);
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
        // Cmd/Ctrl+W: 이 창 닫기 → dock 복귀(pane 을 메인 레이아웃으로 되돌림).
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.dock_pane_terminal(idx);
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
        self.spawn_aux_terminal(pane_id.to_string(), event_loop, near);
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
        self.close_aux_window(idx);
        // 셸이 이미 종료돼 세션이 사라졌으면 되돌릴 게 없다.
        if !self.pty.contains_key(&pane_id) {
            return;
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
