use crate::*;
use kasa_mcp::share::{
    catalog::{Catalog, File},
    shortcut::{Location, Preferences},
};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;
mod files;
mod input;
mod paint;
use files::{perform, read_preview};
pub(crate) use paint::draw;

pub(crate) type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum View {
    #[default]
    Recent,
    Folders,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Recent,
    Folders,
    Back,
    Root,
    Search,
    ClearSearch,
    Refresh,
    Settings,
    OpenPanel,
    Folder(String),
    Preview(String),
    ClosePreview,
    Reveal,
    CopyPath,
    Desktop,
    ChooseLocation,
    HideShortcut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub path: String,
    pub title: String,
    pub detail: String,
    pub folder: bool,
    pub kind: String,
    pub modified: u64,
}

fn normalized(value: &str) -> String {
    value.nfc().flat_map(char::to_lowercase).collect()
}

pub(crate) fn rows(files: &[File], view: View, directory: &str, query: &str) -> Vec<Row> {
    let words: Vec<_> = query.split_whitespace().map(normalized).collect();
    let searching = !words.is_empty();
    let mut folders = std::collections::BTreeMap::<String, (usize, u64)>::new();
    let mut rows = Vec::new();
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    for file in files {
        let aliases = match file.kind.as_str() {
            "image" => "이미지 사진 그림",
            "markdown" | "text" | "pdf" | "html" => "문서 텍스트",
            "video" => "동영상 영상",
            "audio" => "음성 음악",
            _ => "파일",
        };
        let haystack = normalized(&format!(
            "{} {} {} {aliases}",
            file.path, file.origin, file.kind
        ));
        if !words.iter().all(|word| haystack.contains(word)) {
            continue;
        }
        if view == View::Folders && !searching {
            let Some(tail) = file.path.strip_prefix(&prefix) else {
                continue;
            };
            if let Some((name, _)) = tail.split_once('/') {
                let folder = folders.entry(format!("{prefix}{name}")).or_default();
                folder.0 += 1;
                folder.1 = folder.1.max(file.modified_ms);
                continue;
            }
        }
        rows.push(Row {
            path: file.path.clone(),
            title: file.name.clone(),
            detail: format!("{} · {} · {}", file.origin, size(file.size), file.path),
            folder: false,
            kind: file.kind.clone(),
            modified: file.modified_ms,
        });
    }
    rows.extend(folders.into_iter().map(|(path, (count, modified))| Row {
        title: path.rsplit('/').next().unwrap_or(&path).to_string(),
        detail: format!("파일 {count}개"),
        folder: true,
        kind: "folder".into(),
        path,
        modified,
    }));
    rows.sort_by(|a, b| {
        if view == View::Folders && !searching {
            b.folder.cmp(&a.folder).then_with(|| a.title.cmp(&b.title))
        } else {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.path.cmp(&b.path))
        }
    });
    rows
}

pub(crate) fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.0} KB", bytes as f64 / (1u64 << 10) as f64)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) enum Preview {
    Image {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    },
    Text(String),
    Other,
}

#[derive(Clone)]
pub(crate) struct SettingsView {
    pub catalog: Arc<Catalog>,
    pub preferences: Preferences,
    pub notice: Option<String>,
    pub problem: Option<String>,
    pub ready: bool,
    pub busy: bool,
}

enum Update {
    Catalog(Option<Catalog>, Result<Preferences, String>),
    Preview(u64, String, Result<Preview, String>),
    Action(Result<Option<String>, String>),
}

pub(crate) struct State {
    pub catalog: Arc<Catalog>,
    pub preferences: Preferences,
    pub ready: bool,
    pub problem: Option<String>,
    pub notice: Option<String>,
    pub view: View,
    pub directory: String,
    pub query: String,
    pub caret: usize,
    pub search_focus: bool,
    pub pending_search_commit: bool,
    pub keyboard_focus: Option<Action>,
    select_all: bool,
    pub scroll: f32,
    pub scroll_max: f32,
    scroll_track: Option<Rect>,
    scroll_drag: Option<f32>,
    pub rows: Vec<Row>,
    pub hits: Vec<(Action, Rect)>,
    pub search_rect: Option<Rect>,
    pub caret_rect: Option<Rect>,
    pub selected: Option<String>,
    pub preview: Option<Result<Preview, String>>,
    pub action_busy: bool,
    preview_uploaded: bool,
    preview_busy: bool,
    preview_generation: u64,
    last_refresh: Option<Instant>,
    refresh_busy: bool,
    tx: Sender<Update>,
    rx: Receiver<Update>,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            catalog: Arc::new(Catalog::default()),
            preferences: Preferences::default(),
            ready: false,
            problem: None,
            notice: None,
            view: View::Recent,
            directory: String::new(),
            query: String::new(),
            caret: 0,
            search_focus: false,
            pending_search_commit: false,
            keyboard_focus: None,
            select_all: false,
            scroll: 0.0,
            scroll_max: 0.0,
            scroll_track: None,
            scroll_drag: None,
            rows: Vec::new(),
            hits: Vec::new(),
            search_rect: None,
            caret_rect: None,
            selected: None,
            preview: None,
            action_busy: false,
            preview_uploaded: false,
            preview_busy: false,
            preview_generation: 0,
            last_refresh: None,
            refresh_busy: false,
            tx,
            rx,
        }
    }
}

impl State {
    fn home(&mut self) {
        self.view = View::Recent;
        self.directory.clear();
        self.query.clear();
        self.caret = 0;
        self.select_all = false;
        self.select(None);
        self.scroll = 0.0;
        self.rebuild();
    }
    fn deferred_ime(&mut self, event: &Ime) -> bool {
        if !self.pending_search_commit {
            return false;
        }
        self.pending_search_commit = false;
        if let Ime::Commit(text) = event {
            self.insert(text);
            return true;
        }
        false
    }
    fn thumb(&self) -> Option<Rect> {
        let (x, y, w, h) = self.scroll_track?;
        if self.scroll_max <= 0.0 || h <= 0.0 {
            return None;
        }
        let height = (h * h / (h + self.scroll_max)).max(24.0).min(h);
        Some((
            x + 2.0,
            y + self.scroll / self.scroll_max * (h - height),
            (w - 4.0).max(1.0),
            height,
        ))
    }

    fn drag_scroll(&mut self, pointer_y: f32) {
        if let (Some(track), Some(thumb), Some(grab)) =
            (self.scroll_track, self.thumb(), self.scroll_drag)
        {
            let travel = track.3 - thumb.3;
            self.scroll = if travel > 0.0 {
                ((pointer_y - grab - track.1) / travel).clamp(0.0, 1.0) * self.scroll_max
            } else {
                0.0
            };
        }
    }
    fn select(&mut self, path: Option<String>) {
        self.selected = path;
        self.preview = None;
        self.preview_generation = self.preview_generation.wrapping_add(1);
    }

    fn replace_catalog(&mut self, catalog: Catalog) {
        let revision = |catalog: &Catalog, selected: Option<&str>| {
            catalog
                .files
                .iter()
                .find(|file| Some(file.path.as_str()) == selected)
                .map(|file| file.sha.clone())
        };
        if revision(&self.catalog, self.selected.as_deref())
            != revision(&catalog, self.selected.as_deref())
        {
            self.select(self.selected.clone());
        }
        self.catalog = Arc::new(catalog);
        self.rebuild();
    }

    fn accept_preview(&mut self, generation: u64, path: &str, result: Result<Preview, String>) {
        self.preview_busy = false;
        if generation == self.preview_generation && self.selected.as_deref() == Some(path) {
            self.preview = Some(result);
            self.preview_uploaded = false;
        }
    }

    pub(crate) fn settings_view(&self) -> SettingsView {
        SettingsView {
            catalog: self.catalog.clone(),
            preferences: self.preferences.clone(),
            notice: self.notice.clone(),
            problem: self.problem.clone(),
            ready: self.ready,
            busy: self.action_busy,
        }
    }
    fn rebuild(&mut self) {
        self.hits.clear();
        self.rows = rows(&self.catalog.files, self.view, &self.directory, &self.query);
    }
    fn insert(&mut self, text: &str) {
        if self.select_all {
            self.query.clear();
            self.caret = 0;
            self.select_all = false;
        }
        let available = 512usize.saturating_sub(self.query.chars().count());
        let text: String = text
            .chars()
            .filter(|character| !character.is_control())
            .take(available)
            .collect();
        crate::lineedit::insert(&mut self.query, &mut self.caret, &text);
        self.scroll = 0.0;
        self.rebuild();
    }
}

fn contains(rect: Rect, point: (f32, f32)) -> bool {
    point.0 >= rect.0 && point.0 < rect.0 + rect.2 && point.1 >= rect.1 && point.1 < rect.1 + rect.3
}

impl App {
    pub(crate) fn share_panel_active(&self) -> bool {
        self.git.col_visible && self.info.shared_page() && !self.internal_room_active_any()
    }

    pub(crate) fn select_info_page(&mut self, page: state::InfoPage) {
        if self.info.page == page { return; }
        if self.share.search_focus {
            if let Some(text) = self.hangul.flush() { self.share.insert(&text); }
            self.share.pending_search_commit = self.in_preedit;
        }
        self.share.search_focus = false;
        self.share.keyboard_focus = None;
        self.share.scroll_drag = None;
        self.share.hits.clear();
        self.share.search_rect = None;
        self.share.caret_rect = None;
        self.info.clear_info_targets();
        self.info.page = page;
        if page == state::InfoPage::SharedFiles { self.share.last_refresh = None; }
        else { self.info.last_refresh = None; self.info.frozen_since = None; }
        self.chrome_dirty = true;
    }

    pub(crate) fn open_share_panel(&mut self) {
        if self.lite {
            return;
        }
        if self.settings_room_active() {
            self.return_from_settings_room();
        }
        self.info.tab = state::SideTab::Info;
        self.select_info_page(state::InfoPage::SharedFiles);
        // The global entry must expose new arrivals, not a previously filtered folder.
        self.share.home();
        if !self.git.col_visible {
            self.toggle_git_col();
        }
        self.ime_retarget(ImeFocus::ShareSearch);
        self.share.search_focus = true;
        self.share.keyboard_focus = Some(Action::Search);
        self.share.last_refresh = None;
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    pub(crate) fn pump_share_panel(&mut self) {
        while let Ok(update) = self.share.rx.try_recv() {
            match update {
                Update::Catalog(catalog, preferences) => {
                    self.share.refresh_busy = false;
                    self.share.ready = true;
                    self.share.problem = if catalog.is_none() {
                        Some("공유 동기화를 시작하는 중이에요".into())
                    } else {
                        None
                    };
                    if let Some(catalog) = catalog {
                        self.share.replace_catalog(catalog);
                    }
                    match preferences {
                        Ok(preferences) => self.share.preferences = preferences,
                        Err(error) => self.share.notice = Some(error),
                    }
                    self.share.rebuild();
                }
                Update::Preview(generation, path, result) => {
                    self.share.accept_preview(generation, &path, result)
                }
                Update::Action(result) => {
                    self.share.action_busy = false;
                    self.share.notice = result.unwrap_or_else(|error| Some(error));
                    self.share.last_refresh = None;
                }
            }
            self.chrome_dirty = true;
        }
        if self.share.preview.is_none() && !self.share.preview_busy {
            if let Some(path) = self.share.selected.clone() {
                self.share.preview_busy = true;
                let generation = self.share.preview_generation;
                let tx = self.share.tx.clone();
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let result = kasa_mcp::share::engine()
                        .ok_or_else(|| "공유 기능이 아직 준비되지 않았어요".into())
                        .and_then(|engine| read_preview(&engine, &path));
                    let _ = tx.send(Update::Preview(generation, path, result));
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
        }
        let visible = self.share_panel_active();
        let settings =
            self.settings_room_active() && self.settings_scene.category() == SettingsCat::Share;
        if (!visible && !settings)
            || self.share.refresh_busy
            || self
                .share
                .last_refresh
                .is_some_and(|last| last.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        self.share.refresh_busy = true;
        self.share.last_refresh = Some(Instant::now());
        let tx = self.share.tx.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let (catalog, preferences) = match kasa_mcp::share::engine() {
                Some(engine) => (
                    Some(engine.catalog()),
                    engine
                        .shortcut_preferences()
                        .map_err(|error| error.to_string()),
                ),
                None => (None, Ok(Preferences::default())),
            };
            let _ = tx.send(Update::Catalog(catalog, preferences));
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    pub(crate) fn share_click(&mut self, x: f32, y: f32) -> bool {
        let action = self
            .share
            .hits
            .iter()
            .rev()
            .find(|(_, rect)| contains(*rect, (x, y)))
            .map(|(action, _)| action.clone());
        if let Some(action) = action {
            self.share.keyboard_focus = Some(action.clone());
            self.share_action(action);
            return true;
        }
        false
    }

    pub(crate) fn share_action(&mut self, action: Action) {
        self.share.notice = None;
        match action {
            Action::Recent => {
                self.share.view = View::Recent;
                self.share.scroll = 0.0;
                self.share.rebuild();
            }
            Action::Folders => {
                self.share.view = View::Folders;
                self.share.scroll = 0.0;
                self.share.rebuild();
            }
            Action::Folder(path) => {
                self.share.directory = path;
                self.share.view = View::Folders;
                self.share.query.clear();
                self.share.caret = 0;
                self.share.scroll = 0.0;
                self.share.rebuild();
            }
            Action::Back => {
                self.share.directory = self
                    .share
                    .directory
                    .rsplit_once('/')
                    .map(|(parent, _)| parent.to_string())
                    .unwrap_or_default();
                self.share.scroll = 0.0;
                self.share.rebuild();
            }
            Action::Root => {
                self.share.directory.clear();
                self.share.scroll = 0.0;
                self.share.rebuild();
            }
            Action::ClearSearch => {
                self.share.query.clear();
                self.share.caret = 0;
                self.share.scroll = 0.0;
                self.share.rebuild();
            }
            Action::Search => {
                self.share.select_all = false;
                self.ime_retarget(ImeFocus::ShareSearch);
                self.share.search_focus = true;
                self.share.caret = self.share.query.chars().count();
            }
            Action::Refresh => self.share.last_refresh = None,
            Action::ClosePreview => {
                self.share.select(None);
            }
            Action::Settings => {
                self.open_settings_room(Some(SettingsCat::Share));
            }
            Action::OpenPanel => self.open_share_panel(),
            Action::Preview(path) => {
                self.share.select(Some(path));
            }
            Action::Reveal
            | Action::CopyPath
            | Action::Desktop
            | Action::ChooseLocation
            | Action::HideShortcut => {
                if self.share.action_busy {
                    return;
                }
                self.share.action_busy = true;
                let selected = self
                    .share
                    .selected
                    .clone()
                    .unwrap_or_else(|| self.share.directory.clone());
                let tx = self.share.tx.clone();
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let result = perform(action, &selected);
                    let _ = tx.send(Update::Action(result));
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(path: &str, modified: u64) -> File {
        File {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            kind: "text".into(),
            size: 1,
            modified_ms: modified,
            origin: "mini".into(),
            sha: "hash".into(),
        }
    }
    #[test]
    fn folders_are_single_level_and_search_is_global_and_unicode_aware() {
        let files = vec![
            file("시안/안쪽/a.md", 10),
            file("시안/b.md", 20),
            file("시안2/c.md", 30),
            file("root.txt", 1),
        ];
        let root = rows(&files, View::Folders, "", "");
        assert_eq!(root.len(), 3);
        assert_eq!(root.iter().filter(|row| row.folder).count(), 2);
        assert_eq!(rows(&files, View::Folders, "시안", "").len(), 2);
        assert_eq!(
            rows(&files, View::Folders, "시안", "mini c.md")[0].path,
            "시안2/c.md"
        );
        assert_eq!(
            rows(&files, View::Recent, "", &"시안".nfd().collect::<String>()).len(),
            3
        );
        assert_eq!(rows(&files, View::Recent, "", "")[0].path, "시안2/c.md");
    }
    #[test]
    fn query_text_is_bounded_and_does_not_accept_terminal_controls() {
        let mut state = State::default();
        state.insert("한글\n\u{1b}x");
        assert_eq!(state.query, "한글x");
        assert_eq!(state.caret, 3);
        state.caret = 1;
        state.insert("중간");
        assert_eq!(state.query, "한중간글x");
        state.insert(&"가".repeat(1000));
        assert_eq!(state.query.chars().count(), 512);
    }

    #[test]
    fn stale_preview_cannot_replace_a_new_selection_or_a_new_revision() {
        let mut state = State::default();
        state.replace_catalog(Catalog {
            files: vec![file("a.md", 1)],
            ..Default::default()
        });
        state.select(Some("a.md".into()));
        let first = state.preview_generation;
        state.select(Some("b.md".into()));
        state.select(Some("a.md".into()));
        state.accept_preview(first, "a.md", Ok(Preview::Text("old".into())));
        assert!(state.preview.is_none());
        let second = state.preview_generation;
        let mut changed = file("a.md", 2);
        changed.sha = "new".into();
        state.replace_catalog(Catalog {
            files: vec![changed],
            ..Default::default()
        });
        state.accept_preview(second, "a.md", Ok(Preview::Text("old".into())));
        assert!(state.preview.is_none());
        state.accept_preview(
            state.preview_generation,
            "a.md",
            Ok(Preview::Text("new".into())),
        );
        assert!(matches!(state.preview, Some(Ok(Preview::Text(ref text))) if text == "new"));
    }

    #[test]
    fn scrollbar_reaches_both_ends_and_empty_lists_do_not_divide_by_zero() {
        let mut state = State::default();
        state.scroll_track = Some((0.0, 100.0, 8.0, 200.0));
        state.scroll_max = 800.0;
        let thumb = state.thumb().unwrap();
        assert_eq!(thumb.3, 40.0);
        state.scroll_drag = Some(20.0);
        state.drag_scroll(1000.0);
        assert_eq!(state.scroll, 800.0);
        state.drag_scroll(0.0);
        assert_eq!(state.scroll, 0.0);
        state.scroll_max = 0.0;
        assert!(state.thumb().is_none());
        state.drag_scroll(200.0);
        assert_eq!(state.scroll, 0.0);
    }

    #[test]
    fn delayed_search_commit_stays_with_the_field_after_focus_has_left() {
        let mut state = State::default();
        state.pending_search_commit = true;
        assert!(state.deferred_ime(&Ime::Commit("검색".into())));
        assert_eq!(state.query, "검색");
        assert!(!state.deferred_ime(&Ime::Commit("다른 입력".into())));
        state.pending_search_commit = true;
        assert!(!state.deferred_ime(&Ime::Preedit("새 조합".into(), None)));
        assert!(!state.deferred_ime(&Ime::Commit("새 조합".into())));
        assert_eq!(state.query, "검색");
    }

    #[test]
    fn global_entry_exposes_recent_files_without_an_old_folder_or_query() {
        let mut state = State::default();
        state.replace_catalog(Catalog {
            files: vec![file("새 결과.md", 1)],
            ..Default::default()
        });
        state.view = View::Folders;
        state.directory = "옛 폴더".into();
        state.query = "옛 검색".into();
        state.select(Some("옛 파일".into()));
        state.home();
        assert_eq!(state.view, View::Recent);
        assert!(state.query.is_empty());
        assert!(state.directory.is_empty());
        assert!(state.selected.is_none());
        assert_eq!(state.rows[0].path, "새 결과.md");
    }
}
