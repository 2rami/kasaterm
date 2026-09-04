//! wgpu 본창 안에서 그리는 설정 방.
//!
//! 페인터는 파일이나 HTTP를 읽지 않는다. 열 때와 파일을 바꾸는 액션 뒤에 만든
//! 캐시와 `App`의 메모리 값만 스냅샷으로 받아, 렌더 중 I/O와 재차용을 함께 막는다.

use super::*;

pub(crate) type Rect = (f32, f32, f32, f32);

const HEADER_H: f32 = 92.0;
const CONTENT_MAX_W: f32 = 820.0;
const SECTION_GAP: f32 = 18.0;

#[derive(Clone)]
pub(crate) struct PaletteChoice {
    key: String,
    label: String,
    bg: [u8; 4],
    text: [u8; 4],
    ansi: [[u8; 3]; 6],
}

#[derive(Clone)]
pub(crate) struct CharacterChoice {
    name: String,
    slug: Option<&'static str>,
}

#[derive(Clone, Default)]
pub(crate) struct SettingsCache {
    pub(crate) ready: bool,
    palettes: Arc<Vec<PaletteChoice>>,
    characters: Arc<Vec<CharacterChoice>>,
    themes: Arc<Vec<socket::ThemeRow>>,
    models: Arc<Vec<kasa_mcp::character::ModelChoice>>,
    roster: Option<serde_json::Value>,
    open_apps: Arc<Vec<(String, String)>>,
    character_theme: String,
}

impl std::fmt::Debug for SettingsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsCache")
            .field("ready", &self.ready)
            .field("palettes", &self.palettes.len())
            .field("characters", &self.characters.len())
            .field("themes", &self.themes.len())
            .field("models", &self.models.len())
            .field("open_apps", &self.open_apps.len())
            .finish()
    }
}

impl SettingsCache {
    pub(crate) fn refresh(&mut self) {
        let saved = socket::read_settings();
        let mut palettes = Vec::new();
        let system_key = theme::system_theme_key();
        if let Some((_, label, palette)) = theme::THEME_PRESETS
            .iter()
            .find(|(key, _, _)| *key == system_key)
        {
            palettes.push(palette_choice(
                "system",
                &format!("System · {label}"),
                palette,
            ));
        }
        palettes.extend(
            theme::THEME_PRESETS
                .iter()
                .map(|(key, label, palette)| palette_choice(key, label, palette)),
        );
        palettes.extend(theme::custom_themes(&saved).iter().map(|entry| {
            let palette = theme::custom_palette(entry);
            palette_choice(
                &format!("custom:{}", theme::custom_slug(entry)),
                &theme::custom_label(entry),
                &palette,
            )
        }));

        let roster = kasa_mcp::character::characters_json();
        let characters = roster
            .as_ref()
            .map(|value| {
                kasa_mcp::character::member_names(value)
                    .into_iter()
                    .map(|name| CharacterChoice {
                        slug: theme::character_slug(&name),
                        name,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let models = roster
            .as_ref()
            .map(kasa_mcp::character::model_choices)
            .unwrap_or_default();

        self.ready = true;
        self.palettes = Arc::new(palettes);
        self.characters = Arc::new(characters);
        self.themes = Arc::new(socket::theme_rows());
        self.models = Arc::new(models);
        self.roster = roster;
        self.open_apps = Arc::new(proc::open_with_apps().to_vec());
        self.character_theme = socket::read_character_theme();
    }
}

fn palette_choice(key: &str, label: &str, palette: &theme::Palette) -> PaletteChoice {
    let mut ansi = [[0, 0, 0]; 6];
    ansi.copy_from_slice(&palette.ansi[1..7]);
    PaletteChoice {
        key: key.to_string(),
        label: label.to_string(),
        bg: palette.bg,
        text: palette.text,
        ansi,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitCursor {
    Pointer,
    Text,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Category(SettingsCat),
    Setting(SettingsAction),
    Focus(SettingsInput),
    Close,
    FinishOnboarding,
}

#[derive(Clone)]
pub(crate) struct Hit {
    pub(crate) target: Target,
    pub(crate) rect: Rect,
    pub(crate) cursor: HitCursor,
}

impl std::fmt::Debug for Hit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hit")
            .field("rect", &self.rect)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

pub(crate) struct PaintOutput {
    pub(crate) hits: Vec<Hit>,
    pub(crate) content_h: f32,
    pub(crate) view_h: f32,
    pub(crate) caret_rect: Option<Rect>,
}

pub(crate) struct Snapshot {
    pub(crate) area: Rect,
    pub(crate) cat: SettingsCat,
    pub(crate) cursor: (f32, f32),
    pub(crate) scroll: f32,
    pub(crate) caret_on: bool,
    pub(crate) input: Option<SettingsInput>,
    pub(crate) preedit: String,
    pub(crate) first_run: bool,
    pub(crate) cwd_mode: String,
    pub(crate) file_open_mode: String,
    pub(crate) file_open_app: String,
    pub(crate) file_open_cmd: String,
    pub(crate) file_tree_default: bool,
    pub(crate) footer_default: bool,
    pub(crate) autosave_ms: u64,
    pub(crate) shell: String,
    pub(crate) theme: String,
    pub(crate) accent: String,
    pub(crate) shape: String,
    pub(crate) min_contrast: f32,
    pub(crate) font_size: f32,
    pub(crate) ui_zoom: f32,
    pub(crate) wheel_gain: f32,
    pub(crate) status_h: f32,
    pub(crate) footer_h: f32,
    pub(crate) tabs_on_top: bool,
    pub(crate) cursor_shape: cursor::CursorShape,
    pub(crate) cursor_thickness: f32,
    pub(crate) mouse_cursor: String,
    pub(crate) claude_persona: bool,
    pub(crate) shim_inject: bool,
    pub(crate) claude_model: String,
    pub(crate) claude_effort: String,
    pub(crate) claude_extra: String,
    pub(crate) claude_accounts: Vec<socket::ClaudeAccount>,
    pub(crate) claude_account: String,
    pub(crate) codex_accounts: Vec<socket::CodexAccount>,
    pub(crate) codex_account: String,
    pub(crate) account_autoswitch: bool,
    pub(crate) account_autoswitch_pct: f32,
    pub(crate) statusbar_all_accounts: bool,
    pub(crate) palettes: Arc<Vec<PaletteChoice>>,
    pub(crate) characters: Arc<Vec<CharacterChoice>>,
    pub(crate) themes: Arc<Vec<socket::ThemeRow>>,
    pub(crate) character_theme: String,
    pub(crate) open_apps: Arc<Vec<(String, String)>>,
    pub(crate) student_selected: Option<String>,
    pub(crate) student_name: String,
    pub(crate) student_persona: String,
    pub(crate) student_caret: usize,
    pub(crate) student_model: String,
    pub(crate) student_backend: String,
    pub(crate) models: Arc<Vec<kasa_mcp::character::ModelChoice>>,
    pub(crate) settings_caret: usize,
    pub(crate) feedback_body: String,
    pub(crate) feedback_caret: usize,
    pub(crate) feedback_diag: bool,
}

impl App {
    pub(crate) fn native_settings_snapshot(&self, area: Rect) -> Option<Snapshot> {
        if !self.settings_room_active() {
            return None;
        }
        let scene = &self.settings_scene;
        let cache = scene.cache();
        let selected = self.students_selected.as_deref();
        let (student_model, student_backend) = cache
            .roster
            .as_ref()
            .zip(selected)
            .map(|(roster, name)| {
                (
                    kasa_mcp::character::model_for(roster, name).unwrap_or_default(),
                    kasa_mcp::character::backend_for(roster, name).unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        Some(Snapshot {
            area,
            cat: scene.category(),
            cursor: self.cursor_px,
            scroll: scene.scroll(),
            caret_on: self.last_blink_on,
            input: self.settings_input,
            preedit: self.preedit.clone(),
            first_run: scene.first_run(),
            cwd_mode: self.set_cwd_mode.clone(),
            file_open_mode: self.set_file_open_mode.clone(),
            file_open_app: self.set_file_open_app.clone(),
            file_open_cmd: self.set_file_open_cmd.clone(),
            file_tree_default: self.set_file_tree_default,
            footer_default: self.set_footer_default,
            autosave_ms: self.set_autosave.map_or(0, |d| d.as_millis() as u64),
            shell: self.set_shell.clone(),
            theme: theme::theme_name(),
            accent: theme::accent_name().to_string(),
            shape: theme::shape_name().to_string(),
            min_contrast: theme::min_contrast(),
            font_size: self.font_size,
            ui_zoom: self.ui_zoom,
            wheel_gain: self.set_wheel_pixel_gain,
            status_h: self.set_status_h,
            footer_h: self.set_pane_footer_h,
            tabs_on_top: self.tabs_on_top,
            cursor_shape: self.cursor_shape,
            cursor_thickness: self.cursor_thickness,
            mouse_cursor: self.mouse_cursor.clone(),
            claude_persona: self.set_claude_persona,
            shim_inject: self.set_shim_inject,
            claude_model: self.set_claude_model.clone(),
            claude_effort: self.set_claude_effort.clone(),
            claude_extra: self.set_claude_extra.clone(),
            claude_accounts: self.set_claude_accounts.clone(),
            claude_account: self.set_claude_account.clone(),
            codex_accounts: self.set_codex_accounts.clone(),
            codex_account: self.set_codex_account.clone(),
            account_autoswitch: self.set_account_autoswitch,
            account_autoswitch_pct: self.set_account_autoswitch_pct,
            statusbar_all_accounts: self.set_statusbar_all_accounts,
            palettes: cache.palettes.clone(),
            characters: cache.characters.clone(),
            themes: cache.themes.clone(),
            character_theme: cache.character_theme.clone(),
            open_apps: cache.open_apps.clone(),
            student_selected: self.students_selected.clone(),
            student_name: self.students_name.clone(),
            student_persona: self.students_persona.clone(),
            student_caret: self.students_caret,
            student_model,
            student_backend,
            models: cache.models.clone(),
            settings_caret: self.settings_caret,
            feedback_body: self.feedback_body.clone(),
            feedback_caret: self.feedback_caret,
            feedback_diag: self.feedback_diag,
        })
    }

    pub(crate) fn finish_native_settings_paint(&mut self, output: PaintOutput) {
        self.settings_scene.finish_paint(
            output.hits,
            output.content_h,
            output.view_h,
            output.caret_rect,
        );
        if let (Some(window), Some((x, y, w, h))) =
            (self.window.as_ref(), self.settings_scene.caret_rect())
        {
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(x as f64, y as f64),
                winit::dpi::LogicalSize::new(w.max(1.0) as f64, h.max(1.0) as f64),
            );
        }
    }

    pub(crate) fn native_settings_contains(&self, x: f32, y: f32) -> bool {
        self.settings_room_active()
            && self.window.as_ref().is_some_and(|w| {
                let s = self.effective_scale();
                let size = w.inner_size();
                x >= self.effective_sidebar_w()
                    && x <= size.width as f32 / s
                    && y >= TITLE_HEIGHT
                    && y <= size.height as f32 / s
            })
    }

    pub(crate) fn native_settings_cursor(&self, x: f32, y: f32) -> winit::window::CursorIcon {
        match self.settings_scene.hit_at(x, y).map(|hit| hit.cursor) {
            Some(HitCursor::Pointer) => winit::window::CursorIcon::Pointer,
            Some(HitCursor::Text) => winit::window::CursorIcon::Text,
            _ => winit::window::CursorIcon::Default,
        }
    }

    pub(crate) fn native_settings_click(&mut self, x: f32, y: f32) -> bool {
        if !self.settings_room_active() {
            return false;
        }
        let target = self
            .settings_scene
            .hit_at(x, y)
            .map(|hit| hit.target.clone());
        match target {
            Some(Target::Category(cat)) => {
                self.native_settings_blur();
                self.settings_scene.set_category(cat);
            }
            Some(Target::Setting(action)) => self.native_settings_apply(action),
            Some(Target::Focus(field)) => self.native_settings_focus(field),
            Some(Target::Close) => {
                self.native_settings_blur();
                self.return_from_settings_room();
                return true;
            }
            Some(Target::FinishOnboarding) => {
                if crate::onboarding::skip().is_ok() {
                    self.settings_scene.finish_onboarding();
                    self.set_toast("기본값으로 시작했어요".to_string());
                }
            }
            None => self.native_settings_blur(),
        }
        self.chrome_dirty = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    fn native_settings_apply(&mut self, action: SettingsAction) {
        let refresh = matches!(
            action,
            SettingsAction::ThemeMode(_)
                | SettingsAction::StartCustomTheme
                | SettingsAction::ResetCustomTheme
                | SettingsAction::DeleteCustomTheme(_)
                | SettingsAction::SelectTheme(_)
                | SettingsAction::ExportTheme
                | SettingsAction::DeleteTheme(_)
                | SettingsAction::RefreshStudentAssets
        );
        self.settings_apply(action);
        if refresh {
            self.settings_scene.refresh_cache();
        }
    }

    fn native_settings_focus(&mut self, field: SettingsInput) {
        if self.settings_input != Some(field) {
            self.ime_retarget(crate::ImeFocus::Settings(field));
        }
        self.settings_input = Some(field);
        match field {
            SettingsInput::CwdPath => self.settings_caret = self.set_cwd_mode.chars().count(),
            SettingsInput::FileOpenCmd => {
                self.settings_caret = self.set_file_open_cmd.chars().count()
            }
            SettingsInput::Shell => self.settings_caret = self.set_shell.chars().count(),
            SettingsInput::ClaudeExtra => {
                self.settings_caret = self.set_claude_extra.chars().count()
            }
            SettingsInput::StudentName => self.settings_caret = self.students_name.chars().count(),
            SettingsInput::StudentPersona => {
                self.students_caret = self.students_persona.chars().count()
            }
            SettingsInput::FeedbackBody => self.feedback_caret = self.feedback_body.chars().count(),
            SettingsInput::ThemeLabel | SettingsInput::PaletteHex(_) => {}
        }
        self.ime_focus = Some(crate::ImeFocus::Settings(field));
        self.preedit.clear();
        self.in_preedit = false;
    }

    pub(crate) fn native_settings_insert_into(&mut self, field: SettingsInput, text: &str) {
        match field {
            SettingsInput::CwdPath => {
                crate::lineedit::insert(&mut self.set_cwd_mode, &mut self.settings_caret, text)
            }
            SettingsInput::FileOpenCmd => {
                crate::lineedit::insert(&mut self.set_file_open_cmd, &mut self.settings_caret, text)
            }
            SettingsInput::Shell => {
                crate::lineedit::insert(&mut self.set_shell, &mut self.settings_caret, text)
            }
            SettingsInput::ClaudeExtra => {
                crate::lineedit::insert(&mut self.set_claude_extra, &mut self.settings_caret, text)
            }
            SettingsInput::StudentName => {
                crate::lineedit::insert(&mut self.students_name, &mut self.settings_caret, text)
            }
            SettingsInput::StudentPersona => {
                crate::lineedit::insert(&mut self.students_persona, &mut self.students_caret, text)
            }
            SettingsInput::FeedbackBody => {
                crate::lineedit::insert(&mut self.feedback_body, &mut self.feedback_caret, text)
            }
            SettingsInput::ThemeLabel => {
                if let Some((_, buffer)) = self.theme_label_edit.as_mut() {
                    crate::lineedit::insert(buffer, &mut self.settings_caret, text);
                }
            }
            SettingsInput::PaletteHex(slot) => {
                crate::lineedit::insert(&mut self.set_palette_edit, &mut self.settings_caret, text);
                self.apply_palette_edit(slot);
            }
        }
        if matches!(
            field,
            SettingsInput::CwdPath
                | SettingsInput::FileOpenCmd
                | SettingsInput::Shell
                | SettingsInput::ClaudeExtra
        ) {
            self.settings_save();
        }
        self.chrome_dirty = true;
    }

    pub(crate) fn native_settings_blur(&mut self) {
        if let Some(field) = self.settings_input {
            if let Some(text) = self.hangul.flush() {
                self.native_settings_insert_into(field, &text);
            }
        }
        if self.settings_input == Some(SettingsInput::StudentPersona) {
            self.flush_student_persona();
        }
        if self.settings_input == Some(SettingsInput::StudentName) {
            self.flush_student_name();
            self.settings_scene.refresh_cache();
        }
        if self.settings_input == Some(SettingsInput::ThemeLabel) {
            self.flush_theme_label();
            self.settings_scene.refresh_cache();
        }
        self.settings_input = None;
        if matches!(self.ime_focus, Some(crate::ImeFocus::Settings(_))) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    pub(crate) fn native_settings_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if !self.settings_room_active() {
            return false;
        }
        if event.state != ElementState::Pressed {
            return true;
        }
        let Some(field) = self.settings_input else {
            let at = SettingsCat::ALL
                .iter()
                .position(|cat| *cat == self.settings_scene.category())
                .unwrap_or(0);
            let next = match event.logical_key {
                Key::Named(NamedKey::ArrowUp) => at.saturating_sub(1),
                Key::Named(NamedKey::ArrowDown) => (at + 1).min(SettingsCat::ALL.len() - 1),
                _ => return false,
            };
            self.settings_scene.set_category(SettingsCat::ALL[next]);
            self.chrome_dirty = true;
            return true;
        };

        self.ime_retarget(crate::ImeFocus::Settings(field));
        let host = self.host_mod();
        if host && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV)) {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                if let Ok(mut text) = clipboard.get_text() {
                    if !is_multiline(field) {
                        text = text.replace(['\n', '\r'], " ");
                    }
                    self.native_settings_insert_into(field, &text);
                }
            }
            return true;
        }
        if host {
            return true;
        }

        #[cfg(target_os = "macos")]
        {
            let one = |text: &str| {
                let mut chars = text.chars();
                chars.next().filter(|_| chars.next().is_none())
            };
            let typed = event.text.as_ref().and_then(|text| one(text)).or_else(|| {
                if let Key::Character(text) = &event.logical_key {
                    one(text)
                } else {
                    None
                }
            });
            if let Some(ch) = typed.filter(|ch| is_jamo(*ch)) {
                if let Some(text) = self.hangul.feed(ch) {
                    self.native_settings_insert_into(field, &text);
                }
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.in_preedit = !self.preedit.is_empty();
                self.chrome_dirty = true;
                return true;
            }
            if matches!(event.logical_key, Key::Named(NamedKey::Backspace))
                && self.hangul.backspace()
            {
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.in_preedit = !self.preedit.is_empty();
                self.chrome_dirty = true;
                return true;
            }
            if let Some(text) = self.hangul.flush() {
                self.native_settings_insert_into(field, &text);
            }
            self.preedit.clear();
            self.in_preedit = false;
        }

        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.native_settings_blur();
            return true;
        }

        match event.logical_key {
            Key::Named(NamedKey::Enter) if is_multiline(field) => {
                self.native_settings_insert_into(field, "\n");
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.native_settings_blur();
                self.set_toast("저장됐어요".to_string());
                return true;
            }
            Key::Named(NamedKey::Backspace)
            | Key::Named(NamedKey::ArrowLeft)
            | Key::Named(NamedKey::ArrowRight)
            | Key::Named(NamedKey::Home)
            | Key::Named(NamedKey::End) => {}
            Key::Named(NamedKey::Space) => {
                self.native_settings_insert_into(field, " ");
                return true;
            }
            Key::Character(ref text) => {
                if !(self.ime_active || self.in_preedit) || !text.chars().any(is_hangul_codepoint) {
                    self.native_settings_insert_into(field, text);
                }
                return true;
            }
            _ => return true,
        }
        let changed = field_buffer(self, field).is_some_and(|(buffer, caret)| {
            crate::lineedit::key(buffer, caret, &event.logical_key)
                == crate::lineedit::LineEditAction::Edited
        });
        if changed {
            if matches!(
                field,
                SettingsInput::CwdPath
                    | SettingsInput::FileOpenCmd
                    | SettingsInput::Shell
                    | SettingsInput::ClaudeExtra
            ) {
                self.settings_save();
            }
            if let SettingsInput::PaletteHex(slot) = field {
                self.apply_palette_edit(slot);
            }
        }
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn native_settings_ime(&mut self, ime: winit::event::Ime) {
        if !self.settings_room_active() {
            return;
        }
        match ime {
            winit::event::Ime::Enabled => self.ime_active = true,
            winit::event::Ime::Disabled => {
                self.ime_active = false;
                self.in_preedit = false;
                self.preedit.clear();
            }
            winit::event::Ime::Preedit(text, _) => {
                if let Some(field) = self.settings_input {
                    self.ime_focus = Some(crate::ImeFocus::Settings(field));
                    self.ime_active = true;
                    self.in_preedit = !text.is_empty();
                    self.preedit = text;
                }
            }
            winit::event::Ime::Commit(text) => {
                if let Some(field) = self.settings_input {
                    self.native_settings_insert_into(field, &text);
                }
                self.in_preedit = false;
                self.preedit.clear();
            }
        }
        self.chrome_dirty = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub(crate) fn native_settings_wheel(&mut self, delta: winit::event::MouseScrollDelta) {
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => y * 42.0,
            winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        if self.settings_scene.scroll_by(-dy) {
            self.chrome_dirty = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    pub(crate) fn native_settings_drop(&mut self, path: std::path::PathBuf) -> bool {
        if !self.settings_room_active() {
            return false;
        }
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
            match socket::import_theme(&path) {
                Ok(id) => self.set_toast(format!("'{id}' 을 가져왔어요 — 테마에서 고르면 켜져요")),
                Err(error) => self.set_toast(format!("테마를 못 가져왔어요 — {error}")),
            }
            socket::invalidate_theme_rows();
            kasa_mcp::character::invalidate_active_theme();
            theme::invalidate_roster();
            self.settings_scene.refresh_cache();
            return true;
        }
        if self.settings_scene.category() != SettingsCat::Students {
            self.set_toast("테마 압축 파일은 어느 설정 화면에서든 놓을 수 있어요".to_string());
            return true;
        }
        let Some(slug) = self
            .students_selected
            .as_deref()
            .and_then(theme::character_slug)
        else {
            self.set_toast("먼저 캐릭터를 골라 주세요".to_string());
            return true;
        };
        let theme_id = self.settings_scene.cache().character_theme.clone();
        if theme_id.is_empty() {
            self.set_toast("기본 테마에는 못 구워요 — 테마를 복제한 뒤 놓아 주세요".to_string());
            return true;
        }
        let Some(root) = kasa_mcp::character::themes_root() else {
            return true;
        };
        match crate::themegen::place_themegen_ref(&root.join(theme_id), slug, &path) {
            Ok(_) => self.set_toast("참조 그림을 놓았어요".to_string()),
            Err(error) => self.set_toast(format!("그림을 못 놓았어요 — {error}")),
        }
        true
    }
}

fn is_multiline(field: SettingsInput) -> bool {
    matches!(
        field,
        SettingsInput::StudentPersona | SettingsInput::FeedbackBody
    )
}

fn is_jamo(ch: char) -> bool {
    (0x3130..=0x318f).contains(&(ch as u32))
}

fn field_buffer(app: &mut App, field: SettingsInput) -> Option<(&mut String, &mut usize)> {
    match field {
        SettingsInput::CwdPath => Some((&mut app.set_cwd_mode, &mut app.settings_caret)),
        SettingsInput::FileOpenCmd => Some((&mut app.set_file_open_cmd, &mut app.settings_caret)),
        SettingsInput::Shell => Some((&mut app.set_shell, &mut app.settings_caret)),
        SettingsInput::ClaudeExtra => Some((&mut app.set_claude_extra, &mut app.settings_caret)),
        SettingsInput::StudentName => Some((&mut app.students_name, &mut app.settings_caret)),
        SettingsInput::StudentPersona => Some((&mut app.students_persona, &mut app.students_caret)),
        SettingsInput::FeedbackBody => Some((&mut app.feedback_body, &mut app.feedback_caret)),
        SettingsInput::ThemeLabel => app
            .theme_label_edit
            .as_mut()
            .map(|(_, buffer)| (buffer, &mut app.settings_caret)),
        SettingsInput::PaletteHex(_) => Some((&mut app.set_palette_edit, &mut app.settings_caret)),
    }
}

pub(crate) fn paint(g: &mut gpu::GpuRenderer, snapshot: &Snapshot) -> PaintOutput {
    let (ax, ay, aw, ah) = snapshot.area;
    let nav_w = if aw < 760.0 { 154.0 } else { 190.0 };
    let mut hits = Vec::new();
    let mut caret_rect = None;

    g.rect(ax, ay, aw, ah, theme::bg());
    g.rect(ax, ay, nav_w, ah, theme::panel_bg());
    g.rect(ax + nav_w - 1.0, ay, 1.0, ah, theme::border());
    draw_text(
        g,
        ax + 20.0,
        ay + 20.0,
        "설정 방",
        18.0,
        theme::text(),
        true,
    );
    draw_text(
        g,
        ax + 20.0,
        ay + 47.0,
        "앱의 작업 환경",
        11.0,
        theme::text_mute(),
        false,
    );

    let mut ny = ay + 82.0;
    for cat in SettingsCat::ALL {
        let (label, icon, _) = category_meta(cat);
        let rect = (ax + 10.0, ny, nav_w - 20.0, 36.0);
        let selected = cat == snapshot.cat;
        let hover = contains(rect, snapshot.cursor);
        if selected || hover {
            round_rect(
                g,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                theme::radius_md(),
                if selected {
                    theme::surface_active()
                } else {
                    theme::surface_hover()
                },
            );
        }
        if selected {
            g.rect(rect.0, rect.1 + 8.0, 2.0, rect.3 - 16.0, theme::accent());
        }
        g.queue_icon(
            icon,
            rect.0 + 12.0,
            rect.1 + 10.0,
            15.0,
            if selected {
                theme::text()
            } else {
                theme::text_mute()
            },
        );
        draw_text(
            g,
            rect.0 + 36.0,
            rect.1 + 10.0,
            label,
            13.0,
            if selected {
                theme::text()
            } else {
                theme::text_dim()
            },
            selected,
        );
        register(&mut hits, Target::Category(cat), rect, HitCursor::Pointer);
        g.hover_pointer |= hover;
        ny += 39.0;
    }

    let close = (ax + 12.0, ay + ah - 48.0, nav_w - 24.0, 34.0);
    let close_hover = contains(close, snapshot.cursor);
    if close_hover {
        round_rect(
            g,
            close.0,
            close.1,
            close.2,
            close.3,
            theme::radius_md(),
            theme::surface_hover(),
        );
    }
    g.queue_icon(
        "chevron-left",
        close.0 + 10.0,
        close.1 + 9.0,
        15.0,
        theme::text_dim(),
    );
    draw_text(
        g,
        close.0 + 33.0,
        close.1 + 9.0,
        "작업 방으로",
        12.0,
        theme::text_dim(),
        false,
    );
    register(&mut hits, Target::Close, close, HitCursor::Pointer);

    let content_x = ax + nav_w + if aw < 760.0 { 22.0 } else { 38.0 };
    let content_w = (aw - nav_w - if aw < 760.0 { 44.0 } else { 76.0 })
        .max(180.0)
        .min(CONTENT_MAX_W);
    let (title, _, blurb) = category_meta(snapshot.cat);
    draw_text(g, content_x, ay + 22.0, title, 24.0, theme::text(), true);
    draw_text(
        g,
        content_x,
        ay + 55.0,
        blurb,
        12.5,
        theme::text_dim(),
        false,
    );
    g.rect(
        content_x,
        ay + HEADER_H - 1.0,
        content_w,
        1.0,
        theme::border(),
    );

    let body_top = ay + HEADER_H + 14.0;
    let body_bottom = ay + ah - 12.0;
    let view_h = (body_bottom - body_top).max(0.0);
    g.push_clip(content_x, body_top, content_w, view_h);
    let mut y = body_top - snapshot.scroll;
    if snapshot.first_run {
        let (h, rect) =
            crate::native_onboarding::paint(g, content_x, y, content_w, snapshot.cursor);
        register_clipped(
            g,
            &mut hits,
            Target::FinishOnboarding,
            rect,
            HitCursor::Pointer,
        );
        y += h + SECTION_GAP;
    }

    match snapshot.cat {
        SettingsCat::General => paint_general(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        SettingsCat::Appearance => {
            paint_appearance(g, snapshot, &mut hits, content_x, &mut y, content_w)
        }
        SettingsCat::Shell => paint_shell(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        SettingsCat::Claude => paint_claude(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        SettingsCat::Theme => paint_themes(g, snapshot, &mut hits, content_x, &mut y, content_w),
        SettingsCat::Students => paint_students(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        SettingsCat::Feedback => paint_feedback(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
    }
    g.pop_clip();

    PaintOutput {
        hits,
        content_h: (y + snapshot.scroll - body_top + 18.0).max(view_h),
        view_h,
        caret_rect,
    }
}

fn paint_general(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(
        g,
        x,
        *y,
        "시작과 파일",
        "새 작업 방과 파일을 여는 기본 동작입니다",
    );
    *y += 48.0;
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            (
                "마지막 위치",
                s.cwd_mode == "last",
                SettingsAction::CwdMode("last"),
            ),
            ("홈", s.cwd_mode == "home", SettingsAction::CwdMode("home")),
            (
                "직접 지정",
                s.cwd_mode != "last" && s.cwd_mode != "home",
                SettingsAction::CwdMode("custom"),
            ),
        ],
    );
    *y += 42.0;
    if s.cwd_mode != "last" && s.cwd_mode != "home" {
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            w,
            "시작 폴더",
            &s.cwd_mode,
            SettingsInput::CwdPath,
            s.settings_caret,
            false,
        );
        *y += 58.0;
    }
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            (
                "카사텀",
                s.file_open_mode == "builtin",
                SettingsAction::FileOpenMode("builtin"),
            ),
            (
                "앱",
                matches!(s.file_open_mode.as_str(), "app" | "system"),
                SettingsAction::FileOpenMode("app"),
            ),
            (
                "터미널",
                s.file_open_mode == "terminal",
                SettingsAction::FileOpenMode("terminal"),
            ),
        ],
    );
    *y += 42.0;
    if matches!(s.file_open_mode.as_str(), "app" | "system") && !s.open_apps.is_empty() {
        let choices: Vec<(String, bool, SettingsAction)> = s
            .open_apps
            .iter()
            .take(5)
            .map(|(name, _)| {
                (
                    name.clone(),
                    s.file_open_app == *name,
                    SettingsAction::FileOpenApp(name.clone()),
                )
            })
            .collect();
        chips_owned(g, s, hits, x, y, w, choices);
    }
    if s.file_open_mode == "terminal" {
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            w,
            "편집기 명령",
            &s.file_open_cmd,
            SettingsInput::FileOpenCmd,
            s.settings_caret,
            false,
        );
        *y += 58.0;
    }
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "파일 트리 기본으로 열기",
        s.file_tree_default,
        SettingsAction::ToggleFileTree,
    );
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "pane 하단바 기본으로 켜기",
        s.footer_default,
        SettingsAction::ToggleFooter,
    );

    *y += 12.0;
    section_title(
        g,
        x,
        *y,
        "편집과 스크롤",
        "자주 바꾸지 않는 입력 감각만 모았습니다",
    );
    *y += 48.0;
    let autosave = [("끔", 0), ("0.3초", 300), ("1초", 1000), ("3초", 3000)];
    let autosave_cells: Vec<(&str, bool, SettingsAction)> = autosave
        .iter()
        .map(|(label, ms)| {
            (
                *label,
                s.autosave_ms == *ms,
                SettingsAction::AutosaveDelay(*ms),
            )
        })
        .collect();
    segmented(g, s, hits, x, *y, w, &autosave_cells);
    *y += 42.0;
    let gain = (s.wheel_gain * 100.0).round() as u32;
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            ("차분하게", gain == 30, SettingsAction::WheelPixelGain(30)),
            ("보통", gain == 60, SettingsAction::WheelPixelGain(60)),
            ("빠르게", gain == 100, SettingsAction::WheelPixelGain(100)),
            (
                "아주 빠르게",
                gain == 150,
                SettingsAction::WheelPixelGain(150),
            ),
        ],
    );
    *y += 52.0;
    stepper_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "창 상태줄 높이",
        &format!("{:.0}px", s.status_h),
        SettingsAction::StatusBarH((s.status_h - 2.0).max(socket::STATUS_H_MIN) as u32),
        SettingsAction::StatusBarH((s.status_h + 2.0).min(socket::STATUS_H_MAX) as u32),
    );
    stepper_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "pane 하단바 높이",
        &format!("{:.0}px", s.footer_h),
        SettingsAction::PaneFooterH((s.footer_h - 2.0).max(socket::PANE_FOOTER_H_MIN) as u32),
        SettingsAction::PaneFooterH((s.footer_h + 2.0).min(socket::PANE_FOOTER_H_MAX) as u32),
    );
}

fn paint_appearance(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(
        g,
        x,
        *y,
        "커서 작업대",
        "여덟 모양을 같은 전각 셀에서 비교합니다",
    );
    *y += 48.0;
    let cols = if w >= 620.0 { 4 } else { 2 };
    let gap = 10.0;
    let card_w = (w - gap * (cols - 1) as f32) / cols as f32;
    let card_h = 78.0;
    let labels = [
        "블록",
        "빔",
        "밑줄",
        "프레임",
        "괄호",
        "쌍선",
        "윗줄",
        "모서리",
    ];
    for (i, shape) in cursor::CursorShape::ALL.into_iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let rect = (
            x + col as f32 * (card_w + gap),
            *y + row as f32 * (card_h + gap),
            card_w,
            card_h,
        );
        choice_card(
            g,
            s,
            hits,
            rect,
            s.cursor_shape == shape,
            Target::Setting(SettingsAction::CursorShape(shape)),
        );
        draw_text(
            g,
            rect.0 + 12.0,
            rect.1 + 11.0,
            labels[i],
            12.0,
            theme::text_dim(),
            s.cursor_shape == shape,
        );
        cursor_sample(
            g,
            shape,
            rect.0 + rect.2 - 50.0,
            rect.1 + 23.0,
            s.cursor_thickness,
            true,
        );
    }
    let rows = (8 + cols - 1) / cols;
    *y += rows as f32 * (card_h + gap);
    let selected_col = cursor::CursorShape::ALL
        .iter()
        .position(|shape| *shape == s.cursor_shape)
        .unwrap_or(0)
        % cols;
    let preview = (x + selected_col as f32 * (card_w + gap), *y, card_w, 64.0);
    round_rect(
        g,
        preview.0,
        preview.1,
        preview.2,
        preview.3,
        theme::radius_md(),
        theme::surface(),
    );
    draw_text(
        g,
        preview.0 + 12.0,
        preview.1 + 10.0,
        "실제 깜빡임",
        10.5,
        theme::text_mute(),
        false,
    );
    if s.caret_on {
        cursor_sample(
            g,
            s.cursor_shape,
            preview.0 + preview.2 - 52.0,
            preview.1 + 19.0,
            s.cursor_thickness,
            false,
        );
    }
    *y += 80.0;
    let thickness: Vec<(&str, bool, SettingsAction)> = [1u8, 2, 3, 4, 6]
        .iter()
        .map(|px| {
            let label = match px {
                1 => "1px",
                2 => "2px",
                3 => "3px",
                4 => "4px",
                _ => "6px",
            };
            (
                label,
                (s.cursor_thickness - *px as f32).abs() < 0.1,
                SettingsAction::CursorThickness(*px),
            )
        })
        .collect();
    segmented(g, s, hits, x, *y, w, &thickness);
    *y += 48.0;
    section_title(
        g,
        x,
        *y,
        "마우스 포인터",
        "텍스트 입력 캐럿과 터미널 위 포인터는 서로 다른 설정입니다",
    );
    *y += 48.0;
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            (
                "화살표",
                s.mouse_cursor != "ibeam",
                SettingsAction::MouseCursor("arrow"),
            ),
            (
                "I-빔",
                s.mouse_cursor == "ibeam",
                SettingsAction::MouseCursor("ibeam"),
            ),
        ],
    );
    *y += 54.0;

    section_title(
        g,
        x,
        *y,
        "색과 형태",
        "현재 테마 토큰을 모든 네이티브 화면이 함께 씁니다",
    );
    *y += 48.0;
    let grid_cols = if w >= 600.0 { 3 } else { 2 };
    let pw = (w - gap * (grid_cols - 1) as f32) / grid_cols as f32;
    let ph = 92.0;
    for (i, palette) in s.palettes.iter().enumerate() {
        let rect = (
            x + (i % grid_cols) as f32 * (pw + gap),
            *y + (i / grid_cols) as f32 * (ph + gap),
            pw,
            ph,
        );
        choice_card(
            g,
            s,
            hits,
            rect,
            s.theme == palette.key,
            Target::Setting(SettingsAction::ThemeMode(palette.key.clone())),
        );
        round_rect(
            g,
            rect.0 + 10.0,
            rect.1 + 10.0,
            rect.2 - 20.0,
            42.0,
            theme::radius_sm(),
            palette.bg,
        );
        draw_text(
            g,
            rect.0 + 18.0,
            rect.1 + 23.0,
            "가 Aa",
            13.0,
            palette.text,
            true,
        );
        for (j, color) in palette.ansi.iter().enumerate() {
            g.rect(
                rect.0 + 12.0 + j as f32 * 13.0,
                rect.1 + 60.0,
                9.0,
                9.0,
                [color[0], color[1], color[2], 255],
            );
        }
        let label = fit(g, &palette.label, rect.2 - 105.0, 11.0, false);
        draw_text(
            g,
            rect.0 + 94.0,
            rect.1 + 59.0,
            &label,
            11.0,
            theme::text_dim(),
            false,
        );
    }
    let palette_rows = (s.palettes.len() + grid_cols - 1) / grid_cols;
    *y += palette_rows as f32 * (ph + gap) + 4.0;
    button(
        g,
        s,
        hits,
        (x, *y, 150.0, 34.0),
        "현재 색으로 복제",
        Target::Setting(SettingsAction::StartCustomTheme),
        false,
    );
    *y += 48.0;
    let accents: Vec<(String, bool, SettingsAction)> = theme::ACCENT_PRESETS
        .iter()
        .map(|(name, _)| {
            (
                (*name).to_string(),
                s.accent == *name,
                SettingsAction::Accent((*name).to_string()),
            )
        })
        .collect();
    chips_owned(g, s, hits, x, y, w, accents);
    let shapes: Vec<(&str, bool, SettingsAction)> = theme::SHAPE_PRESETS
        .iter()
        .map(|(key, label, _)| (*label, s.shape == *key, SettingsAction::Shape(key)))
        .collect();
    segmented(g, s, hits, x, *y, w, &shapes);
    *y += 46.0;
    let contrast: Vec<(&str, bool, SettingsAction)> = theme::CONTRAST_PRESETS
        .iter()
        .map(|(label, value)| {
            (
                *label,
                (s.min_contrast - *value).abs() < 0.01,
                SettingsAction::MinContrast(label),
            )
        })
        .collect();
    segmented(g, s, hits, x, *y, w, &contrast);
    *y += 54.0;
    stepper_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "글자 크기",
        &format!("{:.0}px", s.font_size),
        SettingsAction::FontSizeDelta(-1),
        SettingsAction::FontSizeDelta(1),
    );
    stepper_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "UI 배율",
        &format!("{:.0}%", s.ui_zoom * 100.0),
        SettingsAction::UiZoomDelta(-1),
        SettingsAction::UiZoomDelta(1),
    );
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            (
                "탭을 위에",
                s.tabs_on_top,
                SettingsAction::TabPosition("top"),
            ),
            (
                "탭을 옆에",
                !s.tabs_on_top,
                SettingsAction::TabPosition("side"),
            ),
        ],
    );
    *y += 44.0;
    button(
        g,
        s,
        hits,
        (x, *y, 148.0, 34.0),
        "배율 1:1로 되돌리기",
        Target::Setting(SettingsAction::ResetScale),
        false,
    );
    *y += 46.0;
}

fn paint_shell(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(
        g,
        x,
        *y,
        "새 pane의 셸",
        "이미 열린 pane은 그대로 두고 다음 pane부터 적용합니다",
    );
    *y += 48.0;
    let known = matches!(s.shell.as_str(), "" | "/bin/zsh" | "/bin/bash");
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            (
                "시스템 기본",
                s.shell.is_empty(),
                SettingsAction::ShellPreset(String::new()),
            ),
            (
                "zsh",
                s.shell == "/bin/zsh",
                SettingsAction::ShellPreset("/bin/zsh".to_string()),
            ),
            (
                "bash",
                s.shell == "/bin/bash",
                SettingsAction::ShellPreset("/bin/bash".to_string()),
            ),
        ],
    );
    *y += 44.0;
    text_field(
        g,
        s,
        hits,
        caret,
        x,
        *y,
        w,
        "직접 경로",
        if known { "" } else { &s.shell },
        SettingsInput::Shell,
        s.settings_caret,
        false,
    );
    *y += 64.0;
    info_slab(
        g,
        x,
        y,
        w,
        "셸 경로는 실행 파일 하나만 적습니다. 명령 옵션은 각 pane에서 직접 붙여 주세요.",
    );
}

fn paint_claude(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(
        g,
        x,
        *y,
        "Agent 기본값",
        "새로 띄우는 Claude와 Codex 작업대에 적용됩니다",
    );
    *y += 48.0;
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "캐릭터 성격 넣기",
        s.claude_persona,
        SettingsAction::ToggleClaudePersona,
    );
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "협업 연결 넣기",
        s.shim_inject,
        SettingsAction::ToggleShimInject,
    );
    let models = [
        ("기본", ""),
        ("Opus", "opus"),
        ("Sonnet", "sonnet"),
        ("Haiku", "haiku"),
    ];
    let model_cells: Vec<(&str, bool, SettingsAction)> = models
        .iter()
        .map(|(label, key)| {
            (
                *label,
                s.claude_model == *key,
                SettingsAction::ClaudeModel((*key).to_string()),
            )
        })
        .collect();
    segmented(g, s, hits, x, *y, w, &model_cells);
    *y += 42.0;
    let efforts = [
        ("기본", ""),
        ("낮게", "low"),
        ("보통", "medium"),
        ("높게", "high"),
        ("아주 높게", "xhigh"),
    ];
    let effort_cells: Vec<(&str, bool, SettingsAction)> = efforts
        .iter()
        .map(|(label, key)| {
            (
                *label,
                s.claude_effort == *key,
                SettingsAction::ClaudeEffort((*key).to_string()),
            )
        })
        .collect();
    segmented(g, s, hits, x, *y, w, &effort_cells);
    *y += 44.0;
    text_field(
        g,
        s,
        hits,
        caret,
        x,
        *y,
        w,
        "추가 인자",
        &s.claude_extra,
        SettingsInput::ClaudeExtra,
        s.settings_caret,
        false,
    );
    *y += 66.0;

    section_title(
        g,
        x,
        *y,
        "계정 작업대",
        "고르면 실행 중인 작업을 확인한 뒤 안전하게 갈아낍니다",
    );
    *y += 48.0;
    account_group(g, s, hits, x, y, w, AccountProvider::Claude);
    *y += 12.0;
    account_group(g, s, hits, x, y, w, AccountProvider::Codex);
    *y += 14.0;
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "한도에 맞춰 자동 전환",
        s.account_autoswitch,
        SettingsAction::ToggleAccountAutoswitch,
    );
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "상태줄에 다른 계정도 표시",
        s.statusbar_all_accounts,
        SettingsAction::ToggleStatusbarAllAccounts,
    );
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            (
                "80%",
                s.account_autoswitch_pct.round() as u32 == 80,
                SettingsAction::AccountAutoswitchPct(80),
            ),
            (
                "90%",
                s.account_autoswitch_pct.round() as u32 == 90,
                SettingsAction::AccountAutoswitchPct(90),
            ),
            (
                "95%",
                s.account_autoswitch_pct.round() as u32 == 95,
                SettingsAction::AccountAutoswitchPct(95),
            ),
        ],
    );
    *y += 48.0;
}

fn paint_themes(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(
        g,
        x,
        *y,
        "캐릭터 테마",
        "명단과 그림, 성격을 한 벌로 갈아낍니다",
    );
    *y += 48.0;
    let gap = 12.0;
    let cols = if w >= 620.0 { 2 } else { 1 };
    let cw = (w - gap * (cols - 1) as f32) / cols as f32;
    let ch = 112.0;
    for (i, theme_row) in s.themes.iter().enumerate() {
        let rect = (
            x + (i % cols) as f32 * (cw + gap),
            *y + (i / cols) as f32 * (ch + gap),
            cw,
            ch,
        );
        choice_card(
            g,
            s,
            hits,
            rect,
            s.character_theme == theme_row.id,
            Target::Setting(SettingsAction::SelectTheme(theme_row.id.clone())),
        );
        let label = fit(g, &theme_row.label, rect.2 - 32.0, 14.0, true);
        draw_text(
            g,
            rect.0 + 16.0,
            rect.1 + 15.0,
            &label,
            14.0,
            theme::text(),
            true,
        );
        draw_text(
            g,
            rect.0 + 16.0,
            rect.1 + 42.0,
            &format!("캐릭터 {}명", theme_row.count),
            11.0,
            theme::text_mute(),
            false,
        );
        for (j, (slug, _)) in theme_row.faces.iter().take(3).enumerate() {
            let color = color_for_word(slug);
            round_rect(
                g,
                rect.0 + 16.0 + j as f32 * 30.0,
                rect.1 + 67.0,
                24.0,
                24.0,
                12.0,
                color,
            );
        }
        if !theme_row.id.is_empty() {
            let open = (rect.0 + rect.2 - 72.0, rect.1 + 68.0, 26.0, 26.0);
            let delete = (rect.0 + rect.2 - 38.0, rect.1 + 68.0, 26.0, 26.0);
            mini_icon_button(
                g,
                s,
                hits,
                open,
                "folder-open",
                Target::Setting(SettingsAction::OpenThemeDir(theme_row.id.clone())),
            );
            mini_icon_button(
                g,
                s,
                hits,
                delete,
                "x",
                Target::Setting(SettingsAction::DeleteTheme(theme_row.id.clone())),
            );
        }
    }
    let rows = (s.themes.len() + cols - 1) / cols;
    *y += rows as f32 * (ch + gap) + 6.0;
    button(
        g,
        s,
        hits,
        (x, *y, 126.0, 34.0),
        "현재 테마 복제",
        Target::Setting(SettingsAction::ExportTheme),
        true,
    );
    button(
        g,
        s,
        hits,
        (x + 136.0, *y, 108.0, 34.0),
        "목록 새로고침",
        Target::Setting(SettingsAction::RefreshStudentAssets),
        false,
    );
    *y += 50.0;
    info_slab(
        g,
        x,
        y,
        w,
        "ZIP 테마 파일은 이 설정 방 어디에든 놓아 가져올 수 있어요.",
    );
}

fn paint_students(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    if let Some(selected) = s.student_selected.as_deref() {
        button(
            g,
            s,
            hits,
            (x, *y, 98.0, 32.0),
            "목록으로",
            Target::Setting(SettingsAction::CloseStudent),
            false,
        );
        draw_text(g, x + 112.0, *y + 6.0, selected, 17.0, theme::text(), true);
        *y += 48.0;
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            w,
            "이름",
            &s.student_name,
            SettingsInput::StudentName,
            s.settings_caret,
            false,
        );
        *y += 60.0;
        section_title(
            g,
            x,
            *y,
            "모델",
            "이 캐릭터만 다른 실행 통로를 쓸 수 있습니다",
        );
        *y += 46.0;
        let choices: Vec<(String, bool, SettingsAction)> = s
            .models
            .iter()
            .map(|choice| {
                let selected =
                    s.student_model == choice.model && s.student_backend == choice.backend;
                (
                    choice.label.clone(),
                    selected,
                    SettingsAction::StudentModel(choice.model.clone(), choice.backend.clone()),
                )
            })
            .collect();
        chips_owned(g, s, hits, x, y, w, choices);
        section_title(
            g,
            x,
            *y,
            "성격",
            "다른 칸으로 나가거나 목록으로 돌아갈 때 저장합니다",
        );
        *y += 44.0;
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            w,
            "",
            &s.student_persona,
            SettingsInput::StudentPersona,
            s.student_caret,
            true,
        );
        *y += 154.0;
        button(
            g,
            s,
            hits,
            (x, *y, 132.0, 34.0),
            "캐릭터 폴더 열기",
            Target::Setting(SettingsAction::OpenStudentsDir),
            false,
        );
        button(
            g,
            s,
            hits,
            (x + 142.0, *y, 132.0, 34.0),
            "정의 파일 열기",
            Target::Setting(SettingsAction::OpenCharactersJson),
            false,
        );
        button(
            g,
            s,
            hits,
            (x + 284.0, *y, 116.0, 34.0),
            "그림 새로고침",
            Target::Setting(SettingsAction::RefreshStudentAssets),
            false,
        );
        *y += 50.0;
        info_slab(
            g,
            x,
            y,
            w,
            "캐릭터 그림 생성과 모션 스프라이트 편집은 다음 네이티브 단계에서 이어집니다.",
        );
        return;
    }

    section_title(
        g,
        x,
        *y,
        "캐릭터",
        "한 명을 골라 이름과 성격, 모델을 고칩니다",
    );
    *y += 48.0;
    let gap = 10.0;
    let cols = if w >= 680.0 {
        4
    } else if w >= 470.0 {
        3
    } else {
        2
    };
    let cw = (w - gap * (cols - 1) as f32) / cols as f32;
    let ch = 74.0;
    for (i, character) in s.characters.iter().enumerate() {
        let rect = (
            x + (i % cols) as f32 * (cw + gap),
            *y + (i / cols) as f32 * (ch + gap),
            cw,
            ch,
        );
        choice_card(
            g,
            s,
            hits,
            rect,
            false,
            Target::Setting(SettingsAction::SelectStudent(character.name.clone())),
        );
        let color = character
            .slug
            .map(color_for_word)
            .unwrap_or_else(|| color_for_word(&character.name));
        round_rect(g, rect.0 + 12.0, rect.1 + 14.0, 34.0, 34.0, 17.0, color);
        let initial = character
            .name
            .chars()
            .next()
            .map(|ch| ch.to_string())
            .unwrap_or_default();
        draw_text(
            g,
            rect.0 + 22.0,
            rect.1 + 23.0,
            &initial,
            12.0,
            [255, 255, 255, 255],
            true,
        );
        let name = fit(g, &character.name, rect.2 - 66.0, 12.0, false);
        draw_text(
            g,
            rect.0 + 56.0,
            rect.1 + 25.0,
            &name,
            12.0,
            theme::text(),
            false,
        );
    }
    let rows = (s.characters.len() + cols - 1) / cols;
    *y += rows as f32 * (ch + gap) + 8.0;
    button(
        g,
        s,
        hits,
        (x, *y, 132.0, 34.0),
        "캐릭터 폴더 열기",
        Target::Setting(SettingsAction::OpenStudentsDir),
        false,
    );
    button(
        g,
        s,
        hits,
        (x + 142.0, *y, 132.0, 34.0),
        "정의 파일 열기",
        Target::Setting(SettingsAction::OpenCharactersJson),
        false,
    );
    *y += 48.0;
}

fn paint_feedback(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(
        g,
        x,
        *y,
        "무엇이 불편했나요",
        "보내지 않고 이 기기의 피드백 폴더에 한 장씩 저장합니다",
    );
    *y += 50.0;
    text_field(
        g,
        s,
        hits,
        caret,
        x,
        *y,
        w,
        "",
        &s.feedback_body,
        SettingsInput::FeedbackBody,
        s.feedback_caret,
        true,
    );
    *y += 158.0;
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "진단 정보 함께 남기기",
        s.feedback_diag,
        SettingsAction::ToggleFeedbackDiag,
    );
    button(
        g,
        s,
        hits,
        (x, *y, 106.0, 36.0),
        "피드백 저장",
        Target::Setting(SettingsAction::SaveFeedback),
        true,
    );
    button(
        g,
        s,
        hits,
        (x + 118.0, *y, 116.0, 36.0),
        "저장 폴더 열기",
        Target::Setting(SettingsAction::OpenFeedbackDir),
        false,
    );
    *y += 52.0;
}

fn account_group(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    provider: AccountProvider,
) {
    let (accounts, active): (Vec<(String, String)>, &str) = match provider {
        AccountProvider::Claude => (
            s.claude_accounts
                .iter()
                .map(|a| (a.id.clone(), a.label.clone()))
                .collect(),
            &s.claude_account,
        ),
        AccountProvider::Codex => (
            s.codex_accounts
                .iter()
                .map(|a| (a.id.clone(), a.label.clone()))
                .collect(),
            &s.codex_account,
        ),
    };
    draw_text(g, x, *y, provider.label(), 13.0, theme::text(), true);
    *y += 28.0;
    account_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        provider,
        "",
        "기본 로그인",
        active.is_empty(),
        false,
    );
    for (id, label) in accounts {
        let shown = if label.trim().is_empty() {
            id.clone()
        } else {
            label
        };
        account_row(
            g,
            s,
            hits,
            x,
            y,
            w,
            provider,
            &id,
            &shown,
            active == id,
            true,
        );
    }
    let add = match provider {
        AccountProvider::Claude => SettingsAction::AddClaudeAccount,
        AccountProvider::Codex => SettingsAction::AddCodexAccount,
    };
    button(
        g,
        s,
        hits,
        (x, *y, 108.0, 31.0),
        "계정 추가",
        Target::Setting(add),
        false,
    );
    *y += 42.0;
}

#[allow(clippy::too_many_arguments)]
fn account_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    provider: AccountProvider,
    id: &str,
    label: &str,
    selected: bool,
    removable: bool,
) {
    let rect = (x, *y, w, 42.0);
    choice_card(
        g,
        s,
        hits,
        rect,
        selected,
        Target::Setting(SettingsAction::SwitchAccount(provider, id.to_string())),
    );
    g.queue_icon(
        provider.icon(),
        rect.0 + 12.0,
        rect.1 + 12.0,
        17.0,
        if selected {
            theme::accent()
        } else {
            theme::text_mute()
        },
    );
    let shown = fit(g, label, w - 150.0, 12.0, false);
    draw_text(
        g,
        rect.0 + 40.0,
        rect.1 + 12.0,
        &shown,
        12.0,
        theme::text(),
        selected,
    );
    if removable {
        let reauth = (rect.0 + rect.2 - 67.0, rect.1 + 8.0, 26.0, 26.0);
        let remove = (rect.0 + rect.2 - 35.0, rect.1 + 8.0, 26.0, 26.0);
        mini_icon_button(
            g,
            s,
            hits,
            reauth,
            "refresh-cw",
            Target::Setting(SettingsAction::ReauthAccount(
                provider,
                id.to_string(),
                settings::LoginBrowser::Default,
            )),
        );
        let action = match provider {
            AccountProvider::Claude => SettingsAction::RemoveClaudeAccount(id.to_string()),
            AccountProvider::Codex => SettingsAction::RemoveCodexAccount(id.to_string()),
        };
        mini_icon_button(g, s, hits, remove, "x", Target::Setting(action));
    }
    *y += 48.0;
}

fn cursor_sample(
    g: &mut gpu::GpuRenderer,
    shape: cursor::CursorShape,
    x: f32,
    y: f32,
    thickness: f32,
    compact: bool,
) {
    let cw = if compact { 9.0 } else { 11.0 };
    let ch = if compact { 23.0 } else { 29.0 };
    let width = cw * 2.0;
    round_rect(
        g,
        x - 5.0,
        y - 4.0,
        width + 10.0,
        ch + 8.0,
        theme::radius_sm(),
        theme::surface_hover(),
    );
    draw_text(
        g,
        x + 2.0,
        y + 4.0,
        "가",
        if compact { 12.0 } else { 15.0 },
        theme::text_dim(),
        false,
    );
    let mut color = theme::cursor();
    color[3] = if compact { 210 } else { 175 };
    for quad in cursor::cursor_primitives(shape, x, y, cw, ch, 2, thickness).as_slice() {
        g.rect(quad.x, quad.y, quad.width, quad.height, color);
    }
}

fn category_meta(cat: SettingsCat) -> (&'static str, &'static str, &'static str) {
    match cat {
        SettingsCat::General => (
            "일반",
            "settings-2",
            "시작 위치와 파일, 스크롤의 기본값을 정합니다",
        ),
        SettingsCat::Appearance => (
            "모양",
            "sparkles",
            "커서와 색, 글자 크기를 한 화면에서 맞춥니다",
        ),
        SettingsCat::Shell => ("셸", "terminal", "새 pane이 어떤 셸로 시작할지 정합니다"),
        SettingsCat::Claude => (
            "Agent",
            "claude",
            "모델과 계정, 협업 연결의 기본값을 정합니다",
        ),
        SettingsCat::Theme => ("테마", "image", "캐릭터 명단과 그림을 한 벌로 갈아낍니다"),
        SettingsCat::Students => ("캐릭터", "users", "한 명씩 이름과 성격, 모델을 고칩니다"),
        SettingsCat::Feedback => (
            "피드백",
            "message-square-warning",
            "불편한 점을 이 기기에 기록합니다",
        ),
    }
}

fn section_title(g: &mut gpu::GpuRenderer, x: f32, y: f32, title: &str, desc: &str) {
    draw_text(g, x, y, title, 15.0, theme::text(), true);
    draw_text(g, x, y + 24.0, desc, 11.5, theme::text_mute(), false);
}

fn info_slab(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, text: &str) {
    let rect = (x, *y, w, 48.0);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        theme::surface(),
    );
    g.rect(rect.0, rect.1, 2.0, rect.3, theme::border());
    let shown = fit(g, text, rect.2 - 28.0, 11.5, false);
    draw_text(
        g,
        rect.0 + 14.0,
        rect.1 + 16.0,
        &shown,
        11.5,
        theme::text_dim(),
        false,
    );
    *y += rect.3 + 8.0;
}

fn toggle_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    label: &str,
    on: bool,
    action: SettingsAction,
) {
    let rect = (x, *y, w, 44.0);
    let hover = contains(rect, s.cursor);
    if hover {
        round_rect(
            g,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_md(),
            theme::surface_hover(),
        );
    }
    draw_text(
        g,
        rect.0 + 12.0,
        rect.1 + 14.0,
        label,
        12.5,
        theme::text(),
        false,
    );
    let toggle = (rect.0 + rect.2 - 46.0, rect.1 + 10.0, 36.0, 22.0);
    round_rect(
        g,
        toggle.0,
        toggle.1,
        toggle.2,
        toggle.3,
        11.0,
        if on {
            theme::accent()
        } else {
            theme::surface_active()
        },
    );
    round_rect(
        g,
        toggle.0 + if on { 17.0 } else { 3.0 },
        toggle.1 + 3.0,
        16.0,
        16.0,
        8.0,
        if on {
            [255, 255, 255, 255]
        } else {
            theme::text_mute()
        },
    );
    register_clipped(g, hits, Target::Setting(action), rect, HitCursor::Pointer);
    *y += 48.0;
}

fn segmented(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: f32,
    w: f32,
    cells: &[(&str, bool, SettingsAction)],
) {
    if cells.is_empty() {
        return;
    }
    let gap = 4.0;
    let cw = (w - gap * (cells.len() - 1) as f32) / cells.len() as f32;
    for (i, (label, selected, action)) in cells.iter().enumerate() {
        let rect = (x + i as f32 * (cw + gap), y, cw, 34.0);
        choice_card(g, s, hits, rect, *selected, Target::Setting(action.clone()));
        let shown = fit(g, label, cw - 18.0, 11.5, *selected);
        let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, *selected)) / 2.0;
        draw_text(
            g,
            tx,
            rect.1 + 10.0,
            &shown,
            11.5,
            if *selected {
                theme::text()
            } else {
                theme::text_dim()
            },
            *selected,
        );
    }
}

fn chips_owned(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    cells: Vec<(String, bool, SettingsAction)>,
) {
    let mut cx = x;
    let mut cy = *y;
    for (label, selected, action) in cells {
        let cw = (g.measure_chrome_text(&label, 11.5, selected) + 24.0).min(w);
        if cx + cw > x + w && cx > x {
            cx = x;
            cy += 40.0;
        }
        let rect = (cx, cy, cw, 32.0);
        choice_card(g, s, hits, rect, selected, Target::Setting(action));
        let shown = fit(g, &label, rect.2 - 24.0, 11.5, selected);
        draw_text(
            g,
            rect.0 + 12.0,
            rect.1 + 9.0,
            &shown,
            11.5,
            if selected {
                theme::text()
            } else {
                theme::text_dim()
            },
            selected,
        );
        cx += cw + 7.0;
    }
    *y = cy + 42.0;
}

fn stepper_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    label: &str,
    value: &str,
    minus: SettingsAction,
    plus: SettingsAction,
) {
    let rect = (x, *y, w, 42.0);
    draw_text(g, x + 2.0, *y + 13.0, label, 12.5, theme::text(), false);
    let right = x + w;
    let mr = (right - 104.0, *y + 4.0, 30.0, 30.0);
    let pr = (right - 30.0, *y + 4.0, 30.0, 30.0);
    button(g, s, hits, mr, "−", Target::Setting(minus), false);
    button(g, s, hits, pr, "+", Target::Setting(plus), false);
    draw_text(
        g,
        right - 66.0,
        *y + 12.0,
        value,
        12.0,
        theme::text_dim(),
        false,
    );
    let _ = rect;
    *y += 46.0;
}

#[allow(clippy::too_many_arguments)]
fn text_field(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret_out: &mut Option<Rect>,
    x: f32,
    y: f32,
    w: f32,
    label: &str,
    value: &str,
    field: SettingsInput,
    caret: usize,
    multiline: bool,
) {
    if !label.is_empty() {
        draw_text(g, x + 2.0, y, label, 11.0, theme::text_mute(), false);
    }
    let top = y + if label.is_empty() { 0.0 } else { 18.0 };
    let h = if multiline { 132.0 } else { 36.0 };
    let rect = (x, top, w, h);
    let focused = s.input == Some(field);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        if focused {
            theme::surface_hover()
        } else {
            theme::surface()
        },
    );
    if focused {
        stroke_rect(g, rect, theme::accent());
    } else {
        stroke_rect(g, rect, theme::border());
    }
    register_clipped(g, hits, Target::Focus(field), rect, HitCursor::Text);
    g.push_clip(rect.0 + 10.0, rect.1 + 5.0, rect.2 - 20.0, rect.3 - 10.0);
    if multiline {
        let lines = wrap_text(g, value, rect.2 - 22.0, 12.0);
        let mut caret_xy = (rect.0 + 11.0, rect.1 + 10.0);
        for (i, (line, start)) in lines.iter().take(6).enumerate() {
            let ly = rect.1 + 10.0 + i as f32 * 18.0;
            draw_text(g, rect.0 + 11.0, ly, line, 12.0, theme::text(), false);
            let end = start + line.chars().count();
            if caret >= *start && caret <= end {
                let prefix: String = line.chars().take(caret - start).collect();
                caret_xy = (
                    rect.0 + 11.0 + g.measure_chrome_text(&prefix, 12.0, false),
                    ly,
                );
            }
        }
        if focused {
            draw_preedit_and_caret(g, s, caret_xy, 17.0, caret_out);
        }
    } else {
        let shown = if value.is_empty() {
            "입력하세요"
        } else {
            value
        };
        let shown = fit(g, shown, rect.2 - 22.0, 12.0, false);
        draw_text(
            g,
            rect.0 + 11.0,
            rect.1 + 10.0,
            &shown,
            12.0,
            if value.is_empty() {
                theme::text_mute()
            } else {
                theme::text()
            },
            false,
        );
        if focused {
            let prefix: String = value
                .chars()
                .take(caret.min(value.chars().count()))
                .collect();
            let cx = rect.0 + 11.0 + g.measure_chrome_text(&prefix, 12.0, false);
            draw_preedit_and_caret(g, s, (cx, rect.1 + 9.0), 18.0, caret_out);
        }
    }
    g.pop_clip();
}

fn draw_preedit_and_caret(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    pos: (f32, f32),
    h: f32,
    caret_out: &mut Option<Rect>,
) {
    if !s.preedit.is_empty() {
        draw_text(g, pos.0, pos.1, &s.preedit, 12.0, theme::text(), false);
        let width = g.measure_chrome_text(&s.preedit, 12.0, false).max(7.0);
        g.rect(pos.0, pos.1 + h - 2.0, width, 1.0, theme::accent());
    } else if s.caret_on {
        g.rect(pos.0, pos.1, 1.5, h, theme::cursor());
    }
    *caret_out = Some((pos.0, pos.1, 2.0, h));
}

fn wrap_text(g: &mut gpu::GpuRenderer, text: &str, max_w: f32, font: f32) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut start = 0usize;
    for (index, ch) in text.chars().enumerate() {
        if ch == '\n' {
            out.push((std::mem::take(&mut line), start));
            start = index + 1;
            continue;
        }
        let next = format!("{line}{ch}");
        if !line.is_empty() && g.measure_chrome_text(&next, font, false) > max_w {
            out.push((std::mem::take(&mut line), start));
            start = index;
        }
        line.push(ch);
    }
    out.push((line, start));
    out
}

fn choice_card(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    rect: Rect,
    selected: bool,
    target: Target,
) {
    let hover = contains(rect, s.cursor);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        if selected {
            theme::surface_active()
        } else if hover {
            theme::surface_hover()
        } else {
            theme::surface()
        },
    );
    stroke_rect(
        g,
        rect,
        if selected {
            theme::accent()
        } else {
            theme::border()
        },
    );
    register_clipped(g, hits, target, rect, HitCursor::Pointer);
    g.hover_pointer |= hover;
}

fn button(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    rect: Rect,
    label: &str,
    target: Target,
    primary: bool,
) {
    let hover = contains(rect, s.cursor);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        if primary {
            if hover {
                theme::surface_active()
            } else {
                theme::accent()
            }
        } else if hover {
            theme::surface_active()
        } else {
            theme::surface_hover()
        },
    );
    let shown = fit(g, label, rect.2 - 18.0, 11.5, primary);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, primary)) / 2.0;
    draw_text(
        g,
        tx,
        rect.1 + (rect.3 - 12.0) / 2.0 - 1.0,
        &shown,
        11.5,
        if primary {
            [255, 255, 255, 255]
        } else {
            theme::text()
        },
        primary,
    );
    register_clipped(g, hits, target, rect, HitCursor::Pointer);
    g.hover_pointer |= hover;
}

fn mini_icon_button(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    rect: Rect,
    icon: &str,
    target: Target,
) {
    let hover = contains(rect, s.cursor);
    if hover {
        round_rect(
            g,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_sm(),
            theme::surface_active(),
        );
    }
    g.queue_icon(
        icon,
        rect.0 + 6.0,
        rect.1 + 6.0,
        14.0,
        if hover {
            theme::text()
        } else {
            theme::text_mute()
        },
    );
    register_clipped(g, hits, target, rect, HitCursor::Pointer);
}

fn stroke_rect(g: &mut gpu::GpuRenderer, rect: Rect, color: [u8; 4]) {
    g.rect(rect.0, rect.1, rect.2, 1.0, color);
    g.rect(rect.0, rect.1 + rect.3 - 1.0, rect.2, 1.0, color);
    g.rect(rect.0, rect.1, 1.0, rect.3, color);
    g.rect(rect.0 + rect.2 - 1.0, rect.1, 1.0, rect.3, color);
}

fn register(hits: &mut Vec<Hit>, target: Target, rect: Rect, cursor: HitCursor) {
    hits.push(Hit {
        target,
        rect,
        cursor,
    });
}

fn register_clipped(
    g: &gpu::GpuRenderer,
    hits: &mut Vec<Hit>,
    target: Target,
    rect: Rect,
    cursor: HitCursor,
) {
    if let Some(rect) = g.clip_hit(rect) {
        register(hits, target, rect, cursor);
    }
}

fn contains(rect: Rect, point: (f32, f32)) -> bool {
    point.0 >= rect.0
        && point.0 <= rect.0 + rect.2
        && point.1 >= rect.1
        && point.1 <= rect.1 + rect.3
}

fn draw_text(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    text: &str,
    size: f32,
    color: [u8; 4],
    bold: bool,
) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}

fn fit(g: &mut gpu::GpuRenderer, text: &str, width: f32, font: f32, bold: bool) -> String {
    if g.measure_chrome_text(text, font, bold) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}…");
        if g.measure_chrome_text(&candidate, font, bold) > width {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn color_for_word(word: &str) -> [u8; 4] {
    let mut hash = 0u32;
    for byte in word.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u32);
    }
    let accent = theme::accent();
    [
        accent[0].saturating_add((hash & 31) as u8),
        accent[1].saturating_sub(((hash >> 5) & 23) as u8),
        accent[2].saturating_add(((hash >> 10) & 17) as u8),
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_cards_cover_every_persisted_shape_once() {
        let all = cursor::CursorShape::ALL;
        assert_eq!(all.len(), 8);
        for shape in all {
            assert!(cursor::CursorShape::from_str(shape.as_str()).is_some());
        }
    }

    #[test]
    fn settings_hit_uses_visible_rect_only() {
        let hit = Hit {
            target: Target::Close,
            rect: (10.0, 20.0, 30.0, 40.0),
            cursor: HitCursor::Pointer,
        };
        assert!(contains(hit.rect, (40.0, 60.0)));
        assert!(!contains(hit.rect, (40.1, 60.1)));
    }

    #[test]
    fn multiline_classification_stays_narrow() {
        assert!(is_multiline(SettingsInput::StudentPersona));
        assert!(is_multiline(SettingsInput::FeedbackBody));
        assert!(!is_multiline(SettingsInput::CwdPath));
    }

    #[test]
    fn painter_never_reaches_file_or_network_io() {
        let source = include_str!("native_settings.rs");
        let snapshot = &source[source
            .find("pub(crate) fn native_settings_snapshot")
            .unwrap()
            ..source
                .find("pub(crate) fn finish_native_settings_paint")
                .unwrap()];
        let paint = &source
            [source.find("pub(crate) fn paint(").unwrap()..source.find("#[cfg(test)]").unwrap()];
        for forbidden in [
            "read_settings(",
            "characters_json(",
            "theme_rows(",
            "std::fs::",
            "TcpStream",
        ] {
            assert!(
                !snapshot.contains(forbidden) && !paint.contains(forbidden),
                "렌더 경로에 I/O가 들어왔다: {forbidden}"
            );
        }
    }
}
