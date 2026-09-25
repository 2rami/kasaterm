//! 첫 실행 흐름의 네이티브 상태와 wgpu 페인터.
//!
//! 터미널 프로필·폰트 탐색은 느리고 프로세스까지 띄울 수 있어 작업 스레드에서 한
//! 번만 채운다. 프레임은 `State`의 Arc 스냅샷만 읽고, 로그인만 1초 간격으로 캐시를
//! 다시 확인한다.

use super::*;

pub(crate) type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppearanceMode {
    Import,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthStatus {
    LoggedIn,
    LoggedOut,
    Checking,
    NotInstalled,
}

#[derive(Clone, Debug)]
pub(crate) struct AuthInfo {
    status: AuthStatus,
    account: Option<String>,
}

impl AuthInfo {
    fn load(provider: AccountProvider) -> Self {
        match provider {
            AccountProvider::Claude => {
                let mut ids = vec![String::new()];
                ids.extend(
                    socket::read_claude_accounts()
                        .into_iter()
                        .map(|account| account.id),
                );
                let probes: Vec<_> = ids
                    .iter()
                    .map(|id| (id, settings::auth_probe(id)))
                    .collect();
                if let Some((id, probe)) = probes.iter().find_map(|(id, probe)| {
                    probe
                        .as_ref()
                        .filter(|value| value.logged_in)
                        .map(|value| (*id, value))
                }) {
                    let account = if !probe.org.is_empty() && !probe.org.contains(&probe.email) {
                        Some(probe.org.clone())
                    } else if !probe.email.is_empty() {
                        Some(probe.email.clone())
                    } else {
                        settings::account_identity(id)
                    };
                    return Self {
                        status: AuthStatus::LoggedIn,
                        account,
                    };
                }
                let installed = onboarding::command_available("claude");
                Self {
                    status: if installed && probes.iter().any(|(_, probe)| probe.is_none()) {
                        AuthStatus::Checking
                    } else if installed {
                        AuthStatus::LoggedOut
                    } else {
                        AuthStatus::NotInstalled
                    },
                    account: None,
                }
            }
            AccountProvider::Codex => {
                let mut ids = vec![String::new()];
                ids.extend(
                    socket::read_codex_accounts()
                        .into_iter()
                        .map(|account| account.id),
                );
                if let Some(id) = ids.iter().find(|id| settings::codex_logged_in(id)) {
                    return Self {
                        status: AuthStatus::LoggedIn,
                        account: settings::codex_identity(id),
                    };
                }
                Self {
                    status: if onboarding::command_available("codex") {
                        AuthStatus::LoggedOut
                    } else {
                        AuthStatus::NotInstalled
                    },
                    account: None,
                }
            }
        }
    }

    fn logged_in(&self) -> bool {
        self.status == AuthStatus::LoggedIn
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ShellChoice {
    id: String,
    label: String,
    path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Data {
    platform: &'static str,
    imports: Arc<Vec<onboarding::ImportSource>>,
    fonts: Arc<Vec<String>>,
    font_family: Option<String>,
    restart_required: bool,
    shells: Arc<Vec<ShellChoice>>,
    selected_shell: String,
    claude: AuthInfo,
    codex: AuthInfo,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            platform: platform_key(),
            imports: Arc::new(Vec::new()),
            fonts: Arc::new(Vec::new()),
            font_family: None,
            restart_required: false,
            shells: Arc::new(Vec::new()),
            selected_shell: String::new(),
            claude: AuthInfo {
                status: AuthStatus::Checking,
                account: None,
            },
            codex: AuthInfo {
                status: AuthStatus::Checking,
                account: None,
            },
        }
    }
}

impl Data {
    fn load() -> Self {
        let platform = platform_key();
        let shells: Vec<ShellChoice> = available_shells()
            .into_iter()
            .map(|(label, _, path)| ShellChoice {
                id: shell_id(label).to_string(),
                label: label.to_string(),
                path,
            })
            .collect();
        let selected_path = socket::read_default_shell().unwrap_or_default();
        let selected_shell = shells
            .iter()
            .find(|choice| choice.path.eq_ignore_ascii_case(&selected_path))
            .map(|choice| choice.id.clone())
            .unwrap_or(selected_path);
        Self {
            platform,
            imports: Arc::new(if platform == "macos" {
                onboarding::import_sources()
            } else {
                Vec::new()
            }),
            fonts: Arc::new(onboarding::font_families()),
            font_family: onboarding::current_font_family(),
            restart_required: onboarding::font_restart_required(),
            shells: Arc::new(shells),
            selected_shell,
            claude: AuthInfo::load(AccountProvider::Claude),
            codex: AuthInfo::load(AccountProvider::Codex),
        }
    }

    fn auth(&self, provider: AccountProvider) -> &AuthInfo {
        match provider {
            AccountProvider::Claude => &self.claude,
            AccountProvider::Codex => &self.codex,
        }
    }

    fn auth_mut(&mut self, provider: AccountProvider) -> &mut AuthInfo {
        match provider {
            AccountProvider::Claude => &mut self.claude,
            AccountProvider::Codex => &mut self.codex,
        }
    }
}

fn platform_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

fn shell_id(label: &str) -> &'static str {
    match label {
        "PowerShell 7" => "pwsh",
        "Windows PowerShell" => "powershell",
        "Command Prompt" => "cmd",
        "Git Bash" => "git-bash",
        "WSL" => "wsl",
        _ => "shell",
    }
}

#[derive(Clone, Debug)]
pub(crate) struct State {
    step: u8,
    expanded: Option<u8>,
    details: [bool; 4],
    appearance: AppearanceMode,
    last_imported: Option<String>,
    data: Arc<Data>,
    loaded: bool,
    loading: bool,
    load_generation: Option<u64>,
    auth_poll: Option<(AccountProvider, std::time::Instant, u8, bool)>,
    auth_generation: Option<(u64, AccountProvider)>,
    notice: Option<(bool, String)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            step: 0,
            expanded: None,
            details: [false; 4],
            appearance: AppearanceMode::Manual,
            last_imported: None,
            data: Arc::new(Data::default()),
            loaded: false,
            loading: false,
            load_generation: None,
            auth_poll: None,
            auth_generation: None,
            notice: None,
        }
    }
}

impl State {
    pub(crate) fn shell_path_is_preset(&self, path: &str) -> bool {
        self.data
            .shells
            .iter()
            .any(|choice| choice.path.eq_ignore_ascii_case(path))
    }

    pub(crate) fn request_load(&mut self, proxy: EventLoopProxy<UserEvent>) {
        if self.loading || self.loaded {
            return;
        }
        self.loading = true;
        self.load_generation = Some(request_data(proxy));
    }

    fn apply_loaded(&mut self, data: Data) {
        if !self.loaded {
            self.appearance =
                if data.platform == "macos" && data.imports.iter().any(import_supported) {
                    AppearanceMode::Import
                } else {
                    AppearanceMode::Manual
                };
        }
        self.loaded = true;
        self.loading = false;
        self.load_generation = None;
        self.data = Arc::new(data);
        if self.data.claude.status == AuthStatus::Checking {
            self.auth_poll = Some((
                AccountProvider::Claude,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
                60,
                false,
            ));
        }
    }

    fn go(&mut self, step: u8) -> bool {
        if step > 3 || !self.loaded {
            return false;
        }
        let changed = self.expanded != Some(step);
        self.step = step;
        self.expanded = Some(step);
        self.notice = None;
        changed
    }

    fn next(&mut self) -> bool {
        if !self.loaded {
            return false;
        }
        let step = (self.step + 1).min(3);
        self.go(step)
    }

    fn back(&mut self) -> bool {
        self.go(self.step.saturating_sub(1))
    }

    fn start_auth_poll(&mut self, provider: AccountProvider) {
        self.auth_poll = Some((
            provider,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            60,
            true,
        ));
        self.auth_generation = None;
        self.notice = Some((true, "브라우저에서 로그인을 마쳐 주세요".to_string()));
    }

    fn poll_auth(&mut self, proxy: EventLoopProxy<UserEvent>) -> bool {
        let Some((provider, due, left, waiting_login)) = self.auth_poll else {
            return false;
        };
        if let Some((generation, pending_provider)) = self.auth_generation {
            let Some(auth) = take_auth_data(generation, pending_provider) else {
                return false;
            };
            self.auth_generation = None;
            let logged_in = auth.logged_in();
            let settled = auth.status != AuthStatus::Checking;
            let mut data = (*self.data).clone();
            *data.auth_mut(provider) = auth;
            self.data = Arc::new(data);
            if logged_in || left <= 1 || (!waiting_login && settled) {
                self.auth_poll = None;
                if waiting_login {
                    self.notice = logged_in.then(|| (true, "로그인을 확인했어요".to_string()));
                }
            } else {
                self.auth_poll = Some((
                    provider,
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                    left - 1,
                    waiting_login,
                ));
            }
            return true;
        }
        if std::time::Instant::now() < due {
            return false;
        }
        self.auth_generation = Some((request_auth_data(proxy, provider), provider));
        false
    }

    fn set_font(&mut self, family: String) {
        let mut data = (*self.data).clone();
        data.font_family = Some(family);
        data.restart_required = true;
        self.data = Arc::new(data);
    }

    fn set_imported(&mut self, id: String, restart_required: bool, font_family: Option<String>) {
        self.last_imported = Some(id);
        if restart_required || font_family.is_some() {
            let mut data = (*self.data).clone();
            data.restart_required |= restart_required;
            if let Some(family) = font_family {
                data.font_family = Some(family);
            }
            self.data = Arc::new(data);
        }
    }

    fn set_shell(&mut self, id: String) {
        let mut data = (*self.data).clone();
        data.selected_shell = id;
        self.data = Arc::new(data);
    }

    fn set_notice(&mut self, ok: bool, text: impl Into<String>) {
        self.notice = Some((ok, text.into()));
    }

    fn force_step_for_capture(&mut self, step: u8) {
        let step = step.min(3);
        self.step = step;
        self.expanded = Some(step);
    }
}

fn import_supported(source: &onboarding::ImportSource) -> bool {
    source.detected && matches!(source.support.as_str(), "full" | "partial")
}

type LoadCell = std::sync::Mutex<Option<(u64, Data)>>;

fn load_cell() -> &'static LoadCell {
    static CELL: std::sync::OnceLock<LoadCell> = std::sync::OnceLock::new();
    CELL.get_or_init(Default::default)
}

fn request_data(proxy: EventLoopProxy<UserEvent>) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        let data = Data::load();
        if let Ok(mut cell) = load_cell().lock() {
            *cell = Some((generation, data));
        }
        let _ = proxy.send_event(UserEvent::Redraw);
    });
    generation
}

fn take_data(generation: u64) -> Option<Data> {
    let mut cell = load_cell().lock().ok()?;
    if cell.as_ref().is_some_and(|(got, _)| *got == generation) {
        cell.take().map(|(_, data)| data)
    } else {
        None
    }
}

type AuthLoadCell = std::sync::Mutex<Option<(u64, AccountProvider, AuthInfo)>>;

fn auth_load_cell() -> &'static AuthLoadCell {
    static CELL: std::sync::OnceLock<AuthLoadCell> = std::sync::OnceLock::new();
    CELL.get_or_init(Default::default)
}

fn request_auth_data(proxy: EventLoopProxy<UserEvent>, provider: AccountProvider) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        let auth = AuthInfo::load(provider);
        if let Ok(mut cell) = auth_load_cell().lock() {
            *cell = Some((generation, provider, auth));
        }
        let _ = proxy.send_event(UserEvent::Redraw);
    });
    generation
}

fn take_auth_data(generation: u64, provider: AccountProvider) -> Option<AuthInfo> {
    let mut cell = auth_load_cell().lock().ok()?;
    if cell
        .as_ref()
        .is_some_and(|(got, got_provider, _)| *got == generation && *got_provider == provider)
    {
        cell.take().map(|(_, _, auth)| auth)
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Go(u8),
    ToggleSection(u8),
    ToggleDetails(u8),
    Back,
    Next,
    Skip,
    Finish,
    Appearance(AppearanceMode),
    Import(String),
    Font(String),
    FontDelta(i8),
    Accent(String),
    Login(AccountProvider),
    CancelLogin,
    Preferred(AccountProvider),
    Shell(String),
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    area: Rect,
    cursor: (f32, f32),
    scroll: f32,
    caret_on: bool,
    input: Option<SettingsInput>,
    preedit: String,
    settings_caret: usize,
    shell: String,
    theme: String,
    accent: String,
    font_size: f32,
    preferred_agent: String,
    palettes: Arc<Vec<crate::native_settings::PaletteChoice>>,
    state: State,
    language: String,
}

pub(crate) fn snapshot(app: &App, area: Rect) -> Snapshot {
    Snapshot {
        area,
        cursor: app.cursor_px,
        scroll: app.settings_scene.scroll(),
        caret_on: app.last_blink_on,
        input: app.settings_input,
        preedit: app.preedit.clone(),
        settings_caret: app.settings_caret,
        shell: app.set_shell.clone(),
        theme: theme::theme_name(),
        accent: theme::accent_name().to_string(),
        font_size: app.font_size,
        preferred_agent: crate::agent_preferences::preferred_agent().to_string(),
        palettes: app.settings_scene.cache().palettes.clone(),
        state: app.settings_scene.onboarding().clone(),
        language: app.settings_scene.cache().language().to_string(),
    }
}

impl App {
    pub(crate) fn begin_native_onboarding(&mut self) {
        if self.settings_scene.first_run() {
            if let Ok(step) = std::env::var("KASATERM_AUTOONBOARDING_STEP") {
                if let Ok(step) = step.parse::<u8>() {
                    self.settings_scene
                        .onboarding_mut()
                        .force_step_for_capture(step);
                }
            }
            self.settings_scene
                .onboarding_mut()
                .request_load(self.proxy.clone());
        }
    }

    pub(crate) fn pump_native_onboarding(&mut self) {
        if !self.settings_scene.first_run() && self.settings_scene.category() != SettingsCat::Shell
        {
            return;
        }
        self.settings_scene
            .onboarding_mut()
            .request_load(self.proxy.clone());
        let loaded = self
            .settings_scene
            .onboarding()
            .load_generation
            .and_then(take_data);
        let changed = if let Some(data) = loaded {
            self.settings_scene.onboarding_mut().apply_loaded(data);
            true
        } else {
            self.settings_scene
                .onboarding_mut()
                .poll_auth(self.proxy.clone())
        };
        if changed {
            self.chrome_dirty = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    pub(crate) fn native_onboarding_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};
        if event.state != ElementState::Pressed || !self.settings_scene.first_run() {
            return false;
        }
        let action = match event.logical_key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => Action::Back,
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => Action::Next,
            Key::Named(NamedKey::Enter) if self.host_mod() => Action::Finish,
            Key::Named(NamedKey::Enter) => {
                Action::ToggleSection(self.settings_scene.onboarding().step)
            }
            Key::Named(NamedKey::Home) => Action::Go(0),
            Key::Named(NamedKey::End) => Action::Go(3),
            _ => return true,
        };
        self.native_onboarding_action(action);
        true
    }

    pub(crate) fn native_onboarding_action(&mut self, action: Action) {
        match action {
            Action::ToggleSection(section) => {
                if section > 3 || !self.settings_scene.onboarding().loaded {
                    return;
                }
                self.native_settings_blur();
                let state = self.settings_scene.onboarding_mut();
                state.step = section;
                state.expanded = if state.expanded == Some(section) {
                    None
                } else {
                    Some(section)
                };
                self.settings_scene.reset_scroll();
            }
            Action::ToggleDetails(section) => {
                if let Some(details) = self
                    .settings_scene
                    .onboarding_mut()
                    .details
                    .get_mut(section as usize)
                {
                    *details = !*details;
                }
            }
            Action::Go(step) => {
                if self.settings_scene.onboarding_mut().go(step) {
                    self.native_settings_blur();
                    self.settings_scene.reset_scroll();
                }
            }
            Action::Back => {
                if self.settings_scene.onboarding_mut().back() {
                    self.native_settings_blur();
                    self.settings_scene.reset_scroll();
                }
            }
            Action::Next => {
                if self.settings_scene.onboarding_mut().next() {
                    self.native_settings_blur();
                    self.settings_scene.reset_scroll();
                }
            }
            Action::Skip => self.finish_native_onboarding(None, true),
            Action::Finish => {
                if !self.settings_scene.onboarding().loaded {
                    return;
                }
                let preferred = match crate::agent_preferences::preferred_agent() {
                    "codex" => AccountProvider::Codex,
                    _ => AccountProvider::Claude,
                };
                let preferred = self
                    .settings_scene
                    .onboarding()
                    .data
                    .auth(preferred)
                    .logged_in()
                    .then_some(preferred);
                self.finish_native_onboarding(preferred, false);
            }
            Action::Appearance(mode) => {
                self.settings_scene.onboarding_mut().appearance = mode;
                self.settings_scene.reset_scroll();
            }
            Action::Import(id) => match onboarding::apply_terminal_profile(&id) {
                Ok(applied) => {
                    if let Some(size) = applied.font_size {
                        self.font_size = size.clamp(9.0, 32.0);
                    }
                    theme::apply_from_settings();
                    self.apply_effective_scale();
                    self.begin_theme_fx();
                    self.repaint_all();
                    self.settings_scene.refresh_cache();
                    self.settings_scene.onboarding_mut().set_imported(
                        id,
                        applied.restart_required,
                        onboarding::current_font_family(),
                    );
                }
                Err(error) => self
                    .settings_scene
                    .onboarding_mut()
                    .set_notice(false, error),
            },
            Action::Font(family) => match onboarding::apply_font_family(&family) {
                Ok(()) => {
                    self.settings_scene.onboarding_mut().set_font(family);
                    self.set_toast("재시작하면 폰트가 적용돼요".to_string());
                }
                Err(error) => self
                    .settings_scene
                    .onboarding_mut()
                    .set_notice(false, error),
            },
            Action::FontDelta(delta) => self.settings_apply(SettingsAction::FontSizeDelta(delta)),
            Action::Accent(name) => self.settings_apply(SettingsAction::Accent(name)),
            Action::Login(provider) => {
                if settings::hidden_login_running() {
                    self.settings_scene
                        .onboarding_mut()
                        .set_notice(false, "다른 로그인이 끝난 뒤 다시 시도해 주세요");
                    return;
                }
                self.settings_apply(match provider {
                    AccountProvider::Claude => SettingsAction::AddClaudeAccount,
                    AccountProvider::Codex => SettingsAction::AddCodexAccount,
                });
                if settings::hidden_login_running() {
                    self.settings_scene
                        .onboarding_mut()
                        .start_auth_poll(provider);
                }
            }
            Action::CancelLogin => {
                settings::cancel_hidden_login();
                self.settings_scene.onboarding_mut().auth_poll = None;
                self.settings_scene.onboarding_mut().auth_generation = None;
                self.settings_scene
                    .onboarding_mut()
                    .set_notice(true, "로그인을 취소했어요");
            }
            Action::Preferred(provider) => {
                if self
                    .settings_scene
                    .onboarding()
                    .data
                    .auth(provider)
                    .logged_in()
                {
                    self.settings_apply(SettingsAction::PreferredAgent(match provider {
                        AccountProvider::Claude => "claude",
                        AccountProvider::Codex => "codex",
                    }));
                    self.settings_scene.refresh_cache();
                }
            }
            Action::Shell(id) => match onboarding::apply_default_shell(&id) {
                Ok(path) => {
                    self.set_shell = path;
                    self.settings_scene.onboarding_mut().set_shell(id);
                }
                Err(error) => self
                    .settings_scene
                    .onboarding_mut()
                    .set_notice(false, error),
            },
        }
        if !self.settings_scene.first_run() {
            if let Some((_, message)) = self.settings_scene.onboarding_mut().notice.take() {
                self.set_toast(message);
            }
        }
        self.chrome_dirty = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn finish_native_onboarding(&mut self, preferred: Option<AccountProvider>, skip: bool) {
        let provider = preferred.map(|provider| match provider {
            AccountProvider::Claude => "claude",
            AccountProvider::Codex => "codex",
        });
        if !skip {
            if let Some(provider) = preferred {
                let logged_in = self
                    .settings_scene
                    .onboarding()
                    .data
                    .auth(provider)
                    .logged_in();
                if !logged_in {
                    self.settings_scene
                        .onboarding_mut()
                        .set_notice(false, "먼저 로그인해 주세요");
                    return;
                }
            }
        }
        let result = if skip {
            onboarding::skip()
        } else {
            onboarding::complete(provider)
        };
        match result {
            Ok(()) => {
                self.settings_scene.finish_onboarding();
                self.return_from_settings_room();
            }
            Err(error) => self
                .settings_scene
                .onboarding_mut()
                .set_notice(false, error),
        }
    }
}

pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    snapshot: &Snapshot,
    settings: &crate::native_settings::Snapshot,
) -> crate::native_settings::PaintOutput {
    crate::native_strings::set_language(&snapshot.language);
    use crate::native_settings::{HitCursor, PaintOutput, Target};
    let (ax, ay, aw, ah) = snapshot.area;
    let (rail_w, mx, mw) = quick_columns(ax, aw);
    let feedback = snapshot
        .state
        .notice
        .as_ref()
        .map(|(ok, text)| (*ok, text.as_str()))
        .or_else(|| {
            snapshot
                .state
                .data
                .restart_required
                .then_some((true, "새 글꼴은 kasaterm을 다시 열면 적용돼요."))
        });
    let footer_h = if feedback.is_some() { 102.0 } else { 62.0 };
    let mut hits = Vec::new();
    let mut caret_rect = None;
    g.rect(ax, ay, aw, ah, theme::bg());
    g.rect(ax, ay, rail_w, ah, theme::panel_bg());
    text(
        g,
        ax + 20.0,
        ay + 24.0,
        "kasaterm",
        13.0,
        theme::text(),
        true,
    );
    text(
        g,
        ax + 20.0,
        ay + 58.0,
        "처음 설정",
        11.0,
        theme::text_dim(),
        false,
    );
    for (index, (label, icon, _)) in QUICK_SECTIONS.iter().enumerate() {
        let rect = (
            ax + 12.0,
            ay + 88.0 + index as f32 * 40.0,
            rail_w - 24.0,
            40.0,
        );
        let on = snapshot.state.expanded == Some(index as u8);
        let color = if on {
            theme::accent()
        } else {
            theme::text_dim()
        };
        g.queue_icon(icon, rect.0 + 8.0, rect.1 + 12.0, 16.0, color);
        text(g, rect.0 + 32.0, rect.1 + 13.0, label, 12.0, color, on);
        if snapshot.state.loaded {
            hit(
                &mut hits,
                Target::Onboarding(Action::ToggleSection(index as u8)),
                rect,
                HitCursor::Pointer,
            );
            g.hover_pointer |= inside(rect, snapshot.cursor);
        }
    }
    text(g, mx, ay + 24.0, "빠른 설정", 20.0, theme::text(), true);
    let hint = fit(
        g,
        "핵심만 고르고 시작하세요. 나중에도 설정에서 바꿀 수 있어요.",
        mw,
        11.5,
        false,
    );
    text(g, mx, ay + 56.0, &hint, 11.5, theme::text_dim(), false);

    let body_top = ay + 88.0;
    let view_h = (ah - 88.0 - footer_h - 8.0).max(1.0);
    let nav_hit_count = hits.len();
    g.push_clip(mx, body_top, mw, view_h);
    let mut y = body_top - snapshot.scroll;
    if !snapshot.state.loaded {
        loading_slab(g, mx, &mut y, mw);
    } else {
        for (index, (label, icon, hint)) in QUICK_SECTIONS.iter().enumerate() {
            let section = index as u8;
            let expanded = snapshot.state.expanded == Some(section);
            let value = quick_value(snapshot, settings, section);
            let row_h = if mw < 470.0 { 80.0 } else { 64.0 };
            let row = (mx, y, mw, row_h);
            g.queue_icon(icon, mx, y + 14.0, 16.0, theme::text_dim());
            text(g, mx + 26.0, y + 12.0, label, 12.0, theme::text(), true);
            let desc = fit(
                g,
                hint,
                if mw < 470.0 {
                    mw - 100.0
                } else {
                    mw * 0.48 - 26.0
                },
                10.5,
                false,
            );
            text(
                g,
                mx + 26.0,
                y + 33.0,
                &desc,
                10.5,
                theme::text_dim(),
                false,
            );
            let value_w = if mw < 470.0 {
                mw - 100.0
            } else {
                mw * 0.48 - 86.0
            };
            let shown = fit(g, &value, value_w.max(40.0), 11.5, false);
            let value_x = if mw < 470.0 {
                mx + 26.0
            } else {
                mx + mw * 0.52
            };
            text(
                g,
                value_x,
                y + if mw < 470.0 { 56.0 } else { 23.0 },
                &shown,
                11.5,
                theme::text_dim(),
                false,
            );
            button(
                g,
                snapshot,
                &mut hits,
                (mx + mw - 64.0, y + 12.0, 64.0, 26.0),
                if expanded { "접기" } else { "변경" },
                Target::Onboarding(Action::ToggleSection(section)),
                expanded,
            );
            y += row.3;
            if expanded {
                if section == 2 {
                    paint_auth(g, snapshot, &mut hits, mx, &mut y, mw);
                }
                if section == 3 && snapshot.state.data.platform == "windows" {
                    paint_platform(g, snapshot, &mut hits, &mut caret_rect, mx, &mut y, mw);
                } else {
                    crate::native_settings::paint_setup_section(
                        g,
                        settings,
                        &mut hits,
                        &mut caret_rect,
                        mx,
                        &mut y,
                        mw,
                        section,
                    );
                    if section == 3 {
                        onboarding_shell_field(g, snapshot, &mut hits, &mut caret_rect, mx, y, mw);
                        y += 60.0;
                    }
                }
                if section == 1 || section == 3 {
                    let detail_rect = (mx, y, mw, 40.0);
                    g.queue_icon(
                        if snapshot.state.details[index] {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        mx,
                        y + 12.0,
                        16.0,
                        theme::text_dim(),
                    );
                    text(
                        g,
                        mx + 26.0,
                        y + 13.0,
                        if section == 1 {
                            "세부 설정 · 기존 터미널에서 가져오기"
                        } else {
                            "세부 설정 · 터미널 글꼴"
                        },
                        11.5,
                        theme::text_dim(),
                        false,
                    );
                    hit(
                        &mut hits,
                        Target::Onboarding(Action::ToggleDetails(section)),
                        detail_rect,
                        HitCursor::Pointer,
                    );
                    g.hover_pointer |= inside(detail_rect, snapshot.cursor);
                    y += 40.0;
                    if snapshot.state.details[index] {
                        if section == 1 {
                            paint_appearance(g, snapshot, &mut hits, mx, &mut y, mw);
                        } else {
                            paint_fonts(g, snapshot, &mut hits, mx, &mut y, mw);
                        }
                    }
                }
                y += 14.0;
            }
            divider(g, mx, y, mw);
        }
        y += 24.0;
        paint_preview(g, snapshot, settings, mx, &mut y, mw);
    }
    // The scrolling content clip must not erase the fixed navigation targets.
    let mut content_hits = hits.split_off(nav_hit_count);
    clip_hits(g, &mut content_hits);
    hits.extend(content_hits);
    g.pop_clip();

    divider(g, mx, ay + ah - footer_h, mw);
    if let Some((ok, message)) = feedback {
        let feedback_y = ay + ah - footer_h + 12.0;
        g.queue_icon(
            if ok { "check" } else { "triangle-alert" },
            mx,
            feedback_y,
            14.0,
            if ok {
                theme::text_dim()
            } else {
                theme::danger()
            },
        );
        let shown = fit(g, message, mw - 24.0, 11.5, false);
        text(
            g,
            mx + 24.0,
            feedback_y + 1.0,
            &shown,
            11.5,
            theme::text(),
            false,
        );
    }
    text_button(
        g,
        snapshot,
        &mut hits,
        (mx, ay + ah - 47.0, 100.0, 32.0),
        "건너뛰기",
        Target::Onboarding(Action::Skip),
    );
    let forward = (mx + mw - 120.0, ay + ah - 47.0, 120.0, 32.0);
    if snapshot.state.loaded {
        button(
            g,
            snapshot,
            &mut hits,
            forward,
            "시작하기",
            Target::Onboarding(Action::Finish),
            true,
        );
    } else {
        stroke(g, forward, theme::border());
        text(
            g,
            forward.0 + 24.0,
            forward.1 + 9.0,
            "확인 중…",
            11.5,
            theme::text_mute(),
            false,
        );
    }
    let content_h = (y + snapshot.scroll - body_top + 18.0).max(view_h);
    crate::native_settings::paint_scroll_affordance(
        g,
        mx,
        body_top,
        mw + 14.0,
        view_h,
        content_h,
        snapshot.scroll,
    );
    PaintOutput {
        hits,
        dropdown_scroll_max: 0.0,
        content_h,
        view_h,
        caret_rect,
        multiline_layouts: Vec::new(),
        motion_preview_visible: false,
    }
}

const QUICK_SECTIONS: [(&str, &str, &str); 4] = [
    ("캐릭터", "users", "함께 사용할 캐릭터 테마"),
    ("모양", "sparkles", "색상과 화면 형태"),
    ("코딩 에이전트", "claude", "연결 계정과 권한 기본값"),
    ("터미널", "terminal", "새 작업의 셸과 글꼴"),
];

fn quick_columns(ax: f32, width: f32) -> (f32, f32, f32) {
    let rail = if width < 760.0 { 154.0 } else { 190.0 };
    let pad = if width < 760.0 { 20.0 } else { 28.0 };
    let available = (width - rail - pad * 2.0).max(1.0);
    let content = available.min(800.0);
    (rail, ax + rail + pad + (available - content) / 2.0, content)
}

fn quick_value(s: &Snapshot, settings: &crate::native_settings::Snapshot, section: u8) -> String {
    match section {
        0 => settings
            .themes
            .iter()
            .find(|row| row.id == settings.character_theme)
            .map(|row| row.label.clone())
            .unwrap_or_else(|| "기본 테마".into()),
        1 => {
            let label = s
                .palettes
                .iter()
                .find(|p| p.key == s.theme)
                .map(|p| p.label.as_str())
                .unwrap_or(&s.theme);
            let shape = match settings.shape.as_str() {
                "sharp" => "각지게",
                "pixel" => "픽셀",
                _ => "둥글게",
            };
            format!("{label} · {shape}")
        }
        2 => {
            let provider = if settings.preferred_agent == "codex" {
                AccountProvider::Codex
            } else {
                AccountProvider::Claude
            };
            format!(
                "{} · {}",
                provider.label(),
                auth_label(s.state.data.auth(provider).status)
            )
        }
        _ => {
            if s.shell.is_empty() {
                "시스템 기본".into()
            } else {
                s.shell.clone()
            }
        }
    }
}

fn paint_preview(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    settings: &crate::native_settings::Snapshot,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    text(g, x, *y, "미리보기", 11.0, theme::text_dim(), false);
    *y += 24.0;
    let rect = (x, *y, w, 112.0);
    stroke(g, rect, theme::border());
    text(
        g,
        x + 14.0,
        *y + 12.0,
        "kasaterm · 터미널 예시",
        11.0,
        theme::text_dim(),
        false,
    );
    divider(g, x, *y + 34.0, w);
    text(
        g,
        x + 14.0,
        *y + 49.0,
        "$ pwd",
        12.0,
        theme::accent(),
        false,
    );
    text(
        g,
        x + 14.0,
        *y + 72.0,
        "~/project",
        12.0,
        theme::text(),
        false,
    );
    if let Some(row) = settings
        .themes
        .iter()
        .find(|row| row.id == settings.character_theme)
    {
        if let Some((slug, _)) = row.faces.first() {
            settings
                .media
                .draw_face(g, &row.id, slug, (x + w - 60.0, *y + 42.0, 40.0, 54.0));
        }
    }
    *y += 124.0;
    let summary = fit(
        g,
        &format!(
            "{} · {}",
            quick_value(s, settings, 0),
            quick_value(s, settings, 1)
        ),
        w,
        10.5,
        false,
    );
    text(g, x, *y, &summary, 10.5, theme::text_dim(), false);
    *y += 24.0;
}
fn paint_appearance(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    use crate::native_settings::Target;
    if s.state.data.platform == "macos" {
        segmented(
            g,
            s,
            hits,
            x,
            *y,
            w,
            &[
                (
                    "기존 설정 가져오기",
                    s.state.appearance == AppearanceMode::Import,
                    Action::Appearance(AppearanceMode::Import),
                ),
                (
                    "직접 설정",
                    s.state.appearance == AppearanceMode::Manual,
                    Action::Appearance(AppearanceMode::Manual),
                ),
            ],
        );
        *y += 48.0;
    }
    if s.state.appearance == AppearanceMode::Import && s.state.data.platform == "macos" {
        section(
            g,
            x,
            y,
            "Mac 터미널에서 가져오기",
            "색상과 글꼴만 읽고 원본 프로필은 바꾸지 않습니다",
        );
        if s.state.data.imports.is_empty() {
            empty_slab(
                g,
                x,
                y,
                w,
                "가져올 Apple Terminal 또는 iTerm2 프로필을 찾지 못했어요",
            );
        }
        for source in s.state.data.imports.iter() {
            // 목업: 구분선 행. 왼쪽 작은 미리보기, 이름·프로필, 오른쪽 아웃라인 단추.
            let rect = (x, *y, w, 60.0);
            let bg = source
                .background
                .as_deref()
                .and_then(theme::parse_hex)
                .map(rgba)
                .unwrap_or_else(theme::surface);
            let fg = source
                .foreground
                .as_deref()
                .and_then(theme::parse_hex)
                .map(rgba)
                .unwrap_or_else(theme::text_dim);
            let preview = (rect.0, rect.1 + 8.0, 84.0, 44.0);
            round_rect(
                g,
                preview.0,
                preview.1,
                preview.2,
                preview.3,
                theme::radius_sm(),
                bg,
            );
            g.round_rect_stroke(
                preview.0,
                preview.1,
                preview.2,
                preview.3,
                theme::radius_sm(),
                1.0,
                theme::border(),
            );
            text(
                g,
                preview.0 + 8.0,
                preview.1 + 6.0,
                "❯ kasaterm",
                10.0,
                fg,
                false,
            );
            for (index, color) in source.ansi16.iter().skip(1).take(5).enumerate() {
                if let Some(color) = theme::parse_hex(color).map(rgba) {
                    round_rect(
                        g,
                        preview.0 + 8.0 + index as f32 * 9.0,
                        preview.1 + 30.0,
                        6.0,
                        6.0,
                        3.0,
                        color,
                    );
                }
            }
            text(
                g,
                rect.0 + 104.0,
                rect.1 + 13.0,
                &source.label,
                12.5,
                theme::text(),
                true,
            );
            let detail = source
                .profile
                .as_deref()
                .or(source.reason.as_deref())
                .unwrap_or("가져올 수 없음");
            text(
                g,
                rect.0 + 104.0,
                rect.1 + 33.0,
                detail,
                11.0,
                theme::text_dim(),
                false,
            );
            let supported = import_supported(source);
            let applied = s.state.last_imported.as_deref() == Some(source.id.as_str());
            if supported {
                button(
                    g,
                    s,
                    hits,
                    (rect.0 + rect.2 - 76.0, rect.1 + 17.0, 76.0, 26.0),
                    if applied { "적용됨" } else { "가져오기" },
                    Target::Onboarding(Action::Import(source.id.clone())),
                    !applied,
                );
            }
            divider(g, x, rect.1 + 59.0, w);
            *y += 60.0;
        }
        return;
    }

    paint_fonts(g, s, hits, x, y, w);
    *y += 12.0;
    section(g, x, y, "강조색", "선택 영역과 커서, 링크에 함께 씁니다");
    let accents = theme::ACCENT_PRESETS
        .iter()
        .map(|(name, _)| {
            (
                (*name).to_string(),
                s.accent == *name,
                Action::Accent((*name).to_string()),
            )
        })
        .collect();
    chips(g, s, hits, x, y, w, accents);
}

pub(crate) fn paint_fonts(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section(
        g,
        x,
        y,
        "터미널 글꼴",
        "설치된 고정폭 글꼴과 글자 크기를 고릅니다",
    );
    if s.state.data.fonts.is_empty() {
        empty_slab(
            g,
            x,
            y,
            w,
            "감지한 고정폭 글꼴이 없어 현재 시스템 글꼴을 유지합니다",
        );
    } else {
        let fonts = s
            .state
            .data
            .fonts
            .iter()
            .map(|font| {
                (
                    font.clone(),
                    s.state.data.font_family.as_deref() == Some(font.as_str()),
                    Action::Font(font.clone()),
                )
            })
            .collect();
        chips(g, s, hits, x, y, w, fonts);
    }
    stepper(
        g,
        s,
        hits,
        x,
        y,
        w,
        "글자 크기",
        &format!("{:.0}px", s.font_size),
        Action::FontDelta(-1),
        Action::FontDelta(1),
    );
}

fn paint_auth(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section(
        g,
        x,
        y,
        "첫 터미널에서 먼저 쓸 Agent",
        "로그인된 항목을 기본으로 고를 수 있습니다",
    );
    for provider in [AccountProvider::Claude, AccountProvider::Codex] {
        auth_row(g, s, hits, x, y, w, provider);
    }
    *y += 12.0;
    // 목업: 채움 없이 회색 왼쪽 선 하나에 작은 안내문.
    let rect = (x, *y, w, 36.0);
    g.rect(rect.0, rect.1, 2.0, rect.3, theme::border());
    text(
        g,
        rect.0 + 14.0,
        rect.1 + 11.0,
        "인증정보는 각 도구가 보관하고, kasaterm은 토큰이나 비밀번호를 저장하지 않아요.",
        11.0,
        theme::text_dim(),
        false,
    );
    *y += 48.0;
}

fn auth_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    provider: AccountProvider,
) {
    use crate::native_settings::Target;
    let auth = s.state.data.auth(provider);
    let selected = s.preferred_agent
        == match provider {
            AccountProvider::Claude => "claude",
            AccountProvider::Codex => "codex",
        };
    let rect = (x, *y, w, 56.0);
    if selected {
        // 목업 .sel — 행을 살짝 감싸는 강조색 테두리만.
        g.round_rect_stroke(
            rect.0 - 8.0,
            rect.1 + 3.0,
            rect.2 + 16.0,
            rect.3 - 6.0,
            theme::radius_md().min(5.0),
            1.0,
            theme::accent(),
        );
    }
    g.queue_icon(
        provider.icon(),
        rect.0 + 2.0,
        rect.1 + 17.0,
        22.0,
        theme::text(),
    );
    let label = provider.label();
    text(
        g,
        rect.0 + 40.0,
        rect.1 + 10.0,
        label,
        12.5,
        theme::text(),
        true,
    );
    if selected {
        let lw = g.measure_chrome_text(label, 12.5, true);
        text(
            g,
            rect.0 + 46.0 + lw,
            rect.1 + 11.0,
            "기본",
            11.0,
            theme::accent(),
            false,
        );
    }
    text(
        g,
        rect.0 + 40.0,
        rect.1 + 31.0,
        auth.account
            .as_deref()
            .unwrap_or_else(|| auth_label(auth.status)),
        11.0,
        theme::text_dim(),
        false,
    );
    if auth.logged_in() {
        if !selected {
            text_button(
                g,
                s,
                hits,
                (rect.0 + rect.2 - 72.0, rect.1 + 15.0, 72.0, 26.0),
                "기본으로",
                Target::Onboarding(Action::Preferred(provider)),
            );
        }
        hit(
            hits,
            Target::Onboarding(Action::Preferred(provider)),
            rect,
            crate::native_settings::HitCursor::Pointer,
        );
    } else if settings::hidden_login_running() {
        button(
            g,
            s,
            hits,
            (rect.0 + rect.2 - 64.0, rect.1 + 15.0, 64.0, 26.0),
            "취소",
            Target::Onboarding(Action::CancelLogin),
            false,
        );
    } else if auth.status == AuthStatus::LoggedOut {
        button(
            g,
            s,
            hits,
            (rect.0 + rect.2 - 64.0, rect.1 + 15.0, 64.0, 26.0),
            "로그인",
            Target::Onboarding(Action::Login(provider)),
            true,
        );
    } else if auth.status == AuthStatus::Checking {
        g.queue_icon(
            "rotate-cw",
            rect.0 + rect.2 - 22.0,
            rect.1 + 21.0,
            14.0,
            theme::text_mute(),
        );
    }
    divider(g, x, rect.1 + 55.0, w);
    *y += 56.0;
}

pub(crate) fn paint_platform(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    use crate::native_settings::Target;
    if s.state.data.platform == "windows" {
        section(
            g,
            x,
            y,
            "Windows 기본 셸",
            "새 pane을 열 때 시작할 셸입니다",
        );
        for shell in s.state.data.shells.iter() {
            let rect = (x, *y, w, 48.0);
            let selected = s.state.data.selected_shell == shell.id;
            if selected {
                g.round_rect_stroke(
                    rect.0 - 8.0,
                    rect.1 + 3.0,
                    rect.2 + 16.0,
                    rect.3 - 6.0,
                    theme::radius_md().min(5.0),
                    1.0,
                    theme::accent(),
                );
            }
            let hover = inside(rect, s.cursor);
            text(
                g,
                rect.0,
                rect.1 + 8.0,
                &shell.label,
                12.5,
                theme::text(),
                true,
            );
            text(
                g,
                rect.0,
                rect.1 + 28.0,
                &shell.path,
                10.5,
                theme::text_dim(),
                false,
            );
            hit(
                hits,
                Target::Onboarding(Action::Shell(shell.id.clone())),
                rect,
                crate::native_settings::HitCursor::Pointer,
            );
            g.hover_pointer |= hover;
            divider(g, x, rect.1 + 47.0, w);
            *y += 48.0;
        }
        *y += 14.0;
        onboarding_shell_field(g, s, hits, caret, x, *y, w);
        *y += 60.0;
        return;
    }
    section(
        g,
        x,
        y,
        if s.state.data.platform == "macos" {
            "Mac 터미널 설정"
        } else {
            "기본 셸"
        },
        "지금까지 고른 값을 첫 pane에 그대로 사용합니다",
    );
    let imported = s
        .state
        .last_imported
        .as_deref()
        .and_then(|id| s.state.data.imports.iter().find(|source| source.id == id))
        .map(|source| source.label.as_str())
        .unwrap_or("kasaterm에서 직접 설정");
    summary_row(g, x, y, w, "가져온 곳", imported);
    let theme_label = s
        .palettes
        .iter()
        .find(|palette| palette.key == s.theme)
        .map(|palette| palette.label.as_str())
        .unwrap_or(s.theme.as_str());
    summary_row(g, x, y, w, "현재 테마", theme_label);
    let font = s.state.data.font_family.as_deref().unwrap_or("시스템 글꼴");
    summary_row(
        g,
        x,
        y,
        w,
        "현재 글꼴",
        &format!("{font} · {:.0}px", s.font_size),
    );
    summary_row(
        g,
        x,
        y,
        w,
        "기본 셸",
        if s.shell.is_empty() {
            "시스템 기본"
        } else {
            &s.shell
        },
    );
}

fn onboarding_shell_field(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    caret_out: &mut Option<Rect>,
    x: f32,
    y: f32,
    w: f32,
) {
    use crate::native_settings::{HitCursor, Target};
    text(g, x, y, "셸 경로 직접 입력", 11.0, theme::text_dim(), false);
    let rect = (x, y + 18.0, w, 32.0);
    let focused = s.input == Some(SettingsInput::Shell);
    g.round_rect_stroke(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md().min(5.0),
        1.0,
        if focused {
            theme::accent()
        } else {
            theme::border()
        },
    );
    hit(
        hits,
        Target::Focus(SettingsInput::Shell),
        rect,
        HitCursor::Text,
    );
    text(
        g,
        rect.0 + 10.0,
        rect.1 + 8.0,
        if s.shell.is_empty() {
            "경로를 입력하세요"
        } else {
            &s.shell
        },
        12.0,
        if s.shell.is_empty() {
            theme::text_mute()
        } else {
            theme::text()
        },
        false,
    );
    if focused {
        let prefix: String = s
            .shell
            .chars()
            .take(s.settings_caret.min(s.shell.chars().count()))
            .collect();
        let cx = rect.0 + 10.0 + g.measure_chrome_text(&prefix, 12.0, false);
        if !s.preedit.is_empty() {
            text(g, cx, rect.1 + 7.0, &s.preedit, 12.0, theme::text(), false);
        } else if s.caret_on {
            g.rect(cx, rect.1 + 7.0, 1.5, 18.0, theme::cursor());
        }
        *caret_out = Some((cx, rect.1 + 7.0, 2.0, 18.0));
    }
}

fn auth_label(status: AuthStatus) -> &'static str {
    match status {
        AuthStatus::LoggedIn => "로그인됨",
        AuthStatus::LoggedOut => "로그인이 필요해요",
        AuthStatus::Checking => "확인 중…",
        AuthStatus::NotInstalled => "설치되지 않았어요",
    }
}

fn loading_slab(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32) {
    // 목업(플랫): 채움 없이 아이콘 하나와 두 줄.
    g.queue_icon("rotate-cw", x, *y + 10.0, 16.0, theme::accent());
    text(
        g,
        x + 26.0,
        *y + 8.0,
        "설치 환경을 확인하고 있어요",
        12.5,
        theme::text(),
        true,
    );
    text(
        g,
        x + 26.0,
        *y + 28.0,
        "테마, 터미널 설정, Agent 로그인을 한 번만 살펴봅니다.",
        11.0,
        theme::text_dim(),
        false,
    );
    divider(g, x, *y + 55.0, w);
    *y += 64.0;
}

fn empty_slab(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str) {
    text(g, x, *y + 14.0, message, 11.0, theme::text_mute(), false);
    divider(g, x, *y + 43.0, w);
    *y += 52.0;
}

/// 묶음 머리 한 줄 — 「제목 — 설명」 흐린 글자(목업 .group h2). 줄을 나아가게 한다.
fn section(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, title: &str, subtitle: &str) {
    let line = if subtitle.is_empty() {
        title.to_string()
    } else {
        format!("{title} — {subtitle}")
    };
    text(g, x, *y + 6.0, &line, 11.5, theme::text_dim(), false);
    *y += 32.0;
}

/// 행 아래 얇은 구분선. 카드 채움 대신 이것으로 행을 가른다.
fn divider(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32) {
    g.rect(x, y, w, 1.0, theme::with_alpha(theme::border(), 140));
}

/// 이름표 왼쪽 · 값 오른쪽의 정보 행(목업 .kv).
fn summary_row(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str, value: &str) {
    text(g, x, *y + 10.0, label, 11.5, theme::text_dim(), false);
    let shown = fit(g, value, w - 120.0, 11.5, false);
    let vw = g.measure_chrome_text(&shown, 11.5, false);
    text(g, x + w - vw, *y + 10.0, &shown, 11.5, theme::text(), false);
    divider(g, x, *y + 33.0, w);
    *y += 34.0;
}

/// 목업의 구분 선택 — 테두리 하나, 고른 칸만 강조색 테두리+글자. 왼쪽에 붙는다.
fn segmented(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: f32,
    w: f32,
    cells: &[(&str, bool, Action)],
) {
    let h = 26.0;
    let inset = 2.0;
    let pad = 10.0;
    let widths: Vec<f32> = cells
        .iter()
        .map(|(label, _, _)| g.measure_chrome_text(label, 11.5, false) + pad * 2.0)
        .collect();
    let total = (widths.iter().sum::<f32>() + inset * 2.0).min(w);
    g.round_rect_stroke(x, y, total, h, 4.0, 1.0, theme::border());
    let mut cx = x + inset;
    for (index, (label, selected, action)) in cells.iter().enumerate() {
        let rect = (cx, y + inset, widths[index], h - inset * 2.0);
        cx += widths[index];
        let hover = inside(rect, s.cursor);
        if *selected {
            g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, 3.0, 1.0, theme::accent());
        }
        let tx = rect.0 + (rect.2 - g.measure_chrome_text(label, 11.5, false)) / 2.0;
        text(
            g,
            tx,
            rect.1 + 4.0,
            label,
            11.5,
            if *selected {
                theme::accent()
            } else if hover {
                theme::text()
            } else {
                theme::text_dim()
            },
            false,
        );
        hit(
            hits,
            crate::native_settings::Target::Onboarding(action.clone()),
            rect,
            crate::native_settings::HitCursor::Pointer,
        );
        g.hover_pointer |= hover;
    }
}

/// 아웃라인 칩 여러 개를 줄바꿈해 깐다 — 고른 것만 강조색.
fn chips(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    cells: Vec<(String, bool, Action)>,
) {
    let mut cx = x;
    let mut cy = *y;
    for (label, selected, action) in cells {
        let cw = (g.measure_chrome_text(&label, 11.0, false) + 22.0).min(w);
        if cx + cw > x + w && cx > x {
            cx = x;
            cy += 32.0;
        }
        let rect = (cx, cy, cw, 26.0);
        choice(
            g,
            s,
            hits,
            rect,
            selected,
            crate::native_settings::Target::Onboarding(action),
        );
        let hover = inside(rect, s.cursor);
        let shown = fit(g, &label, rect.2 - 20.0, 11.0, false);
        text(
            g,
            rect.0 + 11.0,
            rect.1 + 6.0,
            &shown,
            11.0,
            if selected {
                theme::accent()
            } else if hover {
                theme::text()
            } else {
                theme::text_dim()
            },
            false,
        );
        cx += cw + 6.0;
    }
    *y = cy + 26.0 + 16.0;
}

/// 이름표 왼쪽 · 오른쪽에 − 값 + 의 조절 행. 구분선으로 마감.
#[allow(clippy::too_many_arguments)]
fn stepper(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    label: &str,
    value: &str,
    minus: Action,
    plus: Action,
) {
    text(g, x, *y + 12.0, label, 12.0, theme::text(), false);
    let right = x + w;
    button(
        g,
        s,
        hits,
        (right - 96.0, *y + 7.0, 26.0, 26.0),
        "−",
        crate::native_settings::Target::Onboarding(minus),
        false,
    );
    button(
        g,
        s,
        hits,
        (right - 26.0, *y + 7.0, 26.0, 26.0),
        "+",
        crate::native_settings::Target::Onboarding(plus),
        false,
    );
    let vw = g.measure_chrome_text(value, 11.5, false);
    text(
        g,
        right - 48.0 - vw / 2.0,
        *y + 13.0,
        value,
        11.5,
        theme::text(),
        false,
    );
    divider(g, x, *y + 39.0, w);
    *y += 40.0;
}

/// 고를 수 있는 아웃라인 칸(목업 .btn / .sel) — 채움 없이 테두리 색만 바뀐다.
fn choice(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    rect: Rect,
    selected: bool,
    target: crate::native_settings::Target,
) {
    let hover = inside(rect, s.cursor);
    if hover && !selected {
        round_rect(
            g,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_md().min(5.0),
            theme::surface_hover(),
        );
    }
    stroke(
        g,
        rect,
        if selected {
            theme::accent()
        } else if hover {
            theme::text_dim()
        } else {
            theme::border()
        },
    );
    hit(
        hits,
        target,
        rect,
        crate::native_settings::HitCursor::Pointer,
    );
    g.hover_pointer |= hover;
}

/// 목업(플랫): 채움 없는 아웃라인 단추. 주 단추는 강조색 테두리+글자.
fn button(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    rect: Rect,
    label: &str,
    target: crate::native_settings::Target,
    primary: bool,
) {
    let hover = inside(rect, s.cursor);
    if hover {
        round_rect(
            g,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_md().min(5.0),
            theme::surface_hover(),
        );
    }
    let line = if primary {
        theme::accent()
    } else if hover {
        theme::text_dim()
    } else {
        theme::border()
    };
    g.round_rect_stroke(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md().min(5.0),
        1.0,
        line,
    );
    let shown = fit(g, label, rect.2 - 14.0, 11.5, false);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, false)) / 2.0;
    text(
        g,
        tx,
        rect.1 + (rect.3 - 12.0) / 2.0 - 1.0,
        &shown,
        11.5,
        if primary {
            theme::accent()
        } else {
            theme::text()
        },
        false,
    );
    hit(
        hits,
        target,
        rect,
        crate::native_settings::HitCursor::Pointer,
    );
    g.hover_pointer |= hover;
}

/// 테두리도 없는 글자 단추(목업 .btn.txt) — 보조 동작.
fn text_button(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    rect: Rect,
    label: &str,
    target: crate::native_settings::Target,
) {
    let hover = inside(rect, s.cursor);
    let shown = fit(g, label, rect.2 - 4.0, 11.5, false);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, false)) / 2.0;
    text(
        g,
        tx,
        rect.1 + (rect.3 - 12.0) / 2.0 - 1.0,
        &shown,
        11.5,
        if hover {
            theme::text()
        } else {
            theme::text_dim()
        },
        false,
    );
    hit(
        hits,
        target,
        rect,
        crate::native_settings::HitCursor::Pointer,
    );
    g.hover_pointer |= hover;
}

fn hit(
    hits: &mut Vec<crate::native_settings::Hit>,
    target: crate::native_settings::Target,
    rect: Rect,
    cursor: crate::native_settings::HitCursor,
) {
    hits.push(crate::native_settings::Hit {
        target,
        rect,
        cursor,
    });
}

fn clip_hits(g: &gpu::GpuRenderer, hits: &mut Vec<crate::native_settings::Hit>) {
    hits.retain_mut(|hit| match g.clip_hit(hit.rect) {
        Some(rect) => {
            hit.rect = rect;
            true
        }
        None => false,
    });
}

fn stroke(g: &mut gpu::GpuRenderer, rect: Rect, color: [u8; 4]) {
    g.round_rect_stroke(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md().min(5.0),
        1.0,
        color,
    );
}

fn text(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    value: &str,
    size: f32,
    color: [u8; 4],
    bold: bool,
) {
    let value = crate::native_strings::text(value);
    g.draw_text(
        x,
        y,
        &value,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}

fn fit(g: &mut gpu::GpuRenderer, value: &str, width: f32, size: f32, bold: bool) -> String {
    let translated = crate::native_strings::text(value);
    let value = translated.as_ref();
    if g.measure_chrome_text(value, size, bold) <= width {
        return value.to_string();
    }
    let mut out = String::new();
    for ch in value.chars() {
        let candidate = format!("{out}{ch}…");
        if g.measure_chrome_text(&candidate, size, bold) > width {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn inside(rect: Rect, point: (f32, f32)) -> bool {
    point.0 >= rect.0
        && point.0 <= rect.0 + rect.2
        && point.1 >= rect.1
        && point.1 <= rect.1 + rect.3
}

fn rgba(color: [u8; 3]) -> [u8; 4] {
    [color[0], color[1], color[2], 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_setup_opens_any_section_after_loading_without_overwriting_choices() {
        let mut state = State::default();
        assert_eq!(state.expanded, None);
        assert!(!state.go(2));
        state.loaded = true;
        state.details[1] = true;
        assert!(state.go(3));
        assert_eq!(state.expanded, Some(3));
        assert!(state.back());
        assert_eq!(state.expanded, Some(2));
        assert!(state.go(0));
        assert!(state.next());
        assert_eq!(state.expanded, Some(1));
        assert!(state.details[1]);
        assert!(!state.go(4));
    }

    #[test]
    fn quick_setup_column_stays_inside_window_and_centers_at_wide_sizes() {
        for width in [520.0, 640.0, 760.0, 1280.0, 2560.0] {
            let (rail, x, content) = quick_columns(0.0, width);
            assert!(x >= rail + 20.0);
            assert!(content <= 800.0 && content > 0.0);
            assert!(x + content <= width - 20.0);
            let left = x - rail;
            let right = width - x - content;
            assert!((left - right).abs() < 0.01);
        }
    }

    #[test]
    fn auth_reload_preserves_expanded_controls() {
        let mut state = State::default();
        let mut data = Data::default();
        data.claude.status = AuthStatus::LoggedIn;
        state.apply_loaded(data.clone());
        state.go(2);
        state.details[1] = true;
        state.apply_loaded(data);
        assert_eq!(state.expanded, Some(2));
        assert!(state.details[1]);
        assert!(state.data.auth(AccountProvider::Claude).logged_in());
    }

    #[test]
    fn painter_source_is_io_free() {
        let source = include_str!("native_onboarding.rs");
        let paint = &source
            [source.find("pub(crate) fn paint(").unwrap()..source.find("#[cfg(test)]").unwrap()];
        for forbidden in ["std::fs::", "read_settings(", "Command::new", "TcpStream"] {
            assert!(
                !paint.contains(forbidden),
                "렌더 경로에 I/O가 들어왔다: {forbidden}"
            );
        }
    }

    #[test]
    fn launch_records_open_only_after_the_room_exists_and_before_restore_prompt() {
        let handler = include_str!("handler.rs");
        let resumed = handler.split_once("fn resumed(").unwrap().1;
        let open = resumed.find("self.open_settings_inline").unwrap();
        let marked = resumed.find("crate::onboarding::mark_opened();").unwrap();
        let restore = resumed.find("self.restore_prompt = Some(state);").unwrap();
        assert!(open < marked && marked < restore);
    }

    #[test]
    fn desktop_inline_host_no_longer_has_a_settings_variant_but_http_api_remains() {
        let main = include_str!("main.rs");
        let variants = main
            .split_once("pub(crate) enum InlineWebKind {")
            .unwrap()
            .1
            .split_once('}')
            .unwrap()
            .0;
        assert!(!variants.contains("Settings"));
        for removed in ["student_web_webview", "student_web_window"] {
            assert!(
                !main.contains(removed),
                "desktop Settings still owns Wry: {removed}"
            );
        }
        let chrome = include_str!("chrome.rs");
        for removed in ["open_student_web_window", "settings.html"] {
            assert!(
                !chrome.contains(removed),
                "desktop Settings Wry route remains: {removed}"
            );
        }
        let socket = include_str!("socket.rs");
        assert!(socket.contains("\"onboarding-state\""));
        assert!(socket.contains("crate::settings::onboarding_state_json()"));
    }

    #[test]
    fn auth_poll_never_runs_the_probe_on_the_gui_thread() {
        let source = include_str!("native_onboarding.rs");
        let poll = source
            .split_once("fn poll_auth(")
            .unwrap()
            .1
            .split_once("fn set_font(")
            .unwrap()
            .0;
        assert!(!poll.contains("AuthInfo::load"));
        assert!(poll.contains("request_auth_data"));
        let worker = source
            .split_once("fn request_auth_data(")
            .unwrap()
            .1
            .split_once("fn take_auth_data(")
            .unwrap()
            .0;
        assert!(worker.contains("std::thread::spawn"));
        assert!(worker.contains("AuthInfo::load"));
    }

    #[test]
    fn login_actions_are_guarded_before_a_second_job_can_spawn() {
        let settings = include_str!("settings.rs");
        let spawn = settings
            .split_once("fn spawn_hidden_login(")
            .unwrap()
            .1
            .split_once("/// 결과를 기록한다")
            .unwrap()
            .0;
        assert!(
            spawn.find("LoginState::Running").unwrap() < spawn.find("std::thread::spawn").unwrap()
        );

        let action = include_str!("native_onboarding.rs")
            .split_once("Action::Login(provider) =>")
            .unwrap()
            .1
            .split_once("Action::CancelLogin")
            .unwrap()
            .0;
        assert!(
            action.find("hidden_login_running").unwrap() < action.find("settings_apply").unwrap()
        );
    }

    #[test]
    fn imported_profile_refreshes_the_summary_font_immediately() {
        let source = include_str!("native_onboarding.rs");
        let import = source
            .split_once("Action::Import(id) =>")
            .unwrap()
            .1
            .split_once("Action::Font(family)")
            .unwrap()
            .0;
        assert!(import.contains("onboarding::current_font_family()"));
    }
}
