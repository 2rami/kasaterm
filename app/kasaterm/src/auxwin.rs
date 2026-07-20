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
    fn editor(&self) -> Option<&MarkdownPane> {
        match &self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings => None,
        }
    }
    fn editor_mut(&mut self) -> Option<&mut MarkdownPane> {
        match &mut self.kind {
            AuxWindowKind::Editor(m) => Some(m),
            AuxWindowKind::Settings => None,
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
                    m.scroll as f32,
                    m.h_scroll,
                    lang,
                    &pe,
                    cursor_on,
                );
            }
            // Settings 창은 `aux_render` 가 App 스냅샷으로 직접 페인트한다 —
            // 이 편집기 전용 render 로는 오지 않는다.
            AuxWindowKind::Settings => {}
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
                (m.edit_lines.len(), line, prefix, m.scroll as f32, m.h_scroll)
            }
            AuxWindowKind::Settings => return,
        };
        let (line_count, cur_line, prefix, scroll, h_scroll) = snap;
        let (ns, nh) = self
            .gpu
            .raw_editor_ensure_visible(line_count, cur_line, &prefix, w, h, scroll, h_scroll);
        if let Some(m) = self.editor_mut() {
            m.scroll = ns.max(0.0) as usize;
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
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico"
        ) {
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
        let id = format!("aux%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let is_md = matches!(ext.as_str(), "md" | "markdown");
        let doc = Arc::new(build_markdown_doc(&id, &path, &raw));
        let edit_lines: Vec<String> = raw.split('\n').map(|s| s.to_string()).collect();
        let md = MarkdownPane {
            doc,
            is_md_doc: is_md,
            raw_mode: true,
            edit_lines,
            cur_line: 0,
            cur_col: 0,
            scroll: 0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
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
        if matches!(self.aux_windows.get(idx).map(|a| &a.kind), Some(AuxWindowKind::Settings)) {
            self.aux_settings_event(idx, event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.close_aux_window(idx);
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
        // Cmd+W: 이 별도창 닫기(dirty 여도 그냥 — 메인 pane 을 안 건드림).
        if self.host_mod()
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW))
        {
            self.close_aux_window(idx);
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
        match code {
            KeyCode::KeyS => {
                self.aux_flush_hangul(idx);
                self.aux_editor_save(idx);
                true
            }
            KeyCode::KeyV => {
                self.aux_flush_hangul(idx);
                self.aux_editor_paste(idx);
                true
            }
            KeyCode::KeyC => {
                self.aux_copy(idx, false);
                true
            }
            KeyCode::KeyX => {
                self.aux_flush_hangul(idx);
                self.aux_copy(idx, true);
                true
            }
            KeyCode::KeyA => {
                self.aux_flush_hangul(idx);
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.select_all_buf();
                }
                true
            }
            KeyCode::KeyZ => {
                self.aux_flush_hangul(idx);
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

    fn aux_insert(&mut self, idx: usize, text: &str) {
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
        match std::fs::write(&path, &text) {
            Ok(()) => {
                if let Some(m) = self.aux_windows.get_mut(idx).and_then(|a| a.editor_mut()) {
                    m.modified = false;
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
                m.scroll as f32,
                m.h_scroll,
                a.cursor_px.0,
                a.cursor_px.1,
            )
        };
        let (lines, scroll, h_scroll, cx, cy) = snap;
        let Some(a) = self.aux_windows.get_mut(idx) else { return (0, 0) };
        a.gpu
            .raw_editor_caret_at(&lines, 0.0, 0.0, scroll, h_scroll, cx, cy)
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
            let ns = (m.scroll as f32 - dy).clamp(0.0, max_scroll);
            m.scroll = ns.max(0.0) as usize;
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
            .with_inner_size(LogicalSize::new(720.0, 620.0));
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

    fn close_settings_window(&mut self, idx: usize) {
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
}
