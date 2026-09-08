//! Detached document windows.
//!
//! Each window owns the same `MarkdownPane` used by an in-pane preview and a
//! small `GpuRenderer`. Moving the pane instead of rebuilding it is what keeps
//! an unsaved buffer, selections, undo history, view mode, and scroll position
//! intact when the user presses "별도창으로".
use super::*;

const HEADER_H: f32 = 36.0;
const BODY_PAD: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderButton {
    View,
    Edit,
    Save,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct FrameRecord {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct CaretRecord {
    line: usize,
    col: usize,
    anchor: Option<(usize, usize)>,
}

impl From<crate::markdown::Caret> for CaretRecord {
    fn from(caret: crate::markdown::Caret) -> Self {
        Self {
            line: caret.line,
            col: caret.col,
            anchor: caret.anchor,
        }
    }
}

impl From<CaretRecord> for crate::markdown::Caret {
    fn from(caret: CaretRecord) -> Self {
        Self {
            line: caret.line,
            col: caret.col,
            anchor: caret.anchor,
        }
    }
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct RestoreRecord {
    path: String,
    is_md_doc: bool,
    raw_mode: bool,
    modified: bool,
    /// Present for dirty or source-less documents. An empty vector/string is
    /// meaningful user data and must not fall back to the file on disk.
    buffer: Option<Vec<String>>,
    cur_line: usize,
    cur_col: usize,
    sel_anchor: Option<(usize, usize)>,
    #[serde(default)]
    extra: Vec<CaretRecord>,
    scroll: f32,
    h_scroll: f32,
    wrap: bool,
    frame: Option<FrameRecord>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct StoredWindows {
    #[serde(default)]
    windows: Vec<RestoreRecord>,
}

enum PendingAuxOpen {
    File {
        path: std::path::PathBuf,
        active: bool,
    },
    Restore(RestoreRecord),
}

/// All detached-document state lives behind this one App field. The launch
/// queue is included so an odoc/socket event received before `resumed` cannot
/// lose the file merely because winit has not supplied an ActiveEventLoop yet.
pub(crate) struct AuxWindows {
    pub(crate) windows: Vec<AuxWindow>,
    pending: Vec<PendingAuxOpen>,
    unopened: Vec<RestoreRecord>,
    saved: std::cell::RefCell<Option<SavedState>>,
}

impl AuxWindows {
    pub(crate) fn load() -> Self {
        let pending = state_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<StoredWindows>(&bytes).ok())
            .map(|saved| {
                saved
                    .windows
                    .into_iter()
                    .map(PendingAuxOpen::Restore)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            windows: Vec::new(),
            pending,
            unopened: Vec::new(),
            saved: Default::default(),
        }
    }
}

/// `gpu` is declared before `window`: the surface must be dropped while the
/// native window it references is still alive.
pub(crate) struct AuxWindow {
    gpu: gpu::GpuRenderer,
    pub(crate) editor: MarkdownPane,
    dirty: bool,
    cursor_px: (f32, f32),
    selecting: bool,
    focused: bool,
    preedit: String,
    last_title: String,
    md_content_h: f32,
    header_hits: Vec<(HeaderButton, (f32, f32, f32, f32))>,
    find_hits: Vec<(FindBtn, (f32, f32, f32, f32))>,
    pub(crate) missing_source: bool,
    status: Option<String>,
    pub(crate) window: Arc<Window>,
}

impl AuxWindow {
    fn logical_size(&self) -> (f32, f32) {
        let scale = self.gpu.scale().max(0.5);
        let size = self.window.inner_size();
        (
            size.width.max(1) as f32 / scale,
            size.height.max(1) as f32 / scale,
        )
    }

    fn body_box(&self) -> (f32, f32, f32, f32) {
        let (w, h) = self.logical_size();
        (
            BODY_PAD,
            HEADER_H,
            (w - BODY_PAD * 2.0).max(1.0),
            (h - HEADER_H - BODY_PAD).max(1.0),
        )
    }

    fn title(&self) -> String {
        let name = std::path::Path::new(&self.editor.doc.path)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("새 문서");
        if self.editor.modified {
            format!("● {name}")
        } else {
            name.to_string()
        }
    }

    fn record(&self) -> RestoreRecord {
        let size = self.window.inner_size();
        // WindowAttributes::with_position places the client area's origin on
        // macOS. Persisting outer_position moved a restored decorated window
        // upward by exactly its titlebar height on every launch.
        #[cfg(target_os = "macos")]
        let pos = self
            .window
            .inner_position()
            .or_else(|_| self.window.outer_position())
            .unwrap_or_else(|_| winit::dpi::PhysicalPosition::new(0, 0));
        #[cfg(not(target_os = "macos"))]
        let pos = self
            .window
            .outer_position()
            .unwrap_or_else(|_| winit::dpi::PhysicalPosition::new(0, 0));
        let source_missing = self.editor.doc.path.is_empty()
            || !std::path::Path::new(&self.editor.doc.path).exists();
        RestoreRecord {
            path: self.editor.doc.path.clone(),
            is_md_doc: self.editor.is_md_doc,
            raw_mode: self.editor.raw_mode,
            modified: self.editor.modified,
            buffer: (self.editor.modified || self.missing_source || source_missing)
                .then(|| self.editor.edit_lines.as_ref().clone()),
            cur_line: self.editor.cur_line,
            cur_col: self.editor.cur_col,
            sel_anchor: self.editor.sel_anchor,
            extra: self.editor.extra.iter().copied().map(Into::into).collect(),
            scroll: self.editor.scroll,
            h_scroll: self.editor.h_scroll,
            wrap: self.editor.wrap,
            frame: Some(FrameRecord {
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
            }),
        }
    }

    fn render(&mut self, cursor_on: bool) {
        let scale = self.gpu.scale();
        let (w, h) = self.logical_size();
        self.gpu.clear_chrome();
        self.gpu.rect(0.0, 0.0, w, h, crate::theme::bg());
        self.draw_header(w);
        let (x, y, bw, bh) = self.body_box();
        if self.editor.raw_mode {
            let lang = crate::code_lang_for_path(std::path::Path::new(&self.editor.doc.path));
            let selection = self.editor.sel_range();
            let find = self
                .editor
                .find
                .as_ref()
                .map(|f| (f.hits.as_slice(), f.idx));
            let complete = self
                .editor
                .complete
                .as_ref()
                .map(|c| (c.items.as_slice(), c.sel, c.from_col));
            let diff = self
                .editor
                .diff
                .as_ref()
                .filter(|d| !d.is_empty())
                .map(|d| crate::gitdiff::DiffView {
                    marks: &d.marks,
                    dels: &d.dels,
                    peek: self
                        .editor
                        .diff_peek
                        .and_then(|line| d.hunk_at(line).map(|h| (line, h.old.as_slice())))
                        .filter(|(_, old)| !old.is_empty()),
                });
            let find_open = self.editor.find.is_some();
            self.gpu.draw_raw_editor(
                &self.editor.edit_lines,
                (self.editor.cur_line, self.editor.cur_col),
                selection,
                x,
                y,
                bw,
                bh,
                self.editor.scroll,
                self.editor.h_scroll,
                lang,
                if find_open { "" } else { &self.preedit },
                cursor_on,
                find,
                complete,
                &[],
                &self.editor.folds,
                self.editor.wrap,
                &self.editor.extra,
                diff.as_ref(),
            );
            self.find_hits = self.editor.find.as_ref().map_or_else(Vec::new, |find| {
                App::draw_find_bar(
                    &mut self.gpu,
                    find,
                    x,
                    y,
                    bw,
                    &self.preedit,
                    cursor_on,
                    self.cursor_px,
                )
            });
        } else {
            self.find_hits.clear();
            self.md_content_h = self.gpu.draw_markdown(
                &self.editor.doc.blocks,
                self.editor.doc.gen,
                x,
                y,
                bw,
                bh,
                self.editor.scroll,
                None,
            );
        }
        let _ = self.gpu.render(&[], scale, 0.0, true);
        self.dirty = false;
    }

    fn draw_header(&mut self, width: f32) {
        self.header_hits.clear();
        self.gpu
            .rect(0.0, 0.0, width, HEADER_H, crate::theme::surface());
        self.gpu
            .rect(0.0, HEADER_H - 1.0, width, 1.0, crate::theme::border());
        let mut x = 10.0;
        if self.editor.is_md_doc {
            for (kind, label, active) in [
                (HeaderButton::View, "보기", !self.editor.raw_mode),
                (HeaderButton::Edit, "편집", self.editor.raw_mode),
            ] {
                let tw = self.gpu.measure_chrome_text(label, 12.0, false);
                let rect = (x, 6.0, tw + 20.0, 24.0);
                if active {
                    crate::round_rect(
                        &mut self.gpu,
                        rect.0,
                        rect.1,
                        rect.2,
                        rect.3,
                        crate::theme::radius_sm(),
                        crate::theme::surface_active(),
                    );
                }
                self.gpu.draw_text(
                    rect.0 + 10.0,
                    rect.1 + 4.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 12.0,
                        color: if active {
                            crate::theme::text()
                        } else {
                            crate::theme::text_dim()
                        },
                        bold: false,
                        italic: false,
                    },
                );
                self.header_hits.push((kind, rect));
                x += rect.2 + 4.0;
            }
        }
        let save_label = if self.editor.modified {
            "저장 안 됨 · 저장"
        } else {
            "저장됨"
        };
        let save_w = self.gpu.measure_chrome_text(save_label, 12.0, false) + 20.0;
        let save_rect = ((width - save_w - 10.0).max(x), 6.0, save_w, 24.0);
        crate::round_rect(
            &mut self.gpu,
            save_rect.0,
            save_rect.1,
            save_rect.2,
            save_rect.3,
            crate::theme::radius_sm(),
            if self.editor.modified {
                crate::theme::surface_active()
            } else {
                crate::theme::panel_bg()
            },
        );
        self.gpu.draw_text(
            save_rect.0 + 10.0,
            save_rect.1 + 4.0,
            save_label,
            gpu::DrawOpts {
                font_size: 12.0,
                color: if self.editor.modified {
                    crate::theme::text()
                } else {
                    crate::theme::text_dim()
                },
                bold: false,
                italic: false,
            },
        );
        self.header_hits.push((HeaderButton::Save, save_rect));
        if let Some(message) = self.status.as_deref() {
            let available = (save_rect.0 - x - 12.0).max(0.0);
            let label = crate::screenread::clip_px(&mut self.gpu, message, 11.0, false, available);
            self.gpu.draw_text(
                x + 6.0,
                10.0,
                &label,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: crate::theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
        }
    }
}

fn state_path() -> Option<std::path::PathBuf> {
    if crate::verification_run() && std::env::var_os("KASATERM_SESSION_FILE").is_none() {
        return None;
    }
    let session = crate::socket::session_file_path()?;
    let stem = session.file_stem()?.to_string_lossy();
    Some(session.with_file_name(format!("{stem}.documents.json")))
}

#[derive(PartialEq, Eq)]
struct StateStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl StateStamp {
    fn read(path: &std::path::Path) -> std::io::Result<Self> {
        std::fs::metadata(path).map(Self::from_metadata)
    }

    fn from_metadata(metadata: std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        }
    }
}

struct SavedState {
    path: std::path::PathBuf,
    bytes: Vec<u8>,
    stamp: StateStamp,
}

fn write_state(
    path: &std::path::Path,
    records: &[RestoreRecord],
    saved: &mut Option<SavedState>,
) -> std::io::Result<bool> {
    write_state_after_rename(path, records, saved, || {})
}

fn write_state_after_rename(
    path: &std::path::Path,
    records: &[RestoreRecord],
    saved: &mut Option<SavedState>,
    after_rename: impl FnOnce(),
) -> std::io::Result<bool> {
    use std::io::Write;

    #[derive(serde::Serialize)]
    struct BorrowedWindows<'a> {
        windows: &'a [RestoreRecord],
    }
    let bytes = serde_json::to_vec(&BorrowedWindows { windows: records })?;
    if saved.as_ref().is_some_and(|saved| {
        saved.path == path
            && saved.bytes == bytes
            && StateStamp::read(path).is_ok_and(|stamp| stamp == saved.stamp)
    }) {
        return Ok(false);
    }
    // A failed replacement must not leave an earlier snapshot eligible for a
    // cache hit; the next autosave must retry even if the editor changes back.
    *saved = None;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let before = StateStamp::from_metadata(file.metadata()?);
        std::fs::rename(&tmp, path)?;
        after_rename();
        file.metadata()
            .map(|metadata| (before, StateStamp::from_metadata(metadata)))
    })();
    let (before, stamp) = match result {
        Ok(stamps) => stamps,
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
    };
    // Checking the destination stamp also invalidates hits after external
    // deletion/replacement without rereading the whole document every tick.
    // The open handle identifies our bytes even if another writer replaces
    // the destination between rename and this check.
    if before.len == stamp.len
        && before.modified == stamp.modified
        && StateStamp::read(path).is_ok_and(|current| current == stamp)
    {
        *saved = Some(SavedState {
            path: path.to_owned(),
            bytes,
            stamp,
        });
    }
    Ok(true)
}

fn source_for_restore(
    record: &RestoreRecord,
    disk: Option<String>,
) -> Option<(String, Arc<Vec<String>>, bool)> {
    if record.modified {
        let lines = Arc::new(record.buffer.clone()?);
        return Some((lines.join("\n"), lines, disk.is_none()));
    }
    match disk {
        Some(text) => {
            let lines = Arc::new(text.split('\n').map(String::from).collect());
            Some((text, lines, false))
        }
        None => {
            let lines = Arc::new(record.buffer.clone()?);
            Some((lines.join("\n"), lines, true))
        }
    }
}

fn make_editor(
    path: &std::path::Path,
    text: &str,
    lines: Arc<Vec<String>>,
    is_md_doc: bool,
    raw_mode: bool,
    modified: bool,
) -> MarkdownPane {
    let mut lines = lines;
    if lines.is_empty() {
        lines = Arc::new(vec![String::new()]);
    }
    MarkdownPane {
        doc: Arc::new(build_markdown_doc(path, text)),
        is_md_doc,
        raw_mode,
        edit_lines: lines,
        cur_line: 0,
        cur_col: 0,
        scroll: 0.0,
        h_scroll: 0.0,
        modified,
        edited_at: modified.then(Instant::now),
        sel_anchor: None,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
        last_edit: EditKind::Break,
        find: None,
        complete: None,
        longest_cache: None,
        wrap: false,
        extra: Vec::new(),
        undo_locked: false,
        folds: Vec::new(),
        folds_gen: 0,
        edit_gen: 0,
        diff: None,
        diff_peek: None,
        diff_head: None,
    }
}

fn create_untabbed(
    event_loop: &ActiveEventLoop,
    attrs: WindowAttributes,
) -> std::result::Result<Window, winit::error::OsError> {
    let window = event_loop.create_window(attrs)?;
    #[cfg(target_os = "macos")]
    disallow_tabbing(&window);
    Ok(window)
}

#[cfg(target_os = "macos")]
fn disallow_tabbing(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowTabbingMode};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    unsafe {
        let view: &NSView = handle.ns_view.cast().as_ref();
        if let Some(native) = view.window() {
            native.setTabbingMode(NSWindowTabbingMode::Disallowed);
        }
    }
}

impl App {
    pub(crate) fn queue_aux_file(&mut self, path: std::path::PathBuf, active: bool) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(aux) = self
            .aux
            .windows
            .iter()
            .find(|aux| std::path::Path::new(&aux.editor.doc.path) == path.as_path())
        {
            if active {
                aux.window.focus_window();
            }
            return;
        }
        if let Some(PendingAuxOpen::File { active: queued, .. }) = self.aux.pending.iter_mut().find(
            |pending| matches!(pending, PendingAuxOpen::File { path: queued, .. } if queued == &path),
        ) {
            *queued |= active;
            return;
        }
        self.aux.pending.push(PendingAuxOpen::File { path, active });
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    pub(crate) fn flush_aux_opens(&mut self, event_loop: &ActiveEventLoop) {
        let pending = std::mem::take(&mut self.aux.pending);
        for request in pending {
            match request {
                PendingAuxOpen::File { path, active } => {
                    let raw = match std::fs::read_to_string(&path) {
                        Ok(raw) => raw,
                        Err(error) => {
                            self.set_toast(format!("문서를 열지 못했어요: {error}"));
                            continue;
                        }
                    };
                    let is_md = path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown")
                        });
                    let lines = Arc::new(raw.split('\n').map(String::from).collect());
                    let editor = make_editor(&path, &raw, lines, is_md, !is_md, false);
                    let _ = self.spawn_aux_editor(editor, event_loop, active, None, false);
                }
                PendingAuxOpen::Restore(record) => {
                    let keep = record.clone();
                    let path = std::path::PathBuf::from(&record.path);
                    let disk = std::fs::read_to_string(&path).ok();
                    let Some((text, lines, missing_source)) = source_for_restore(&record, disk)
                    else {
                        self.set_toast(format!(
                            "{} 원본을 찾지 못해 문서창을 복원하지 않았어요",
                            path.file_name().and_then(|s| s.to_str()).unwrap_or("문서")
                        ));
                        continue;
                    };
                    let mut editor = make_editor(
                        &path,
                        &text,
                        lines,
                        record.is_md_doc,
                        record.raw_mode,
                        record.modified || missing_source,
                    );
                    editor.cur_line = record
                        .cur_line
                        .min(editor.edit_lines.len().saturating_sub(1));
                    editor.cur_col = record
                        .cur_col
                        .min(editor.edit_lines[editor.cur_line].chars().count());
                    editor.sel_anchor = record.sel_anchor;
                    editor.extra = record.extra.into_iter().map(Into::into).collect();
                    editor.scroll = record.scroll.max(0.0);
                    editor.h_scroll = record.h_scroll.max(0.0);
                    editor.wrap = record.wrap;
                    match self.spawn_aux_editor(
                        editor,
                        event_loop,
                        false,
                        record.frame,
                        missing_source,
                    ) {
                        Ok(index) if missing_source => {
                            self.aux.windows[index].status =
                                Some("원본이 없어 복원본만 보관 중이에요".to_string());
                            self.aux.windows[index].window.request_redraw();
                        }
                        Ok(_) => {}
                        Err(_) => self.aux.unopened.push(keep),
                    }
                }
            }
        }
        self.save_aux_windows_state();
    }

    fn spawn_aux_editor(
        &mut self,
        mut editor: MarkdownPane,
        event_loop: &ActiveEventLoop,
        active: bool,
        frame: Option<FrameRecord>,
        missing_source: bool,
    ) -> std::result::Result<usize, MarkdownPane> {
        let background = crate::background_launch();
        let wants_focus = active && !background;
        if editor.edit_lines.is_empty() {
            editor.edit_lines = Arc::new(editor.doc.raw.split('\n').map(String::from).collect());
            if editor.edit_lines.is_empty() {
                editor.edit_lines = Arc::new(vec![String::new()]);
            }
        }
        let title = {
            let name = std::path::Path::new(&editor.doc.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("새 문서");
            if editor.modified {
                format!("● {name}")
            } else {
                name.to_string()
            }
        };
        let mut attrs = WindowAttributes::default()
            .with_title(title.clone())
            .with_theme(Some(Theme::Dark))
            .with_active(wants_focus);
        if let Some(frame) = frame {
            attrs = attrs
                .with_position(winit::dpi::PhysicalPosition::new(frame.x, frame.y))
                .with_inner_size(winit::dpi::PhysicalSize::new(
                    frame.width.max(320),
                    frame.height.max(240),
                ));
        } else {
            attrs = attrs.with_inner_size(LogicalSize::new(760.0, 560.0));
        }
        let window = match create_untabbed(event_loop, attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("[auxwin] 문서창 생성 실패: {error}");
                return Err(editor);
            }
        };
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        let gpu = match gpu::GpuRenderer::new(window.clone(), FONT_SIZE) {
            Ok(gpu) => gpu,
            Err(error) => {
                eprintln!("[auxwin] 문서창 렌더러 생성 실패: {error}");
                return Err(editor);
            }
        };
        self.aux.windows.push(AuxWindow {
            gpu,
            editor,
            dirty: true,
            cursor_px: (0.0, 0.0),
            selecting: false,
            focused: wants_focus,
            preedit: String::new(),
            last_title: title,
            md_content_h: 0.0,
            header_hits: Vec::new(),
            find_hits: Vec::new(),
            missing_source,
            status: None,
            window,
        });
        let index = self.aux.windows.len() - 1;
        self.aux.windows[index].window.request_redraw();
        if wants_focus {
            self.aux.windows[index].window.focus_window();
        } else if !background {
            if let Some(main) = &self.window {
                main.focus_window();
            }
        }
        Ok(index)
    }

    pub(crate) fn popout_pane_tab(
        &mut self,
        outer: &str,
        tab_index: usize,
        event_loop: &ActiveEventLoop,
    ) {
        let editor = {
            let mut ws = self.ws.lock().unwrap();
            let Some(tab) = ws
                .panes
                .get_mut(outer)
                .and_then(|pane| pane.tabs.get_mut(tab_index))
            else {
                return;
            };
            match std::mem::take(&mut tab.content) {
                PaneContent::Markdown(editor) => editor,
                other => {
                    tab.content = other;
                    return;
                }
            }
        };
        if let Err(editor) = self.spawn_aux_editor(editor, event_loop, true, None, false) {
            let mut ws = self.ws.lock().unwrap();
            if let Some(tab) = ws
                .panes
                .get_mut(outer)
                .and_then(|pane| pane.tabs.get_mut(tab_index))
            {
                tab.content = PaneContent::Markdown(editor);
            }
            return;
        }
        // The source tab is now only a husk. Removing it through the existing
        // tab path preserves every sibling terminal and collapses a PTY-less
        // document leaf without treating the move as a closed/revivable file.
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(tab) = ws
                .panes
                .get_mut(outer)
                .and_then(|pane| pane.tabs.get_mut(tab_index))
            {
                tab.preview_path = None;
            }
        }
        self.close_tab(outer, tab_index);
        self.save_aux_windows_state();
    }

    pub(crate) fn aux_window_event(
        &mut self,
        id: WindowId,
        event: WindowEvent,
        _event_loop: &ActiveEventLoop,
    ) -> bool {
        let Some(index) = self
            .aux
            .windows
            .iter()
            .position(|aux| aux.window.id() == id)
        else {
            return false;
        };
        if let WindowEvent::ModifiersChanged(modifiers) = &event {
            self.modifiers = modifiers.state();
        }
        match event {
            WindowEvent::CloseRequested => self.close_aux_editor(index),
            WindowEvent::Moved(_) => self.save_aux_windows_state(),
            WindowEvent::Resized(size) => {
                let aux = &mut self.aux.windows[index];
                aux.gpu.resize(size.width, size.height);
                aux.dirty = true;
                aux.window.request_redraw();
                self.save_aux_windows_state();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let aux = &mut self.aux.windows[index];
                let scale = aux.window.scale_factor() as f32;
                aux.gpu.set_scale(scale);
                aux.gpu.set_font_size(FONT_SIZE);
                let size = aux.window.inner_size();
                aux.gpu.resize(size.width, size.height);
                aux.dirty = true;
                aux.window.request_redraw();
                self.save_aux_windows_state();
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.aux_flush_hangul(index);
                }
                let aux = &mut self.aux.windows[index];
                aux.focused = focused;
                aux.dirty = true;
                aux.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.aux.windows[index].gpu.scale().max(0.5);
                self.aux.windows[index].cursor_px =
                    (position.x as f32 / scale, position.y as f32 / scale);
                self.aux_drag_selection(index);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.aux_mouse_press(index),
                ElementState::Released => {
                    let aux = &mut self.aux.windows[index];
                    aux.selecting = false;
                    if aux.editor.sel_anchor == Some((aux.editor.cur_line, aux.editor.cur_col)) {
                        aux.editor.sel_anchor = None;
                    }
                    self.save_aux_windows_state();
                }
            },
            WindowEvent::MouseWheel { delta, .. } => self.aux_wheel(index, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_key(index, &event),
            WindowEvent::Ime(ime) => self.aux_ime(index, ime),
            WindowEvent::RedrawRequested => self.aux_render(index),
            _ => {}
        }
        true
    }

    pub(crate) fn aux_request_redraws(&mut self) {
        for aux in &mut self.aux.windows {
            if aux.focused || aux.dirty {
                aux.window.request_redraw();
            }
        }
    }

    /// Headless product-route probe. It exposes only structural state and a
    /// digest/length of the buffer, never document text.
    pub(crate) fn aux_probe_summary(&self) -> serde_json::Value {
        use std::hash::{Hash, Hasher};
        if !crate::verification_run() {
            return serde_json::Value::Null;
        }
        serde_json::Value::Array(
            self.aux
                .windows
                .iter()
                .map(|aux| {
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    aux.editor.edit_lines.hash(&mut hash);
                    let size = aux.window.inner_size();
                    let pos = aux.window.outer_position().ok();
                    serde_json::json!({
                        "path": aux.editor.doc.path.clone(),
                        "raw": aux.editor.raw_mode,
                        "modified": aux.editor.modified,
                        "cursor": [aux.editor.cur_line, aux.editor.cur_col],
                        "scroll": aux.editor.scroll,
                        "h_scroll": aux.editor.h_scroll,
                        "buffer_lines": aux.editor.edit_lines.len(),
                        "buffer_hash": hash.finish(),
                        "focused": aux.focused,
                        "find": aux.editor.find.as_ref().map(|find| serde_json::json!({
                            "hits": find.hits.len(),
                            "index": find.idx,
                            "replacing": find.replacing,
                        })),
                        "frame": {
                            "x": pos.map(|p| p.x),
                            "y": pos.map(|p| p.y),
                            "width": size.width,
                            "height": size.height,
                        },
                    })
                })
                .collect(),
        )
    }

    pub(crate) fn aux_probe_ids(&self) -> Vec<WindowId> {
        if !crate::verification_run() {
            return Vec::new();
        }
        self.aux.windows.iter().map(|aux| aux.window.id()).collect()
    }

    /// Open find through the real aux shortcut handler, then feed the query
    /// through the same committed-text route used by keyboard/IME input.
    pub(crate) fn aux_probe_open_find(&mut self, index: usize, query: &str) -> bool {
        if !crate::verification_run() || self.aux.windows.get(index).is_none() {
            return false;
        }
        if !self.aux_shortcut(index, winit::keyboard::KeyCode::KeyF) {
            return false;
        }
        let id = self.aux.windows[index].window.id();
        self.aux_insert_id(id, query);
        self.aux_redraw(index);
        true
    }

    /// Physical center of a header hit target from the last real paint. The
    /// testkit feeds it back as CursorMoved + MouseInput, exercising the same
    /// hit-test and action route as a person's click.
    pub(crate) fn aux_probe_header_center(
        &self,
        index: usize,
        kind: &str,
    ) -> Option<(WindowId, winit::dpi::PhysicalPosition<f64>)> {
        if !crate::verification_run() {
            return None;
        }
        let aux = self.aux.windows.get(index)?;
        let wanted = match kind {
            "view" => HeaderButton::View,
            "edit" => HeaderButton::Edit,
            "save" => HeaderButton::Save,
            _ => return None,
        };
        let (_, rect) = aux
            .header_hits
            .iter()
            .find(|(button, _)| *button == wanted)?;
        let scale = aux.gpu.scale() as f64;
        Some((
            aux.window.id(),
            winit::dpi::PhysicalPosition::new(
                (rect.0 + rect.2 * 0.5) as f64 * scale,
                (rect.1 + rect.3 * 0.5) as f64 * scale,
            ),
        ))
    }

    /// Arm one detached renderer's ordinary GPU readback. Testkit still has
    /// to open the document through the public product route before this can
    /// succeed, so the hook cannot manufacture a fake aux window.
    pub(crate) fn aux_capture(&mut self, index: usize, path: String) -> bool {
        if !crate::verification_run() {
            return false;
        }
        let Some(aux) = self.aux.windows.get_mut(index) else {
            return false;
        };
        aux.gpu.capture_next = Some(path);
        aux.dirty = true;
        aux.window.request_redraw();
        true
    }

    pub(crate) fn save_aux_windows_state(&self) {
        let mut records: Vec<RestoreRecord> =
            self.aux.windows.iter().map(AuxWindow::record).collect();
        // Explicit in-pane previews are intentionally absent from the PTY
        // layout JSON. Save them beside aux windows so choosing tab/split does
        // not make a dirty document less recoverable; restore may reopen them
        // as detached windows without reviving a fake PTY leaf.
        if let Ok(ws) = self.ws.lock() {
            records.extend(ws.panes.values().flat_map(|pane| {
                pane.tabs.iter().filter_map(|tab| {
                    let editor = tab.markdown()?;
                    let missing = editor.doc.path.is_empty()
                        || !std::path::Path::new(&editor.doc.path).exists();
                    let buffer = (editor.modified || missing).then(|| {
                        if editor.edit_lines.is_empty() {
                            editor.doc.raw.split('\n').map(String::from).collect()
                        } else {
                            editor.edit_lines.as_ref().clone()
                        }
                    });
                    Some(RestoreRecord {
                        path: editor.doc.path.clone(),
                        is_md_doc: editor.is_md_doc,
                        raw_mode: editor.raw_mode,
                        modified: editor.modified,
                        buffer,
                        cur_line: editor.cur_line,
                        cur_col: editor.cur_col,
                        sel_anchor: editor.sel_anchor,
                        extra: editor.extra.iter().copied().map(Into::into).collect(),
                        scroll: editor.scroll,
                        h_scroll: editor.h_scroll,
                        wrap: editor.wrap,
                        frame: None,
                    })
                })
            }));
        }
        records.extend(self.aux.pending.iter().filter_map(|pending| match pending {
            PendingAuxOpen::Restore(record) => Some(record.clone()),
            PendingAuxOpen::File { .. } => None,
        }));
        records.extend(self.aux.unopened.iter().cloned());
        let mut seen = std::collections::HashSet::new();
        records.retain(|record| record.path.is_empty() || seen.insert(record.path.clone()));
        if let Some(path) = state_path() {
            let _ = write_state(&path, &records, &mut self.aux.saved.borrow_mut());
        }
    }

    fn aux_render(&mut self, index: usize) {
        let cursor_on = self.cursor_blink_on(Instant::now());
        let Some(aux) = self.aux.windows.get_mut(index) else {
            return;
        };
        let title = aux.title();
        if title != aux.last_title {
            aux.window.set_title(&title);
            aux.last_title = title;
        }
        aux.render(aux.focused && cursor_on);
    }

    pub(crate) fn aux_redraw(&mut self, index: usize) {
        if let Some(aux) = self.aux.windows.get_mut(index) {
            aux.dirty = true;
            aux.window.request_redraw();
        }
    }

    pub(crate) fn aux_index(&self, id: WindowId) -> Option<usize> {
        self.aux
            .windows
            .iter()
            .position(|aux| aux.window.id() == id)
    }

    pub(crate) fn aux_owns_window(&self, id: WindowId) -> bool {
        self.aux_index(id).is_some()
    }

    pub(crate) fn focus_aux_window(&self, id: WindowId) {
        if let Some(index) = self.aux_index(id) {
            self.aux.windows[index].window.focus_window();
        }
    }

    pub(crate) fn set_aux_status(&mut self, id: WindowId, message: String) {
        if let Some(index) = self.aux_index(id) {
            self.aux.windows[index].status = Some(message);
            self.aux_redraw(index);
        }
    }

    pub(crate) fn close_aux_by_id(&mut self, id: WindowId) {
        if let Some(index) = self.aux_index(id) {
            self.aux.windows.remove(index);
            self.save_aux_windows_state();
        }
    }

    fn close_aux_editor(&mut self, index: usize) {
        self.aux_flush_hangul(index);
        let Some(id) = self.aux.windows.get(index).map(|aux| aux.window.id()) else {
            return;
        };
        if self.guard_dirty(&PendingClose::AuxEditor(id)) {
            if let Some(main) = &self.window {
                main.focus_window();
                main.request_user_attention(Some(winit::window::UserAttentionType::Critical));
            }
            return;
        }
        self.close_aux_by_id(id);
    }

    pub(crate) fn aux_doc(&self, id: WindowId) -> Option<&MarkdownPane> {
        self.aux
            .windows
            .iter()
            .find(|aux| aux.window.id() == id)
            .map(|aux| &aux.editor)
    }

    pub(crate) fn aux_doc_mut(&mut self, id: WindowId) -> Option<&mut MarkdownPane> {
        self.aux
            .windows
            .iter_mut()
            .find(|aux| aux.window.id() == id)
            .map(|aux| &mut aux.editor)
    }

    fn aux_key(&mut self, index: usize, event: &KeyEvent) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed || crate::input::is_modifier_key(event) {
            return;
        }
        self.last_input_at = Instant::now();
        let id = self.aux.windows[index].window.id();
        self.ime_retarget(ImeFocus::AuxEditor(id));
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.close_aux_editor(index);
            return;
        }
        if self.host_mod() {
            self.aux_flush_hangul(index);
            if let PhysicalKey::Code(code) = event.physical_key {
                if self.aux_shortcut(index, code) {
                    self.aux_redraw(index);
                    self.save_aux_windows_state();
                    return;
                }
            }
            // An unowned host chord belongs to the application/OS menu layer;
            // it must never fall through and type its letter into the buffer.
            return;
        }
        if !self.aux.windows[index].editor.raw_mode {
            let page = self.aux.windows[index].body_box().3;
            let line = self.aux.windows[index].gpu.raw_editor_line_h();
            let delta = match event.logical_key {
                Key::Named(NamedKey::ArrowUp) => Some(-line),
                Key::Named(NamedKey::ArrowDown) => Some(line),
                Key::Named(NamedKey::PageUp) => Some(-page),
                Key::Named(NamedKey::PageDown) => Some(page),
                Key::Named(NamedKey::Space) => Some(if self.modifiers.shift_key() {
                    -page
                } else {
                    page
                }),
                _ => None,
            };
            if let Some(delta) = delta {
                self.aux_scroll_by(index, delta);
                return;
            }
            if !crate::markdown::md_mutating_key(event) {
                return;
            }
            self.aux_set_mode(index, true);
        }
        #[cfg(target_os = "macos")]
        if let Some(text) = &event.text {
            if text.chars().count() == 1 {
                if let Some(ch) = text.chars().next() {
                    if (0x3130..=0x318f).contains(&(ch as u32)) {
                        if let Some(commit) = self.hangul.feed(ch) {
                            self.aux_insert_id(id, &commit);
                        }
                        let preedit = self.hangul.preedit().unwrap_or_default();
                        self.aux.windows[index].preedit = preedit.clone();
                        self.preedit = preedit;
                        self.in_preedit = !self.preedit.is_empty();
                        self.aux_redraw(index);
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            let preedit = self.hangul.preedit().unwrap_or_default();
            self.aux.windows[index].preedit = preedit.clone();
            self.preedit = preedit;
            self.in_preedit = !self.preedit.is_empty();
            self.aux_redraw(index);
            return;
        }
        self.aux_flush_hangul(index);
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let page_lines = if matches!(
            event.logical_key,
            Key::Named(NamedKey::PageUp | NamedKey::PageDown)
        ) {
            let height = self.aux.windows[index].body_box().3;
            let line_h = self.aux.windows[index].gpu.raw_editor_line_h();
            (((height / line_h).floor() as usize).saturating_sub(1)).max(1)
        } else {
            0
        };
        let rows = {
            let aux = &mut self.aux.windows[index];
            let width = aux.body_box().2;
            let wrap_cols =
                aux.gpu
                    .raw_editor_wrap_cols(width, aux.editor.edit_lines.len(), aux.editor.wrap);
            crate::markdown::layout_rows(&aux.editor.edit_lines, &aux.editor.folds, wrap_cols)
        };
        {
            let editor = &mut self.aux.windows[index].editor;
            if editor.find.is_some() {
                editor.find_key(event, shift);
            } else if editor.complete_key(event) {
                // Completion owns Enter/Tab/Escape before the buffer does.
            } else {
                editor.each_caret(|editor| {
                    editor.apply_edit_key(event, shift, alt, page_lines, Some(&rows))
                });
                editor.complete_after_key(event);
            }
        }
        self.aux_ensure_visible(index);
        self.aux.windows[index].status = None;
        self.aux_redraw(index);
        self.save_aux_windows_state();
    }

    fn aux_shortcut(&mut self, index: usize, code: winit::keyboard::KeyCode) -> bool {
        use winit::keyboard::KeyCode;
        match code {
            KeyCode::KeyS => {
                self.aux_save(index);
                true
            }
            KeyCode::KeyV => {
                let Ok(mut clipboard) = arboard::Clipboard::new() else {
                    return true;
                };
                let Ok(text) = clipboard.get_text() else {
                    return true;
                };
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                self.aux.windows[index]
                    .editor
                    .each_caret(|editor| editor.paste_at_caret(&text));
                self.aux_ensure_visible(index);
                true
            }
            KeyCode::KeyC | KeyCode::KeyX => {
                let cut = code == KeyCode::KeyX;
                if let Some(text) = self.aux.windows[index].editor.take_copy(cut) {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(text);
                    }
                }
                true
            }
            KeyCode::KeyA => {
                self.aux.windows[index].editor.select_all_buf();
                true
            }
            KeyCode::KeyD if !self.modifiers.shift_key() => {
                self.aux.windows[index].editor.select_next_occurrence();
                self.aux_ensure_visible(index);
                true
            }
            KeyCode::KeyZ => {
                if self.modifiers.shift_key() {
                    self.aux.windows[index].editor.do_redo();
                } else {
                    self.aux.windows[index].editor.do_undo();
                }
                self.aux_ensure_visible(index);
                true
            }
            KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown => {
                self.aux.windows[index]
                    .editor
                    .apply_cmd_arrow(code, self.modifiers.shift_key());
                self.aux_ensure_visible(index);
                true
            }
            KeyCode::KeyF => {
                self.aux.windows[index]
                    .editor
                    .find_open(self.modifiers.alt_key());
                true
            }
            KeyCode::KeyG => {
                self.aux.windows[index]
                    .editor
                    .find_step(self.modifiers.shift_key());
                true
            }
            _ => false,
        }
    }

    pub(crate) fn aux_insert_id(&mut self, id: WindowId, text: &str) {
        let Some(index) = self.aux_index(id) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.aux.windows[index].preedit.clear();
        let editor = &mut self.aux.windows[index].editor;
        if editor.find.is_some() {
            editor.find_type(text);
        } else {
            editor.each_caret(|editor| editor.insert_at_caret(text));
        }
        self.aux_ensure_visible(index);
        self.aux_redraw(index);
        self.save_aux_windows_state();
    }

    pub(crate) fn aux_flush_hangul(&mut self, index: usize) {
        let Some(id) = self.aux.windows.get(index).map(|aux| aux.window.id()) else {
            return;
        };
        if !matches!(self.ime_focus, Some(ImeFocus::AuxEditor(owner)) if owner == id) {
            return;
        }
        if let Some(commit) = self.hangul.flush() {
            self.aux_insert_id(id, &commit);
        }
        if let Some(aux) = self.aux.windows.get_mut(index) {
            aux.preedit.clear();
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    fn aux_ime(&mut self, index: usize, ime: Ime) {
        let id = self.aux.windows[index].window.id();
        self.ime_retarget(ImeFocus::AuxEditor(id));
        match ime {
            Ime::Enabled | Ime::Disabled => self.aux.windows[index].preedit.clear(),
            Ime::Preedit(text, _) => self.aux.windows[index].preedit = text,
            Ime::Commit(text) => {
                self.aux.windows[index].preedit.clear();
                self.aux_insert_id(id, &text);
            }
        }
        self.aux_redraw(index);
    }

    fn aux_save(&mut self, index: usize) -> bool {
        let Some(aux) = self.aux.windows.get(index) else {
            return false;
        };
        let path = aux.editor.doc.path.clone();
        if path.is_empty() {
            self.aux.windows[index].status =
                Some("새 문서는 저장할 위치를 먼저 골라야 해요".into());
            self.aux_redraw(index);
            return false;
        }
        let text = aux.editor.text_for_save();
        match crate::markdown::write_atomic(&path, &text) {
            Ok(()) => {
                let aux = &mut self.aux.windows[index];
                aux.editor.mark_saved();
                if aux.editor.is_md_doc {
                    aux.editor.doc =
                        Arc::new(build_markdown_doc(std::path::Path::new(&path), &text));
                }
                aux.missing_source = false;
                aux.status = Some("저장했어요".into());
                self.save_aux_windows_state();
                self.aux_redraw(index);
                true
            }
            Err(error) => {
                self.aux.windows[index].status = Some(format!("저장 실패: {error}"));
                self.aux_redraw(index);
                false
            }
        }
    }

    fn aux_set_mode(&mut self, index: usize, want_raw: bool) {
        let (anchor, metrics, ys) = {
            let aux = &mut self.aux.windows[index];
            let metrics = aux.gpu.raw_editor_metrics();
            let anchor = if aux.editor.raw_mode {
                let visual = ((aux.editor.scroll - metrics.0) / metrics.1)
                    .floor()
                    .max(0.0) as usize;
                let width = aux.body_box().2;
                let wrap_cols = aux.gpu.raw_editor_wrap_cols(
                    width,
                    aux.editor.edit_lines.len(),
                    aux.editor.wrap,
                );
                crate::markdown::layout_rows(&aux.editor.edit_lines, &aux.editor.folds, wrap_cols)
                    .get(visual)
                    .map(|(line, _)| *line)
            } else {
                let at = aux
                    .gpu
                    .md_block_ys
                    .partition_point(|&y| y <= aux.editor.scroll)
                    .saturating_sub(1);
                aux.editor.doc.block_lines.get(at).copied()
            };
            (anchor, metrics, aux.gpu.md_block_ys.clone())
        };
        let changed = crate::markdown::switch_md_mode(
            &mut self.aux.windows[index].editor,
            want_raw,
            anchor,
            Some(metrics),
            Some(&ys),
        );
        if changed {
            self.aux.windows[index].status = None;
            self.aux_redraw(index);
            self.save_aux_windows_state();
        }
    }

    fn aux_caret_at_cursor(&mut self, index: usize) -> (usize, usize) {
        let (lines, folds, wrap, scroll, h_scroll, cursor, body) = {
            let aux = &self.aux.windows[index];
            (
                aux.editor.edit_lines.clone(),
                aux.editor.folds.clone(),
                aux.editor.wrap,
                aux.editor.scroll,
                aux.editor.h_scroll,
                aux.cursor_px,
                aux.body_box(),
            )
        };
        let aux = &mut self.aux.windows[index];
        let wrap_cols = aux.gpu.raw_editor_wrap_cols(body.2, lines.len(), wrap);
        aux.gpu.raw_editor_caret_at(
            &lines, body.0, body.1, scroll, h_scroll, cursor.0, cursor.1, &folds, wrap_cols,
        )
    }

    fn aux_mouse_press(&mut self, index: usize) {
        self.aux_flush_hangul(index);
        let cursor = self.aux.windows[index].cursor_px;
        if let Some(button) = self.aux.windows[index]
            .header_hits
            .iter()
            .find(|(_, rect)| hit(cursor, *rect))
            .map(|(button, _)| *button)
        {
            match button {
                HeaderButton::View => self.aux_set_mode(index, false),
                HeaderButton::Edit => self.aux_set_mode(index, true),
                HeaderButton::Save => {
                    self.aux_save(index);
                }
            }
            return;
        }
        if let Some(button) = self.aux.windows[index]
            .find_hits
            .iter()
            .find(|(_, rect)| hit(cursor, *rect))
            .map(|(button, _)| *button)
        {
            let editor = &mut self.aux.windows[index].editor;
            match button {
                FindBtn::ToggleReplace => {
                    if let Some(find) = editor.find.as_mut() {
                        find.replacing = !find.replacing;
                        find.focus_replace = find.replacing && !find.query.is_empty();
                    }
                }
                FindBtn::Prev => editor.find_step(true),
                FindBtn::Next => editor.find_step(false),
                FindBtn::Close => editor.find_close(),
                FindBtn::ReplaceOne => editor.find_replace_one(),
                FindBtn::ReplaceAll => {
                    editor.find_replace_all();
                }
            }
            self.aux_redraw(index);
            self.save_aux_windows_state();
            return;
        }
        if !self.aux.windows[index].editor.raw_mode {
            return;
        }
        let (line, col) = self.aux_caret_at_cursor(index);
        let id = self.aux.windows[index].window.id();
        self.ime_retarget(ImeFocus::AuxEditor(id));
        let editor = &mut self.aux.windows[index].editor;
        editor.cur_line = line;
        editor.cur_col = col;
        editor.sel_anchor = Some((line, col));
        editor.extra.clear();
        editor.last_edit = EditKind::Break;
        self.aux.windows[index].selecting = true;
        self.aux_redraw(index);
        self.save_aux_windows_state();
    }

    fn aux_drag_selection(&mut self, index: usize) {
        if !self.aux.windows[index].selecting || !self.aux.windows[index].editor.raw_mode {
            return;
        }
        let (line, col) = self.aux_caret_at_cursor(index);
        self.aux.windows[index].editor.cur_line = line;
        self.aux.windows[index].editor.cur_col = col;
        self.aux_redraw(index);
        self.save_aux_windows_state();
    }

    fn aux_wheel(&mut self, index: usize, delta: MouseScrollDelta) {
        let aux = &mut self.aux.windows[index];
        let line_h = aux.gpu.raw_editor_line_h();
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * line_h * 3.0,
            MouseScrollDelta::PixelDelta(point) => point.y as f32,
        };
        let body_h = aux.body_box().3;
        let content_h = if aux.editor.raw_mode {
            let width = aux.body_box().2;
            let wrap_cols =
                aux.gpu
                    .raw_editor_wrap_cols(width, aux.editor.edit_lines.len(), aux.editor.wrap);
            crate::markdown::layout_rows(&aux.editor.edit_lines, &aux.editor.folds, wrap_cols).len()
                as f32
                * line_h
        } else if aux.md_content_h > 0.0 {
            aux.md_content_h
        } else {
            aux.editor.edit_lines.len() as f32 * line_h
        };
        aux.editor.scroll = clamped_document_scroll(aux.editor.scroll, -dy, content_h, body_h);
        aux.dirty = true;
        aux.window.request_redraw();
        self.save_aux_windows_state();
    }

    fn aux_scroll_by(&mut self, index: usize, delta: f32) {
        let aux = &mut self.aux.windows[index];
        let visible = aux.body_box().3;
        let content = aux.md_content_h.max(visible);
        aux.editor.scroll = clamped_document_scroll(aux.editor.scroll, delta, content, visible);
        aux.dirty = true;
        aux.window.request_redraw();
        self.save_aux_windows_state();
    }

    fn aux_ensure_visible(&mut self, index: usize) {
        let (lines, line, col, scroll, h_scroll, folds, wrap, body) = {
            let aux = &self.aux.windows[index];
            (
                aux.editor.edit_lines.clone(),
                aux.editor.cur_line,
                aux.editor.cur_col,
                aux.editor.scroll,
                aux.editor.h_scroll,
                aux.editor.folds.clone(),
                aux.editor.wrap,
                aux.body_box(),
            )
        };
        let prefix: String = lines
            .get(line)
            .map(|text| text.chars().take(col).collect())
            .unwrap_or_default();
        let aux = &mut self.aux.windows[index];
        let wrap_cols = aux.gpu.raw_editor_wrap_cols(body.2, lines.len(), wrap);
        let (scroll, h_scroll) = aux.gpu.raw_editor_ensure_visible(
            lines.len(),
            line,
            &prefix,
            body.2,
            body.3,
            scroll,
            h_scroll,
            &folds,
            wrap_cols,
            &lines,
        );
        aux.editor.scroll = scroll.max(0.0);
        aux.editor.h_scroll = h_scroll.max(0.0);
    }
}

fn hit(point: (f32, f32), rect: (f32, f32, f32, f32)) -> bool {
    point.0 >= rect.0
        && point.0 <= rect.0 + rect.2
        && point.1 >= rect.1
        && point.1 <= rect.1 + rect.3
}

pub(crate) fn clamped_document_scroll(
    current: f32,
    delta: f32,
    content_height: f32,
    visible_height: f32,
) -> f32 {
    (current + delta).clamp(0.0, (content_height - visible_height).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StateDir(std::path::PathBuf);

    impl StateDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "kasaterm-aux-save-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> std::path::PathBuf {
            self.0.join("documents.json")
        }
    }

    impl Drop for StateDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unchanged_state_skips_even_opening_the_temporary_file() {
        let dir = StateDir::new();
        let path = dir.path();
        let mut saved = None;
        assert!(write_state(&path, &[], &mut saved).unwrap());
        let original = StateStamp::read(&path).unwrap();
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::create_dir(&tmp).unwrap();
        for _ in 0..100 {
            assert!(!write_state(&path, &[], &mut saved).unwrap());
        }
        assert!(original == StateStamp::read(&path).unwrap());
        assert!(tmp.is_dir());
    }

    #[test]
    fn changed_state_preserves_scroll_carets_mode_order_and_final_close() {
        let dir = StateDir::new();
        let path = dir.path();
        let mut saved = None;
        let mut records = vec![
            RestoreRecord {
                path: "a".into(),
                ..Default::default()
            },
            RestoreRecord {
                path: "b".into(),
                ..Default::default()
            },
        ];
        assert!(write_state(&path, &records, &mut saved).unwrap());
        records[0].scroll = 41.5;
        records[0].h_scroll = 8.0;
        records[0].cur_line = 3;
        records[0].cur_col = 7;
        records[0].sel_anchor = Some((1, 2));
        records[0].extra = vec![CaretRecord {
            line: 5,
            col: 6,
            anchor: Some((4, 1)),
        }];
        records[0].raw_mode = true;
        records[0].wrap = true;
        records[0].modified = true;
        records[0].buffer = Some(vec![String::new()]);
        records.swap(0, 1);
        assert!(write_state(&path, &records, &mut saved).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes,
            serde_json::to_vec(&StoredWindows { windows: records }).unwrap()
        );
        let restored: StoredWindows = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.windows[0].path, "b");
        assert_eq!(restored.windows[1].scroll, 41.5);
        assert_eq!(restored.windows[1].extra[0].anchor, Some((4, 1)));
        assert!(write_state(&path, &[], &mut saved).unwrap());
        let restored: StoredWindows =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(restored.windows.is_empty());
        assert!(!write_state(&path, &[], &mut saved).unwrap());
    }

    #[test]
    fn same_bytes_at_a_new_destination_are_saved() {
        let dir = StateDir::new();
        let mut saved = None;
        assert!(write_state(&dir.path(), &[], &mut saved).unwrap());
        let other = dir.0.join("other.json");
        assert!(write_state(&other, &[], &mut saved).unwrap());
        assert_eq!(
            std::fs::read(other).unwrap(),
            std::fs::read(dir.path()).unwrap()
        );
    }

    #[test]
    fn failed_atomic_replace_retries_identical_state() {
        let dir = StateDir::new();
        let path = dir.path();
        std::fs::create_dir(&path).unwrap();
        let mut saved = None;
        assert!(write_state(&path, &[], &mut saved).is_err());
        assert!(saved.is_none());
        std::fs::remove_dir(&path).unwrap();
        assert!(write_state(&path, &[], &mut saved).unwrap());
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::create_dir(&tmp).unwrap();
        let records = [RestoreRecord::default()];
        assert!(write_state(&path, &records, &mut saved).is_err());
        assert!(saved.is_none());
        std::fs::remove_dir(&tmp).unwrap();
        assert!(write_state(&path, &records, &mut saved).unwrap());
    }

    #[test]
    fn replacement_during_save_is_not_cached_as_our_snapshot() {
        let dir = StateDir::new();
        let path = dir.path();
        let mut saved = None;
        assert!(write_state_after_rename(&path, &[], &mut saved, || {
            let replacement = dir.0.join("replacement.json");
            std::fs::write(&replacement, b"{\"windows\":{}}").unwrap();
            std::fs::rename(replacement, &path).unwrap();
        })
        .unwrap());
        assert!(saved.is_none());
        assert!(write_state(&path, &[], &mut saved).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"{\"windows\":[]}");
    }

    #[test]
    fn in_place_overwrite_during_save_is_not_cached_as_our_snapshot() {
        let dir = StateDir::new();
        let path = dir.path();
        let mut saved = None;
        assert!(write_state_after_rename(&path, &[], &mut saved, || {
            std::fs::write(&path, b"{\"windows\":{}}").unwrap();
            // Deliberately change mtime so this regression is independent of
            // the temporary filesystem's clock precision.
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                .unwrap();
        })
        .unwrap());
        assert!(saved.is_none());
        assert!(write_state(&path, &[], &mut saved).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"{\"windows\":[]}");
    }

    #[test]
    fn external_deletion_or_replacement_invalidates_the_cache() {
        let dir = StateDir::new();
        let path = dir.path();
        let mut saved = None;
        assert!(write_state(&path, &[], &mut saved).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(write_state(&path, &[], &mut saved).unwrap());
        let replacement = dir.0.join("replacement.json");
        std::fs::write(&replacement, "{\"windows\":0}").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(write_state(&path, &[], &mut saved).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"{\"windows\":[]}");
    }

    #[test]
    fn dirty_empty_buffer_wins_over_existing_disk_text() {
        let record = RestoreRecord {
            modified: true,
            buffer: Some(vec![String::new()]),
            ..Default::default()
        };
        let (text, lines, missing) = source_for_restore(&record, Some("old text".into())).unwrap();
        assert_eq!(text, "");
        assert_eq!(lines.as_slice(), &[String::new()]);
        assert!(!missing);
    }

    #[test]
    fn missing_clean_source_is_not_restored_as_empty() {
        let record = RestoreRecord::default();
        assert!(source_for_restore(&record, None).is_none());
    }

    #[test]
    fn source_removed_after_open_keeps_the_last_buffer_as_a_draft() {
        let record = RestoreRecord {
            buffer: Some(vec!["last known text".into()]),
            ..Default::default()
        };
        let (text, _, missing) = source_for_restore(&record, None).unwrap();
        assert_eq!(text, "last known text");
        assert!(missing);
    }

    #[test]
    fn missing_dirty_source_keeps_the_saved_buffer() {
        let record = RestoreRecord {
            modified: true,
            buffer: Some(vec!["kept".into(), "buffer".into()]),
            ..Default::default()
        };
        let (text, _, missing) = source_for_restore(&record, None).unwrap();
        assert_eq!(text, "kept\nbuffer");
        assert!(missing);
    }

    #[test]
    fn restore_record_round_trips_mode_frame_scroll_and_carets() {
        let record = RestoreRecord {
            path: "/tmp/readme.md".into(),
            is_md_doc: true,
            raw_mode: true,
            modified: true,
            buffer: Some(vec!["hello".into()]),
            cur_line: 2,
            cur_col: 3,
            sel_anchor: Some((1, 1)),
            extra: vec![CaretRecord {
                line: 4,
                col: 5,
                anchor: Some((4, 1)),
            }],
            scroll: 12.5,
            h_scroll: 7.0,
            wrap: true,
            frame: Some(FrameRecord {
                x: -20,
                y: 40,
                width: 800,
                height: 600,
            }),
        };
        let bytes = serde_json::to_vec(&record).unwrap();
        let got: RestoreRecord = serde_json::from_slice(&bytes).unwrap();
        assert!(got.raw_mode && got.wrap && got.modified);
        assert_eq!(got.sel_anchor, Some((1, 1)));
        assert_eq!(got.extra[0].anchor, Some((4, 1)));
        assert_eq!(got.frame.unwrap().width, 800);
        assert_eq!(got.scroll, 12.5);
    }

    #[test]
    fn document_scroll_clamps_at_both_edges() {
        assert_eq!(clamped_document_scroll(10.0, -50.0, 500.0, 100.0), 0.0);
        assert_eq!(clamped_document_scroll(390.0, 50.0, 500.0, 100.0), 400.0);
        assert_eq!(clamped_document_scroll(5.0, 10.0, 80.0, 100.0), 0.0);
    }
}
