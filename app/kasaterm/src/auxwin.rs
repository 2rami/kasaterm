//! Detached document windows.
//!
//! Each window owns the same `MarkdownPane` used by an in-pane preview and a
//! small `GpuRenderer`. Moving the pane instead of rebuilding it is what keeps
//! an unsaved buffer, selections, undo history, view mode, and scroll position
//! intact when the user presses "별도창으로".
use super::*;

const TITLE_ROW_H: f32 = 42.0;
const TOOLBAR_ROW_H: f32 = 40.0;
const VIEW_HEADER_H: f32 = TITLE_ROW_H + TOOLBAR_ROW_H;
const EDIT_HEADER_H: f32 = TITLE_ROW_H + TOOLBAR_ROW_H;
const FIND_ROW_H: f32 = 54.0;
const FIND_REPLACE_ROW_H: f32 = 84.0;
const BODY_PAD: f32 = 6.0;
const OUTLINE_W: f32 = 244.0;
const OUTLINE_ROW_H: f32 = 26.0;
const OUTLINE_RAIL_MIN_W: f32 = 680.0;
const AUX_FONT_SCALE_MIN: f32 = 0.75;
const AUX_FONT_SCALE_MAX: f32 = 1.60;
const AUX_FONT_SCALE_STEP: f32 = 0.10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderButton {
    Open,
    View,
    Edit,
    Find,
    Outline,
    Wrap,
    ZoomOut,
    ZoomIn,
    Save,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewerPromptAction {
    Close,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewerPromptButton {
    Cancel,
    Discard,
    Save,
}

#[derive(Clone, Copy)]
struct ScrollbarGeometry {
    x: f32,
    track_y: f32,
    track_h: f32,
    thumb_y: f32,
    thumb_h: f32,
    max_scroll: f32,
}

#[derive(Clone)]
struct OutlineEntry {
    block: usize,
    source_line: usize,
    level: u8,
    title: String,
}

#[derive(Clone)]
struct RenderedFindTarget {
    block: usize,
    ordinal: usize,
    source_line: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct FrameRecord {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
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
    #[serde(default)]
    font_scale: Option<f32>,
    #[serde(default)]
    outline_open: Option<bool>,
    #[serde(default)]
    outline_scroll: f32,
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
    /// 세션 복원이 되살린 별도창 터미널 pane — 트리에 안 꽂고 창만 기다린다.
    /// `restore_session_state` 는 event loop 없이 돌아 창을 못 만들기 때문에
    /// 문서와 같은 길로 다음 틱의 `flush_aux_opens` 에서 연다.
    Terminal {
        pane_id: String,
        home_window: usize,
        frame: Option<FrameRecord>,
    },
}

/// All detached-document state lives behind this one App field. The launch
/// queue is included so an odoc/socket event received before `resumed` cannot
/// lose the file merely because winit has not supplied an ActiveEventLoop yet.
pub(crate) struct AuxWindows {
    pub(crate) windows: Vec<AuxWindow>,
    /// 별도 OS 창으로 뗀 터미널 pane(auxterm.rs). 문서 창과 달리 내용을 안 들고
    /// `pane_id` 만 든다 — PTY 와 화면은 App.pty / App.ws 에 그대로 산다.
    pub(crate) terminals: Vec<crate::auxterm::AuxTerminal>,
    /// 사이드바 배치도의 「별도창」 칸 명중 영역(방, pane, rect) — 렌더가 채운다.
    pub(crate) undock_hits: Vec<(usize, String, (f32, f32, f32, f32))>,
    pending: Vec<PendingAuxOpen>,
    unopened: Vec<RestoreRecord>,
    saved: std::cell::RefCell<Option<SavedState>>,
    viewer_only: bool,
    viewer_quit_requested: bool,
    viewer_message: Option<String>,
    focus_on_open: std::collections::HashSet<String>,
}

impl AuxWindows {
    pub(crate) fn load(viewer_only: bool) -> Self {
        let pending = state_path(viewer_only)
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
            terminals: Vec::new(),
            undock_hits: Vec::new(),
            pending,
            unopened: Vec::new(),
            saved: Default::default(),
            viewer_only,
            viewer_quit_requested: false,
            viewer_message: None,
            focus_on_open: std::collections::HashSet::new(),
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
    cursor_seen: bool,
    selecting: bool,
    focused: bool,
    preedit: String,
    last_title: String,
    md_content_h: f32,
    font_scale: f32,
    header_hits: Vec<(HeaderButton, (f32, f32, f32, f32))>,
    find_hits: Vec<(FindBtn, (f32, f32, f32, f32))>,
    viewer_prompt: Option<ViewerPromptAction>,
    viewer_prompt_hits: Vec<(ViewerPromptButton, (f32, f32, f32, f32))>,
    scrollbar: Option<ScrollbarGeometry>,
    scrollbar_drag_offset: Option<f32>,
    outline_open: bool,
    outline_scroll: f32,
    outline_gen: u64,
    outline_entries: Vec<OutlineEntry>,
    outline_hits: Vec<(usize, (f32, f32, f32, f32))>,
    outline_rect: Option<(f32, f32, f32, f32)>,
    outline_content_h: f32,
    outline_active: Option<usize>,
    outline_scrollbar_drawn: bool,
    outline_scrollbar: Option<ScrollbarGeometry>,
    outline_scrollbar_drag_offset: Option<f32>,
    outline_layout_anchor: Option<usize>,
    view_find_targets: Vec<RenderedFindTarget>,
    view_find_revealed: Option<(u64, String, usize)>,
    viewer_style: bool,
    welcome: bool,
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

    fn base_header_height(&self) -> f32 {
        if self.editor.raw_mode {
            EDIT_HEADER_H
        } else {
            VIEW_HEADER_H
        }
    }

    fn find_row_height(&self) -> f32 {
        self.editor.find.as_ref().map_or(0.0, |find| {
            if self.editor.raw_mode && find.replacing {
                FIND_REPLACE_ROW_H
            } else {
                FIND_ROW_H
            }
        })
    }

    fn chrome_height(&self) -> f32 {
        self.base_header_height() + self.find_row_height()
    }

    fn outline_overlays(&self) -> bool {
        self.logical_size().0 < OUTLINE_RAIL_MIN_W
    }

    fn outline_width(&self) -> f32 {
        let width = self.logical_size().0;
        if self.outline_overlays() {
            OUTLINE_W.min((width - BODY_PAD * 4.0).max(200.0))
        } else {
            (width * 0.23)
                .clamp(168.0, OUTLINE_W)
                .min((width - BODY_PAD * 2.0).max(1.0))
        }
    }

    fn editor_origin_x(&self) -> f32 {
        if self.outline_open && !self.outline_overlays() {
            self.outline_width() + 1.0
        } else {
            0.0
        }
    }

    fn toolbar_background(&self) -> [u8; 4] {
        if self.viewer_style {
            crate::theme::bg()
        } else {
            crate::theme::surface()
        }
    }

    fn body_box(&self) -> (f32, f32, f32, f32) {
        let (w, h) = self.logical_size();
        let chrome_h = self.chrome_height();
        let editor_x = self.editor_origin_x();
        (
            editor_x + BODY_PAD,
            chrome_h,
            (w - editor_x - BODY_PAD * 2.0).max(1.0),
            (h - chrome_h - BODY_PAD).max(1.0),
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
            font_scale: Some(self.font_scale),
            outline_open: Some(self.outline_open),
            outline_scroll: self.outline_scroll,
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
        let mut find_reveal: Option<((u64, String, usize), f32)> = None;
        let mut layout_reveal: Option<f32> = None;
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
            self.md_content_h = self.gpu.draw_raw_editor(
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
            self.find_hits.clear();
        } else {
            self.find_hits.clear();
            for image in &self.editor.doc.images {
                if !self.gpu.has_image(&image.key) {
                    self.gpu
                        .upload_image(&image.key, &image.rgba, image.w, image.h);
                }
            }
            let find_request = self.editor.find.as_ref().and_then(|find| {
                self.view_find_targets.get(find.idx).map(|target| {
                    (find.query.clone(), target.block, target.ordinal, find.idx)
                })
            });
            let find_draw = find_request.as_ref().map(|(query, block, ordinal, index)| {
                let key = (self.editor.doc.gen, query.clone(), *index);
                let target_block = if self.view_find_revealed.as_ref() == Some(&key) {
                    usize::MAX
                } else {
                    *block
                };
                (query.as_str(), target_block, *ordinal)
            });
            self.md_content_h = self.gpu.draw_markdown_with_find(
                &self.editor.doc.blocks,
                self.editor.doc.gen,
                x,
                y,
                bw,
                bh,
                self.editor.scroll,
                None,
                find_draw,
                false,
            );
            if let Some((query, block, _, index)) = find_request {
                let key = (self.editor.doc.gen, query, index);
                if self.view_find_revealed.as_ref() != Some(&key) {
                    let target_y = self
                        .gpu
                        .md_find_target_y
                        .or_else(|| self.gpu.md_block_ys.get(block).copied());
                    if let Some(target_y) = target_y {
                        let wanted = clamped_document_scroll(
                            0.0,
                            (target_y - bh * 0.35).max(0.0),
                            self.md_content_h.max(bh),
                            bh,
                        );
                        find_reveal = Some((key, wanted));
                    }
                }
            } else {
                self.view_find_revealed = None;
            }
            if let Some(line) = self.outline_layout_anchor.take() {
                let block = self
                    .editor
                    .doc
                    .block_lines
                    .partition_point(|&source| source <= line)
                    .saturating_sub(1);
                if let Some(target_y) = self.gpu.md_block_ys.get(block).copied() {
                    layout_reveal = Some(clamped_document_scroll(
                        0.0,
                        target_y,
                        self.md_content_h.max(bh),
                        bh,
                    ));
                }
            }
        }
        self.draw_find_row(w, cursor_on);
        self.refresh_outline_cache();
        let outline_scrollbar = self.outline_scrollbar_visible(y, bh);
        self.draw_scroll_indicator(x, y, bw, bh, !outline_scrollbar);
        self.draw_outline(x, y, bw, bh, outline_scrollbar);
        if self.viewer_prompt.is_some() {
            self.draw_viewer_prompt(w, h);
        } else {
            self.viewer_prompt_hits.clear();
        }
        let _ = self.gpu.render(&[], scale, 0.0, true);
        self.dirty = false;
        if let Some(scroll) = layout_reveal {
            if (self.editor.scroll - scroll).abs() > 0.5 {
                self.editor.scroll = scroll;
                self.dirty = true;
                self.window.request_redraw();
            }
        } else if let Some((key, scroll)) = find_reveal {
            self.view_find_revealed = Some(key);
            if (self.editor.scroll - scroll).abs() > 0.5 {
                self.editor.scroll = scroll;
                self.dirty = true;
                self.window.request_redraw();
            }
        }
    }

    fn draw_find_row(&mut self, width: f32, cursor_on: bool) {
        let Some(find) = self.editor.find.as_ref() else {
            self.find_hits.clear();
            return;
        };
        let y = self.base_header_height();
        let h = self.find_row_height();
        let editor_x = self.editor_origin_x();
        let editor_w = (width - editor_x).max(1.0);
        self.gpu
            .rect(editor_x, y, editor_w, h, self.toolbar_background());
        self.gpu
            .rect(editor_x, y + h - 1.0, editor_w, 1.0, crate::theme::border());
        self.find_hits = App::draw_find_bar_with_options(
            &mut self.gpu,
            find,
            editor_x + BODY_PAD,
            y,
            (editor_w - BODY_PAD * 2.0).max(1.0),
            &self.preedit,
            cursor_on,
            self.cursor_px,
            self.editor.raw_mode,
        );
    }

    fn draw_header(&mut self, width: f32) {
        if !self.editor.raw_mode {
            self.draw_view_header(width);
            return;
        }
        const INFO_H: f32 = TITLE_ROW_H;
        const CONTROL_H: f32 = 32.0;
        const GAP: f32 = 4.0;
        self.header_hits.clear();
        let editor_x = self.editor_origin_x();
        let editor_w = (width - editor_x).max(1.0);
        let toolbar_bg = self.toolbar_background();
        self.gpu.rect(editor_x, 0.0, editor_w, EDIT_HEADER_H, toolbar_bg);
        self.gpu.rect(
            editor_x,
            INFO_H,
            editor_w,
            EDIT_HEADER_H - INFO_H,
            toolbar_bg,
        );
        self.gpu
            .rect(editor_x, INFO_H, editor_w, 1.0, crate::theme::border());
        self.gpu
            .rect(editor_x, EDIT_HEADER_H - 1.0, editor_w, 1.0, crate::theme::border());

        let compact = editor_w < 560.0;
        let control_y = INFO_H + 4.0;
        let mut hovered = None;

        if self.welcome {
            let open_label = if compact { "열기" } else { "문서 열기" };
            let open_w = self.gpu.measure_chrome_text(open_label, 12.0, true) + 22.0;
            let open_rect = (width - open_w - 10.0, control_y, open_w, CONTROL_H);
            if self.paint_header_button(
                HeaderButton::Open,
                open_rect,
                open_label,
                false,
                true,
                true,
            ) {
                hovered = Some(HeaderButton::Open);
            }
            self.gpu.queue_icon(
                "file-text",
                editor_x + 12.0,
                12.0,
                16.0,
                if self.focused {
                    crate::theme::text_dim()
                } else {
                    crate::theme::text_mute()
                },
            );
            self.gpu.draw_text(
                editor_x + 36.0,
                10.0,
                "Kasaterm Viewer",
                gpu::DrawOpts {
                    font_size: 13.0,
                    color: crate::theme::text(),
                    bold: true,
                    italic: false,
                },
            );
            let message = self
                .status
                .as_deref()
                .unwrap_or("Cmd+O 또는 파일을 끌어놓아 문서를 여세요");
            let detail = hovered
                .map(|button| self.header_tooltip(button))
                .unwrap_or(message);
            let color = if message.contains("못했") || message.contains("실패") {
                crate::theme::danger()
            } else {
                crate::theme::text_dim()
            };
            let available = (open_rect.0 - editor_x - 22.0).max(0.0);
            let clipped = crate::screenread::clip_px(&mut self.gpu, detail, 11.0, false, available);
            self.gpu.draw_text(
                editor_x + 12.0,
                control_y + 7.0,
                &clipped,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color,
                    bold: false,
                    italic: false,
                },
            );
            return;
        }

        let save_label = if self.editor.modified {
            if compact {
                "저장"
            } else {
                "변경 내용 저장"
            }
        } else {
            "저장됨"
        };
        let save_w = self
            .gpu
            .measure_chrome_text(save_label, 12.0, self.editor.modified)
            + 22.0;
        let save_rect = (width - save_w - 10.0, 8.0, save_w, CONTROL_H);
        if self.paint_header_button(
            HeaderButton::Save,
            save_rect,
            save_label,
            false,
            self.editor.modified,
            self.editor.modified,
        ) {
            hovered = Some(HeaderButton::Save);
        }

        let centered_title = self.viewer_style && !compact;
        if !centered_title {
            self.gpu.queue_icon(
                "file-text",
                editor_x + 12.0,
                11.0,
                16.0,
                if self.focused {
                    crate::theme::text_dim()
                } else {
                    crate::theme::text_mute()
                },
            );
        }
        let name = std::path::Path::new(&self.editor.doc.path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("새 문서");
        let left_bound = editor_x + if centered_title { 12.0 } else { 36.0 };
        let right_bound = save_rect.0 - 12.0;
        let center_x = editor_x + editor_w * 0.5;
        let title_w = if centered_title {
            ((center_x - left_bound).min(right_bound - center_x) * 2.0).max(0.0)
        } else {
            (right_bound - left_bound).max(0.0)
        };
        let title = crate::screenread::clip_px(&mut self.gpu, name, 13.0, true, title_w);
        let title_x = if centered_title {
            center_x - self.gpu.measure_chrome_text(&title, 13.0, true) * 0.5
        } else {
            left_bound + if self.editor.modified { 7.0 } else { 0.0 }
        };
        if self.editor.modified {
            crate::round_rect(
                &mut self.gpu,
                title_x - 9.0,
                17.0,
                5.0,
                5.0,
                2.5,
                crate::theme::accent(),
            );
        }
        self.gpu.draw_text(
            title_x,
            9.0,
            &title,
            gpu::DrawOpts {
                font_size: 13.0,
                color: crate::theme::text(),
                bold: true,
                italic: false,
            },
        );

        let mut left_x = editor_x + 10.0;
        if self.editor.is_md_doc {
            let segment_w = if compact { 43.0 } else { 48.0 };
            if !self.viewer_style {
                crate::round_rect(
                    &mut self.gpu,
                    left_x,
                    control_y,
                    segment_w * 2.0,
                    CONTROL_H,
                    crate::theme::radius_sm(),
                    crate::theme::surface(),
                );
            }
            for (kind, label, active) in [
                (HeaderButton::View, "보기", !self.editor.raw_mode),
                (HeaderButton::Edit, "편집", self.editor.raw_mode),
            ] {
                let rect = (left_x, control_y, segment_w, CONTROL_H);
                if self.paint_header_button(kind, rect, label, active, false, true) {
                    hovered = Some(kind);
                }
                left_x += segment_w;
            }
            left_x += 8.0;
        }

        let small_label = editor_w < 440.0;
        let find_label = "찾기";
        let wrap_label = if small_label { "줄" } else { "줄바꿈" };
        let find_w = self.gpu.measure_chrome_text(find_label, 12.0, false) + 18.0;
        let wrap_w = self.gpu.measure_chrome_text(wrap_label, 12.0, false) + 18.0;
        let outline_w = 28.0;
        let zoom_w = 28.0;
        let zoom_label_w = if compact { 0.0 } else { 48.0 };
        let tools_w = find_w
            + GAP
            + outline_w
            + 9.0
            + wrap_w
            + 9.0
            + zoom_w * 2.0
            + zoom_label_w
            + GAP;
        let mut tool_x = (width - 10.0 - tools_w).max(left_x);
        let find_active = self.editor.find.is_some();
        let find_rect = (tool_x, control_y, find_w, CONTROL_H);
        if self.paint_header_button(
            HeaderButton::Find,
            find_rect,
            find_label,
            find_active,
            false,
            true,
        ) {
            hovered = Some(HeaderButton::Find);
        }
        tool_x += find_w + GAP;
        let outline_rect = (tool_x, control_y, outline_w, CONTROL_H);
        if self.paint_header_icon_button(
            HeaderButton::Outline,
            outline_rect,
            "panel-left",
            self.outline_open,
            true,
        ) {
            hovered = Some(HeaderButton::Outline);
        }
        tool_x += outline_w + 4.0;
        self.gpu.rect(
            tool_x,
            control_y + 6.0,
            1.0,
            20.0,
            crate::theme::border(),
        );
        tool_x += 5.0;
        let wrap_rect = (tool_x, control_y, wrap_w, CONTROL_H);
        if self.paint_header_button(
            HeaderButton::Wrap,
            wrap_rect,
            wrap_label,
            self.editor.raw_mode && self.editor.wrap,
            false,
            self.editor.raw_mode,
        ) {
            hovered = Some(HeaderButton::Wrap);
        }
        tool_x += wrap_w + 3.0;
        self.gpu.rect(
            tool_x,
            control_y + 6.0,
            1.0,
            20.0,
            crate::theme::border(),
        );
        tool_x += 5.0;
        let out_rect = (tool_x, control_y, zoom_w, CONTROL_H);
        if self.paint_header_icon_button(
            HeaderButton::ZoomOut,
            out_rect,
            "minus",
            false,
            self.font_scale > AUX_FONT_SCALE_MIN + 0.01,
        ) {
            hovered = Some(HeaderButton::ZoomOut);
        }
        tool_x += zoom_w;
        if zoom_label_w > 0.0 {
            let zoom = format!("{}%", (self.font_scale * 100.0).round() as i32);
            let tw = self.gpu.measure_chrome_text(&zoom, 11.0, false);
            self.gpu.draw_text(
                tool_x + (zoom_label_w - tw) * 0.5,
                control_y + 8.0,
                &zoom,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: crate::theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
            tool_x += zoom_label_w;
        }
        let in_rect = (tool_x, control_y, zoom_w, CONTROL_H);
        if self.paint_header_icon_button(
            HeaderButton::ZoomIn,
            in_rect,
            "plus",
            false,
            self.font_scale < AUX_FONT_SCALE_MAX - 0.01,
        ) {
            hovered = Some(HeaderButton::ZoomIn);
        }

        let detail = hovered
            .map(|button| self.header_tooltip(button))
            .or(self.status.as_deref());
        if let Some(detail) = detail {
            let available = title_w;
            let detail = crate::screenread::clip_px(&mut self.gpu, detail, 10.5, false, available);
            let color = if detail.contains("실패") || detail.contains("찾지 못") {
                crate::theme::danger()
            } else if detail.contains("저장했") {
                crate::theme::success()
            } else {
                crate::theme::text_dim()
            };
            self.gpu.draw_text(
                title_x,
                26.0,
                &detail,
                gpu::DrawOpts {
                    font_size: 10.5,
                    color,
                    bold: false,
                    italic: false,
                },
            );
        }
    }

    fn draw_view_header(&mut self, width: f32) {
        const CONTROL_H: f32 = 32.0;
        const GAP: f32 = 4.0;
        self.header_hits.clear();
        let editor_x = self.editor_origin_x();
        let editor_w = (width - editor_x).max(1.0);
        let toolbar_bg = self.toolbar_background();
        self.gpu.rect(editor_x, 0.0, editor_w, VIEW_HEADER_H, toolbar_bg);
        self.gpu
            .rect(editor_x, TITLE_ROW_H, editor_w, 1.0, crate::theme::border());
        self.gpu.rect(
            editor_x,
            VIEW_HEADER_H - 1.0,
            editor_w,
            1.0,
            crate::theme::border(),
        );
        let title_y = 5.0;
        let control_y = TITLE_ROW_H + 4.0;
        let compact = editor_w < 520.0;
        if self.welcome {
            let label = if compact { "열기" } else { "문서 열기" };
            let button_w = self.gpu.measure_chrome_text(label, 12.0, true) + 22.0;
            let rect = (width - button_w - 10.0, control_y, button_w, CONTROL_H);
            self.paint_header_button(HeaderButton::Open, rect, label, false, true, true);
            self.gpu
                .queue_icon("file-text", editor_x + 12.0, 13.0, 16.0, crate::theme::text_dim());
            let text = self
                .status
                .as_deref()
                .unwrap_or("Cmd+O 또는 파일을 끌어놓아 문서를 여세요");
            let text = crate::screenread::clip_px(
                &mut self.gpu,
                text,
                12.0,
                false,
                (rect.0 - editor_x - 42.0).max(0.0),
            );
            self.gpu.draw_text(
                editor_x + 36.0,
                12.0,
                &text,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: if text.contains("실패") || text.contains("못했") {
                        crate::theme::danger()
                    } else {
                        crate::theme::text_dim()
                    },
                    bold: false,
                    italic: false,
                },
            );
            return;
        }

        let save_label = if self.editor.modified {
            if compact { "저장" } else { "변경 내용 저장" }
        } else {
            "저장됨"
        };
        let save_w = self
            .gpu
            .measure_chrome_text(save_label, 12.0, self.editor.modified)
            + 20.0;
        let right = width - 10.0;
        let save_rect = (right - save_w, title_y, save_w, CONTROL_H);
        self.paint_header_button(
            HeaderButton::Save,
            save_rect,
            save_label,
            false,
            self.editor.modified,
            self.editor.modified,
        );
        let mut tool_right = width - 10.0;

        let outline_w = self.gpu.measure_chrome_text("목차", 12.0, false) + 18.0;
        let outline_rect = (tool_right - outline_w, control_y, outline_w, CONTROL_H);
        self.paint_header_button(
            HeaderButton::Outline,
            outline_rect,
            "목차",
            self.outline_open,
            false,
            true,
        );
        tool_right = outline_rect.0 - GAP;

        let find_label = "찾기";
        let find_w = self.gpu.measure_chrome_text(find_label, 12.0, false) + 18.0;
        let find_rect = (tool_right - find_w, control_y, find_w, CONTROL_H);
        self.paint_header_button(
            HeaderButton::Find,
            find_rect,
            find_label,
            self.editor.find.is_some(),
            false,
            true,
        );
        tool_right = find_rect.0 - GAP;

        let segment_w = if compact { 40.0 } else { 44.0 };
        let segment_x = (tool_right - segment_w * 2.0).max(editor_x + 10.0);
        if !self.viewer_style {
            crate::round_rect(
                &mut self.gpu,
                segment_x,
                control_y,
                segment_w * 2.0,
                CONTROL_H,
                crate::theme::radius_sm(),
                crate::theme::panel_bg(),
            );
        }
        for (offset, kind, label, active) in [
            (0.0, HeaderButton::View, "보기", true),
            (segment_w, HeaderButton::Edit, "편집", false),
        ] {
            self.paint_header_button(
                kind,
                (segment_x + offset, control_y, segment_w, CONTROL_H),
                label,
                active,
                false,
                true,
            );
        }

        let centered_title = self.viewer_style && !compact;
        if !centered_title {
            self.gpu.queue_icon(
                "file-text",
                editor_x + 12.0,
                13.0,
                16.0,
                if self.focused {
                    crate::theme::text_dim()
                } else {
                    crate::theme::text_mute()
                },
            );
        }
        let name = std::path::Path::new(&self.editor.doc.path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("새 문서");
        let center_x = editor_x + editor_w * 0.5;
        let left_bound = editor_x + if centered_title { 12.0 } else { 36.0 };
        let right_bound = save_rect.0 - 12.0;
        let available = if centered_title {
            ((center_x - left_bound).min(right_bound - center_x) * 2.0).max(0.0)
        } else {
            (right_bound - left_bound).max(0.0)
        };
        let title = crate::screenread::clip_px(&mut self.gpu, name, 12.5, true, available);
        let title_x = if centered_title {
            center_x - self.gpu.measure_chrome_text(&title, 12.5, true) * 0.5
        } else {
            left_bound + if self.editor.modified { 7.0 } else { 0.0 }
        };
        if self.editor.modified {
            crate::round_rect(
                &mut self.gpu,
                title_x - 9.0,
                17.0,
                5.0,
                5.0,
                2.5,
                crate::theme::accent(),
            );
        }
        self.gpu.draw_text(
            title_x,
            12.0,
            &title,
            gpu::DrawOpts {
                font_size: 12.5,
                color: crate::theme::text(),
                bold: true,
                italic: false,
            },
        );
    }

    fn paint_header_button(
        &mut self,
        kind: HeaderButton,
        rect: (f32, f32, f32, f32),
        label: &str,
        active: bool,
        primary: bool,
        enabled: bool,
    ) -> bool {
        let hot = enabled && hit(self.cursor_px, rect);
        let fill = if primary {
            crate::theme::accent()
        } else if active {
            crate::theme::surface_active()
        } else if hot {
            crate::theme::surface_hover()
        } else {
            crate::theme::surface()
        };
        if !self.viewer_style || primary || active || hot {
            crate::round_rect(
                &mut self.gpu,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                crate::theme::radius_sm(),
                fill,
            );
        }
        let tw = self.gpu.measure_chrome_text(label, 12.0, primary);
        self.gpu.draw_text(
            rect.0 + (rect.2 - tw) * 0.5,
            rect.1 + 7.0,
            label,
            gpu::DrawOpts {
                font_size: 12.0,
                color: if primary {
                    crate::theme::foreground_on(crate::theme::accent())
                } else if !enabled || !self.focused {
                    crate::theme::text_mute()
                } else if active || hot {
                    crate::theme::text()
                } else {
                    crate::theme::text_dim()
                },
                bold: primary || active,
                italic: false,
            },
        );
        if enabled {
            self.header_hits.push((kind, rect));
        }
        hot
    }

    fn paint_header_icon_button(
        &mut self,
        kind: HeaderButton,
        rect: (f32, f32, f32, f32),
        icon: &str,
        active: bool,
        enabled: bool,
    ) -> bool {
        let hot = enabled && hit(self.cursor_px, rect);
        if !self.viewer_style || active || hot {
            crate::round_rect(
                &mut self.gpu,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                crate::theme::radius_sm(),
                if active {
                    crate::theme::surface_active()
                } else if hot {
                    crate::theme::surface_hover()
                } else {
                    crate::theme::surface()
                },
            );
        }
        self.gpu.queue_icon(
            icon,
            rect.0 + (rect.2 - 14.0) * 0.5,
            rect.1 + (rect.3 - 14.0) * 0.5,
            14.0,
            if !enabled || !self.focused {
                crate::theme::text_mute()
            } else if active || hot {
                crate::theme::text()
            } else {
                crate::theme::text_dim()
            },
        );
        if enabled {
            self.header_hits.push((kind, rect));
        }
        hot
    }

    fn header_tooltip(&self, button: HeaderButton) -> &'static str {
        match button {
            HeaderButton::Open => "문서 열기 (⌘O)",
            HeaderButton::View => "읽기 화면으로 전환",
            HeaderButton::Edit => "원문 편집으로 전환",
            HeaderButton::Find => "문서에서 찾기 (⌘F)",
            HeaderButton::Outline if self.outline_open => "목차 닫기 (Esc)",
            HeaderButton::Outline => "문서 목차 열기",
            HeaderButton::Wrap if self.editor.wrap => "긴 줄 줄바꿈 끄기",
            HeaderButton::Wrap => "긴 줄 줄바꿈 켜기",
            HeaderButton::ZoomOut => "보기 축소 (⌘−)",
            HeaderButton::ZoomIn => "보기 확대 (⌘+)",
            HeaderButton::Save => "변경 내용 저장 (⌘S)",
        }
    }

    fn draw_scroll_indicator(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        visible: bool,
    ) {
        let content = self.md_content_h.max(height);
        let max_scroll = (content - height).max(0.0);
        if !visible || max_scroll <= 1.0 || height < 48.0 {
            self.scrollbar = None;
            self.scrollbar_drag_offset = None;
            return;
        }
        let track_y = y + 8.0;
        let track_h = (height - 16.0).max(1.0);
        let thumb_h = (track_h * height / content).clamp(32.0, track_h);
        let thumb_y =
            track_y + (self.editor.scroll / max_scroll).clamp(0.0, 1.0) * (track_h - thumb_h);
        let track_x = x + width - 7.0;
        self.scrollbar = Some(ScrollbarGeometry {
            x: track_x,
            track_y,
            track_h,
            thumb_y,
            thumb_h,
            max_scroll,
        });
        let hot = self.cursor_px.0 >= track_x - 7.0
            && self.cursor_px.0 <= track_x + 7.0
            && self.cursor_px.1 >= y
            && self.cursor_px.1 <= y + height;
        let dragging = self.scrollbar_drag_offset.is_some();
        let visual_w = if dragging {
            8.0
        } else if hot {
            7.0
        } else {
            5.0
        };
        let alpha = if dragging {
            0xd0
        } else if hot {
            0x98
        } else {
            0x58
        };
        crate::round_rect(
            &mut self.gpu,
            track_x - visual_w * 0.5,
            thumb_y,
            visual_w,
            thumb_h,
            visual_w * 0.5,
            crate::theme::with_alpha(crate::theme::text_dim(), alpha),
        );
    }

    fn refresh_outline_cache(&mut self) {
        if self.outline_gen == self.editor.doc.gen {
            return;
        }
        self.outline_entries = document_outline(&self.editor.doc);
        self.outline_gen = self.editor.doc.gen;
        self.outline_scroll = self.outline_scroll.max(0.0);
        self.outline_content_h = self.outline_entries.len() as f32 * OUTLINE_ROW_H;
    }

    fn outline_scrollbar_visible(&self, y: f32, height: f32) -> bool {
        if self.outline_scrollbar_drag_offset.is_some() {
            return true;
        }
        if !self.outline_open || self.scrollbar_drag_offset.is_some() || self.outline_entries.is_empty() {
            return false;
        }
        if !self.cursor_seen {
            return false;
        }
        let overlay = self.outline_overlays();
        let panel = if overlay {
            (
                BODY_PAD,
                y + 6.0,
                self.outline_width(),
                (height - 12.0).max(1.0),
            )
        } else {
            (0.0, 0.0, self.outline_width(), self.logical_size().1)
        };
        let list_y = panel.1 + if overlay { 36.0 } else { TITLE_ROW_H + 8.0 };
        let list_h = (panel.1 + panel.3 - list_y - 8.0).max(1.0);
        self.outline_content_h > list_h && hit(self.cursor_px, panel)
    }

    fn current_source_line(&mut self, body_width: f32) -> usize {
        if !self.editor.raw_mode {
            if self.gpu.md_block_ys.is_empty() {
                return self.editor.cur_line;
            }
            let block = self
                .gpu
                .md_block_ys
                .partition_point(|&y| y <= self.editor.scroll + 1.0)
                .saturating_sub(1);
            return self.editor.doc.block_lines.get(block).copied().unwrap_or(0);
        }
        let (pad, line_h) = self.gpu.raw_editor_metrics();
        let wrap_cols = self.gpu.raw_editor_wrap_cols(
            body_width,
            self.editor.edit_lines.len(),
            self.editor.wrap,
        );
        let rows = crate::markdown::layout_rows(
            &self.editor.edit_lines,
            &self.editor.folds,
            wrap_cols,
        );
        let row = ((self.editor.scroll - pad) / line_h).floor().max(0.0) as usize;
        crate::markdown::row_at(&rows, row).0
    }

    fn draw_outline(
        &mut self,
        _x: f32,
        y: f32,
        width: f32,
        height: f32,
        show_scrollbar: bool,
    ) {
        self.outline_hits.clear();
        self.outline_rect = None;
        self.outline_content_h = 0.0;
        self.outline_active = None;
        self.outline_scrollbar_drawn = false;
        self.outline_scrollbar = None;
        if !self.outline_open || self.welcome || height < 80.0 {
            self.outline_scrollbar_drag_offset = None;
            return;
        }
        let (window_w, window_h) = self.logical_size();
        let overlay = self.outline_overlays();
        let panel_w = self.outline_width();
        let panel = if overlay {
            (BODY_PAD, y + 6.0, panel_w, (height - 12.0).max(1.0))
        } else {
            (0.0, 0.0, panel_w, window_h)
        };
        self.outline_rect = Some(panel);
        if overlay {
            crate::round_rect(
                &mut self.gpu,
                panel.0 - 1.0,
                panel.1 - 1.0,
                panel.2 + 2.0,
                panel.3 + 2.0,
                crate::theme::radius_md() + 1.0,
                crate::theme::border(),
            );
            crate::round_rect(
                &mut self.gpu,
                panel.0,
                panel.1,
                panel.2,
                panel.3,
                crate::theme::radius_md(),
                crate::theme::surface(),
            );
        } else {
            self.gpu.rect(panel.0, panel.1, panel.2, panel.3, crate::theme::surface());
            self.gpu.rect(
                panel.0 + panel.2,
                panel.1,
                1.0,
                panel.3,
                crate::theme::border(),
            );
            self.gpu.rect(
                panel.0,
                TITLE_ROW_H,
                panel.2,
                1.0,
                crate::theme::border(),
            );
        }
        self.gpu.draw_text(
            panel.0 + 12.0,
            panel.1 + 10.0,
            "목차",
            gpu::DrawOpts {
                font_size: 12.5,
                color: crate::theme::text(),
                bold: true,
                italic: false,
            },
        );
        let close = (panel.0 + panel.2 - 30.0, panel.1 + 5.0, 24.0, 24.0);
        let close_hot = hit(self.cursor_px, close);
        if close_hot {
            crate::round_rect(
                &mut self.gpu,
                close.0,
                close.1,
                close.2,
                close.3,
                crate::theme::radius_sm(),
                crate::theme::surface_hover(),
            );
        }
        self.gpu.queue_icon(
            "x",
            close.0 + 5.0,
            close.1 + 5.0,
            14.0,
            if close_hot {
                crate::theme::text()
            } else {
                crate::theme::text_dim()
            },
        );
        self.header_hits.push((HeaderButton::Outline, close));

        let list_y = panel.1 + if overlay { 36.0 } else { TITLE_ROW_H + 8.0 };
        let list_h = (panel.1 + panel.3 - list_y - 8.0).max(1.0);
        self.outline_content_h = self.outline_entries.len() as f32 * OUTLINE_ROW_H;
        let max_scroll = (self.outline_content_h - list_h).max(0.0);
        self.outline_scroll = self.outline_scroll.clamp(0.0, max_scroll);
        if self.outline_entries.is_empty() {
            self.gpu.draw_text(
                panel.0 + 12.0,
                list_y + 8.0,
                "제목이 없는 문서예요",
                gpu::DrawOpts {
                    font_size: 11.5,
                    color: crate::theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
            return;
        }

        let source_line = self.current_source_line(width);
        let active = self
            .outline_entries
            .partition_point(|entry| entry.source_line <= source_line)
            .checked_sub(1);
        self.outline_active = active;
        let mut tooltip: Option<(String, f32)> = None;
        self.gpu.push_clip(panel.0, list_y, panel.2, list_h);
        for (index, entry) in self.outline_entries.iter().enumerate() {
            let row_y = list_y + index as f32 * OUTLINE_ROW_H - self.outline_scroll;
            if row_y + OUTLINE_ROW_H <= list_y || row_y >= list_y + list_h {
                continue;
            }
            let rect = (panel.0 + 5.0, row_y, panel.2 - 10.0, OUTLINE_ROW_H);
            let hot = hit(self.cursor_px, rect);
            if active == Some(index) || hot {
                crate::round_rect(
                    &mut self.gpu,
                    rect.0,
                    rect.1 + 1.0,
                    rect.2,
                    rect.3 - 2.0,
                    crate::theme::radius_sm(),
                    if active == Some(index) {
                        crate::theme::surface_active()
                    } else {
                        crate::theme::surface_hover()
                    },
                );
            }
            let indent = entry.level.saturating_sub(1).min(4) as f32 * 11.0;
            let text_x = rect.0 + 8.0 + indent;
            let available = (rect.0 + rect.2 - text_x - 7.0).max(1.0);
            let clipped = crate::screenread::clip_px(
                &mut self.gpu,
                &entry.title,
                11.5,
                active == Some(index),
                available,
            );
            if hot
                && self
                    .gpu
                    .measure_chrome_text(&entry.title, 11.5, active == Some(index))
                    > available
            {
                tooltip = Some((entry.title.clone(), row_y));
            }
            self.gpu.draw_text(
                text_x,
                row_y + 7.0,
                &clipped,
                gpu::DrawOpts {
                    font_size: 11.5,
                    color: if active == Some(index) {
                        crate::theme::accent()
                    } else if hot {
                        crate::theme::text()
                    } else {
                        crate::theme::text_dim()
                    },
                    bold: active == Some(index),
                    italic: false,
                },
            );
            self.outline_hits.push((index, rect));
        }
        self.gpu.pop_clip();

        if max_scroll > 0.0 {
            let track_h = list_h;
            let thumb_h = (track_h * list_h / self.outline_content_h).clamp(24.0, track_h);
            let thumb_y = list_y + self.outline_scroll / max_scroll * (track_h - thumb_h);
            let track_x = panel.0 + panel.2 - 7.0;
            self.outline_scrollbar = Some(ScrollbarGeometry {
                x: track_x,
                track_y: list_y,
                track_h,
                thumb_y,
                thumb_h,
                max_scroll,
            });
            if show_scrollbar {
                self.outline_scrollbar_drawn = true;
                let visual_w = if self.outline_scrollbar_drag_offset.is_some() {
                    8.0
                } else {
                    7.0
                };
                crate::round_rect(
                    &mut self.gpu,
                    track_x - visual_w * 0.5,
                    thumb_y,
                    visual_w,
                    thumb_h,
                    visual_w * 0.5,
                    crate::theme::with_alpha(
                        crate::theme::text_dim(),
                        if self.outline_scrollbar_drag_offset.is_some() {
                            0xd0
                        } else {
                            0x98
                        },
                    ),
                );
            }
        } else {
            self.outline_scrollbar_drag_offset = None;
        }

        if let Some((title, row_y)) = tooltip {
            let max_w = (window_w - panel.0 - 12.0).max(panel.2);
            let tip_w = (self.gpu.measure_chrome_text(&title, 11.5, false) + 20.0)
                .min(max_w)
                .max(panel.2);
            let tip_y = (row_y + OUTLINE_ROW_H + 3.0)
                .min(y + height - 34.0)
                .max(y + 4.0);
            crate::round_rect(
                &mut self.gpu,
                panel.0,
                tip_y,
                tip_w,
                30.0,
                crate::theme::radius_sm(),
                crate::theme::surface_active(),
            );
            let title = crate::screenread::clip_px(
                &mut self.gpu,
                &title,
                11.5,
                false,
                tip_w - 20.0,
            );
            self.gpu.draw_text(
                panel.0 + 10.0,
                tip_y + 8.0,
                &title,
                gpu::DrawOpts {
                    font_size: 11.5,
                    color: crate::theme::text(),
                    bold: false,
                    italic: false,
                },
            );
        }
    }

    fn draw_viewer_prompt(&mut self, width: f32, height: f32) {
        self.viewer_prompt_hits.clear();
        self.gpu.rect(
            0.0,
            0.0,
            width,
            height,
            crate::theme::with_alpha([0, 0, 0, 255], 0x88),
        );
        let panel_w = (width - 24.0).min(460.0).max(280.0);
        let panel_h = 172.0;
        let x = (width - panel_w) * 0.5;
        let y = (height - panel_h) * 0.5;
        crate::round_rect(
            &mut self.gpu,
            x - 1.0,
            y - 1.0,
            panel_w + 2.0,
            panel_h + 2.0,
            crate::theme::radius_md() + 1.0,
            crate::theme::border(),
        );
        crate::round_rect(
            &mut self.gpu,
            x,
            y,
            panel_w,
            panel_h,
            crate::theme::radius_md(),
            crate::theme::surface(),
        );
        let name = std::path::Path::new(&self.editor.doc.path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("이 문서");
        let prompt = format!("{name}에 저장하지 않은 내용이 있어요");
        let prompt = crate::screenread::clip_px(&mut self.gpu, &prompt, 14.0, true, panel_w - 44.0);
        self.gpu.draw_text(
            x + 22.0,
            y + 24.0,
            &prompt,
            gpu::DrawOpts {
                font_size: 14.0,
                color: crate::theme::text(),
                bold: true,
                italic: false,
            },
        );
        let detail = self
            .status
            .as_deref()
            .filter(|message| message.contains("실패"))
            .unwrap_or("저장할까요? 저장 실패 시 창과 내용은 그대로 남습니다.");
        let detail = crate::screenread::clip_px(&mut self.gpu, detail, 12.0, false, panel_w - 44.0);
        self.gpu.draw_text(
            x + 22.0,
            y + 56.0,
            &detail,
            gpu::DrawOpts {
                font_size: 12.0,
                color: if detail.contains("실패") {
                    crate::theme::danger()
                } else {
                    crate::theme::text_dim()
                },
                bold: false,
                italic: false,
            },
        );
        let labels = [
            (ViewerPromptButton::Cancel, "취소"),
            (ViewerPromptButton::Discard, "저장 안 함"),
            (ViewerPromptButton::Save, "저장"),
        ];
        let button_h = 36.0;
        let gap = 8.0;
        let widths: Vec<f32> = labels
            .iter()
            .map(|(_, label)| self.gpu.measure_chrome_text(label, 12.0, true) + 24.0)
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + gap * 2.0;
        let mut button_x = x + panel_w - 20.0 - total;
        let button_y = y + panel_h - button_h - 20.0;
        for ((button, label), button_w) in labels.into_iter().zip(widths) {
            let rect = (button_x, button_y, button_w, button_h);
            let hot = hit(self.cursor_px, rect);
            let color = if button == ViewerPromptButton::Save {
                crate::theme::accent()
            } else if hot {
                crate::theme::surface_active()
            } else {
                crate::theme::surface_hover()
            };
            crate::round_rect(
                &mut self.gpu,
                button_x,
                button_y,
                button_w,
                button_h,
                crate::theme::radius_sm(),
                color,
            );
            self.gpu.draw_text(
                button_x + 12.0,
                button_y + 10.0,
                label,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: if button == ViewerPromptButton::Save {
                        crate::theme::foreground_on(crate::theme::accent())
                    } else {
                        crate::theme::text()
                    },
                    bold: true,
                    italic: false,
                },
            );
            self.viewer_prompt_hits.push((button, rect));
            button_x += button_w + gap;
        }
    }
}

fn state_path(viewer_only: bool) -> Option<std::path::PathBuf> {
    if viewer_only {
        if let Some(path) = std::env::var_os("KASATERM_VIEWER_STATE_FILE") {
            return Some(std::path::PathBuf::from(path));
        }
        return Some(kasa_socket::home_dir()?.join(".config/kasaterm/viewer-documents.json"));
    }
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

fn is_markdown_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mkd"
            )
        })
}

fn document_outline(doc: &MarkdownDoc) -> Vec<OutlineEntry> {
    doc.blocks
        .iter()
        .enumerate()
        .filter_map(|(block, item)| {
            let MdBlock::Heading { level, spans } = item else {
                return None;
            };
            let title = spans.iter().map(|span| span.text.as_str()).collect::<String>();
            let title = title.trim();
            (!title.is_empty()).then(|| OutlineEntry {
                block,
                source_line: doc.block_lines.get(block).copied().unwrap_or(0),
                level: *level,
                title: title.to_string(),
            })
        })
        .collect()
}

fn spans_plain(spans: &[MdSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

fn document_block_text(block: &MdBlock) -> String {
    match block {
        MdBlock::Heading { spans, .. }
        | MdBlock::Para { spans }
        | MdBlock::Quote { spans } => spans_plain(spans),
        MdBlock::ListItem {
            marker, spans, task, ..
        } => {
            let body = spans_plain(spans);
            if task.is_none() {
                format!("{marker} {body}")
            } else {
                body
            }
        }
        MdBlock::Callout {
            kind, spans, first, ..
        } => {
            let body = spans_plain(spans);
            if *first {
                format!("{} {body}", kind.face().1)
            } else {
                body
            }
        }
        MdBlock::Code { code, .. } => code.clone(),
        MdBlock::Meta { rows } => rows
            .iter()
            .map(|(key, value)| format!("{key} {value}"))
            .collect::<Vec<_>>()
            .join("\n"),
        MdBlock::Image { alt, key, w, h, .. } if key.is_empty() || *w == 0 || *h == 0 => {
            format!("이미지: {alt}")
        }
        MdBlock::Table { head, rows, .. } => head
            .iter()
            .chain(rows.iter().flat_map(|row| row.iter()))
            .map(|cell| spans_plain(cell))
            .collect::<Vec<_>>()
            .join(" "),
        MdBlock::Rule | MdBlock::Image { .. } => String::new(),
    }
}

fn rendered_find_targets(doc: &MarkdownDoc, query: &str) -> Vec<RenderedFindTarget> {
    if query.is_empty() {
        return Vec::new();
    }
    doc.blocks
        .iter()
        .enumerate()
        .flat_map(|(block, item)| {
            let text = document_block_text(item);
            let hits = crate::markdown::find_hits(std::slice::from_ref(&text), query);
            let source_line = doc.block_lines.get(block).copied().unwrap_or(0);
            hits.into_iter()
                .enumerate()
                .map(move |(ordinal, (_, start, end))| RenderedFindTarget {
                    block,
                    ordinal,
                    source_line,
                    start,
                    end,
                })
        })
        .collect()
}

fn default_outline_open(viewer_only: bool, is_markdown: bool, raw_mode: bool, width: f32) -> bool {
    viewer_only && is_markdown && !raw_mode && width >= OUTLINE_RAIL_MIN_W
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

pub(crate) fn create_untabbed(
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
        if let Some(index) = self
            .aux
            .windows
            .iter()
            .position(|aux| std::path::Path::new(&aux.editor.doc.path) == path.as_path())
        {
            if !self.aux.windows[index].editor.modified {
                match std::fs::read_to_string(&path) {
                    Ok(raw) if raw != self.aux.windows[index].editor.text_for_save() => {
                        let old = &self.aux.windows[index].editor;
                        let raw_mode = old.raw_mode;
                        let cur_line = old.cur_line;
                        let cur_col = old.cur_col;
                        let scroll = old.scroll;
                        let h_scroll = old.h_scroll;
                        let wrap = old.wrap;
                        let is_md = is_markdown_path(&path);
                        let lines = Arc::new(raw.split('\n').map(String::from).collect());
                        let mut editor =
                            make_editor(&path, &raw, lines, is_md, raw_mode || !is_md, false);
                        editor.cur_line = cur_line.min(editor.edit_lines.len().saturating_sub(1));
                        editor.cur_col =
                            cur_col.min(editor.edit_lines[editor.cur_line].chars().count());
                        editor.scroll = scroll.max(0.0);
                        editor.h_scroll = h_scroll.max(0.0);
                        editor.wrap = wrap;
                        let aux = &mut self.aux.windows[index];
                        aux.editor = editor;
                        aux.missing_source = false;
                        aux.status = Some("디스크의 최신 내용을 다시 읽었어요".to_string());
                        aux.dirty = true;
                        aux.window.request_redraw();
                        self.save_aux_windows_state();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let aux = &mut self.aux.windows[index];
                        aux.status = Some(format!("문서를 다시 읽지 못했어요: {error}"));
                        aux.dirty = true;
                        aux.window.request_redraw();
                    }
                }
            }
            if active {
                self.aux.windows[index].window.focus_window();
            }
            return;
        }
        if let Some(record_path) = self.aux.pending.iter().find_map(|pending| match pending {
            PendingAuxOpen::Restore(record)
                if std::path::Path::new(&record.path) == path.as_path() =>
            {
                Some(record.path.clone())
            }
            _ => None,
        }) {
            if active {
                self.aux.focus_on_open.insert(record_path);
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

    /// 세션 복원이 살린 별도창 pane 을 다음 틱에 열도록 줄 세운다.
    pub(crate) fn queue_aux_terminal(
        &mut self,
        pane_id: String,
        home_window: usize,
        frame: Option<FrameRecord>,
    ) {
        self.aux.pending.push(PendingAuxOpen::Terminal {
            pane_id,
            home_window,
            frame,
        });
    }

    pub(crate) fn flush_aux_opens(&mut self, event_loop: &ActiveEventLoop) {
        let pending = std::mem::take(&mut self.aux.pending);
        let had_pending = !pending.is_empty();
        for request in pending {
            match request {
                PendingAuxOpen::File { path, active } => {
                    let raw = match std::fs::read_to_string(&path) {
                        Ok(raw) => raw,
                        Err(error) => {
                            let message = format!("문서를 열지 못했어요: {error}");
                            if self.viewer_only {
                                self.viewer_status(message);
                            } else {
                                self.set_toast(message);
                            }
                            continue;
                        }
                    };
                    if self.viewer_only {
                        self.aux.windows.retain(|aux| !aux.welcome);
                    }
                    let is_md = is_markdown_path(&path);
                    let lines = Arc::new(raw.split('\n').map(String::from).collect());
                    let editor = make_editor(&path, &raw, lines, is_md, !is_md, false);
                    let _ = self.spawn_aux_editor(editor, event_loop, active, None, false);
                }
                PendingAuxOpen::Restore(record) => {
                    let keep = record.clone();
                    let path = std::path::PathBuf::from(&record.path);
                    let active = self.aux.focus_on_open.remove(&record.path);
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
                    let font_scale = record
                        .font_scale
                        .unwrap_or(1.0)
                        .clamp(AUX_FONT_SCALE_MIN, AUX_FONT_SCALE_MAX);
                    let outline_open = record.outline_open;
                    let outline_scroll = record.outline_scroll.max(0.0);
                    match self.spawn_aux_editor(
                        editor,
                        event_loop,
                        active,
                        record.frame,
                        missing_source,
                    ) {
                        Ok(index) => {
                            let aux = &mut self.aux.windows[index];
                            aux.font_scale = font_scale;
                            aux.gpu.set_font_size(FONT_SIZE * font_scale);
                            if let Some(outline_open) = outline_open {
                                aux.outline_open = outline_open;
                            }
                            aux.outline_scroll = outline_scroll;
                            if missing_source {
                                aux.status = Some("원본이 없어 복원본만 보관 중이에요".to_string());
                            }
                            aux.window.request_redraw();
                        }
                        Err(_) => self.aux.unopened.push(keep),
                    }
                }
                PendingAuxOpen::Terminal {
                    pane_id,
                    home_window,
                    frame,
                } => self.open_restored_aux_terminal(pane_id, home_window, frame, event_loop),
            }
        }
        if had_pending {
            self.save_aux_windows_state();
        }
    }

    pub(crate) fn prepare_viewer_windows(&mut self, event_loop: &ActiveEventLoop) {
        self.flush_aux_opens(event_loop);
        if !self.aux.windows.is_empty() || !self.aux.pending.is_empty() {
            return;
        }
        let text = "# kasaterm 문서 뷰어\n\nCmd+O로 문서를 열거나 이 창에 파일을 끌어놓으세요.\n\n열어 둔 문서는 다음 실행에 같은 위치와 읽던 자리로 돌아옵니다.";
        let editor = make_editor(
            std::path::Path::new(""),
            text,
            Arc::new(text.split('\n').map(String::from).collect()),
            true,
            false,
            false,
        );
        if let Ok(index) = self.spawn_aux_editor(editor, event_loop, true, None, false) {
            let message = self.aux.viewer_message.take();
            let aux = &mut self.aux.windows[index];
            aux.welcome = true;
            aux.status = message;
            aux.last_title = "kasaterm 문서 뷰어".to_string();
            aux.window.set_title(&aux.last_title);
            aux.window.request_redraw();
        }
    }

    pub(crate) fn viewer_has_work(&self) -> bool {
        !self.aux.windows.is_empty()
            || !self.aux.pending.is_empty()
            || !self.aux.unopened.is_empty()
    }

    pub(crate) fn viewer_needs_blink(&self) -> bool {
        self.aux.windows.iter().any(|aux| {
            aux.focused && aux.editor.raw_mode && !aux.welcome && aux.viewer_prompt.is_none()
        })
    }

    fn viewer_status(&mut self, message: String) {
        if let Some(aux) = self.aux.windows.first_mut() {
            aux.status = Some(message);
            aux.dirty = true;
            aux.window.request_redraw();
        } else {
            eprintln!("[viewer] {message}");
            self.aux.viewer_message = Some(message);
        }
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
            .with_theme(Some(if crate::theme::current_is_light() {
                Theme::Light
            } else {
                Theme::Dark
            }))
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
        let initial_outline_open = default_outline_open(
            self.viewer_only,
            editor.is_md_doc,
            editor.raw_mode,
            window.inner_size().width as f32 / gpu.scale().max(0.5),
        );
        self.aux.windows.push(AuxWindow {
            gpu,
            editor,
            dirty: true,
            cursor_px: (0.0, 0.0),
            cursor_seen: false,
            selecting: false,
            focused: wants_focus,
            preedit: String::new(),
            last_title: title,
            md_content_h: 0.0,
            font_scale: 1.0,
            header_hits: Vec::new(),
            find_hits: Vec::new(),
            viewer_prompt: None,
            viewer_prompt_hits: Vec::new(),
            scrollbar: None,
            scrollbar_drag_offset: None,
            outline_open: initial_outline_open,
            outline_scroll: 0.0,
            outline_gen: 0,
            outline_entries: Vec::new(),
            outline_hits: Vec::new(),
            outline_rect: None,
            outline_content_h: 0.0,
            outline_active: None,
            outline_scrollbar_drawn: false,
            outline_scrollbar: None,
            outline_scrollbar_drag_offset: None,
            outline_layout_anchor: None,
            view_find_targets: Vec::new(),
            view_find_revealed: None,
            viewer_style: self.viewer_only,
            welcome: false,
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
        event_loop: &ActiveEventLoop,
    ) -> bool {
        if let WindowEvent::ModifiersChanged(modifiers) = &event {
            self.modifiers = modifiers.state();
        }
        if let Some(term) = self.aux_terminal_window_index(id) {
            self.aux_terminal_event(term, event, event_loop);
            return true;
        }
        let Some(index) = self
            .aux
            .windows
            .iter()
            .position(|aux| aux.window.id() == id)
        else {
            return false;
        };
        match event {
            WindowEvent::CloseRequested => self.close_aux_editor(index, event_loop),
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
                aux.gpu.set_font_size(FONT_SIZE * aux.font_scale);
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
                self.aux.windows[index].cursor_seen = true;
                self.aux.windows[index].cursor_px =
                    (position.x as f32 / scale, position.y as f32 / scale);
                let chrome_h = self.aux.windows[index].chrome_height();
                let hover_chrome = self.aux.windows[index].cursor_px.1 <= chrome_h
                    || self.aux.windows[index].viewer_prompt.is_some()
                    || self.aux.windows[index]
                        .outline_rect
                        .is_some_and(|rect| hit(self.aux.windows[index].cursor_px, rect))
                    || self.aux.windows[index].scrollbar.is_some_and(|bar| {
                        self.aux.windows[index].cursor_px.0 >= bar.x - 7.0
                            && self.aux.windows[index].cursor_px.0 <= bar.x + 7.0
                    });
                let point = self.aux.windows[index].cursor_px;
                let over_action = self.aux.windows[index]
                    .header_hits
                    .iter()
                    .any(|(_, rect)| hit(point, *rect))
                    || self.aux.windows[index]
                        .viewer_prompt_hits
                        .iter()
                        .any(|(_, rect)| hit(point, *rect))
                    || self.aux.windows[index]
                        .outline_hits
                        .iter()
                        .any(|(_, rect)| hit(point, *rect));
                let over_scrollbar = self.aux.windows[index].scrollbar.is_some_and(|bar| {
                    point.0 >= bar.x - 7.0
                        && point.0 <= bar.x + 7.0
                        && point.1 >= bar.track_y
                        && point.1 <= bar.track_y + bar.track_h
                });
                let over_outline_scrollbar = self.aux.windows[index]
                    .outline_scrollbar
                    .is_some_and(|bar| {
                        point.0 >= bar.x - 7.0
                            && point.0 <= bar.x + 7.0
                            && point.1 >= bar.track_y
                            && point.1 <= bar.track_y + bar.track_h
                    });
                let cursor = if self.aux.windows[index].scrollbar_drag_offset.is_some()
                    || self.aux.windows[index]
                        .outline_scrollbar_drag_offset
                        .is_some()
                {
                    winit::window::CursorIcon::Grabbing
                } else if over_scrollbar || over_outline_scrollbar {
                    winit::window::CursorIcon::Grab
                } else if over_action {
                    winit::window::CursorIcon::Pointer
                } else if self.aux.windows[index].editor.raw_mode && point.1 >= chrome_h {
                    winit::window::CursorIcon::Text
                } else {
                    winit::window::CursorIcon::Default
                };
                self.aux.windows[index].window.set_cursor(cursor);
                if !self.aux_outline_scrollbar_drag(index) && !self.aux_scrollbar_drag(index) {
                    self.aux_drag_selection(index);
                }
                if hover_chrome {
                    self.aux.windows[index].dirty = true;
                    self.aux.windows[index].window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.aux_mouse_press(index, event_loop),
                ElementState::Released => {
                    let aux = &mut self.aux.windows[index];
                    aux.scrollbar_drag_offset = None;
                    aux.outline_scrollbar_drag_offset = None;
                    aux.selecting = false;
                    if aux.editor.sel_anchor == Some((aux.editor.cur_line, aux.editor.cur_col)) {
                        aux.editor.sel_anchor = None;
                    }
                    self.save_aux_windows_state();
                }
            },
            WindowEvent::MouseWheel { delta, .. } => self.aux_wheel(index, delta),
            WindowEvent::KeyboardInput { event, .. } => self.aux_key(index, &event, event_loop),
            WindowEvent::DroppedFile(path) if self.viewer_only => {
                self.queue_aux_file(path, true);
                self.flush_aux_opens(event_loop);
            }
            WindowEvent::Ime(ime) => self.aux_ime(index, ime),
            WindowEvent::RedrawRequested => self.aux_render(index),
            _ => {}
        }
        true
    }

    pub(crate) fn aux_request_redraws(&mut self) {
        for aux in &mut self.aux.windows {
            if aux.dirty
                || (aux.focused
                    && aux.editor.raw_mode
                    && !aux.welcome
                    && aux.viewer_prompt.is_none())
            {
                aux.window.request_redraw();
            }
        }
        // 터미널 별도창은 PTY 에코가 곧 바뀐 것이라 포커스와 무관하게 다시 그린다
        // (winit 이 같은 프레임의 요청을 합친다).
        for term in &self.aux.terminals {
            term.window.request_redraw();
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
                        "wrap": aux.editor.wrap,
                        "font_scale": aux.font_scale,
                        "chrome_height": aux.chrome_height(),
                        "editor_origin_x": aux.editor_origin_x(),
                        "viewer_style": aux.viewer_style,
                        "outline_open": aux.outline_open,
                        "outline_scroll": aux.outline_scroll,
                        "outline_entries": aux.outline_entries.len(),
                        "outline_active": aux.outline_active,
                        "outline_overlay": aux.outline_overlays(),
                        "outline_rect": aux.outline_rect,
                        "body_rect": aux.body_box(),
                        "scrollbar": aux.scrollbar.is_some(),
                        "visible_scrollbars": usize::from(aux.scrollbar.is_some())
                            + usize::from(aux.outline_scrollbar_drawn),
                        "buffer_lines": aux.editor.edit_lines.len(),
                        "buffer_hash": hash.finish(),
                        "focused": aux.focused,
                        "welcome": aux.welcome,
                        "viewer_prompt": aux.viewer_prompt.map(|prompt| match prompt {
                            ViewerPromptAction::Close => "close",
                            ViewerPromptAction::Quit => "quit",
                        }),
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
            "open" => HeaderButton::Open,
            "view" => HeaderButton::View,
            "edit" => HeaderButton::Edit,
            "find" => HeaderButton::Find,
            "outline" => HeaderButton::Outline,
            "wrap" => HeaderButton::Wrap,
            "zoom-out" => HeaderButton::ZoomOut,
            "zoom-in" => HeaderButton::ZoomIn,
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

    pub(crate) fn aux_probe_outline_center(
        &self,
        index: usize,
        heading: usize,
    ) -> Option<(WindowId, winit::dpi::PhysicalPosition<f64>)> {
        if !crate::verification_run() {
            return None;
        }
        let aux = self.aux.windows.get(index)?;
        let (_, rect) = aux
            .outline_hits
            .iter()
            .find(|(entry, _)| *entry == heading)?;
        let scale = aux.gpu.scale() as f64;
        Some((
            aux.window.id(),
            winit::dpi::PhysicalPosition::new(
                (rect.0 + rect.2 * 0.5) as f64 * scale,
                (rect.1 + rect.3 * 0.5) as f64 * scale,
            ),
        ))
    }

    pub(crate) fn aux_probe_scrollbar_center(
        &self,
        index: usize,
        outline: bool,
    ) -> Option<(WindowId, winit::dpi::PhysicalPosition<f64>)> {
        if !crate::verification_run() {
            return None;
        }
        let aux = self.aux.windows.get(index)?;
        let bar = if outline {
            aux.outline_scrollbar?
        } else {
            aux.scrollbar?
        };
        let scale = aux.gpu.scale() as f64;
        Some((
            aux.window.id(),
            winit::dpi::PhysicalPosition::new(
                bar.x as f64 * scale,
                (bar.thumb_y + bar.thumb_h * 0.5) as f64 * scale,
            ),
        ))
    }

    pub(crate) fn aux_probe_resize(&self, index: usize, width: u32, height: u32) -> bool {
        if !crate::verification_run() {
            return false;
        }
        let Some(aux) = self.aux.windows.get(index) else {
            return false;
        };
        let _ = aux.window.request_inner_size(winit::dpi::LogicalSize::new(
            f64::from(width.max(320)),
            f64::from(height.max(240)),
        ));
        true
    }

    pub(crate) fn aux_probe_viewer_prompt_center(
        &self,
        index: usize,
        kind: &str,
    ) -> Option<(WindowId, winit::dpi::PhysicalPosition<f64>)> {
        if !crate::verification_run() {
            return None;
        }
        let aux = self.aux.windows.get(index)?;
        let wanted = match kind {
            "cancel" => ViewerPromptButton::Cancel,
            "discard" => ViewerPromptButton::Discard,
            "save" => ViewerPromptButton::Save,
            _ => return None,
        };
        let (_, rect) = aux
            .viewer_prompt_hits
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
        let mut records: Vec<RestoreRecord> = self
            .aux
            .windows
            .iter()
            .filter(|aux| !aux.welcome)
            .map(AuxWindow::record)
            .collect();
        // Explicit in-pane previews are intentionally absent from the PTY
        // layout JSON. Save them beside aux windows so choosing tab/split does
        // not make a dirty document less recoverable; restore may reopen them
        // as detached windows without reviving a fake PTY leaf.
        if !self.viewer_only {
            let Ok(ws) = self.ws.lock() else {
                return;
            };
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
                        font_scale: None,
                        outline_open: Some(false),
                        outline_scroll: 0.0,
                        frame: None,
                    })
                })
            }));
        }
        records.extend(self.aux.pending.iter().filter_map(|pending| match pending {
            PendingAuxOpen::Restore(record) => Some(record.clone()),
            PendingAuxOpen::File { .. } | PendingAuxOpen::Terminal { .. } => None,
        }));
        records.extend(self.aux.unopened.iter().cloned());
        let mut seen = std::collections::HashSet::new();
        records.retain(|record| record.path.is_empty() || seen.insert(record.path.clone()));
        if let Some(path) = state_path(self.aux.viewer_only) {
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
        self.aux_index(id).is_some() || self.aux_terminal_window_index(id).is_some()
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

    fn close_aux_editor(&mut self, index: usize, event_loop: &ActiveEventLoop) {
        self.aux_flush_hangul(index);
        let Some(id) = self.aux.windows.get(index).map(|aux| aux.window.id()) else {
            return;
        };
        if self.viewer_only {
            if self.aux.windows[index].editor.modified {
                self.aux.windows[index].viewer_prompt = Some(ViewerPromptAction::Close);
                self.aux_redraw(index);
                return;
            }
            self.close_aux_by_id(id);
            if !self.viewer_has_work() {
                event_loop.exit();
            }
            return;
        }
        if self.guard_dirty(&PendingClose::AuxEditor(id)) {
            if let Some(main) = &self.window {
                main.focus_window();
                main.request_user_attention(Some(winit::window::UserAttentionType::Critical));
            }
            return;
        }
        self.close_aux_by_id(id);
    }

    fn viewer_prompt_pick(
        &mut self,
        index: usize,
        button: ViewerPromptButton,
        event_loop: &ActiveEventLoop,
    ) {
        let Some(action) = self
            .aux
            .windows
            .get_mut(index)
            .and_then(|aux| aux.viewer_prompt.take())
        else {
            return;
        };
        if button == ViewerPromptButton::Cancel {
            self.aux.viewer_quit_requested = false;
            self.aux_redraw(index);
            return;
        }
        let id = self.aux.windows[index].window.id();
        if button == ViewerPromptButton::Save && !self.aux_save(index) {
            self.aux.windows[index].viewer_prompt = Some(action);
            self.aux.windows[index].window.focus_window();
            self.aux_redraw(index);
            return;
        }
        match action {
            ViewerPromptAction::Close => {
                self.close_aux_by_id(id);
                if !self.viewer_has_work() {
                    event_loop.exit();
                }
            }
            ViewerPromptAction::Quit => {
                if button == ViewerPromptButton::Discard {
                    self.viewer_discard_for_quit(index);
                }
                self.viewer_continue_quit(event_loop);
            }
        }
    }

    fn viewer_discard_for_quit(&mut self, index: usize) {
        let path = self.aux.windows[index].editor.doc.path.clone();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let editor = &mut self.aux.windows[index].editor;
                editor.edit_lines = Arc::new(text.split('\n').map(String::from).collect());
                if editor.edit_lines.is_empty() {
                    editor.edit_lines = Arc::new(vec![String::new()]);
                }
                editor.doc = Arc::new(build_markdown_doc(std::path::Path::new(&path), &text));
                editor.cur_line = editor.cur_line.min(editor.edit_lines.len() - 1);
                editor.cur_col = editor
                    .cur_col
                    .min(editor.edit_lines[editor.cur_line].chars().count());
                editor.sel_anchor = None;
                editor.extra.clear();
                editor.mark_saved();
                self.aux.windows[index].missing_source = false;
            }
            Err(_) => {
                self.aux.windows.remove(index);
            }
        }
    }

    pub(crate) fn viewer_begin_quit(&mut self, event_loop: &ActiveEventLoop) {
        self.aux.viewer_quit_requested = true;
        self.viewer_continue_quit(event_loop);
    }

    fn viewer_continue_quit(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(index) = self
            .aux
            .windows
            .iter()
            .position(|aux| !aux.welcome && aux.editor.modified)
        {
            self.aux.windows[index].viewer_prompt = Some(ViewerPromptAction::Quit);
            self.aux.windows[index].window.focus_window();
            self.aux_redraw(index);
            return;
        }
        self.save_aux_windows_state();
        event_loop.exit();
    }

    fn viewer_choose_file(&mut self, index: usize) {
        #[cfg(target_os = "macos")]
        {
            let proxy = self.proxy.clone();
            self.aux.windows[index].status = Some("파일 선택 중…".to_string());
            self.aux_redraw(index);
            std::thread::spawn(move || {
                let output = std::process::Command::new("/usr/bin/osascript")
                    .args([
                        "-e",
                        "POSIX path of (choose file with prompt \"kasaterm 문서 열기\")",
                    ])
                    .output();
                let Ok(output) = output else { return };
                if !output.status.success() {
                    return;
                }
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    let _ = proxy.send_event(UserEvent::OpenMarkdownWindow(path));
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.aux.windows[index].status =
                Some("파일을 이 창에 끌어놓아 열 수 있어요".to_string());
            self.aux_redraw(index);
        }
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

    fn aux_key(&mut self, index: usize, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if event.state != ElementState::Pressed || crate::input::is_modifier_key(event) {
            return;
        }
        self.last_input_at = Instant::now();
        let id = self.aux.windows[index].window.id();
        self.ime_retarget(ImeFocus::AuxEditor(id));
        if self.viewer_only && self.aux.windows[index].viewer_prompt.is_some() {
            match event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.viewer_prompt_pick(index, ViewerPromptButton::Cancel, event_loop)
                }
                Key::Named(NamedKey::Enter) => {
                    self.viewer_prompt_pick(index, ViewerPromptButton::Save, event_loop)
                }
                _ => {}
            }
            return;
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Escape))
            && self.aux.windows[index].outline_open
        {
            self.aux.windows[index].outline_open = false;
            self.aux.windows[index].outline_rect = None;
            self.aux_redraw(index);
            self.save_aux_windows_state();
            return;
        }
        if self.viewer_only && self.host_mod() {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::KeyQ) => {
                    self.viewer_begin_quit(event_loop);
                    return;
                }
                PhysicalKey::Code(KeyCode::KeyO) => {
                    self.viewer_choose_file(index);
                    return;
                }
                _ => {}
            }
        }
        if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW)) {
            self.close_aux_editor(index, event_loop);
            return;
        }
        if self.aux.windows[index].welcome {
            return;
        }
        if self.host_mod() {
            self.aux_flush_hangul(index);
            if let PhysicalKey::Code(code) = event.physical_key {
                let logical = match &event.logical_key {
                    Key::Character(text) => Some(text.as_str()),
                    _ => None,
                };
                if let Some(zoom) = crate::input::zoom_key(Some(code), logical) {
                    match zoom {
                        crate::input::ZoomKey::In => {
                            self.aux_adjust_zoom(index, AUX_FONT_SCALE_STEP)
                        }
                        crate::input::ZoomKey::Out => {
                            self.aux_adjust_zoom(index, -AUX_FONT_SCALE_STEP)
                        }
                        crate::input::ZoomKey::Reset => self.aux_set_zoom(index, 1.0),
                    }
                    return;
                }
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
        if !self.aux.windows[index].editor.raw_mode
            && self.aux.windows[index].editor.find.is_none()
        {
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
        let view_find_query = (!self.aux.windows[index].editor.raw_mode)
            .then(|| {
                self.aux.windows[index]
                    .editor
                    .find
                    .as_ref()
                    .map(|find| find.query.clone())
            })
            .flatten();
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
        if let Some(before) = view_find_query {
            match self.aux.windows[index].editor.find.as_ref() {
                Some(find) if find.query != before => self.aux_refresh_view_find(index, true),
                None => {
                    self.aux.windows[index].view_find_targets.clear();
                    self.aux.windows[index].view_find_revealed = None;
                }
                Some(_) => {}
            }
        } else {
            self.aux_ensure_visible(index);
        }
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
                let raw_mode = self.aux.windows[index].editor.raw_mode;
                self.aux.windows[index].editor.find_open(self.modifiers.alt_key() && raw_mode);
                if !raw_mode {
                    self.aux_refresh_view_find(index, true);
                }
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
        let view_find = !self.aux.windows[index].editor.raw_mode
            && self.aux.windows[index].editor.find.is_some();
        let editor = &mut self.aux.windows[index].editor;
        if editor.find.is_some() {
            editor.find_type(text);
        } else {
            editor.each_caret(|editor| editor.insert_at_caret(text));
        }
        if view_find {
            self.aux_refresh_view_find(index, true);
        } else {
            self.aux_ensure_visible(index);
        }
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
            if want_raw {
                self.aux.windows[index].editor.find_refresh(true);
                self.aux.windows[index].view_find_targets.clear();
                self.aux.windows[index].view_find_revealed = None;
            } else if self.aux.windows[index].editor.find.is_some() {
                self.aux_refresh_view_find(index, true);
            }
            self.aux.windows[index].status = None;
            self.aux_redraw(index);
            self.save_aux_windows_state();
        }
    }

    fn aux_refresh_view_find(&mut self, index: usize, seek: bool) {
        if self.aux.windows[index].editor.raw_mode {
            return;
        }
        let Some(find) = self.aux.windows[index].editor.find.as_ref() else {
            self.aux.windows[index].view_find_targets.clear();
            self.aux.windows[index].view_find_revealed = None;
            return;
        };
        let query = find.query.clone();
        let old_index = find.idx;
        let targets = rendered_find_targets(&self.aux.windows[index].editor.doc, &query);
        let body_width = self.aux.windows[index].body_box().2;
        let source_line = self.aux.windows[index].current_source_line(body_width);
        let next_index = if targets.is_empty() {
            0
        } else if seek {
            targets
                .iter()
                .position(|target| target.source_line >= source_line)
                .unwrap_or(0)
        } else {
            old_index.min(targets.len() - 1)
        };
        let aux = &mut self.aux.windows[index];
        if let Some(find) = aux.editor.find.as_mut() {
            find.hits = targets
                .iter()
                .map(|target| (target.source_line, target.start, target.end))
                .collect();
            find.idx = next_index;
        }
        if let Some(target) = targets.get(next_index) {
            aux.editor.cur_line = target.source_line.min(aux.editor.edit_lines.len() - 1);
            aux.editor.cur_col = target.end.min(
                aux.editor.edit_lines[aux.editor.cur_line]
                    .chars()
                    .count(),
            );
            aux.editor.sel_anchor = None;
        }
        aux.view_find_targets = targets;
        aux.view_find_revealed = None;
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

    fn aux_mouse_press(&mut self, index: usize, event_loop: &ActiveEventLoop) {
        self.aux_flush_hangul(index);
        let cursor = self.aux.windows[index].cursor_px;
        if self.viewer_only && self.aux.windows[index].viewer_prompt.is_some() {
            if let Some(button) = self.aux.windows[index]
                .viewer_prompt_hits
                .iter()
                .find(|(_, rect)| hit(cursor, *rect))
                .map(|(button, _)| *button)
            {
                self.viewer_prompt_pick(index, button, event_loop);
            }
            return;
        }
        if self.aux_outline_scrollbar_press(index) || self.aux_scrollbar_press(index) {
            return;
        }
        if let Some(button) = self.aux.windows[index]
            .header_hits
            .iter()
            .find(|(_, rect)| hit(cursor, *rect))
            .map(|(button, _)| *button)
        {
            match button {
                HeaderButton::Open => self.viewer_choose_file(index),
                HeaderButton::View => self.aux_set_mode(index, false),
                HeaderButton::Edit => self.aux_set_mode(index, true),
                HeaderButton::Find => {
                    if self.aux.windows[index].editor.find.is_some() {
                        self.aux.windows[index].editor.find_close();
                        self.aux.windows[index].view_find_targets.clear();
                    } else {
                        self.aux.windows[index].editor.find_open(false);
                        if !self.aux.windows[index].editor.raw_mode {
                            self.aux_refresh_view_find(index, true);
                        }
                    }
                    self.aux.windows[index].status = None;
                    self.aux_redraw(index);
                    self.save_aux_windows_state();
                }
                HeaderButton::Outline => self.aux_toggle_outline(index),
                HeaderButton::Wrap => {
                    let editor = &mut self.aux.windows[index].editor;
                    editor.wrap = !editor.wrap;
                    if editor.wrap {
                        editor.h_scroll = 0.0;
                    }
                    self.aux.windows[index].status = Some(if self.aux.windows[index].editor.wrap {
                        "긴 줄 줄바꿈을 켰어요".to_string()
                    } else {
                        "긴 줄 줄바꿈을 껐어요".to_string()
                    });
                    self.aux_redraw(index);
                    self.save_aux_windows_state();
                }
                HeaderButton::ZoomOut => self.aux_adjust_zoom(index, -AUX_FONT_SCALE_STEP),
                HeaderButton::ZoomIn => self.aux_adjust_zoom(index, AUX_FONT_SCALE_STEP),
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
            if !self.aux.windows[index].editor.raw_mode
                && self.aux.windows[index].editor.find.is_none()
            {
                self.aux.windows[index].view_find_targets.clear();
                self.aux.windows[index].view_find_revealed = None;
            }
            self.aux_redraw(index);
            self.save_aux_windows_state();
            return;
        }
        if let Some(entry) = self.aux.windows[index]
            .outline_hits
            .iter()
            .find(|(_, rect)| hit(cursor, *rect))
            .map(|(entry, _)| *entry)
        {
            self.aux_outline_jump(index, entry);
            return;
        }
        if self.aux.windows[index]
            .outline_rect
            .is_some_and(|rect| hit(cursor, rect))
        {
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

    fn aux_toggle_outline(&mut self, index: usize) {
        let source_line = {
            let aux = &mut self.aux.windows[index];
            let body = aux.body_box();
            aux.current_source_line(body.2)
        };
        if self.aux.windows[index].outline_open {
            let aux = &mut self.aux.windows[index];
            aux.outline_open = false;
            aux.outline_rect = None;
        } else {
            let aux = &mut self.aux.windows[index];
            aux.refresh_outline_cache();
            let body = aux.body_box();
            let active = aux
                .outline_entries
                .partition_point(|entry| entry.source_line <= source_line)
                .saturating_sub(1);
            let list_h = (body.3 - 56.0).max(1.0);
            let max_scroll =
                (aux.outline_entries.len() as f32 * OUTLINE_ROW_H - list_h).max(0.0);
            aux.outline_scroll = (active as f32 * OUTLINE_ROW_H - list_h * 0.35)
                .clamp(0.0, max_scroll);
            aux.outline_open = true;
        }
        let aux = &mut self.aux.windows[index];
        if aux.editor.raw_mode {
            let body = aux.body_box();
            let (pad, line_h) = aux.gpu.raw_editor_metrics();
            let wrap_cols = aux.gpu.raw_editor_wrap_cols(
                body.2,
                aux.editor.edit_lines.len(),
                aux.editor.wrap,
            );
            let rows = crate::markdown::layout_rows(
                &aux.editor.edit_lines,
                &aux.editor.folds,
                wrap_cols,
            );
            let row = rows
                .partition_point(|(line, _)| *line < source_line)
                .min(rows.len().saturating_sub(1));
            aux.editor.scroll = pad + row as f32 * line_h;
        } else {
            aux.outline_layout_anchor = Some(source_line);
        }
        self.aux_redraw(index);
        self.save_aux_windows_state();
    }

    fn aux_outline_jump(&mut self, index: usize, entry_index: usize) {
        let Some(entry) = self.aux.windows[index]
            .outline_entries
            .get(entry_index)
            .cloned()
        else {
            return;
        };
        let body = self.aux.windows[index].body_box();
        let aux = &mut self.aux.windows[index];
        if aux.editor.raw_mode {
            aux.editor.folds.retain(|(start, end)| {
                entry.source_line <= *start || entry.source_line > *end
            });
            let (pad, line_h) = aux.gpu.raw_editor_metrics();
            let wrap_cols = aux.gpu.raw_editor_wrap_cols(
                body.2,
                aux.editor.edit_lines.len(),
                aux.editor.wrap,
            );
            let rows = crate::markdown::layout_rows(
                &aux.editor.edit_lines,
                &aux.editor.folds,
                wrap_cols,
            );
            let row = rows
                .iter()
                .position(|(line, _)| *line == entry.source_line)
                .unwrap_or_else(|| rows.partition_point(|(line, _)| *line < entry.source_line));
            aux.editor.cur_line = entry
                .source_line
                .min(aux.editor.edit_lines.len().saturating_sub(1));
            aux.editor.cur_col = 0;
            aux.editor.sel_anchor = None;
            aux.editor.extra.clear();
            aux.editor.scroll = clamped_document_scroll(
                0.0,
                pad + row as f32 * line_h,
                aux.md_content_h.max(body.3),
                body.3,
            );
        } else if let Some(y) = aux.gpu.md_block_ys.get(entry.block).copied() {
            aux.editor.scroll = clamped_document_scroll(
                0.0,
                y,
                aux.md_content_h.max(body.3),
                body.3,
            );
        }
        aux.outline_active = Some(entry_index);
        if aux.outline_overlays() {
            aux.outline_open = false;
            aux.outline_rect = None;
        }
        aux.dirty = true;
        aux.window.request_redraw();
        self.save_aux_windows_state();
    }

    fn aux_outline_scrollbar_press(&mut self, index: usize) -> bool {
        let aux = &mut self.aux.windows[index];
        let Some(bar) = aux.outline_scrollbar else {
            return false;
        };
        let point = aux.cursor_px;
        if point.0 < bar.x - 7.0
            || point.0 > bar.x + 7.0
            || point.1 < bar.track_y
            || point.1 > bar.track_y + bar.track_h
        {
            return false;
        }
        aux.outline_scrollbar_drag_offset = Some(
            if point.1 >= bar.thumb_y && point.1 <= bar.thumb_y + bar.thumb_h {
                point.1 - bar.thumb_y
            } else {
                bar.thumb_h * 0.5
            },
        );
        self.aux_outline_scrollbar_drag(index);
        true
    }

    fn aux_outline_scrollbar_drag(&mut self, index: usize) -> bool {
        let aux = &mut self.aux.windows[index];
        let (Some(bar), Some(offset)) = (
            aux.outline_scrollbar,
            aux.outline_scrollbar_drag_offset,
        ) else {
            return false;
        };
        let travel = (bar.track_h - bar.thumb_h).max(1.0);
        let thumb_y =
            (aux.cursor_px.1 - offset).clamp(bar.track_y, bar.track_y + bar.track_h - bar.thumb_h);
        aux.outline_scroll = ((thumb_y - bar.track_y) / travel) * bar.max_scroll;
        aux.dirty = true;
        aux.window.request_redraw();
        self.save_aux_windows_state();
        true
    }

    fn aux_scrollbar_press(&mut self, index: usize) -> bool {
        let aux = &mut self.aux.windows[index];
        let Some(bar) = aux.scrollbar else {
            return false;
        };
        let point = aux.cursor_px;
        if point.0 < bar.x - 7.0
            || point.0 > bar.x + 7.0
            || point.1 < bar.track_y
            || point.1 > bar.track_y + bar.track_h
        {
            return false;
        }
        aux.scrollbar_drag_offset = Some(
            if point.1 >= bar.thumb_y && point.1 <= bar.thumb_y + bar.thumb_h {
                point.1 - bar.thumb_y
            } else {
                bar.thumb_h * 0.5
            },
        );
        self.aux_scrollbar_drag(index);
        true
    }

    fn aux_scrollbar_drag(&mut self, index: usize) -> bool {
        let aux = &mut self.aux.windows[index];
        let (Some(bar), Some(offset)) = (aux.scrollbar, aux.scrollbar_drag_offset) else {
            return false;
        };
        let travel = (bar.track_h - bar.thumb_h).max(1.0);
        let thumb_y =
            (aux.cursor_px.1 - offset).clamp(bar.track_y, bar.track_y + bar.track_h - bar.thumb_h);
        aux.editor.scroll = ((thumb_y - bar.track_y) / travel) * bar.max_scroll;
        aux.dirty = true;
        aux.window.request_redraw();
        self.save_aux_windows_state();
        true
    }

    fn aux_adjust_zoom(&mut self, index: usize, delta: f32) {
        let current = self.aux.windows[index].font_scale;
        let next = ((current + delta) * 10.0).round() / 10.0;
        self.aux_set_zoom(index, next);
    }

    fn aux_set_zoom(&mut self, index: usize, scale: f32) {
        let aux = &mut self.aux.windows[index];
        aux.font_scale = scale.clamp(AUX_FONT_SCALE_MIN, AUX_FONT_SCALE_MAX);
        aux.gpu.set_font_size(FONT_SIZE * aux.font_scale);
        aux.status = Some(format!(
            "보기 크기 {}%",
            (aux.font_scale * 100.0).round() as i32
        ));
        aux.dirty = true;
        aux.window.request_redraw();
        self.save_aux_windows_state();
    }

    fn aux_wheel(&mut self, index: usize, delta: MouseScrollDelta) {
        let aux = &mut self.aux.windows[index];
        let (top_pad, line_h) = aux.gpu.raw_editor_metrics();
        let (dy, outline_dy) = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                (y * line_h * 3.0, y * OUTLINE_ROW_H * 3.0)
            }
            MouseScrollDelta::PixelDelta(point) => (point.y as f32, point.y as f32),
        };
        if aux
            .outline_rect
            .is_some_and(|rect| hit(aux.cursor_px, rect))
        {
            let list_h = aux
                .outline_rect
                .map(|rect| (rect.3 - 44.0).max(1.0))
                .unwrap_or(1.0);
            aux.outline_scroll = clamped_document_scroll(
                aux.outline_scroll,
                -outline_dy,
                aux.outline_content_h,
                list_h,
            );
            aux.dirty = true;
            aux.window.request_redraw();
            self.save_aux_windows_state();
            return;
        }
        let body_h = aux.body_box().3;
        let content_h = if aux.md_content_h > 0.0 {
            aux.md_content_h
        } else if aux.editor.raw_mode {
            let width = aux.body_box().2;
            let wrap_cols =
                aux.gpu
                    .raw_editor_wrap_cols(width, aux.editor.edit_lines.len(), aux.editor.wrap);
            crate::markdown::layout_rows(&aux.editor.edit_lines, &aux.editor.folds, wrap_cols).len()
                as f32
                * line_h
                + top_pad
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
            font_scale: Some(1.2),
            outline_open: Some(true),
            outline_scroll: 91.0,
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
        assert_eq!(got.font_scale, Some(1.2));
        assert_eq!(got.outline_open, Some(true));
        assert_eq!(got.outline_scroll, 91.0);
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

    #[test]
    fn viewer_markdown_extensions_match_bundle_declarations() {
        for name in ["note.md", "note.markdown", "note.mdown", "note.mkd"] {
            assert!(is_markdown_path(std::path::Path::new(name)), "{name}");
        }
        assert!(!is_markdown_path(std::path::Path::new("note.txt")));
    }

    #[test]
    fn outline_uses_only_ast_headings_with_levels_and_source_lines() {
        let doc = build_markdown_doc(
            std::path::Path::new("outline.md"),
            "# 시작\n\n```md\n# 코드 안 제목 아님\n```\n\n### 깊은 *제목*",
        );
        let entries = document_outline(&doc);
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].level, entries[0].source_line), (1, 0));
        assert_eq!(entries[0].title, "시작");
        assert_eq!((entries[1].level, entries[1].source_line), (3, 6));
        assert_eq!(entries[1].title, "깊은 제목");
    }

    #[test]
    fn rendered_find_counts_visible_text_but_not_hidden_link_destinations() {
        let doc = build_markdown_doc(
            std::path::Path::new("find.md"),
            "[보이는 이름](https://hidden.example/path)\n\n```txt\n보이는 코드\n```",
        );
        assert!(rendered_find_targets(&doc, "hidden.example").is_empty());
        let labels = rendered_find_targets(&doc, "보이는");
        assert_eq!(labels.len(), 2);
        assert_ne!(labels[0].block, labels[1].block);
    }

    #[test]
    fn fresh_viewer_opens_wide_outline_but_not_narrow_or_existing_editors() {
        assert!(default_outline_open(true, true, false, 760.0));
        assert!(!default_outline_open(true, true, false, 320.0));
        assert!(!default_outline_open(true, true, true, 760.0));
        assert!(!default_outline_open(false, true, false, 760.0));
    }
}
