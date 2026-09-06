//! wgpu 본창 안에서 그리는 설정 방.
//!
//! 페인터는 파일이나 HTTP를 읽지 않는다. 열 때와 파일을 바꾸는 액션 뒤에 만든
//! 캐시와 `App`의 메모리 값만 스냅샷으로 받아, 렌더 중 I/O와 재차용을 함께 막는다.

use super::*;

pub(crate) type Rect = (f32, f32, f32, f32);

const HEADER_H: f32 = 92.0;
const CONTENT_MAX_W: f32 = 820.0;
const SPRITE_DROP_MAX_BYTES: u64 = 4 << 20;
const THEMEGEN_DROP_MAX_BYTES: u64 = 32 << 20;

#[derive(Clone)]
pub(crate) struct PaletteChoice {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) bg: [u8; 4],
    pub(crate) text: [u8; 4],
    ansi: [[u8; 3]; 6],
}

#[derive(Clone)]
pub(crate) struct CharacterChoice {
    name: String,
    slug: String,
}

#[derive(Clone)]
pub(crate) struct CustomThemeChoice {
    slug: String,
    label: String,
}

#[derive(Clone)]
pub(crate) struct AccountChoice {
    provider: AccountProvider,
    id: String,
    name: String,
    sub: String,
    sub_kind: &'static str,
    active: bool,
    slot: bool,
    usage: Option<UsageBadge>,
}

/// 본진(홈 기계)의 계정 칸. 이 값이 있으면 계정 화면은 **그 기계 것**을 그린다.
///
/// 학생이 본진에서 태어나는 동안 계정을 이 기계에 등록해 봐야 아무 일도 안 난다 —
/// 한도가 차는 곳과 등록되는 곳이 달라서다. 화면이 어느 기계를 다루는지 항상
/// 보이게 이름을 함께 싣는다.
#[derive(Clone)]
pub(crate) struct HomeAccountsView {
    pub(crate) label: String,
    pub(crate) accounts: Arc<Vec<AccountChoice>>,
    pub(crate) autoswitch: bool,
    pub(crate) autoswitch_pct: f32,
    /// `(슬롯 id, 상태, 실패 이유)` — 상태는 `running`·`need_code`·`ok`·`error`.
    pub(crate) login: Option<(String, String, Option<String>)>,
    /// 아직 한 번도 못 읽었거나 마지막 조회가 실패한 이유.
    pub(crate) error: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct SettingsCache {
    pub(crate) ready: bool,
    pub(crate) palettes: Arc<Vec<PaletteChoice>>,
    characters: Arc<Vec<CharacterChoice>>,
    themes: Arc<Vec<socket::ThemeRow>>,
    models: Arc<Vec<kasa_mcp::character::ModelChoice>>,
    roster: Option<serde_json::Value>,
    open_apps: Arc<Vec<(String, String)>>,
    character_theme: String,
    language: String,
    system_light: String,
    system_dark: String,
    custom_themes: Arc<Vec<CustomThemeChoice>>,
    custom_active: String,
    palette_hex: Arc<Vec<String>>,
    theme_rosters: Arc<std::collections::HashMap<String, Vec<CharacterChoice>>>,
    theme_picks: Arc<std::collections::HashMap<String, Vec<String>>>,
    accounts: Arc<Vec<AccountChoice>>,
    themegen_providers: Arc<Vec<crate::themegen::ProviderStatus>>,
    themegen_provider: String,
    themegen_key_masked: String,
    themegen_refs: Arc<std::collections::HashSet<String>>,
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
            .field("theme_rosters", &self.theme_rosters.len())
            .field("accounts", &self.accounts.len())
            .finish()
    }
}

impl SettingsCache {
    pub(crate) fn refresh(&mut self) {
        let saved = socket::read_settings();
        self.refresh_palette_from(&saved);
        let roster = kasa_mcp::character::characters_json();
        let characters = roster
            .as_ref()
            .map(|value| {
                kasa_mcp::character::member_names(value)
                    .into_iter()
                    .map(|name| CharacterChoice {
                        slug: kasa_mcp::character::member_def(value, &name)
                            .and_then(|entry| entry.get("slug").and_then(|v| v.as_str()).map(str::to_string))
                            .unwrap_or_else(|| theme::agent_slug(&name)),
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
        self.characters = Arc::new(characters);
        self.themes = Arc::new(socket::theme_rows());
        self.models = Arc::new(models);
        self.roster = roster;
        self.open_apps = Arc::new(proc::open_with_apps().to_vec());
        self.character_theme = socket::read_character_theme();
        self.language = socket::read_ui_language();

        let mut theme_rosters = std::collections::HashMap::new();
        let mut theme_picks = std::collections::HashMap::new();
        for row in self.themes.iter() {
            let key = if row.id.is_empty() {
                kasa_mcp::character::BASE_THEME_KEY.to_string()
            } else {
                row.id.clone()
            };
            let value = if row.id.is_empty() {
                kasa_mcp::character::base_characters_json()
            } else {
                kasa_mcp::character::theme_characters_json(&row.id)
            };
            let members = value
                .as_ref()
                .map(|roster| {
                    kasa_mcp::character::member_names(roster)
                        .into_iter()
                        .map(|name| CharacterChoice {
                            slug: kasa_mcp::character::member_def(roster, &name)
                                .and_then(|entry| entry.get("slug").and_then(|v| v.as_str()).map(str::to_string))
                                .unwrap_or_else(|| theme::agent_slug(&name)),
                            name,
                        })
                        .collect()
                })
                .unwrap_or_default();
            theme_picks.insert(key.clone(), kasa_mcp::character::picks_of_theme(&key));
            theme_rosters.insert(key, members);
        }
        self.theme_rosters = Arc::new(theme_rosters);
        self.theme_picks = Arc::new(theme_picks);

        let settings = socket::read_settings();
        self.themegen_provider = settings
            .get("theme_gen_provider")
            .and_then(|value| value.as_str())
            .unwrap_or("opengateway")
            .to_string();
        self.themegen_key_masked = settings
            .get("gemini_api_key")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(crate::themegen::mask_key)
            .unwrap_or_default();
        self.themegen_providers = Arc::new(crate::themegen::detect_providers());
        let mut refs = std::collections::HashSet::new();
        for (theme_id, roster) in self.theme_rosters.iter() {
            if theme_id == kasa_mcp::character::BASE_THEME_KEY {
                continue;
            }
            for character in roster {
                if crate::settings::themegen_ref_info(theme_id, &character.slug).is_some() {
                    refs.insert(format!("{theme_id}\0{}", character.slug));
                }
            }
        }
        self.themegen_refs = Arc::new(refs);
    }

    fn refresh_palette_from(&mut self, saved: &serde_json::Value) {
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

        self.palettes = Arc::new(palettes);
        self.system_light = theme::system_slot_theme(true);
        self.system_dark = theme::system_slot_theme(false);
        let customs = theme::custom_themes(&saved);
        self.custom_active = theme::active_custom_slug().unwrap_or_default();
        self.custom_themes = Arc::new(
            customs
                .iter()
                .map(|entry| CustomThemeChoice {
                    slug: theme::custom_slug(entry),
                    label: theme::custom_label(entry),
                })
                .collect(),
        );
        self.palette_hex = Arc::new(crate::settings::palette_hex_list(
            &saved,
            (!self.custom_active.is_empty()).then_some(self.custom_active.as_str()),
        ));
    }

    pub(crate) fn refresh_palette(&mut self) {
        self.refresh_palette_from(&socket::read_settings());
    }

    pub(crate) fn set_accounts(&mut self, accounts: Vec<AccountChoice>) {
        self.accounts = Arc::new(accounts);
    }

    pub(crate) fn language(&self) -> &str {
        &self.language
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

fn account_choices(app: &App) -> Vec<AccountChoice> {
    let usage_table = app
        .claude_usage_all
        .lock()
        .ok()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let active_usage = app.claude_usage.lock().ok().and_then(|guard| guard.clone());
    let active_id = app.set_claude_account.clone();
    let mut rows = Vec::new();

    if active_id.is_empty() {
        let probe = crate::settings::auth_probe("");
        rows.push(AccountChoice {
            provider: AccountProvider::Claude,
            id: String::new(),
            name: "기본 로그인".to_string(),
            sub: probe
                .as_ref()
                .map(|value| {
                    if value.logged_in {
                        account_sub(&value.email, &value.org)
                    } else {
                        "로그인 필요".to_string()
                    }
                })
                .unwrap_or_else(|| "확인 중…".to_string()),
            sub_kind: match probe {
                Some(ref value) if !value.logged_in => "danger",
                Some(_) => "mute",
                None => "faint",
            },
            active: true,
            slot: false,
            usage: active_usage.clone().filter(|badge| badge.account_dir.is_empty()),
        });
    }
    for (index, account) in app.set_claude_accounts.iter().enumerate() {
        let probe = crate::settings::auth_probe(&account.id);
        let sub = probe
            .as_ref()
            .map(|value| {
                if value.logged_in {
                    account_sub(&value.email, &value.org)
                } else {
                    "로그인 필요".to_string()
                }
            })
            .unwrap_or_else(|| "확인 중…".to_string());
        let dir = crate::claude_auth::runtime_dir_for_cached(&account.id, &active_id)
            .map_or(String::new(), |path| path.to_string_lossy().into_owned());
        let usage = usage_table.get(&dir).cloned().or_else(|| {
            active_usage
                .clone()
                .filter(|badge| account.id == active_id && badge.account_dir == dir)
        });
        rows.push(AccountChoice {
            provider: AccountProvider::Claude,
            id: account.id.clone(),
            name: crate::settings::account_display(
                &account.id,
                &account.label,
                &format!("계정 {}", index + 2),
            ),
            sub,
            sub_kind: match probe {
                Some(ref value) if !value.logged_in => "danger",
                Some(_) => "mute",
                None => "faint",
            },
            active: account.id == active_id,
            slot: true,
            usage,
        });
    }

    rows.push(AccountChoice {
        provider: AccountProvider::Codex,
        id: String::new(),
        name: "기본 로그인".to_string(),
        sub: crate::settings::codex_identity("")
            .unwrap_or_else(|| "로그인 필요".to_string()),
        sub_kind: if crate::settings::codex_logged_in("") { "mute" } else { "danger" },
        active: app.set_codex_account.is_empty(),
        slot: false,
        usage: None,
    });
    for (index, account) in app.set_codex_accounts.iter().enumerate() {
        let identity = crate::settings::codex_identity(&account.id);
        rows.push(AccountChoice {
            provider: AccountProvider::Codex,
            id: account.id.clone(),
            name: if account.label.trim().is_empty() {
                identity.clone().unwrap_or_else(|| format!("계정 {}", index + 2))
            } else {
                account.label.clone()
            },
            sub: if account.label.trim().is_empty() {
                String::new()
            } else {
                identity.unwrap_or_else(|| "로그인 필요".to_string())
            },
            sub_kind: if crate::settings::codex_logged_in(&account.id) { "mute" } else { "danger" },
            active: account.id == app.set_codex_account,
            slot: true,
            usage: None,
        });
    }
    rows
}

fn account_sub(email: &str, org: &str) -> String {
    let personal = format!("{email}'s Organization");
    if !org.is_empty() && org != personal {
        format!("{email} · {org}")
    } else {
        email.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitCursor {
    Pointer,
    Text,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Target {
    Category(SettingsCat),
    Setting(SettingsAction),
    Focus(SettingsInput),
    Close,
    Onboarding(crate::native_onboarding::Action),
}

#[derive(Clone)]
pub(crate) struct Hit {
    pub(crate) target: Target,
    pub(crate) rect: Rect,
    pub(crate) cursor: HitCursor,
}

#[derive(Clone, Debug)]
pub(crate) struct FieldBackup {
    pub(crate) field: SettingsInput,
    pub(crate) value: String,
    pub(crate) caret: usize,
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
    pub(crate) multiline_layouts: Vec<MultilineLayout>,
    pub(crate) motion_preview_visible: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct VisualRow {
    pub(crate) start: usize,
    pub(crate) len: usize,
    /// 각 caret 경계의 실제 GPU 측정 x 좌표(필드 안 상대좌표).
    pub(crate) caret_xs: Vec<f32>,
}

#[derive(Clone, Debug)]
pub(crate) struct MultilineLayout {
    pub(crate) field: SettingsInput,
    pub(crate) rect: Rect,
    pub(crate) rows: Vec<VisualRow>,
    pub(crate) first_line: usize,
    pub(crate) visible_lines: usize,
}

#[derive(Default)]
struct PaintFeedback {
    multiline_layouts: Vec<MultilineLayout>,
    motion_preview_visible: bool,
}

thread_local! {
    static PAINT_FEEDBACK: std::cell::RefCell<PaintFeedback> = Default::default();
}

fn begin_paint_feedback() {
    PAINT_FEEDBACK.with(|feedback| *feedback.borrow_mut() = PaintFeedback::default());
}

fn push_multiline_layout(layout: MultilineLayout) {
    PAINT_FEEDBACK.with(|feedback| feedback.borrow_mut().multiline_layouts.push(layout));
}

fn mark_motion_preview_visible() {
    PAINT_FEEDBACK.with(|feedback| feedback.borrow_mut().motion_preview_visible = true);
}

fn take_paint_feedback() -> PaintFeedback {
    PAINT_FEEDBACK.with(|feedback| std::mem::take(&mut *feedback.borrow_mut()))
}

pub(crate) struct Snapshot {
    pub(crate) area: Rect,
    pub(crate) cat: SettingsCat,
    pub(crate) cursor: (f32, f32),
    pub(crate) scroll: f32,
    pub(crate) caret_on: bool,
    pub(crate) input: Option<SettingsInput>,
    pub(crate) select_all: bool,
    pub(crate) preedit: String,
    pub(crate) first_run: bool,
    pub(crate) language: String,
    pub(crate) cwd_mode: String,
    pub(crate) file_open_mode: String,
    pub(crate) file_open_app: String,
    pub(crate) file_open_cmd: String,
    pub(crate) file_tree_default: bool,
    pub(crate) footer_default: bool,
    pub(crate) autosave_ms: u64,
    pub(crate) shell: String,
    pub(crate) theme: String,
    pub(crate) system_light: String,
    pub(crate) system_dark: String,
    pub(crate) custom_themes: Arc<Vec<CustomThemeChoice>>,
    pub(crate) custom_active: String,
    pub(crate) custom_theme_label_edit: Option<(String, String)>,
    pub(crate) palette_hex: Arc<Vec<String>>,
    pub(crate) palette_edit: String,
    pub(crate) picker_hsv: (f32, f32, f32),
    pub(crate) eyedropper: bool,
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
    pub(crate) account_autoswitch: bool,
    pub(crate) account_autoswitch_pct: f32,
    pub(crate) statusbar_all_accounts: bool,
    pub(crate) accounts: Arc<Vec<AccountChoice>>,
    pub(crate) account_label_edit: Option<(AccountProvider, String, String)>,
    pub(crate) login_job: Option<crate::settings::LoginJob>,
    /// 로그인이 코드를 기다릴 때 그 칸에 든 값.
    pub(crate) login_code: String,
    /// 본진이 살아 있으면 그 기계의 계정 칸. 「본진」 칸을 골랐을 때 쓴다.
    pub(crate) home_accounts: Option<HomeAccountsView>,
    /// 계정 칸이 지금 다루는 기계 — `true` = 본진.
    pub(crate) account_scope_home: bool,
    pub(crate) palettes: Arc<Vec<PaletteChoice>>,
    pub(crate) characters: Arc<Vec<CharacterChoice>>,
    pub(crate) themes: Arc<Vec<socket::ThemeRow>>,
    pub(crate) theme_rosters: Arc<std::collections::HashMap<String, Vec<CharacterChoice>>>,
    pub(crate) theme_picks: Arc<std::collections::HashMap<String, Vec<String>>>,
    pub(crate) inspected_theme: Option<String>,
    pub(crate) theme_label_edit: Option<(String, String)>,
    pub(crate) character_theme: String,
    pub(crate) open_apps: Arc<Vec<(String, String)>>,
    pub(crate) student_selected: Option<String>,
    pub(crate) student_theme: String,
    pub(crate) student_slug: String,
    pub(crate) student_name: String,
    pub(crate) student_persona: String,
    pub(crate) student_caret: usize,
    pub(crate) student_model: String,
    pub(crate) student_backend: String,
    pub(crate) student_raw_open: bool,
    pub(crate) student_raw_yaml: bool,
    pub(crate) student_raw_text: String,
    pub(crate) student_raw_caret: usize,
    pub(crate) student_raw_error: Option<String>,
    pub(crate) models: Arc<Vec<kasa_mcp::character::ModelChoice>>,
    pub(crate) settings_caret: usize,
    pub(crate) feedback_body: String,
    pub(crate) feedback_caret: usize,
    pub(crate) feedback_diag: bool,
    pub(crate) feedback_diag_line: String,
    pub(crate) themegen_providers: Arc<Vec<crate::themegen::ProviderStatus>>,
    pub(crate) themegen_provider: String,
    pub(crate) themegen_key_masked: String,
    pub(crate) themegen_key_edit: String,
    pub(crate) themegen_has_ref: bool,
    pub(crate) themegen_phase: Option<crate::themegen::GenPhase>,
    pub(crate) sprite_slot: Option<(String, usize)>,
    pub(crate) media: Arc<crate::settings_media::SettingsMediaCache>,
    pub(crate) media_elapsed: std::time::Duration,
    pub(crate) onboarding: crate::native_onboarding::Snapshot,
}

impl App {
    pub(crate) fn refresh_native_settings_media_cache(&mut self) {
        let mut plan = crate::settings_media::MediaPlan::new();
        {
            let cache = self.settings_scene.cache();
            match self.settings_scene.category() {
                SettingsCat::Theme => {
                    plan.include_theme_cards(&cache.themes);
                    if let Some(theme_id) = self.settings_scene.inspected_theme() {
                        let key = if theme_id.is_empty() {
                            kasa_mcp::character::BASE_THEME_KEY
                        } else {
                            theme_id
                        };
                        if let Some(roster) = cache.theme_rosters.get(key) {
                            plan.include_student_faces(
                                theme_id,
                                roster.iter().map(|character| character.slug.as_str()),
                            );
                        }
                    }
                }
                SettingsCat::Students if self.students_selected.is_none() => {
                    plan.include_student_faces(
                        &cache.character_theme,
                        cache.characters.iter().map(|character| character.slug.as_str()),
                    );
                }
                _ => {}
            }
        }
        if self.settings_scene.category() == SettingsCat::Students
            && self.students_selected.is_some()
            && !self.students_slug.is_empty()
        {
            plan.include_student_detail(&self.students_theme, &self.students_slug);
        }
        self.settings_scene.refresh_media_cache(&plan);
    }

    pub(crate) fn reload_native_settings_media_cache(&mut self) {
        self.settings_scene.invalidate_media_cache();
        self.refresh_native_settings_media_cache();
    }

    pub(crate) fn settings_media_animating(&self) -> bool {
        if !motion_preview_pump_needed(
            self.settings_room_active(),
            self.students_selected.is_some() && !self.students_slug.is_empty(),
            self.settings_scene.motion_preview_visible(),
        ) {
            return false;
        }
        let elapsed = self.settings_scene.media_elapsed();
        ["idle", "walk", "wave", "cheer", "gif"].iter().any(|motion| {
            self.settings_scene
                .media()
                .next_motion_frame_in(
                    &self.students_theme,
                    &self.students_slug,
                    motion,
                    elapsed,
                )
                .is_some()
        })
    }

    /// 계정 신원/한도는 렌더 스냅샷에서 조회하지 않는다. 이 함수는 event-loop의
    /// 느린 틱에서만 캐시를 갱신하므로 auth probe가 자식 프로세스를 띄우더라도
    /// 프레임마다 반복되지 않는다.
    pub(crate) fn refresh_native_settings_dynamic_cache(&mut self) {
        if !self.settings_room_active() {
            return;
        }
        let accounts = account_choices(self);
        self.settings_scene.set_account_cache(accounts);
    }

    pub(crate) fn native_settings_tick(&mut self) {
        if self.settings_room_active() && self.settings_scene.dynamic_refresh_due() {
            self.refresh_native_settings_dynamic_cache();
            self.chrome_dirty = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    pub(crate) fn native_settings_snapshot(&self, area: Rect) -> Option<Snapshot> {
        if !self.settings_room_active() {
            return None;
        }
        let scene = &self.settings_scene;
        let cache = scene.cache();
        Some(Snapshot {
            area,
            cat: scene.category(),
            cursor: self.cursor_px,
            scroll: scene.scroll(),
            caret_on: self.last_blink_on,
            input: self.settings_input,
            select_all: scene.field_select_all(),
            preedit: self.preedit.clone(),
            first_run: scene.first_run(),
            language: cache.language.clone(),
            cwd_mode: self.set_cwd_mode.clone(),
            file_open_mode: self.set_file_open_mode.clone(),
            file_open_app: self.set_file_open_app.clone(),
            file_open_cmd: self.set_file_open_cmd.clone(),
            file_tree_default: self.set_file_tree_default,
            footer_default: self.set_footer_default,
            autosave_ms: self.set_autosave.map_or(0, |d| d.as_millis() as u64),
            shell: self.set_shell.clone(),
            theme: theme::theme_name(),
            system_light: cache.system_light.clone(),
            system_dark: cache.system_dark.clone(),
            custom_themes: cache.custom_themes.clone(),
            custom_active: cache.custom_active.clone(),
            custom_theme_label_edit: self.custom_theme_label_edit.clone(),
            palette_hex: cache.palette_hex.clone(),
            palette_edit: self.set_palette_edit.clone(),
            picker_hsv: self.set_picker_hsv,
            eyedropper: crate::eyedropper::supported(),
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
            account_autoswitch: self.set_account_autoswitch,
            account_autoswitch_pct: self.set_account_autoswitch_pct,
            statusbar_all_accounts: self.set_statusbar_all_accounts,
            accounts: cache.accounts.clone(),
            account_label_edit: self.account_label_edit.clone(),
            login_job: crate::settings::hidden_login_job(),
            login_code: self.login_code_edit.clone(),
            home_accounts: home_accounts_view(),
            account_scope_home: self.set_account_scope_home,
            palettes: cache.palettes.clone(),
            characters: cache.characters.clone(),
            themes: cache.themes.clone(),
            theme_rosters: cache.theme_rosters.clone(),
            theme_picks: cache.theme_picks.clone(),
            inspected_theme: scene.inspected_theme().map(str::to_string),
            theme_label_edit: self.theme_label_edit.clone(),
            character_theme: cache.character_theme.clone(),
            open_apps: cache.open_apps.clone(),
            student_selected: self.students_selected.clone(),
            student_theme: self.students_theme.clone(),
            student_slug: self.students_slug.clone(),
            student_name: self.students_name.clone(),
            student_persona: self.students_persona.clone(),
            student_caret: self.students_caret,
            student_model: self.students_model.clone(),
            student_backend: self.students_backend.clone(),
            student_raw_open: self.students_raw.open,
            student_raw_yaml: self.students_raw.yaml,
            student_raw_text: self.students_raw.text.clone(),
            student_raw_caret: self.students_raw.caret,
            student_raw_error: self.students_raw.err.clone(),
            models: cache.models.clone(),
            settings_caret: self.settings_caret,
            feedback_body: self.feedback_body.clone(),
            feedback_caret: self.feedback_caret,
            feedback_diag: self.feedback_diag,
            feedback_diag_line: crate::settings::diag_line(),
            themegen_providers: cache.themegen_providers.clone(),
            themegen_provider: cache.themegen_provider.clone(),
            themegen_key_masked: cache.themegen_key_masked.clone(),
            themegen_key_edit: self.themegen.key_edit.clone(),
            themegen_has_ref: cache
                .themegen_refs
                .contains(&format!("{}\0{}", self.students_theme, self.students_slug)),
            themegen_phase: (!self.students_slug.is_empty())
                .then(|| self.themegen_view(&self.students_slug))
                .flatten()
                .map(|view| view.phase),
            sprite_slot: scene
                .sprite_slot()
                .map(|(motion, frame)| (motion.to_string(), frame)),
            media: scene.media(),
            media_elapsed: scene.media_elapsed(),
            onboarding: crate::native_onboarding::snapshot(self, area),
        })
    }

    pub(crate) fn finish_native_settings_paint(&mut self, output: PaintOutput) {
        self.settings_scene.finish_paint(
            output.hits,
            output.content_h,
            output.view_h,
            output.caret_rect,
            output.multiline_layouts,
            output.motion_preview_visible,
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
        let hit = self.settings_scene.hit_at(x, y).cloned();
        let target = hit.as_ref().map(|hit| hit.target.clone());
        match target {
            Some(Target::Category(cat)) => {
                self.native_settings_blur();
                self.settings_scene.set_category(cat);
                self.refresh_native_settings_media_cache();
            }
            Some(Target::Setting(action)) => {
                if matches!(action, SettingsAction::PickerSV | SettingsAction::PickerHue) {
                    if !matches!(self.settings_input, Some(SettingsInput::PaletteHex(_))) {
                        self.settings_apply(SettingsAction::FocusPaletteHex(0));
                        self.native_settings_arm_backup(SettingsInput::PaletteHex(0));
                    }
                    if let Some(rect) = hit.map(|hit| hit.rect) {
                        self.settings_scene.mark_field_dirty();
                        self.picker_preview(&action, rect, (x, y));
                        self.settings_scene.begin_picker_drag(action, rect);
                    }
                    self.chrome_dirty = true;
                    return true;
                }
                self.native_settings_blur();
                self.native_settings_apply(action);
                if let Some(field) = self.settings_input {
                    self.native_settings_arm_backup(field);
                }
            }
            Some(Target::Focus(field)) => {
                if let SettingsInput::PaletteHex(slot) = field {
                    if self.settings_input != Some(field) {
                        self.native_settings_blur();
                        self.settings_apply(SettingsAction::FocusPaletteHex(slot));
                        self.native_settings_arm_backup(field);
                        self.ime_focus = Some(crate::ImeFocus::Settings(field));
                    }
                } else {
                    self.native_settings_focus(field);
                }
                if let Some(rect) = hit.map(|hit| hit.rect) {
                    self.native_settings_place_caret(field, rect, (x, y));
                }
            }
            Some(Target::Close) => {
                self.native_settings_blur();
                self.return_from_settings_room();
                return true;
            }
            Some(Target::Onboarding(action)) => {
                self.native_settings_blur();
                self.native_onboarding_action(action);
            }
            None => self.native_settings_blur(),
        }
        self.chrome_dirty = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    pub(crate) fn native_settings_drag_move(&mut self, x: f32, y: f32) -> bool {
        let Some((action, rect)) = self.settings_scene.picker_drag() else { return false };
        self.settings_scene.mark_field_dirty();
        self.picker_preview(&action, rect, (x, y));
        self.chrome_dirty = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    pub(crate) fn native_settings_end_drag(&mut self) -> bool {
        let ended = self.settings_scene.end_picker_drag();
        if ended {
            if let Some(SettingsInput::PaletteHex(slot)) = self.settings_input {
                self.apply_palette_edit(slot);
            }
        }
        ended
    }

    fn native_settings_apply(&mut self, action: SettingsAction) {
        let refresh = action_refreshes_cache(&action);
        let accounts = matches!(
            action,
            SettingsAction::SwitchAccount(_, _)
                | SettingsAction::AddClaudeAccount
                | SettingsAction::AddCodexAccount
                | SettingsAction::RemoveClaudeAccount(_)
                | SettingsAction::RemoveCodexAccount(_)
                | SettingsAction::ReauthAccount(_, _, _)
        );
        let media = action_refreshes_media(&action);
        self.settings_apply(action);
        if refresh {
            self.settings_scene.refresh_cache();
        }
        if accounts {
            self.refresh_native_settings_dynamic_cache();
        }
        if media {
            self.reload_native_settings_media_cache();
        }
    }

    fn native_settings_focus(&mut self, field: SettingsInput) {
        if self.settings_input != Some(field) {
            self.native_settings_blur();
        }
        self.native_settings_arm_backup(field);
        self.settings_scene.clear_field_selection();
        self.settings_input = Some(field);
        match field {
            SettingsInput::CwdPath => self.settings_caret = self.set_cwd_mode.chars().count(),
            SettingsInput::FileOpenCmd => {
                self.settings_caret = self.set_file_open_cmd.chars().count()
            }
            SettingsInput::Shell => {
                let detected_preset = self.settings_scene.first_run()
                    && self
                        .settings_scene
                        .onboarding()
                        .shell_path_is_preset(&self.set_shell);
                self.set_shell = direct_shell_seed(&self.set_shell, detected_preset);
                self.settings_caret = self.set_shell.chars().count();
            }
            SettingsInput::ClaudeExtra => {
                self.settings_caret = self.set_claude_extra.chars().count()
            }
            SettingsInput::StudentName => self.settings_caret = self.students_name.chars().count(),
            SettingsInput::StudentPersona => {
                self.students_caret = self.students_persona.chars().count()
            }
            SettingsInput::FeedbackBody => self.feedback_caret = self.feedback_body.chars().count(),
            SettingsInput::StudentRaw => self.students_raw.caret = self.students_raw.text.chars().count(),
            SettingsInput::ThemeGenKey => {
                self.themegen.key_edit.clear();
                self.settings_caret = 0;
            }
            SettingsInput::CustomThemeLabel
            | SettingsInput::AccountLabel
            | SettingsInput::LoginCode => {
                self.settings_caret = self
                    .native_settings_field_value(field)
                    .0
                    .chars()
                    .count();
            }
            SettingsInput::ThemeLabel | SettingsInput::PaletteHex(_) => {}
        }
        self.ime_focus = Some(crate::ImeFocus::Settings(field));
        self.preedit.clear();
        self.in_preedit = false;
    }

    fn native_settings_place_caret(
        &mut self,
        field: SettingsInput,
        rect: Rect,
        point: (f32, f32),
    ) {
        let multiline_caret = is_multiline(field)
            .then(|| self.settings_scene.multiline_layout(field).cloned())
            .flatten()
            .map(|layout| multiline_caret_from_point(&layout, point));
        if let Some((buffer, caret)) = field_buffer(self, field) {
            if is_multiline(field) {
                if let Some(next) = multiline_caret {
                    *caret = next.min(buffer.chars().count());
                }
            } else {
                let ratio =
                    ((point.0 - rect.0 - 10.0) / (rect.2 - 20.0).max(1.0)).clamp(0.0, 1.0);
                *caret = (buffer.chars().count() as f32 * ratio).round() as usize;
            }
        }
        self.settings_scene.clear_field_selection();
    }

    fn native_settings_arm_backup(&mut self, field: SettingsInput) {
        if self.settings_scene.field_backup_matches(field) {
            return;
        }
        let (value, caret) = self.native_settings_field_value(field);
        self.settings_scene.arm_field_backup(FieldBackup {
            field,
            value,
            caret,
        });
    }

    fn native_settings_field_value(&self, field: SettingsInput) -> (String, usize) {
        match field {
            SettingsInput::CwdPath => (self.set_cwd_mode.clone(), self.settings_caret),
            SettingsInput::FileOpenCmd => (self.set_file_open_cmd.clone(), self.settings_caret),
            SettingsInput::Shell => (self.set_shell.clone(), self.settings_caret),
            SettingsInput::ClaudeExtra => (self.set_claude_extra.clone(), self.settings_caret),
            SettingsInput::StudentName => (self.students_name.clone(), self.settings_caret),
            SettingsInput::StudentPersona => (self.students_persona.clone(), self.students_caret),
            SettingsInput::FeedbackBody => (self.feedback_body.clone(), self.feedback_caret),
            SettingsInput::StudentRaw => (self.students_raw.text.clone(), self.students_raw.caret),
            SettingsInput::ThemeGenKey => (self.themegen.key_edit.clone(), self.settings_caret),
            SettingsInput::CustomThemeLabel => (
                self.custom_theme_label_edit
                    .as_ref()
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default(),
                self.settings_caret,
            ),
            SettingsInput::AccountLabel => (
                self.account_label_edit
                    .as_ref()
                    .map(|(_, _, value)| value.clone())
                    .unwrap_or_default(),
                self.settings_caret,
            ),
            SettingsInput::LoginCode => (self.login_code_edit.clone(), self.settings_caret),
            SettingsInput::ThemeLabel => (
                self.theme_label_edit
                    .as_ref()
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default(),
                self.settings_caret,
            ),
            SettingsInput::PaletteHex(_) => (self.set_palette_edit.clone(), self.settings_caret),
        }
    }

    pub(crate) fn native_settings_insert_into(&mut self, field: SettingsInput, text: &str) {
        let replacing = self.settings_scene.take_field_select_all();
        if replacing {
            if let Some((buffer, caret)) = field_buffer(self, field) {
                buffer.clear();
                *caret = 0;
            }
        }
        if !text.is_empty() || replacing {
            self.settings_scene.mark_field_dirty();
        }
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
            SettingsInput::StudentRaw => {
                crate::lineedit::insert(&mut self.students_raw.text, &mut self.students_raw.caret, text)
            }
            SettingsInput::ThemeGenKey => {
                crate::lineedit::insert(&mut self.themegen.key_edit, &mut self.settings_caret, text)
            }
            SettingsInput::CustomThemeLabel => {
                if let Some((_, buffer)) = self.custom_theme_label_edit.as_mut() {
                    crate::lineedit::insert(buffer, &mut self.settings_caret, text);
                }
            }
            SettingsInput::AccountLabel => {
                if let Some((_, _, buffer)) = self.account_label_edit.as_mut() {
                    crate::lineedit::insert(buffer, &mut self.settings_caret, text);
                }
            }
            SettingsInput::LoginCode => {
                crate::lineedit::insert(&mut self.login_code_edit, &mut self.settings_caret, text);
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
        if field == SettingsInput::FeedbackBody {
            socket::write_setting(
                "feedback_draft",
                serde_json::Value::String(self.feedback_body.clone()),
            );
        }
        self.chrome_dirty = true;
    }

    pub(crate) fn native_settings_blur(&mut self) {
        if let Some(field) = self.settings_input {
            if let Some(text) = self.hangul.flush() {
                self.native_settings_insert_into(field, &text);
            }
        }
        let (backup, dirty) = self.settings_scene.take_field_backup();
        if !dirty {
            if let Some(backup) = backup {
                self.native_settings_restore_backup(backup);
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
        if self.settings_input == Some(SettingsInput::CustomThemeLabel) {
            self.flush_custom_theme_label();
            self.settings_scene.refresh_cache();
        }
        if self.settings_input == Some(SettingsInput::AccountLabel) {
            self.flush_account_label();
            self.refresh_native_settings_dynamic_cache();
        }
        if self.settings_input == Some(SettingsInput::LoginCode) {
            self.submit_login_code_field();
        }
        if self.settings_input == Some(SettingsInput::ThemeGenKey) {
            let key = self.themegen.key_edit.trim();
            if !key.is_empty() {
                socket::write_setting("gemini_api_key", serde_json::json!(key));
                self.settings_scene.refresh_cache();
            }
            self.themegen.key_edit.clear();
        }
        self.settings_input = None;
        self.settings_scene.clear_field_selection();
        if matches!(self.ime_focus, Some(crate::ImeFocus::Settings(_))) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    fn native_settings_restore_backup(&mut self, backup: FieldBackup) {
        match backup.field {
            SettingsInput::CwdPath => self.set_cwd_mode = backup.value,
            SettingsInput::FileOpenCmd => self.set_file_open_cmd = backup.value,
            SettingsInput::Shell => self.set_shell = backup.value,
            SettingsInput::ClaudeExtra => self.set_claude_extra = backup.value,
            SettingsInput::StudentName => self.students_name = backup.value,
            SettingsInput::StudentPersona => self.students_persona = backup.value,
            SettingsInput::FeedbackBody => self.feedback_body = backup.value,
            SettingsInput::StudentRaw => self.students_raw.text = backup.value,
            SettingsInput::ThemeGenKey => self.themegen.key_edit = backup.value,
            SettingsInput::CustomThemeLabel => {
                if let Some((_, value)) = self.custom_theme_label_edit.as_mut() {
                    *value = backup.value;
                }
            }
            SettingsInput::AccountLabel => {
                if let Some((_, _, value)) = self.account_label_edit.as_mut() {
                    *value = backup.value;
                }
            }
            SettingsInput::LoginCode => self.login_code_edit = backup.value,
            SettingsInput::ThemeLabel => {
                if let Some((_, value)) = self.theme_label_edit.as_mut() {
                    *value = backup.value;
                }
            }
            SettingsInput::PaletteHex(slot) => {
                self.set_palette_edit = backup.value;
                self.apply_palette_edit(slot);
            }
        }
        match backup.field {
            SettingsInput::StudentPersona => self.students_caret = backup.caret,
            SettingsInput::StudentRaw => self.students_raw.caret = backup.caret,
            SettingsInput::FeedbackBody => self.feedback_caret = backup.caret,
            _ => self.settings_caret = backup.caret,
        }
        if matches!(
            backup.field,
            SettingsInput::CwdPath
                | SettingsInput::FileOpenCmd
                | SettingsInput::Shell
                | SettingsInput::ClaudeExtra
        ) {
            self.settings_save();
        }
        if backup.field == SettingsInput::FeedbackBody {
            socket::write_setting(
                "feedback_draft",
                serde_json::Value::String(self.feedback_body.clone()),
            );
        }
    }

    fn native_settings_cancel_field(&mut self) {
        let field = self.settings_input;
        let _ = self.hangul.flush();
        let (backup, _) = self.settings_scene.take_field_backup();
        if let Some(backup) = backup {
            self.native_settings_restore_backup(backup);
        }
        self.settings_input = None;
        match field {
            Some(SettingsInput::ThemeLabel) => self.theme_label_edit = None,
            Some(SettingsInput::CustomThemeLabel) => self.custom_theme_label_edit = None,
            Some(SettingsInput::AccountLabel) => self.account_label_edit = None,
            // 코드는 지우지 않는다 — 붙여넣다 esc 를 눌러도 다시 치게 하지 않는다.
            Some(SettingsInput::LoginCode) => {}
            _ => {}
        }
        self.ime_focus = None;
        self.preedit.clear();
        self.in_preedit = false;
        self.chrome_dirty = true;
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
            if self.settings_scene.first_run() {
                return self.native_onboarding_key(event);
            }
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
        if host && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyA)) {
            self.settings_scene.select_all_field();
            if let Some((buffer, caret)) = field_buffer(self, field) {
                *caret = buffer.chars().count();
            }
            self.chrome_dirty = true;
            return true;
        }
        if host && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyC)) {
            if self.settings_scene.field_select_all() {
                let value = self.native_settings_field_value(field).0;
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(value);
                }
            }
            return true;
        }
        if host && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyX)) {
            if self.settings_scene.field_select_all() {
                let value = self.native_settings_field_value(field).0;
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(value);
                }
                self.native_settings_insert_into(field, "");
            }
            return true;
        }
        if host && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyZ)) {
            if let Some(backup) = self.settings_scene.field_backup() {
                self.native_settings_restore_backup(backup.clone());
                self.settings_scene.arm_field_backup(backup);
                self.settings_scene.select_all_field();
                self.chrome_dirty = true;
            }
            return true;
        }
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
            self.native_settings_cancel_field();
            return true;
        }

        if self.settings_scene.field_select_all()
            && matches!(
                event.logical_key,
                Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete)
            )
        {
            self.native_settings_insert_into(field, "");
            return true;
        }
        if self.settings_scene.field_select_all()
            && matches!(
                event.logical_key,
                Key::Named(NamedKey::ArrowLeft)
                    | Key::Named(NamedKey::ArrowRight)
                    | Key::Named(NamedKey::Home)
                    | Key::Named(NamedKey::End)
            )
        {
            self.settings_scene.clear_field_selection();
        }

        if is_multiline(field)
            && matches!(
                event.logical_key,
                Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown)
            )
        {
            let current = self.native_settings_field_value(field).1;
            let down = matches!(event.logical_key, Key::Named(NamedKey::ArrowDown));
            let next = self
                .settings_scene
                .multiline_layout(field)
                .map(|layout| move_multiline_caret(layout, current, down));
            if let (Some(next), Some((buffer, caret))) = (next, field_buffer(self, field)) {
                *caret = next.min(buffer.chars().count());
            }
            self.chrome_dirty = true;
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
            | Key::Named(NamedKey::Delete)
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
            self.settings_scene.mark_field_dirty();
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
            self.reload_native_settings_media_cache();
            return true;
        }
        if self.settings_scene.category() != SettingsCat::Students {
            self.set_toast("테마 압축 파일은 어느 설정 화면에서든 놓을 수 있어요".to_string());
            return true;
        }
        if let (Some(name), Some((motion, frame))) = (
            self.students_selected.clone(),
            self.settings_scene
                .sprite_slot()
                .map(|(motion, frame)| (motion.to_string(), frame)),
        ) {
            let slug = if self.students_slug.is_empty() {
                theme::agent_slug(&name)
            } else {
                self.students_slug.clone()
            };
            let Some((count, ext)) = socket::character_sprite_spec(&motion) else {
                self.set_toast("그 모션 칸을 못 찾았어요".to_string());
                return true;
            };
            if let Err(error) = drop_size_ok(&path, SPRITE_DROP_MAX_BYTES) {
                self.set_toast(error);
                return true;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.set_toast(format!("그림을 못 읽었어요 — {error}"));
                    return true;
                }
            };
            let mut frames = Vec::with_capacity(count);
            for index in 0..count {
                if index == frame {
                    frames.push(bytes.clone());
                } else if let Some(existing) = socket::character_sprite_bytes_in_theme(
                    &self.students_theme,
                    &slug,
                    &motion,
                    index,
                ) {
                    frames.push(existing);
                } else {
                    self.set_toast(format!(
                        "{motion}은 {count}장의 {ext}가 모두 있어야 해요 — 없는 칸부터 채워 주세요"
                    ));
                    return true;
                }
            }
            match socket::save_character_sprite_files_in_theme(
                &self.students_theme,
                &slug,
                &motion,
                &frames,
            ) {
                Ok(_) => {
                    self.settings_apply(SettingsAction::RefreshStudentAssets);
                    self.reload_native_settings_media_cache();
                    self.set_toast(format!("{motion} {}번째 그림을 바꿨어요", frame + 1));
                }
                Err(error) => self.set_toast(format!("그림을 못 바꿨어요 — {error}")),
            }
            return true;
        }
        let theme_id = if self.students_selected.is_some() {
            self.students_theme.clone()
        } else {
            self.settings_scene.cache().character_theme.clone()
        };
        if theme_id.is_empty() {
            self.set_toast("기본 테마에는 못 구워요 — 테마를 복제한 뒤 놓아 주세요".to_string());
            return true;
        }
        if self.students_selected.is_none() {
            if let Err(error) = drop_size_ok(&path, THEMEGEN_DROP_MAX_BYTES) {
                self.set_toast(error);
                return true;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.set_toast(format!("그림을 못 읽었어요 — {error}"));
                    return true;
                }
            };
            let name = path.file_name().and_then(|value| value.to_str());
            match crate::themegen::themegen_put_ref(None, name, &bytes) {
                Ok(slug) => {
                    socket::invalidate_theme_rows();
                    kasa_mcp::character::invalidate_active_theme();
                    theme::invalidate_roster();
                    self.settings_scene.refresh_cache();
                    if let Some(name) = self
                        .settings_scene
                        .cache()
                        .characters
                        .iter()
                        .find(|character| character.slug == slug)
                        .map(|character| character.name.clone())
                    {
                        self.select_student_for_edit(name);
                    }
                    self.set_toast(format!("{slug} 캐릭터와 참조 그림을 만들었어요"));
                }
                Err(error) => self.set_toast(format!("캐릭터를 못 만들었어요 — {error}")),
            }
            return true;
        }
        let slug = self.students_slug.clone();
        if slug.is_empty() {
            self.set_toast("캐릭터의 그림 이름을 못 찾았어요".to_string());
            return true;
        }
        let Some(root) = kasa_mcp::character::themes_root() else {
            return true;
        };
        if let Err(error) = drop_size_ok(&path, THEMEGEN_DROP_MAX_BYTES) {
            self.set_toast(error);
            return true;
        }
        match crate::themegen::place_themegen_ref(&root.join(theme_id), &slug, &path) {
            Ok(_) => {
                self.settings_scene.refresh_cache();
                self.reload_native_settings_media_cache();
                self.set_toast("참조 그림을 놓았어요".to_string());
            }
            Err(error) => self.set_toast(format!("그림을 못 놓았어요 — {error}")),
        }
        true
    }
}

fn motion_preview_pump_needed(room_active: bool, detail_open: bool, visible_this_frame: bool) -> bool {
    room_active && detail_open && visible_this_frame
}

fn drop_size_ok(path: &std::path::Path, limit: u64) -> Result<u64, String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("파일 크기를 못 읽었어요 — {error}"))?;
    if !metadata.is_file() {
        return Err("파일 하나를 놓아 주세요".to_string());
    }
    if metadata.len() > limit {
        return Err(format!("그림이 너무 커요 — 최대 {}MB", limit >> 20));
    }
    Ok(metadata.len())
}

fn is_multiline(field: SettingsInput) -> bool {
    matches!(
        field,
        SettingsInput::StudentPersona | SettingsInput::StudentRaw | SettingsInput::FeedbackBody
    )
}

fn action_refreshes_cache(action: &SettingsAction) -> bool {
    matches!(
        action,
        SettingsAction::UiLanguage(_)
            | SettingsAction::ThemeMode(_)
            | SettingsAction::ThemeSystemSlot(_, _)
            | SettingsAction::StartCustomTheme
            | SettingsAction::ResetCustomTheme
            | SettingsAction::DeleteCustomTheme(_)
            | SettingsAction::SelectTheme(_)
            | SettingsAction::ExportTheme
            | SettingsAction::DeleteTheme(_)
            | SettingsAction::RefreshStudentAssets
            | SettingsAction::StudentModel(_, _)
            | SettingsAction::ThemePickAll(_, _)
            | SettingsAction::CharacterPick(_, _, _)
            | SettingsAction::ThemeGenProvider(_)
    )
}

fn action_refreshes_media(action: &SettingsAction) -> bool {
    matches!(
        action,
        SettingsAction::ExportTheme
            | SettingsAction::DeleteTheme(_)
            | SettingsAction::InspectTheme(_)
            | SettingsAction::RefreshStudentAssets
            | SettingsAction::ResetMotion(_)
    )
}

pub(crate) fn remote_action_refreshes_media(action: &str) -> bool {
    matches!(action, "new-theme" | "delete-theme" | "refresh-assets")
}

fn direct_shell_seed(current: &str, detected_preset: bool) -> String {
    if detected_preset || matches!(current, "" | "/bin/zsh" | "/bin/bash") {
        String::new()
    } else {
        current.to_string()
    }
}

fn multiline_first_line(caret_line: usize, visible_lines: usize, focused: bool) -> usize {
    if focused {
        caret_line.saturating_sub(visible_lines.saturating_sub(1))
    } else {
        0
    }
}

fn visual_row_at(rows: &[VisualRow], caret: usize) -> usize {
    rows.iter()
        .position(|row| caret >= row.start && caret <= row.start + row.len)
        .unwrap_or_else(|| rows.len().saturating_sub(1))
}

fn nearest_caret_boundary(xs: &[f32], x: f32) -> usize {
    xs.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - x)
                .abs()
                .partial_cmp(&(*b - x).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(0, |(index, _)| index)
}

fn multiline_caret_from_point(layout: &MultilineLayout, point: (f32, f32)) -> usize {
    if layout.rows.is_empty() {
        return 0;
    }
    let visible_row = ((point.1 - layout.rect.1 - 10.0) / 18.0)
        .floor()
        .max(0.0) as usize;
    let row_index = (layout.first_line + visible_row.min(layout.visible_lines.saturating_sub(1)))
        .min(layout.rows.len() - 1);
    let row = &layout.rows[row_index];
    let x = (point.0 - layout.rect.0 - 11.0).max(0.0);
    row.start + nearest_caret_boundary(&row.caret_xs, x).min(row.len)
}

fn move_multiline_caret(layout: &MultilineLayout, caret: usize, down: bool) -> usize {
    if layout.rows.is_empty() {
        return caret;
    }
    let row = visual_row_at(&layout.rows, caret);
    let local = caret.saturating_sub(layout.rows[row].start).min(layout.rows[row].len);
    let x = layout.rows[row].caret_xs.get(local).copied().unwrap_or_default();
    let target = if down {
        (row + 1).min(layout.rows.len().saturating_sub(1))
    } else {
        row.saturating_sub(1)
    };
    layout.rows[target].start
        + nearest_caret_boundary(&layout.rows[target].caret_xs, x).min(layout.rows[target].len)
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
        SettingsInput::StudentRaw => Some((&mut app.students_raw.text, &mut app.students_raw.caret)),
        SettingsInput::ThemeGenKey => Some((&mut app.themegen.key_edit, &mut app.settings_caret)),
        SettingsInput::LoginCode => Some((&mut app.login_code_edit, &mut app.settings_caret)),
        SettingsInput::CustomThemeLabel => app
            .custom_theme_label_edit
            .as_mut()
            .map(|(_, buffer)| (buffer, &mut app.settings_caret)),
        SettingsInput::AccountLabel => app
            .account_label_edit
            .as_mut()
            .map(|(_, _, buffer)| (buffer, &mut app.settings_caret)),
        SettingsInput::ThemeLabel => app
            .theme_label_edit
            .as_mut()
            .map(|(_, buffer)| (buffer, &mut app.settings_caret)),
        SettingsInput::PaletteHex(_) => Some((&mut app.set_palette_edit, &mut app.settings_caret)),
    }
}

pub(crate) fn paint(g: &mut gpu::GpuRenderer, snapshot: &Snapshot) -> PaintOutput {
    crate::native_strings::set_language(&snapshot.language);
    if snapshot.first_run {
        return crate::native_onboarding::paint(g, &snapshot.onboarding);
    }
    begin_paint_feedback();
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
        theme::text_dim(),
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
            paint_appearance(
                g,
                snapshot,
                &mut hits,
                &mut caret_rect,
                content_x,
                &mut y,
                content_w,
            )
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
        SettingsCat::Accounts => paint_accounts(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
        SettingsCat::Theme => paint_themes(
            g,
            snapshot,
            &mut hits,
            &mut caret_rect,
            content_x,
            &mut y,
            content_w,
        ),
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
    let content_h = (y + snapshot.scroll - body_top + 18.0).max(view_h);
    // 스크롤바는 **스크롤되는 영역**의 오른쪽에 붙어야 한다. 글자가 앉는
    // `content_*` 를 그대로 주면 그 좌우 여백만큼 안으로 들어와, 막대가 패널
    // 가장자리에서 58px 떨어진 허공에 뜬다(2026-09-05 지적 「스크롤바가 왜 저기
    // 있어 영역설정이 잘못된듯」 · 실측 패널끝 1100 · 막대 1042). 뷰포트는 좌측
    // nav 오른쪽부터 패널 끝까지다.
    let scroll_x = ax + nav_w;
    let scroll_w = (ax + aw - scroll_x).max(0.0);
    paint_scroll_affordance(
        g,
        scroll_x,
        body_top,
        scroll_w,
        view_h,
        content_h,
        snapshot.scroll,
    );

    let feedback = take_paint_feedback();
    PaintOutput {
        hits,
        content_h,
        view_h,
        caret_rect,
        multiline_layouts: feedback.multiline_layouts,
        motion_preview_visible: feedback.motion_preview_visible,
    }
}

pub(crate) fn paint_scroll_affordance(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    view_h: f32,
    content_h: f32,
    scroll: f32,
) {
    let max_scroll = (content_h - view_h).max(0.0);
    if max_scroll <= 1.0 || view_h <= 1.0 {
        return;
    }
    let track_x = x + w - 6.0;
    g.rect(
        track_x,
        y,
        4.0,
        view_h,
        theme::with_alpha(theme::border(), 90),
    );
    let thumb_h = (view_h * view_h / content_h).clamp(28.0, view_h);
    let thumb_y = y + (view_h - thumb_h) * (scroll / max_scroll).clamp(0.0, 1.0);
    round_rect(
        g,
        track_x,
        thumb_y,
        4.0,
        thumb_h,
        2.0,
        theme::text_dim(),
    );
    if scroll < max_scroll - 1.0 {
        g.rect(
            x,
            y + view_h - 22.0,
            w,
            22.0,
            theme::with_alpha(theme::bg(), 210),
        );
        g.queue_icon(
            "chevron-down",
            x + w - 20.0,
            y + view_h - 19.0,
            13.0,
            theme::text_dim(),
        );
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
    section_title(g, x, *y, "언어", "설정과 안내 화면에서 쓸 말을 고릅니다");
    *y += 48.0;
    segmented(
        g,
        s,
        hits,
        x,
        *y,
        w,
        &[
            ("한국어", s.language == "ko", SettingsAction::UiLanguage("ko")),
            ("English", s.language == "en", SettingsAction::UiLanguage("en")),
        ],
    );
    *y += 54.0;
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
    if matches!(s.file_open_mode.as_str(), "app" | "system") {
        let mut choices: Vec<(String, bool, SettingsAction)> = s
            .open_apps
            .iter()
            .map(|(name, short)| {
                (
                    short.clone(),
                    s.file_open_app == *name,
                    SettingsAction::FileOpenApp(name.clone()),
                )
            })
            .collect();
        choices.push((
            "기본 앱".to_string(),
            s.file_open_app.is_empty(),
            SettingsAction::FileOpenApp(String::new()),
        ));
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
    let autosave = [("끔", 0), ("1초", 1000), ("3초", 3000), ("10초", 10000)];
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
    caret: &mut Option<Rect>,
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
        theme::text_dim(),
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
    if s.theme == "system" {
        section_title(
            g,
            x,
            *y,
            "시스템 밝기별 테마",
            "운영체제가 밝음/어두움을 바꿀 때 입을 팔레트입니다",
        );
        *y += 46.0;
        for (light, label, current) in [
            (true, "밝은 화면", s.system_light.as_str()),
            (false, "어두운 화면", s.system_dark.as_str()),
        ] {
            draw_text(g, x + 2.0, *y + 9.0, label, 11.5, theme::text_dim(), false);
            let choices = s
                .palettes
                .iter()
                .filter(|palette| palette.key != "system")
                .map(|palette| {
                    (
                        palette.label.clone(),
                        palette.key == current,
                        SettingsAction::ThemeSystemSlot(light, palette.key.clone()),
                    )
                })
                .collect();
            *y += 28.0;
            chips_owned(g, s, hits, x, y, w, choices);
        }
        *y += 8.0;
    }
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
    if !s.custom_active.is_empty() {
        let label = s
            .custom_themes
            .iter()
            .find(|custom| custom.slug == s.custom_active)
            .map(|custom| custom.label.as_str())
            .unwrap_or(s.custom_active.as_str());
        let edit_value = s
            .custom_theme_label_edit
            .as_ref()
            .filter(|(slug, _)| slug == &s.custom_active)
            .map(|(_, value)| value.as_str())
            .unwrap_or(label);
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            (w - 196.0).max(160.0),
            "커스텀 팔레트 이름",
            edit_value,
            SettingsInput::CustomThemeLabel,
            s.settings_caret,
            false,
        );
        register_clipped(
            g,
            hits,
            Target::Setting(SettingsAction::FocusCustomThemeLabel(s.custom_active.clone())),
            (x, *y + 18.0, (w - 196.0).max(160.0), 36.0),
            HitCursor::Text,
        );
        button(
            g,
            s,
            hits,
            (x + w - 184.0, *y + 18.0, 86.0, 36.0),
            "초기화",
            Target::Setting(SettingsAction::ResetCustomTheme),
            false,
        );
        button(
            g,
            s,
            hits,
            (x + w - 92.0, *y + 18.0, 92.0, 36.0),
            "팔레트 치우기",
            Target::Setting(SettingsAction::DeleteCustomTheme(s.custom_active.clone())),
            false,
        );
        *y += 72.0;
        paint_palette_editor(g, s, hits, caret, x, y, w);
    }
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

fn paint_palette_editor(
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
        "팔레트 색",
        "색 칸을 고른 뒤 휠이나 #rrggbb 값으로 바꿉니다",
    );
    *y += 48.0;
    let selected = match s.input {
        Some(SettingsInput::PaletteHex(index)) => index.min(s.palette_hex.len().saturating_sub(1)),
        _ => 0,
    };
    let (hue, sat, val) = s.picker_hsv;
    let wheel_w = w.min(310.0).max(180.0);
    let sv = (x, *y, wheel_w, 132.0);
    let cells_x = 24;
    let cells_y = 12;
    for row in 0..cells_y {
        for col in 0..cells_x {
            let saturation = (col + 1) as f32 / cells_x as f32;
            let value = 1.0 - row as f32 / cells_y as f32;
            let rgb = hsv_rgb(hue, saturation, value);
            g.rect(
                sv.0 + col as f32 * sv.2 / cells_x as f32,
                sv.1 + row as f32 * sv.3 / cells_y as f32,
                sv.2 / cells_x as f32 + 0.5,
                sv.3 / cells_y as f32 + 0.5,
                [rgb[0], rgb[1], rgb[2], 255],
            );
        }
    }
    stroke_rect(g, sv, theme::border());
    let marker_x = sv.0 + sat * sv.2;
    let marker_y = sv.1 + (1.0 - val) * sv.3;
    stroke_rect(g, (marker_x - 4.0, marker_y - 4.0, 8.0, 8.0), [255, 255, 255, 255]);
    register_clipped(
        g,
        hits,
        Target::Setting(SettingsAction::PickerSV),
        sv,
        HitCursor::Pointer,
    );

    let hue_rect = (x, *y + 141.0, wheel_w, 18.0);
    for col in 0..60 {
        let rgb = hsv_rgb(col as f32 * 6.0, 1.0, 1.0);
        g.rect(
            hue_rect.0 + col as f32 * hue_rect.2 / 60.0,
            hue_rect.1,
            hue_rect.2 / 60.0 + 0.5,
            hue_rect.3,
            [rgb[0], rgb[1], rgb[2], 255],
        );
    }
    stroke_rect(g, hue_rect, theme::border());
    g.rect(
        hue_rect.0 + (hue / 360.0) * hue_rect.2 - 1.0,
        hue_rect.1 - 2.0,
        2.0,
        hue_rect.3 + 4.0,
        [255, 255, 255, 255],
    );
    register_clipped(
        g,
        hits,
        Target::Setting(SettingsAction::PickerHue),
        hue_rect,
        HitCursor::Pointer,
    );
    let field_x = x + wheel_w + 16.0;
    let field_w = (w - wheel_w - 16.0).max(110.0);
    let slot_label = if selected < theme::PALETTE_KEYS.len() {
        theme::PALETTE_KEYS[selected].0.to_string()
    } else {
        format!("ANSI {}", selected.saturating_sub(theme::PALETTE_KEYS.len()))
    };
    draw_text(g, field_x, *y + 4.0, &slot_label, 12.0, theme::text(), true);
    text_field(
        g,
        s,
        hits,
        caret,
        field_x,
        *y + 28.0,
        field_w,
        "HEX",
        if s.input == Some(SettingsInput::PaletteHex(selected)) {
            &s.palette_edit
        } else {
            s.palette_hex.get(selected).map(String::as_str).unwrap_or("#000000")
        },
        SettingsInput::PaletteHex(selected),
        s.settings_caret,
        false,
    );
    if s.eyedropper {
        button(
            g,
            s,
            hits,
            (field_x, *y + 91.0, field_w.min(126.0), 34.0),
            "화면에서 색 집기",
            Target::Setting(SettingsAction::PaletteEyedropper(selected)),
            false,
        );
    }
    *y += 178.0;

    let swatch_w = 31.0;
    let gap = 7.0;
    let cols = ((w + gap) / (swatch_w + gap)).floor().max(1.0) as usize;
    for (index, hex) in s.palette_hex.iter().enumerate() {
        let rect = (
            x + (index % cols) as f32 * (swatch_w + gap),
            *y + (index / cols) as f32 * 36.0,
            swatch_w,
            28.0,
        );
        round_rect(
            g,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_sm(),
            theme::parse_hex(hex)
                .map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                .unwrap_or([0, 0, 0, 255]),
        );
        stroke_rect(
            g,
            rect,
            if index == selected { theme::accent() } else { theme::border() },
        );
        register_clipped(
            g,
            hits,
            Target::Setting(SettingsAction::FocusPaletteHex(index)),
            rect,
            HitCursor::Pointer,
        );
    }
    let rows = (s.palette_hex.len() + cols - 1) / cols;
    *y += rows as f32 * 36.0 + 18.0;
}

fn hsv_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    ]
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

}

/// 계정 칸. 모델 기본값과 한 화면에 있던 것을 뺐다 — 로그인과 제거는 위쪽
/// 토글들과 되돌리기 무게가 다르고, 계정을 보러 온 사람이 스크롤을 내려야 했다.
fn paint_accounts(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    account_group(g, s, hits, caret, x, y, w, AccountProvider::Claude);
    *y += 12.0;
    account_group(g, s, hits, caret, x, y, w, AccountProvider::Codex);
    *y += 14.0;
    toggle_row(
        g,
        s,
        hits,
        x,
        y,
        w,
        "한도에 맞춰 자동 전환",
        s.home_accounts.as_ref().map_or(s.account_autoswitch, |h| h.autoswitch),
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
    let switch_on = s.home_accounts.as_ref().map_or(s.account_autoswitch, |h| h.autoswitch);
    let switch_pct = s
        .home_accounts
        .as_ref()
        .map_or(s.account_autoswitch_pct, |h| h.autoswitch_pct)
        .round() as u32;
    if switch_on {
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
                    switch_pct == 80,
                    SettingsAction::AccountAutoswitchPct(80),
                ),
                (
                    "85%",
                    switch_pct == 85,
                    SettingsAction::AccountAutoswitchPct(85),
                ),
                (
                    "90%",
                    switch_pct == 90,
                    SettingsAction::AccountAutoswitchPct(90),
                ),
                (
                    "95%",
                    switch_pct == 95,
                    SettingsAction::AccountAutoswitchPct(95),
                ),
            ],
        );
        *y += 48.0;
    }
}

fn paint_themes(
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
            theme::text_dim(),
            false,
        );
        for (j, (slug, _)) in theme_row.faces.iter().take(3).enumerate() {
            let face = (
                rect.0 + 12.0 + j as f32 * 34.0,
                rect.1 + 60.0,
                30.0,
                38.0,
            );
            let status = s.media.draw_face(g, &theme_row.id, slug, face);
            if !status.is_ready() {
                round_rect(g, face.0, face.1 + 7.0, 24.0, 24.0, 12.0, color_for_word(slug));
            }
        }
        let inspect = (rect.0 + rect.2 - 38.0, rect.1 + 8.0, 26.0, 26.0);
        mini_icon_button(
            g,
            s,
            hits,
            inspect,
            "users",
            Target::Setting(SettingsAction::InspectTheme(theme_row.id.clone())),
        );
        if !theme_row.id.is_empty() {
            let rename = (rect.0 + rect.2 - 106.0, rect.1 + 68.0, 26.0, 26.0);
            let open = (rect.0 + rect.2 - 72.0, rect.1 + 68.0, 26.0, 26.0);
            let delete = (rect.0 + rect.2 - 38.0, rect.1 + 68.0, 26.0, 26.0);
            mini_icon_button(
                g,
                s,
                hits,
                rename,
                "edit-3",
                Target::Setting(SettingsAction::FocusThemeLabel(theme_row.id.clone())),
            );
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
    if let Some((id, label)) = s.theme_label_edit.as_ref() {
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            w,
            &format!("{} 이름", id),
            label,
            SettingsInput::ThemeLabel,
            s.settings_caret,
            false,
        );
        *y += 62.0;
    }
    if let Some(id) = s.inspected_theme.as_deref() {
        let key = if id.is_empty() {
            kasa_mcp::character::BASE_THEME_KEY
        } else {
            id
        };
        let roster = s.theme_rosters.get(key).cloned().unwrap_or_default();
        let picked = s.theme_picks.get(key).cloned().unwrap_or_default();
        let fallback = picked.is_empty();
        let label = s
            .themes
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.label.as_str())
            .unwrap_or(key);
        section_title(
            g,
            x,
            *y,
            &format!("{label} 명단"),
            "아무도 따로 고르지 않으면 이 테마의 전원이 기본 후보입니다",
        );
        *y += 48.0;
        button(
            g,
            s,
            hits,
            (x, *y, 112.0, 34.0),
            "전부 고르기",
            Target::Setting(SettingsAction::ThemePickAll(key.to_string(), true)),
            false,
        );
        button(
            g,
            s,
            hits,
            (x + 120.0, *y, 124.0, 34.0),
            "기본값으로",
            Target::Setting(SettingsAction::ThemePickAll(key.to_string(), false)),
            false,
        );
        *y += 46.0;
        let cols = if w >= 680.0 { 7 } else if w >= 470.0 { 5 } else { 3 };
        let gap = 7.0;
        let card_w = (w - gap * (cols - 1) as f32) / cols as f32;
        let card_h = 82.0;
        for (index, character) in roster.iter().enumerate() {
            let on = fallback || picked.iter().any(|name| name == &character.name);
            let rect = (
                x + (index % cols) as f32 * (card_w + gap),
                *y + (index / cols) as f32 * (card_h + gap),
                card_w,
                card_h,
            );
            choice_card(
                g,
                s,
                hits,
                rect,
                on,
                Target::Setting(SettingsAction::CharacterPick(
                    key.to_string(),
                    character.name.clone(),
                    !on,
                )),
            );
            s.media.draw_face(
                g,
                id,
                &character.slug,
                (rect.0 + 8.0, rect.1 + 4.0, rect.2 - 16.0, 55.0),
            );
            let name = fit(g, &character.name, rect.2 - 12.0, 10.5, on);
            draw_text(
                g,
                rect.0 + 6.0,
                rect.1 + 64.0,
                &name,
                10.5,
                if on { theme::text() } else { theme::text_mute() },
                on,
            );
        }
        let rows = (roster.len() + cols - 1) / cols;
        *y += rows as f32 * (card_h + gap);
        *y += 10.0;
    }
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
    paint_themegen_engine(g, s, hits, caret, x, y, w);
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
        draw_text(
            g,
            x + 112.0,
            *y + 28.0,
            if s.student_theme.is_empty() {
                "Bundled"
            } else {
                &s.student_theme
            },
            10.5,
            theme::text_dim(),
            false,
        );
        if !s.student_slug.is_empty() {
            let face = (x + w - 72.0, *y - 8.0, 64.0, 76.0);
            let status = s
                .media
                .draw_face(g, &s.student_theme, &s.student_slug, face);
            if !status.is_ready() {
                round_rect(
                    g,
                    face.0 + 10.0,
                    face.1 + 16.0,
                    42.0,
                    42.0,
                    21.0,
                    color_for_word(&s.student_slug),
                );
            }
        }
        *y += 48.0;
        segmented(
            g,
            s,
            hits,
            x,
            *y,
            w.min(280.0),
            &[
                (
                    "화면으로",
                    !s.student_raw_open,
                    SettingsAction::ToggleStudentRaw(false),
                ),
                (
                    "원본",
                    s.student_raw_open,
                    SettingsAction::ToggleStudentRaw(true),
                ),
            ],
        );
        *y += 46.0;
        if s.student_raw_open {
            segmented(
                g,
                s,
                hits,
                x,
                *y,
                230.0,
                &[
                    ("JSON", !s.student_raw_yaml, SettingsAction::StudentRawFormat(false)),
                    ("YAML", s.student_raw_yaml, SettingsAction::StudentRawFormat(true)),
                ],
            );
            button(
                g,
                s,
                hits,
                (x + 242.0, *y, 104.0, 34.0),
                "원본 저장",
                Target::Setting(SettingsAction::SaveStudentRaw),
                true,
            );
            *y += 46.0;
            text_field(
                g,
                s,
                hits,
                caret,
                x,
                *y,
                w,
                "정의 전체",
                &s.student_raw_text,
                SettingsInput::StudentRaw,
                s.student_raw_caret,
                true,
            );
            *y += 158.0;
            if let Some(error) = s.student_raw_error.as_deref() {
                info_slab(g, x, y, w, error);
            }
            return;
        }
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
        section_title(
            g,
            x,
            *y,
            "그림 생성",
            "참조 그림을 이 화면에 놓고 모든 기본 동작을 한 번에 굽습니다",
        );
        *y += 48.0;
        let status = match s.themegen_phase {
            Some(crate::themegen::GenPhase::Describing) => "그림 살펴보는 중",
            Some(crate::themegen::GenPhase::Generating) => "굽는 중",
            Some(crate::themegen::GenPhase::Installing) => "설치하는 중",
            Some(crate::themegen::GenPhase::Done) => "완성",
            Some(crate::themegen::GenPhase::Failed) => "실패",
            None if s.themegen_has_ref => "참조 그림 준비됨",
            None => "참조 그림을 놓아 주세요",
        };
        let reference_rect = (x, *y, 72.0, 72.0);
        let reference_status = s.media.draw_reference(
            g,
            &s.student_theme,
            &s.student_slug,
            reference_rect,
        );
        draw_text(
            g,
            x + 84.0,
            *y + 9.0,
            status,
            12.0,
            theme::text_dim(),
            false,
        );
        draw_text(
            g,
            x + 84.0,
            *y + 31.0,
            reference_status.label(),
            10.5,
            theme::text_mute(),
            false,
        );
        if s.themegen_has_ref
            && !matches!(
                s.themegen_phase,
                Some(crate::themegen::GenPhase::Describing)
                    | Some(crate::themegen::GenPhase::Generating)
                    | Some(crate::themegen::GenPhase::Installing)
            )
        {
            button(
                g,
                s,
                hits,
                (x + w - 112.0, *y + 38.0, 112.0, 34.0),
                "그림 굽기",
                Target::Setting(SettingsAction::ThemeGenStart),
                true,
            );
        }
        *y += 84.0;
        paint_motion_sprites(g, s, hits, x, y, w);
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
        info_slab(g, x, y, w, "그림 파일을 이 화면에 놓으면 이 캐릭터의 참조로 저장합니다.");
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
        let face = (rect.0 + 8.0, rect.1 + 7.0, 42.0, 54.0);
        let status = s
            .media
            .draw_face(g, &s.character_theme, &character.slug, face);
        if !status.is_ready() {
            let color = color_for_word(if character.slug.is_empty() {
                &character.name
            } else {
                &character.slug
            });
            round_rect(g, rect.0 + 12.0, rect.1 + 14.0, 34.0, 34.0, 17.0, color);
        }
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
    info_slab(
        g,
        x,
        y,
        w,
        if s.character_theme.is_empty() {
            "새 캐릭터는 먼저 테마를 복제한 뒤 그림 파일을 이 화면에 놓아 만듭니다."
        } else {
            "새 캐릭터 그림을 이 화면에 놓으면 파일 이름으로 명단에 추가합니다."
        },
    );
}

fn paint_motion_sprites(
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
        "모션 그림",
        "프레임 칸을 고르고 그림 파일을 놓으면 그 한 장만 바뀝니다",
    );
    *y += 48.0;
    for (motion, title) in [
        ("idle", "대기"),
        ("walk", "걷기"),
        ("wave", "손 흔들기"),
        ("cheer", "완료"),
        ("profile", "프로필"),
        ("gif", "대기 GIF"),
    ] {
        let Some((count, ext)) = socket::character_sprite_spec(motion) else { continue };
        let media_status = s
            .media
            .motion_status(&s.student_theme, &s.student_slug, motion);
        draw_text(g, x + 2.0, *y + 9.0, title, 12.0, theme::text(), true);
        draw_text(
            g,
            x + 92.0,
            *y + 9.0,
            &format!("{} × {} · {}", ext.to_uppercase(), count, media_status.label()),
            10.5,
            theme::text_dim(),
            false,
        );
        let preview = (x, *y + 25.0, 54.0, 46.0);
        if g.clip_hit(preview).is_some() && matches!(media_status, crate::settings_media::MediaStatus::Ready { frames, .. } if frames > 1) {
            mark_motion_preview_visible();
        }
        s.media.draw_motion_preview(
            g,
            &s.student_theme,
            &s.student_slug,
            motion,
            s.media_elapsed,
            preview,
        );
        let mut bx = x + 64.0;
        for frame in 0..count {
            let selected = s
                .sprite_slot
                .as_ref()
                .is_some_and(|(picked_motion, picked_frame)| picked_motion == motion && *picked_frame == frame);
            let rect = (bx, *y + 31.0, 34.0, 34.0);
            choice_card(
                g,
                s,
                hits,
                rect,
                selected,
                Target::Setting(SettingsAction::SelectMotionFrame(motion.to_string(), frame)),
            );
            s.media.draw_motion_frame(
                g,
                &s.student_theme,
                &s.student_slug,
                motion,
                frame,
                (rect.0 + 2.0, rect.1 + 2.0, rect.2 - 4.0, rect.3 - 4.0),
            );
            draw_text(
                g,
                rect.0 + rect.2 - 10.0,
                rect.1 + rect.3 - 13.0,
                &(frame + 1).to_string(),
                11.0,
                theme::text(),
                selected,
            );
            bx += 40.0;
        }
        button(
            g,
            s,
            hits,
            (x + w - 88.0, *y, 88.0, 32.0),
            "기본으로",
            Target::Setting(SettingsAction::ResetMotion(motion.to_string())),
            false,
        );
        *y += 78.0;
    }
    *y += 12.0;
}

fn paint_themegen_engine(
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
        "그림 생성 엔진",
        "준비되지 않은 엔진은 이유를 함께 표시합니다",
    );
    *y += 48.0;
    let providers = s
        .themegen_providers
        .iter()
        .map(|provider| {
            (
                if provider.available {
                    provider.label.clone()
                } else {
                    format!("{} · 준비 안 됨", provider.label)
                },
                provider.kind == s.themegen_provider,
                SettingsAction::ThemeGenProvider(provider.kind.to_string()),
            )
        })
        .collect();
    chips_owned(g, s, hits, x, y, w, providers);
    if let Some(provider) = s
        .themegen_providers
        .iter()
        .find(|provider| provider.kind == s.themegen_provider && !provider.available)
    {
        info_slab(g, x, y, w, &provider.why);
    }
    if s.themegen_provider == "nanobanana" {
        let value = if s.input == Some(SettingsInput::ThemeGenKey) {
            s.themegen_key_edit.as_str()
        } else {
            s.themegen_key_masked.as_str()
        };
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            w.min(420.0),
            "Gemini API 키",
            value,
            SettingsInput::ThemeGenKey,
            s.settings_caret,
            false,
        );
        *y += 64.0;
    }
    *y += 12.0;
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
    info_slab(g, x, y, w, &s.feedback_diag_line);
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

/// 본진 계정 칸을 화면이 쓰는 모양으로 바꾼다. 폴링도 여기서 태운다 — 계정
/// 화면이 떠 있는 동안만 물어보면 되고, 그 밖에서는 한 번도 안 나간다.
fn home_accounts_view() -> Option<HomeAccountsView> {
    crate::homeaccounts::poll();
    let (label, value, error) = crate::homeaccounts::snapshot()?;
    let value = value.unwrap_or_default();
    let rows: Vec<AccountChoice> = value
        .accounts
        .iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?.to_string();
            let usage = v.get("usage").and_then(serde_json::Value::as_f64).map(|pct| {
                crate::UsageBadge {
                    pct: pct as f32,
                    label: v
                        .get("usage_label")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    stale: v
                        .get("usage_stale")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    account_dir: id.clone(),
                    resets_at: v.get("usage_resets").and_then(serde_json::Value::as_u64),
                    windows: Vec::new(),
                }
            });
            Some(AccountChoice {
                provider: AccountProvider::Claude,
                active: id == value.active,
                name: v.get("name").and_then(|x| x.as_str()).unwrap_or(&id).to_string(),
                sub: v.get("sub").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                sub_kind: match v.get("sub_kind").and_then(|x| x.as_str()) {
                    Some("danger") => "danger",
                    Some("faint") => "faint",
                    _ => "mute",
                },
                slot: v.get("slot").and_then(serde_json::Value::as_bool).unwrap_or(false),
                id,
                usage,
            })
        })
        .collect();
    Some(HomeAccountsView {
        label,
        accounts: Arc::new(rows),
        autoswitch: value.autoswitch,
        autoswitch_pct: value.autoswitch_pct,
        login: value.login,
        error,
    })
}

fn account_group(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
    provider: AccountProvider,
) {
    // Claude 계정은 **학생이 실제로 사는 기계**의 것을 다룬다. 본진이 켜져 있으면
    // 등록도 전환도 거기서 일어나야 하고, 이 기계에 등록해 봐야 한 도가 차는 곳과
    // 달라 아무 일도 안 난다(2026-09-05). codex 는 아직 로컬 그대로다.
    let home = s
        .home_accounts
        .as_ref()
        .filter(|_| provider == AccountProvider::Claude && s.account_scope_home);
    // 진행 상태는 두 곳에서 온다 — 로컬은 프로세스 셀, 본진은 그 기계가 실어
    // 보낸 문자열. 화면은 같은 모양으로 그린다. 머리에서 미리 재는 것은 로그인이
    // 도는 동안 「계정 추가」를 감추기 위해서다 — 시작 단추와 진행 안내가 한
    // 화면에 함께 서면 어느 쪽이 지금인지 읽히지 않는다.
    let local_state = s.login_job.as_ref().map(|job| &job.state);
    let needs_code = match home {
        Some(h) => h.login.as_ref().is_some_and(|(_, st, _)| st == "need_code"),
        None => local_state == Some(&crate::settings::LoginState::NeedCode),
    };
    let running = match home {
        Some(h) => h.login.as_ref().is_some_and(|(_, st, _)| st == "running"),
        None => local_state == Some(&crate::settings::LoginState::Running),
    };

    g.queue_icon(provider.icon(), x, *y - 1.0, 17.0, theme::text());
    draw_text(g, x + 24.0, *y, provider.label(), 14.5, theme::text(), true);
    *y += 23.0;
    let blurb = match provider {
        AccountProvider::Claude => {
            "한도가 차면 다음 계정으로 스스로 넘어갑니다. 한 계정만 쓰신다면 더 넣지 않으셔도 됩니다."
        }
        AccountProvider::Codex => {
            "코덱스도 같은 방식으로 여러 계정을 둘 수 있습니다. 인증은 이 기기에 남습니다."
        }
    };
    let blurb = fit(g, blurb, w, 11.0, false);
    draw_text(g, x, *y, &blurb, 11.0, theme::text_dim(), false);
    *y += 26.0;
    // 기계를 고르는 두 칸. claude 는 **양쪽에서 돌기 때문에** 한쪽만 보여 주면
    // 하단 상태줄(늘 이 기계 것)과 어긋난다(2026-09-05 거노 「설정창이랑 하단이랑
    // 왜 다른데」). 본진이 없거나 꺼져 있으면 칸을 안 그린다 — 고를 것이 하나뿐인
    // 화면에 선택지를 세우면 그 자체가 물음이 된다.
    if provider == AccountProvider::Claude {
        if let Some(view) = s.home_accounts.as_ref() {
            segmented(
                g,
                s,
                hits,
                x,
                *y,
                w.min(360.0),
                &[
                    (
                        "이 맥북",
                        !s.account_scope_home,
                        SettingsAction::AccountScopeHome(false),
                    ),
                    (
                        &view.label,
                        s.account_scope_home,
                        SettingsAction::AccountScopeHome(true),
                    ),
                ],
            );
            *y += 40.0;
            let note = if s.account_scope_home {
                format!("{} 에서 도는 claude 의 계정이에요", view.label)
            } else {
                "이 맥북에서 도는 claude 의 계정이에요 — 하단 막대에 뜨는 숫자가 이것".to_string()
            };
            draw_text(g, x, *y, &note, 10.5, theme::text_mute(), false);
            *y += 24.0;
        }
    }

    // 목록 머리. 추가 단추는 **목록 위 오른쪽** — 새 줄이 어디에 생기는지가
    // 단추 자리로 설명되고, 계정이 늘어도 단추가 화면 아래로 도망가지 않는다.
    let add = match provider {
        AccountProvider::Claude => SettingsAction::AddClaudeAccount,
        AccountProvider::Codex => SettingsAction::AddCodexAccount,
    };
    draw_text(g, x, *y + 5.0, "계정", 12.5, theme::text(), true);
    if !needs_code && !running {
        button(
            g,
            s,
            hits,
            (x + w - 104.0, *y, 104.0, 30.0),
            "＋ 계정 추가",
            Target::Setting(add),
            false,
        );
    }
    *y += 26.0;
    draw_text(
        g,
        x,
        *y,
        "한도가 차면 위에서부터 차례로 넘어갑니다",
        10.5,
        theme::text_mute(),
        false,
    );
    *y += 22.0;

    if let Some(h) = home {
        if let Some(why) = h.error.as_deref() {
            draw_text(g, x + 2.0, *y, why, 10.5, theme::danger(), false);
            *y += 18.0;
        }
        if h.accounts.is_empty() && h.error.is_none() {
            draw_text(
                g,
                x + 2.0,
                *y,
                "아직 등록된 계정이 없어요 — 위 「계정 추가」로 하나 넣어 주세요",
                11.0,
                theme::text_mute(),
                false,
            );
            *y += 22.0;
        }
        for row in h.accounts.iter() {
            account_row(g, s, hits, caret, x, y, w, row);
        }
    } else {
        for row in s.accounts.iter().filter(|row| row.provider == provider) {
            account_row(g, s, hits, caret, x, y, w, row);
        }
    }

    if needs_code {
        // 브라우저가 승인해도 CLI 로 돌아오는 길이 없다 — 화면에 뜬 코드를 여기
        // 붙여넣어야 로그인이 끝난다. 이 칸이 없던 동안 로그인은 전부 실패했다.
        *y += 4.0;
        draw_text(
            g,
            x,
            *y,
            "브라우저에서 「승인」을 누르면 끝나요 — 코드가 보이면 복사만 하셔도 됩니다",
            11.0,
            theme::attention(),
            false,
        );
        *y += 20.0;
        text_field(
            g,
            s,
            hits,
            caret,
            x,
            *y,
            (w - 178.0).max(140.0),
            "",
            &s.login_code,
            SettingsInput::LoginCode,
            s.settings_caret,
            false,
        );
        button(
            g,
            s,
            hits,
            (x + w - 172.0, *y, 76.0, 31.0),
            "확인",
            Target::Setting(SettingsAction::SubmitLoginCode),
            true,
        );
        button(
            g,
            s,
            hits,
            (x + w - 92.0, *y, 92.0, 31.0),
            "로그인 취소",
            Target::Setting(SettingsAction::CancelLogin),
            false,
        );
        *y += 42.0;
    } else if running {
        *y += 4.0;
        draw_text(g, x, *y + 9.0, "로그인 진행 중", 11.5, theme::text_dim(), false);
        button(
            g,
            s,
            hits,
            (x + w - 92.0, *y, 92.0, 31.0),
            "로그인 취소",
            Target::Setting(SettingsAction::CancelLogin),
            false,
        );
        *y += 42.0;
    } else {
        *y += 6.0;
    }
}

#[allow(clippy::too_many_arguments)]
fn account_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
    row: &AccountChoice,
) {
    let editing = s
        .account_label_edit
        .as_ref()
        .is_some_and(|(provider, id, _)| *provider == row.provider && id == &row.id);
    let busy = s.login_job.as_ref().is_some_and(|job| {
        matches!(
            job.state,
            crate::settings::LoginState::Running | crate::settings::LoginState::NeedCode
        )
    }) || s.home_accounts.as_ref().is_some_and(|h| {
        h.login
            .as_ref()
            .is_some_and(|(_, st, _)| st == "running" || st == "need_code")
    });
    let show_actions = row.slot && !editing && !busy;
    let rect = (x, *y, w, if editing { 62.0 } else { 54.0 });
    choice_card(
        g,
        s,
        hits,
        rect,
        row.active,
        Target::Setting(SettingsAction::SwitchAccount(row.provider, row.id.clone())),
    );
    g.queue_icon(
        row.provider.icon(),
        rect.0 + 12.0,
        rect.1 + 12.0,
        17.0,
        if row.active {
            theme::accent()
        } else {
            theme::text_mute()
        },
    );
    if editing {
        let value = s
            .account_label_edit
            .as_ref()
            .map(|(_, _, value)| value.as_str())
            .unwrap_or_default();
        text_field(
            g,
            s,
            hits,
            caret,
            rect.0 + 40.0,
            rect.1 + 4.0,
            (w - 54.0).max(120.0),
            "별명",
            value,
            SettingsInput::AccountLabel,
            s.settings_caret,
            false,
        );
        *y += rect.3 + 6.0;
        return;
    }

    // 오른쪽 단추 묶음의 폭을 **먼저** 재 둔다 — 이름과 부제가 그 자리를 넘지
    // 않게 잘라야 하는데, 글자 단추는 라벨 길이에 따라 폭이 달라진다.
    let w_remove = g.measure_chrome_text("제거", 10.5, false) + 32.0;
    let w_reauth = g.measure_chrome_text("재인증", 10.5, false) + 32.0;
    let actions_w = if show_actions {
        w_remove + w_reauth + 2.0 + 60.0 + 10.0
    } else {
        14.0
    };
    let text_x = rect.0 + 40.0;
    let avail = (rect.0 + rect.2 - actions_w - text_x).max(80.0);

    // 첫 줄: 이름 + 「활성」 알약. 카드 테두리 색만으로 지금 쓰이는 줄을 알리던
    // 것을 낱말로 바꾼다 — 색은 활성과 호버가 서로 비슷해 읽히지 않았다.
    let badge = if row.active { Some("활성") } else { None };
    let badge_w = badge
        .map(|t| g.measure_chrome_text(t, 9.5, false) + 22.0)
        .unwrap_or(0.0);
    let shown = fit(g, &row.name, avail - badge_w, 12.0, row.active);
    let name_w = g.measure_chrome_text(&shown, 12.0, row.active);
    draw_text(g, text_x, rect.1 + 8.0, &shown, 12.0, theme::text(), row.active);
    if let Some(t) = badge {
        pill(g, text_x + name_w + 8.0, rect.1 + 7.0, t, true);
    }

    // 둘째 줄: 조직·마지막 로그인, 그 뒤에 사용량. 한 줄로 잇는 편이 오른쪽
    // 빈 자리에 숫자를 따로 띄우는 것보다 시선이 덜 튄다.
    let home_sub = s.home_accounts.as_ref().and_then(|h| {
        let (id, state, err) = h.login.as_ref()?;
        (id == &row.id).then(|| match state.as_str() {
            "running" => "로그인 진행 중".to_string(),
            "need_code" => "코드를 기다리는 중".to_string(),
            "ok" => "로그인을 마쳤어요".to_string(),
            _ => err.clone().unwrap_or_else(|| "로그인이 실패했어요".to_string()),
        })
    });
    let job_sub = home_sub.or_else(|| {
        s.login_job
            .as_ref()
            .filter(|job| job.id == row.id)
            .map(|job| match &job.state {
                crate::settings::LoginState::Running => "로그인 진행 중".to_string(),
                crate::settings::LoginState::NeedCode => "코드를 기다리는 중".to_string(),
                crate::settings::LoginState::Ok => "로그인을 마쳤어요".to_string(),
                crate::settings::LoginState::Err(error) => error.clone(),
            })
    });
    let usage = row.usage.as_ref().map(|usage| {
        let resets = crate::resets_in_label(usage.resets_at)
            .map(|value| format!(" · {value}"))
            .unwrap_or_default();
        (
            format!(
                "{}{:.0}%{resets}",
                if usage.stale { "~" } else { "" },
                usage.pct
            ),
            usage.pct,
        )
    });
    let usage_w = usage
        .as_ref()
        .map(|(t, _)| g.measure_chrome_text(t, 10.5, false) + 10.0)
        .unwrap_or(0.0);
    let sub_value = job_sub.as_deref().unwrap_or(&row.sub);
    let mut sub_x = text_x;
    if !sub_value.is_empty() {
        let sub = fit(g, sub_value, avail - usage_w, 10.5, false);
        let drawn = g.measure_chrome_text(&sub, 10.5, false);
        draw_text(
            g,
            sub_x,
            rect.1 + 29.0,
            &sub,
            10.5,
            if row.sub_kind == "danger" {
                theme::danger()
            } else {
                theme::text_mute()
            },
            false,
        );
        sub_x += drawn + 10.0;
    }
    if let Some((text, pct)) = usage.as_ref() {
        draw_text(
            g,
            sub_x,
            rect.1 + 29.0,
            text,
            10.5,
            if *pct >= 90.0 {
                theme::danger()
            } else if *pct >= 70.0 {
                theme::attention()
            } else {
                theme::text_dim()
            },
            false,
        );
    }

    if show_actions {
        let by = rect.1 + (rect.3 - 26.0) / 2.0;
        let mut rx = rect.0 + rect.2 - 8.0 - w_remove;
        let action = match row.provider {
            AccountProvider::Claude => SettingsAction::RemoveClaudeAccount(row.id.clone()),
            AccountProvider::Codex => SettingsAction::RemoveCodexAccount(row.id.clone()),
        };
        mini_text_button(
            g,
            s,
            hits,
            rx,
            by,
            "trash-2",
            "제거",
            Target::Setting(action),
            true,
        );
        rx -= w_reauth + 2.0;
        mini_text_button(
            g,
            s,
            hits,
            rx,
            by,
            "rotate-cw",
            "재인증",
            Target::Setting(SettingsAction::ReauthAccount(
                row.provider,
                row.id.clone(),
                settings::LoginBrowser::Default,
            )),
            false,
        );
        // 별명과 격리 로그인은 어쩌다 한 번이라 아이콘으로 남긴다 — 글자까지
        // 세우면 이름이 들어갈 자리가 사라진다.
        rx -= 30.0;
        mini_icon_button(
            g,
            s,
            hits,
            (rx, by, 26.0, 26.0),
            "shield",
            Target::Setting(SettingsAction::ReauthAccount(
                row.provider,
                row.id.clone(),
                settings::LoginBrowser::Isolated,
            )),
        );
        rx -= 30.0;
        mini_icon_button(
            g,
            s,
            hits,
            (rx, by, 26.0, 26.0),
            "pencil",
            Target::Setting(SettingsAction::FocusAccountLabel(
                row.provider,
                row.id.clone(),
            )),
        );
    }
    *y += rect.3 + 6.0;
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
            "모델과 협업 연결의 기본값을 정합니다",
        ),
        SettingsCat::Accounts => (
            "계정",
            "users",
            "로그인을 넣고, 한도가 차면 넘어갈 차례를 정합니다",
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
    draw_text(g, x, y + 24.0, desc, 11.5, theme::text_dim(), false);
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
        draw_text(g, x + 2.0, y, label, 11.0, theme::text_dim(), false);
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
        if s.select_all {
            g.rect(
                rect.0 + 3.0,
                rect.1 + 3.0,
                rect.2 - 6.0,
                rect.3 - 6.0,
                theme::with_alpha(theme::accent(), 42),
            );
        }
    } else {
        stroke_rect(g, rect, theme::border());
    }
    register_clipped(g, hits, Target::Focus(field), rect, HitCursor::Text);
    g.push_clip(rect.0 + 10.0, rect.1 + 5.0, rect.2 - 20.0, rect.3 - 10.0);
    if multiline {
        let lines = wrap_text(g, value, rect.2 - 22.0, 12.0);
        let caret_line = lines
            .iter()
            .position(|(line, start)| caret >= *start && caret <= start + line.chars().count())
            .unwrap_or_else(|| lines.len().saturating_sub(1));
        let visible_lines = ((rect.3 - 20.0) / 18.0).floor().max(1.0) as usize;
        let first_line = multiline_first_line(caret_line, visible_lines, focused);
        let rows = lines
            .iter()
            .map(|(line, start)| {
                let mut caret_xs = Vec::with_capacity(line.chars().count() + 1);
                caret_xs.push(0.0);
                let mut prefix = String::new();
                for ch in line.chars() {
                    prefix.push(ch);
                    caret_xs.push(g.measure_chrome_text(&prefix, 12.0, false));
                }
                VisualRow {
                    start: *start,
                    len: line.chars().count(),
                    caret_xs,
                }
            })
            .collect();
        push_multiline_layout(MultilineLayout {
            field,
            rect,
            rows,
            first_line,
            visible_lines,
        });
        let mut caret_xy = (rect.0 + 11.0, rect.1 + 10.0);
        for (i, (line, start)) in lines
            .iter()
            .skip(first_line)
            .take(visible_lines)
            .enumerate()
        {
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
        let (shown, visible_start) = if value.is_empty() {
            ("입력하세요".to_string(), 0)
        } else if focused {
            single_line_window(g, value, caret, rect.2 - 22.0, 12.0)
        } else {
            (fit(g, value, rect.2 - 22.0, 12.0, false), 0)
        };
        draw_text(
            g,
            rect.0 + 11.0,
            rect.1 + 10.0,
            &shown,
            12.0,
            if value.is_empty() {
                theme::text_dim()
            } else {
                theme::text()
            },
            false,
        );
        if focused {
            let prefix: String = value
                .chars()
                .skip(visible_start)
                .take(caret.saturating_sub(visible_start).min(value.chars().count()))
                .collect();
            let cx = rect.0 + 11.0 + g.measure_chrome_text(&prefix, 12.0, false);
            draw_preedit_and_caret(g, s, (cx, rect.1 + 9.0), 18.0, caret_out);
        }
    }
    g.pop_clip();
}

fn single_line_window(
    g: &mut gpu::GpuRenderer,
    text: &str,
    caret: usize,
    width: f32,
    font: f32,
) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let caret = caret.min(chars.len());
    let mut start = caret;
    while start > 0 {
        let candidate: String = chars[start - 1..caret].iter().collect();
        if g.measure_chrome_text(&candidate, font, false) > width * 0.72 {
            break;
        }
        start -= 1;
    }
    let mut end = caret;
    while end < chars.len() {
        let candidate: String = chars[start..=end].iter().collect();
        if g.measure_chrome_text(&candidate, font, false) > width {
            break;
        }
        end += 1;
    }
    (chars[start..end].iter().collect(), start)
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

/// 이름 옆에 붙는 알약. 상태를 한 낱말로 못박아 두면 어느 줄이 지금 쓰이는
/// 것인지 카드 테두리 색을 해석하지 않고도 읽힌다.
fn pill(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str, accent: bool) -> f32 {
    let f = 9.5;
    let w = g.measure_chrome_text(text, f, false) + 14.0;
    round_rect(
        g,
        x,
        y,
        w,
        17.0,
        8.5,
        if accent {
            theme::surface_active()
        } else {
            theme::surface_hover()
        },
    );
    draw_text(
        g,
        x + 7.0,
        y + 4.0,
        text,
        f,
        if accent {
            theme::accent()
        } else {
            theme::text_mute()
        },
        false,
    );
    w
}

/// 아이콘만 있는 단추는 뜻을 모른 채 나란히 서면 누르기가 무섭다 — 계정 줄처럼
/// 되돌리기 어려운 것이 섞인 자리에서는 글자를 함께 세운다. 폭을 돌려주므로
/// 오른쪽 끝에서부터 거꾸로 쌓을 수 있다.
#[allow(clippy::too_many_arguments)]
fn mini_text_button(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: f32,
    icon: &str,
    label: &str,
    target: Target,
    danger: bool,
) -> f32 {
    let f = 10.5;
    let w = g.measure_chrome_text(label, f, false) + 32.0;
    let rect = (x, y, w, 26.0);
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
    let col = if hover {
        if danger {
            theme::danger()
        } else {
            theme::text()
        }
    } else {
        theme::text_mute()
    };
    g.queue_icon(icon, x + 8.0, y + 6.5, 13.0, col);
    draw_text(g, x + 25.0, y + 7.0, label, f, col, false);
    register_clipped(g, hits, target, rect, HitCursor::Pointer);
    g.hover_pointer |= hover;
    w
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
    let text = crate::native_strings::text(text);
    g.draw_text(
        x,
        y,
        &text,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}

fn fit(g: &mut gpu::GpuRenderer, text: &str, width: f32, font: f32, bold: bool) -> String {
    let translated = crate::native_strings::text(text);
    let text = translated.as_ref();
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
        assert!(is_multiline(SettingsInput::StudentRaw));
        assert!(is_multiline(SettingsInput::FeedbackBody));
        assert!(!is_multiline(SettingsInput::CwdPath));
    }

    #[test]
    fn student_model_and_roster_actions_refresh_the_cached_selection() {
        assert!(action_refreshes_cache(&SettingsAction::StudentModel(
            "model".to_string(),
            "backend".to_string(),
        )));
        assert!(action_refreshes_cache(&SettingsAction::SelectTheme(
            "theme".to_string(),
        )));
        assert!(!action_refreshes_cache(&SettingsAction::CursorShape(
            cursor::CursorShape::Frame,
        )));
    }

    #[test]
    fn ordinary_settings_actions_never_trigger_full_media_decode() {
        for action in [
            SettingsAction::UiLanguage("en"),
            SettingsAction::Accent("blue".to_string()),
            SettingsAction::StudentModel("model".to_string(), String::new()),
            SettingsAction::ToggleFeedbackDiag,
            SettingsAction::AccountAutoswitchPct(85),
        ] {
            assert!(!action_refreshes_media(&action));
        }
        assert!(!remote_action_refreshes_media("set-language"));
        assert!(!remote_action_refreshes_media("palette-hex"));
        assert!(action_refreshes_media(&SettingsAction::RefreshStudentAssets));
        assert!(remote_action_refreshes_media("refresh-assets"));
    }

    #[test]
    fn direct_shell_field_never_appends_to_a_preset() {
        assert_eq!(direct_shell_seed("/bin/zsh", false), "");
        assert_eq!(direct_shell_seed("/bin/bash", false), "");
        assert_eq!(direct_shell_seed("", false), "");
        assert_eq!(direct_shell_seed("C:\\Windows\\System32\\cmd.exe", true), "");
        assert_eq!(
            direct_shell_seed("/opt/homebrew/bin/fish", false),
            "/opt/homebrew/bin/fish"
        );
    }

    #[test]
    fn multiline_view_keeps_a_deep_caret_inside_the_field() {
        assert_eq!(multiline_first_line(14, 6, true), 9);
        assert_eq!(multiline_first_line(2, 6, true), 0);
        assert_eq!(multiline_first_line(14, 6, false), 0);
    }

    #[test]
    fn multiline_click_rows_and_vertical_motion_use_global_character_indices() {
        let layout = MultilineLayout {
            field: SettingsInput::StudentPersona,
            rect: (100.0, 200.0, 300.0, 132.0),
            rows: vec![
                VisualRow {
                    start: 0,
                    len: 2,
                    // 실제 shaper가 잰 한글 두 글자 폭처럼 균등하지 않은 좌표.
                    caret_xs: vec![0.0, 13.0, 27.0],
                },
                VisualRow {
                    start: 3,
                    len: 3,
                    caret_xs: vec![0.0, 7.0, 15.0, 22.0],
                },
            ],
            first_line: 0,
            visible_lines: 6,
        };
        assert_eq!(multiline_caret_from_point(&layout, (126.0, 228.0)), 5);
        assert_eq!(move_multiline_caret(&layout, 1, true), 5);
        assert_eq!(move_multiline_caret(&layout, 5, false), 1);
    }

    #[test]
    fn main_modals_own_pointer_and_click_before_the_settings_view() {
        let handler = include_str!("handler.rs");
        let cursor = handler
            .split_once("WindowEvent::CursorMoved { position, .. } => {")
            .unwrap()
            .1;
        assert!(
            cursor.find("let main_modal").unwrap()
                < cursor.find("native_settings_contains").unwrap()
        );

        let mouse = handler
            .split_once("button: MouseButton::Left,\n                ..\n            } => {")
            .unwrap()
            .1;
        let native = mouse.find("native_settings_contains").unwrap();
        assert!(mouse.find("self.confirm_close.is_some()").unwrap() < native);
        assert!(mouse.find("self.restore_prompt.is_some()").unwrap() < native);
        assert!(mouse.find("self.account_switch_confirm").unwrap() < native);
        assert!(mouse.find("self.git.commit_modal_open").unwrap() < native);
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

    #[test]
    fn native_settings_keeps_every_web_settings_feature_reachable() {
        let native = include_str!("native_settings.rs");
        let web = [
            include_str!("../../../web/arona-ui/src/settings/GeneralTab.tsx"),
            include_str!("../../../web/arona-ui/src/settings/lang.tsx"),
            include_str!("../../../web/arona-ui/src/settings/AppearanceTab.tsx"),
            include_str!("../../../web/arona-ui/src/settings/ShellTab.tsx"),
            include_str!("../../../web/arona-ui/src/settings/ClaudeTab.tsx"),
            include_str!("../../../web/arona-ui/src/settings/ThemeTab.tsx"),
            include_str!("../../../web/arona-ui/src/settings/CharacterDetail.tsx"),
            include_str!("../../../web/arona-ui/src/settings/ThemeGen.tsx"),
            include_str!("../../../web/arona-ui/src/settings/MotionSprites.tsx"),
            include_str!("../../../web/arona-ui/src/settings/FeedbackTab.tsx"),
        ]
        .join("\n");
        for (feature, web_needle, native_needle) in [
            ("language", "set-language", "UiLanguage"),
            ("system theme slots", "theme-system-${slot}", "ThemeSystemSlot"),
            ("custom palette rename", "rename-custom-theme", "FocusCustomThemeLabel"),
            ("palette wheel", "ColorWheel", "PickerSV"),
            ("eyedropper", "palette-eyedropper", "PaletteEyedropper"),
            ("isolated reauth", "reauth-account-isolated", "LoginBrowser::Isolated"),
            ("account label", "account-label", "FocusAccountLabel"),
            ("theme roster", "theme-pick-all", "ThemePickAll"),
            ("raw character", "rawSave", "SaveStudentRaw"),
            ("theme generation", "theme-gen-start", "ThemeGenStart"),
            ("motion frames", "character-sprite", "SelectMotionFrame"),
            ("feedback draft", "feedback-draft", "feedback_draft"),
        ] {
            assert!(web.contains(web_needle), "web lost {feature}: {web_needle}");
            assert!(native.contains(native_needle), "native lost {feature}: {native_needle}");
        }
    }

    #[test]
    fn parity_settings_bundle_roundtrips_through_an_isolated_file() {
        let dir = std::env::temp_dir().join(format!(
            "kasaterm-settings-parity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("settings.json");
        let value = serde_json::json!({
            "language": "en",
            "file_open_mode": "terminal",
            "file_open_app": "",
            "file_open_cmd": "code --goto {path}:{line}",
            "editor_autosave_ms": 10000,
            "theme_system_light": "light",
            "theme_system_dark": "custom:night",
            "claude_account_autoswitch_pct": 85,
            "feedback_draft": "쓰다 만 초안",
            "theme_gen_provider": "nanobanana"
        });
        socket::write_settings_value_atomic_at(&path, &value).unwrap();
        let roundtrip: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(roundtrip, value);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dropped_images_are_bounded_before_their_bytes_are_allocated() {
        let dir = std::env::temp_dir().join(format!("kasaterm-drop-limit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(SPRITE_DROP_MAX_BYTES + 1).unwrap();
        assert!(drop_size_ok(&path, SPRITE_DROP_MAX_BYTES).is_err());
        assert!(drop_size_ok(&path, THEMEGEN_DROP_MAX_BYTES).is_ok());
        drop(file);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn palette_preview_marks_the_field_dirty_and_commit_refreshes_visible_cache() {
        let source = include_str!("native_settings.rs");
        let click = source
            .split_once("pub(crate) fn native_settings_click")
            .unwrap()
            .1
            .split_once("pub(crate) fn native_settings_drag_move")
            .unwrap()
            .0;
        assert!(click.find("mark_field_dirty").unwrap() < click.find("picker_preview").unwrap());
        let settings = include_str!("settings.rs");
        let apply = settings
            .split_once("pub(crate) fn apply_palette_edit")
            .unwrap()
            .1
            .split_once("pub(crate) fn preview_palette_edit")
            .unwrap()
            .0;
        assert!(apply.find("refresh_palette_cache").unwrap() < apply.find("repaint_all").unwrap());
    }

    #[test]
    fn remote_student_open_preserves_the_theme_context() {
        let handler = include_str!("handler.rs");
        let open = handler
            .split_once("\"open-student\" =>")
            .unwrap()
            .1
            .split_once("\"select-theme\"")
            .unwrap()
            .0;
        assert!(open.contains("label.clone().unwrap_or_default()"));
        assert!(open.contains("SelectStudentInTheme(theme, arg.clone())"));
    }

    #[test]
    /// 스크롤바는 스크롤되는 영역의 오른쪽에 붙어야 한다. 글자가 앉는 칼럼
    /// (`content_x`/`content_w`)을 그대로 주면 그 좌우 여백만큼 안으로 들어와,
    /// 막대가 패널 가장자리에서 떨어진 허공에 뜬다(2026-09-05 지적 · 실측 58px).
    /// 설정과 보드가 같은 자리에서 같은 실수를 했으므로 둘 다 지킨다.
    #[test]
    fn the_scrollbar_hugs_the_panel_edge_not_the_text_column() {
        for (label, source) in [
            ("설정", include_str!("native_settings.rs")),
            ("보드", include_str!("native_board.rs")),
        ] {
            assert!(
                source.contains("paint_scroll_affordance(\n        g,\n        scroll_x,"),
                "{label} 스크롤바가 글자 칼럼 기준이면 패널 가장자리에서 떨어져 뜬다"
            );
        }
    }

    #[test]
    fn settings_renderer_uses_cached_visual_feedback_for_every_character_surface() {
        let source = include_str!("native_settings.rs");
        for call in [".draw_face(", ".draw_reference(", ".draw_motion_frame(", ".draw_motion_preview("] {
            assert!(source.contains(call), "missing media integration: {call}");
        }
        let handler = include_str!("handler.rs");
        assert!(handler.contains("self.settings_media_animating()"));
        let motion = source
            .split_once("fn paint_motion_sprites(")
            .unwrap()
            .1
            .split_once("fn paint_themegen_engine(")
            .unwrap()
            .0;
        assert!(motion.find("clip_hit(preview)").unwrap() < motion.find("mark_motion_preview_visible").unwrap());
        let animating = source
            .split_once("pub(crate) fn settings_media_animating")
            .unwrap()
            .1
            .split_once("/// 계정 신원")
            .unwrap()
            .0;
        assert!(
            animating.find("motion_preview_visible").unwrap()
                < animating.find("next_motion_frame_in").unwrap()
        );
        let paint = &source
            [source.find("pub(crate) fn paint(").unwrap()..source.find("#[cfg(test)]").unwrap()];
        assert!(!paint.contains("std::fs::"));
    }

    #[test]
    fn motion_timer_stays_off_for_raw_tabs_and_offscreen_previews() {
        assert!(!motion_preview_pump_needed(true, true, false));
        assert!(!motion_preview_pump_needed(true, false, true));
        assert!(!motion_preview_pump_needed(false, true, true));
        assert!(motion_preview_pump_needed(true, true, true));
    }
}
