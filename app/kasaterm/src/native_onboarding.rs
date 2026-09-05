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
    preferred: Option<AccountProvider>,
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
            preferred: None,
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
        let saved = socket::read_settings();
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
            preferred: match saved
                .get("preferred_agent")
                .and_then(|value| value.as_str())
            {
                Some("claude") => Some(AccountProvider::Claude),
                Some("codex") => Some(AccountProvider::Codex),
                _ => None,
            },
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
    furthest: u8,
    appearance: AppearanceMode,
    preferred: Option<AccountProvider>,
    preferred_touched: bool,
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
            furthest: 0,
            appearance: AppearanceMode::Manual,
            preferred: None,
            preferred_touched: false,
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
        if !self.preferred_touched {
            self.preferred = data
                .preferred
                .filter(|provider| data.auth(*provider).logged_in())
                .or_else(|| {
                    [AccountProvider::Claude, AccountProvider::Codex]
                        .into_iter()
                        .find(|provider| data.auth(*provider).logged_in())
                });
        }
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
        if step > self.furthest || step > 3 {
            return false;
        }
        let changed = self.step != step;
        self.step = step;
        self.notice = None;
        changed
    }

    fn next(&mut self) -> bool {
        if !self.loaded {
            return false;
        }
        let step = (self.step + 1).min(3);
        self.furthest = self.furthest.max(step);
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
                if logged_in && !self.preferred_touched && self.preferred.is_none() {
                    self.preferred = Some(provider);
                }
                self.notice = logged_in.then(|| (true, "로그인을 확인했어요".to_string()));
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

    fn set_imported(
        &mut self,
        id: String,
        restart_required: bool,
        font_family: Option<String>,
    ) {
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
        self.furthest = self.furthest.max(step);
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
    Back,
    Next,
    Skip,
    Finish,
    Appearance(AppearanceMode),
    Import(String),
    Theme(String),
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
    palettes: Arc<Vec<crate::native_settings::PaletteChoice>>,
    state: State,
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
        palettes: app.settings_scene.cache().palettes.clone(),
        state: app.settings_scene.onboarding().clone(),
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
        if !self.settings_scene.first_run() {
            return;
        }
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
            Key::Named(NamedKey::ArrowLeft) => Action::Back,
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Enter) => {
                if self.settings_scene.onboarding().step == 3 {
                    Action::Finish
                } else {
                    Action::Next
                }
            }
            Key::Named(NamedKey::Home) => Action::Go(0),
            Key::Named(NamedKey::End) => Action::Go(self.settings_scene.onboarding().furthest),
            _ => return true,
        };
        self.native_onboarding_action(action);
        true
    }

    pub(crate) fn native_onboarding_action(&mut self, action: Action) {
        match action {
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
                let preferred = self.settings_scene.onboarding().preferred;
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
                    self.settings_scene
                        .onboarding_mut()
                        .set_imported(
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
            Action::Theme(key) => self.settings_apply(SettingsAction::ThemeMode(key)),
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
                    let onboarding = self.settings_scene.onboarding_mut();
                    onboarding.preferred = Some(provider);
                    onboarding.preferred_touched = true;
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
) -> crate::native_settings::PaintOutput {
    use crate::native_settings::{HitCursor, PaintOutput, Target};
    let (ax, ay, aw, ah) = snapshot.area;
    let rail_w = if aw < 760.0 { 154.0 } else { 190.0 };
    let footer_h = 62.0;
    let mut hits = Vec::new();
    let mut caret_rect = None;
    g.rect(ax, ay, aw, ah, theme::bg());
    g.rect(ax, ay, rail_w, ah, theme::panel_bg());
    g.rect(ax + rail_w - 1.0, ay, 1.0, ah, theme::border());
    text(
        g,
        ax + 20.0,
        ay + 20.0,
        "kasaterm",
        18.0,
        theme::text(),
        true,
    );
    text(
        g,
        ax + 20.0,
        ay + 48.0,
        "처음 설정",
        11.0,
        theme::text_dim(),
        false,
    );

    let steps = ["외형", "Agent", "터미널", "완료"];
    let mut py = ay + 92.0;
    for (index, label) in steps.iter().enumerate() {
        let step = index as u8;
        let enabled = step <= snapshot.state.furthest;
        let current = step == snapshot.state.step;
        let rect = (ax + 10.0, py, rail_w - 20.0, 42.0);
        if current || (enabled && inside(rect, snapshot.cursor)) {
            round_rect(
                g,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                theme::radius_md(),
                if current {
                    theme::surface_active()
                } else {
                    theme::surface_hover()
                },
            );
        }
        let dot = (rect.0 + 12.0, rect.1 + 10.0, 22.0, 22.0);
        round_rect(
            g,
            dot.0,
            dot.1,
            dot.2,
            dot.3,
            11.0,
            if current {
                theme::accent()
            } else {
                theme::surface_hover()
            },
        );
        let number = ["1", "2", "3", "4"][index];
        text(
            g,
            dot.0 + 7.0,
            dot.1 + 5.0,
            number,
            10.5,
            if current { [255, 255, 255, 255] } else { theme::text() },
            true,
        );
        text(
            g,
            rect.0 + 44.0,
            rect.1 + 13.0,
            label,
            12.5,
            if enabled {
                theme::text()
            } else {
                theme::text_dim()
            },
            current,
        );
        if enabled {
            hit(
                &mut hits,
                Target::Onboarding(Action::Go(step)),
                rect,
                HitCursor::Pointer,
            );
        }
        py += 48.0;
    }
    let skip = (ax + 12.0, ay + ah - 46.0, rail_w - 24.0, 32.0);
    if inside(skip, snapshot.cursor) {
        round_rect(
            g,
            skip.0,
            skip.1,
            skip.2,
            skip.3,
            theme::radius_md(),
            theme::surface_hover(),
        );
    }
    text(
        g,
        skip.0 + 12.0,
        skip.1 + 9.0,
        "건너뛰고 터미널 열기",
        11.0,
        theme::text(),
        true,
    );
    hit(
        &mut hits,
        Target::Onboarding(Action::Skip),
        skip,
        HitCursor::Pointer,
    );

    let mx = ax + rail_w + if aw < 760.0 { 22.0 } else { 38.0 };
    let mw = (aw - rail_w - if aw < 760.0 { 44.0 } else { 76.0 })
        .max(220.0)
        .min(820.0);
    let (title, subtitle) = step_copy(snapshot.state.step);
    text(g, mx, ay + 22.0, title, 24.0, theme::text(), true);
    text(g, mx, ay + 55.0, subtitle, 12.5, theme::text_dim(), false);
    g.rect(mx, ay + 86.0, mw, 1.0, theme::border());

    let body_top = ay + 102.0;
    let body_bottom = ay + ah - footer_h - 8.0;
    let view_h = (body_bottom - body_top).max(1.0);
    g.push_clip(mx, body_top, mw, view_h);
    let mut y = body_top - snapshot.scroll;
    if snapshot.state.loading && !snapshot.state.loaded {
        loading_slab(g, mx, &mut y, mw);
    } else {
        match snapshot.state.step {
            0 => paint_appearance(g, snapshot, &mut hits, mx, &mut y, mw),
            1 => paint_auth(g, snapshot, &mut hits, mx, &mut y, mw),
            2 => paint_platform(g, snapshot, &mut hits, &mut caret_rect, mx, &mut y, mw),
            _ => paint_ready(g, snapshot, mx, &mut y, mw),
        }
    }
    if let Some((ok, notice)) = snapshot.state.notice.as_ref() {
        notice_slab(g, mx, &mut y, mw, *ok, notice);
    }
    clip_hits(g, &mut hits);
    g.pop_clip();

    g.rect(mx, ay + ah - footer_h, mw, 1.0, theme::border());
    if snapshot.state.step > 0 {
        button(
            g,
            snapshot,
            &mut hits,
            (mx, ay + ah - 48.0, 92.0, 34.0),
            "이전",
            Target::Onboarding(Action::Back),
            false,
        );
    }
    let (label, action) = if snapshot.state.step == 3 {
        ("kasaterm 열기", Action::Finish)
    } else {
        ("다음", Action::Next)
    };
    let forward = (mx + mw - 128.0, ay + ah - 48.0, 128.0, 34.0);
    if snapshot.state.loaded {
        button(
            g,
            snapshot,
            &mut hits,
            forward,
            label,
            Target::Onboarding(action),
            true,
        );
    } else {
        round_rect(
            g,
            forward.0,
            forward.1,
            forward.2,
            forward.3,
            theme::radius_md(),
            theme::surface(),
        );
        text(
            g,
            forward.0 + 45.0,
            forward.1 + 10.0,
            label,
            11.5,
            theme::text_dim(),
            false,
        );
    }

    let content_h = (y + snapshot.scroll - body_top + 18.0).max(view_h);
    crate::native_settings::paint_scroll_affordance(
        g,
        mx,
        body_top,
        mw,
        view_h,
        content_h,
        snapshot.scroll,
    );

    PaintOutput {
        hits,
        content_h,
        view_h,
        caret_rect,
    }
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
            *y,
            "Mac 터미널에서 가져오기",
            "색상과 글꼴만 읽고 원본 프로필은 바꾸지 않습니다",
        );
        *y += 50.0;
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
            let rect = (x, *y, w, 82.0);
            round_rect(
                g,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                theme::radius_md(),
                theme::surface(),
            );
            let bg = source
                .background
                .as_deref()
                .and_then(theme::parse_hex)
                .map(rgba)
                .unwrap_or_else(theme::surface_hover);
            let fg = source
                .foreground
                .as_deref()
                .and_then(theme::parse_hex)
                .map(rgba)
                .unwrap_or_else(theme::text);
            let preview = (rect.0 + 12.0, rect.1 + 12.0, 118.0, 58.0);
            round_rect(
                g,
                preview.0,
                preview.1,
                preview.2,
                preview.3,
                theme::radius_sm(),
                bg,
            );
            text(
                g,
                preview.0 + 10.0,
                preview.1 + 10.0,
                "❯ kasaterm",
                11.0,
                fg,
                true,
            );
            for (index, color) in source.ansi16.iter().take(6).enumerate() {
                if let Some(color) = theme::parse_hex(color).map(rgba) {
                    g.rect(
                        preview.0 + 10.0 + index as f32 * 15.0,
                        preview.1 + 36.0,
                        10.0,
                        8.0,
                        color,
                    );
                }
            }
            text(
                g,
                rect.0 + 144.0,
                rect.1 + 17.0,
                &source.label,
                13.0,
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
                rect.0 + 144.0,
                rect.1 + 43.0,
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
                    (rect.0 + rect.2 - 100.0, rect.1 + 24.0, 86.0, 32.0),
                    if applied { "적용됨" } else { "가져오기" },
                    Target::Onboarding(Action::Import(source.id.clone())),
                    applied,
                );
            }
            *y += 92.0;
        }
        return;
    }

    section(g, x, *y, "색상 테마", "앱과 터미널 ANSI 색이 함께 바뀝니다");
    *y += 50.0;
    let gap = 10.0;
    let cols = if w >= 620.0 { 3 } else { 2 };
    let card_w = (w - gap * (cols - 1) as f32) / cols as f32;
    for (index, palette) in s.palettes.iter().enumerate() {
        let rect = (
            x + (index % cols) as f32 * (card_w + gap),
            *y + (index / cols) as f32 * 88.0,
            card_w,
            78.0,
        );
        let selected = s.theme == palette.key;
        choice(
            g,
            s,
            hits,
            rect,
            selected,
            Target::Onboarding(Action::Theme(palette.key.clone())),
        );
        round_rect(
            g,
            rect.0 + 9.0,
            rect.1 + 9.0,
            rect.2 - 18.0,
            34.0,
            theme::radius_sm(),
            palette.bg,
        );
        text(
            g,
            rect.0 + 16.0,
            rect.1 + 19.0,
            "❯ Aa",
            11.0,
            palette.text,
            true,
        );
        let shown = fit(g, &palette.label, rect.2 - 20.0, 10.5, false);
        text(
            g,
            rect.0 + 10.0,
            rect.1 + 54.0,
            &shown,
            10.5,
            theme::text_dim(),
            selected,
        );
    }
    *y += ((s.palettes.len() + cols - 1) / cols) as f32 * 88.0 + 12.0;
    section(
        g,
        x,
        *y,
        "터미널 글꼴",
        "설치된 고정폭 글꼴과 글자 크기를 고릅니다",
    );
    *y += 48.0;
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
    section(g, x, *y, "강조색", "선택 영역과 커서, 링크에 함께 씁니다");
    *y += 46.0;
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
        *y,
        "첫 터미널에서 먼저 쓸 Agent",
        "로그인된 항목을 기본으로 고를 수 있습니다",
    );
    *y += 52.0;
    for provider in [AccountProvider::Claude, AccountProvider::Codex] {
        auth_row(g, s, hits, x, y, w, provider);
    }
    *y += 8.0;
    let rect = (x, *y, w, 54.0);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        theme::surface(),
    );
    g.queue_icon(
        "info",
        rect.0 + 14.0,
        rect.1 + 18.0,
        16.0,
        theme::text_dim(),
    );
    text(
        g,
        rect.0 + 42.0,
        rect.1 + 17.0,
        "인증정보는 각 도구가 보관하고, kasaterm은 토큰이나 비밀번호를 저장하지 않아요.",
        11.0,
        theme::text_dim(),
        false,
    );
    *y += 68.0;
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
    let selected = s.state.preferred == Some(provider);
    let rect = (x, *y, w, 74.0);
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
            theme::surface()
        },
    );
    stroke(
        g,
        rect,
        if selected {
            theme::accent()
        } else {
            theme::border()
        },
    );
    g.queue_icon(
        provider.icon(),
        rect.0 + 16.0,
        rect.1 + 18.0,
        22.0,
        theme::text(),
    );
    text(
        g,
        rect.0 + 52.0,
        rect.1 + 14.0,
        provider.label(),
        14.0,
        theme::text(),
        true,
    );
    text(
        g,
        rect.0 + 52.0,
        rect.1 + 41.0,
        auth.account
            .as_deref()
            .unwrap_or_else(|| auth_label(auth.status)),
        11.5,
        theme::text_dim(),
        false,
    );
    if auth.logged_in() {
        if selected {
            text(
                g,
                rect.0 + rect.2 - 72.0,
                rect.1 + 28.0,
                "기본",
                11.0,
                theme::accent(),
                true,
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
            (rect.0 + rect.2 - 94.0, rect.1 + 20.0, 78.0, 34.0),
            "취소",
            Target::Onboarding(Action::CancelLogin),
            false,
        );
    } else if auth.status == AuthStatus::LoggedOut {
        button(
            g,
            s,
            hits,
            (rect.0 + rect.2 - 94.0, rect.1 + 20.0, 78.0, 34.0),
            "로그인",
            Target::Onboarding(Action::Login(provider)),
            true,
        );
    } else if auth.status == AuthStatus::Checking {
        g.queue_icon(
            "rotate-cw",
            rect.0 + rect.2 - 34.0,
            rect.1 + 27.0,
            15.0,
            theme::text_mute(),
        );
    }
    *y += 84.0;
}

fn paint_platform(
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
            *y,
            "Windows 기본 셸",
            "새 pane을 열 때 시작할 셸입니다",
        );
        *y += 50.0;
        for shell in s.state.data.shells.iter() {
            let rect = (x, *y, w, 54.0);
            choice(
                g,
                s,
                hits,
                rect,
                s.state.data.selected_shell == shell.id,
                Target::Onboarding(Action::Shell(shell.id.clone())),
            );
            text(
                g,
                rect.0 + 14.0,
                rect.1 + 10.0,
                &shell.label,
                12.5,
                theme::text(),
                true,
            );
            text(
                g,
                rect.0 + 14.0,
                rect.1 + 32.0,
                &shell.path,
                10.5,
                theme::text_dim(),
                false,
            );
            *y += 62.0;
        }
        onboarding_shell_field(g, s, hits, caret, x, *y, w);
        *y += 64.0;
        return;
    }
    section(
        g,
        x,
        *y,
        if s.state.data.platform == "macos" {
            "Mac 터미널 설정"
        } else {
            "기본 셸"
        },
        "지금까지 고른 값을 첫 pane에 그대로 사용합니다",
    );
    *y += 52.0;
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

fn paint_ready(g: &mut gpu::GpuRenderer, s: &Snapshot, x: f32, y: &mut f32, w: f32) {
    let mark = (x + w / 2.0 - 28.0, *y + 4.0, 56.0, 56.0);
    round_rect(g, mark.0, mark.1, mark.2, mark.3, 28.0, theme::success());
    g.queue_icon(
        "square-check",
        mark.0 + 16.0,
        mark.1 + 16.0,
        24.0,
        [255, 255, 255, 255],
    );
    *y += 82.0;
    let theme_label = s
        .palettes
        .iter()
        .find(|palette| palette.key == s.theme)
        .map(|palette| palette.label.as_str())
        .unwrap_or(s.theme.as_str());
    summary_row(
        g,
        x,
        y,
        w,
        "외형",
        &format!("{theme_label} · {:.0}px", s.font_size),
    );
    let signed = [AccountProvider::Claude, AccountProvider::Codex]
        .into_iter()
        .filter(|provider| s.state.data.auth(*provider).logged_in())
        .count();
    let agent = match s.state.preferred {
        Some(provider) => format!("{} · 기본", provider.label()),
        None if signed > 0 => format!("{signed}개 로그인됨"),
        None => "나중에 연결".to_string(),
    };
    summary_row(g, x, y, w, "Agent", &agent);
    summary_row(
        g,
        x,
        y,
        w,
        "터미널",
        if s.shell.is_empty() {
            "시스템 기본"
        } else {
            &s.shell
        },
    );
    if s.state.data.restart_required {
        let rect = (x, *y + 10.0, w, 50.0);
        round_rect(
            g,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_md(),
            theme::surface(),
        );
        g.queue_icon(
            "triangle-alert",
            rect.0 + 14.0,
            rect.1 + 17.0,
            16.0,
            theme::attention(),
        );
        text(
            g,
            rect.0 + 42.0,
            rect.1 + 16.0,
            "새 글꼴은 kasaterm을 다시 열면 적용돼요.",
            11.5,
            theme::text_dim(),
            false,
        );
        *y += 70.0;
    }
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
    text(
        g,
        x + 2.0,
        y,
        "셸 경로 직접 입력",
        11.0,
        theme::text_dim(),
        false,
    );
    let rect = (x, y + 18.0, w, 36.0);
    let focused = s.input == Some(SettingsInput::Shell);
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
    stroke(
        g,
        rect,
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
        rect.0 + 11.0,
        rect.1 + 10.0,
        if s.shell.is_empty() {
            "경로를 입력하세요"
        } else {
            &s.shell
        },
        12.0,
        if s.shell.is_empty() {
            theme::text_dim()
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
        let cx = rect.0 + 11.0 + g.measure_chrome_text(&prefix, 12.0, false);
        if !s.preedit.is_empty() {
            text(g, cx, rect.1 + 9.0, &s.preedit, 12.0, theme::text(), false);
        } else if s.caret_on {
            g.rect(cx, rect.1 + 8.0, 1.5, 19.0, theme::cursor());
        }
        *caret_out = Some((cx, rect.1 + 8.0, 2.0, 19.0));
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

fn step_copy(step: u8) -> (&'static str, &'static str) {
    match step {
        0 => (
            "익숙한 터미널 모습으로 시작하세요",
            "기존 설정을 가져오거나 색과 글꼴을 직접 고릅니다",
        ),
        1 => (
            "이미 로그인한 Agent를 그대로 씁니다",
            "Claude Code와 Codex의 기존 인증만 확인합니다",
        ),
        2 => (
            "새 pane의 환경을 확인하세요",
            "운영체제에 맞는 셸과 외형을 마지막으로 확인합니다",
        ),
        _ => (
            "준비가 끝났어요",
            "지금 고른 값은 나중에도 설정 방에서 바꿀 수 있습니다",
        ),
    }
}

fn loading_slab(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32) {
    let rect = (x, *y, w, 84.0);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        theme::surface(),
    );
    g.queue_icon(
        "rotate-cw",
        rect.0 + 18.0,
        rect.1 + 27.0,
        20.0,
        theme::accent(),
    );
    text(
        g,
        rect.0 + 52.0,
        rect.1 + 20.0,
        "설치 환경을 확인하고 있어요",
        14.0,
        theme::text(),
        true,
    );
    text(
        g,
        rect.0 + 52.0,
        rect.1 + 47.0,
        "테마, 터미널 설정, Agent 로그인을 한 번만 살펴봅니다.",
        11.0,
        theme::text_dim(),
        false,
    );
    *y += 98.0;
}

fn empty_slab(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, message: &str) {
    let rect = (x, *y, w, 58.0);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        theme::surface(),
    );
    g.queue_icon(
        "info",
        rect.0 + 14.0,
        rect.1 + 20.0,
        16.0,
        theme::text_dim(),
    );
    text(
        g,
        rect.0 + 42.0,
        rect.1 + 19.0,
        message,
        11.0,
        theme::text_dim(),
        false,
    );
    *y += 68.0;
}

fn notice_slab(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, ok: bool, message: &str) {
    let rect = (x, *y + 8.0, w, 48.0);
    round_rect(
        g,
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_md(),
        theme::surface(),
    );
    g.rect(
        rect.0,
        rect.1,
        3.0,
        rect.3,
        if ok {
            theme::success()
        } else {
            theme::danger()
        },
    );
    text(
        g,
        rect.0 + 16.0,
        rect.1 + 16.0,
        message,
        11.5,
        theme::text_dim(),
        false,
    );
    *y += 64.0;
}

fn section(g: &mut gpu::GpuRenderer, x: f32, y: f32, title: &str, subtitle: &str) {
    text(g, x, y, title, 15.0, theme::text(), true);
    text(g, x, y + 24.0, subtitle, 11.5, theme::text_dim(), false);
}

fn summary_row(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, label: &str, value: &str) {
    let rect = (x, *y, w, 48.0);
    g.rect(rect.0, rect.1 + rect.3 - 1.0, rect.2, 1.0, theme::border());
    text(
        g,
        rect.0 + 4.0,
        rect.1 + 16.0,
        label,
        11.5,
        theme::text_dim(),
        false,
    );
    let shown = fit(g, value, rect.2 - 180.0, 12.0, false);
    let tx = rect.0 + rect.2 - g.measure_chrome_text(&shown, 12.0, false) - 4.0;
    text(
        g,
        tx.max(rect.0 + 120.0),
        rect.1 + 15.0,
        &shown,
        12.0,
        theme::text(),
        false,
    );
    *y += 48.0;
}

fn segmented(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    x: f32,
    y: f32,
    w: f32,
    cells: &[(&str, bool, Action)],
) {
    let gap = 4.0;
    let cw = (w - gap * cells.len().saturating_sub(1) as f32) / cells.len().max(1) as f32;
    for (index, (label, selected, action)) in cells.iter().enumerate() {
        let rect = (x + index as f32 * (cw + gap), y, cw, 34.0);
        choice(
            g,
            s,
            hits,
            rect,
            *selected,
            crate::native_settings::Target::Onboarding(action.clone()),
        );
        let shown = fit(g, label, rect.2 - 16.0, 11.5, *selected);
        let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, *selected)) / 2.0;
        text(
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
        let cw = (g.measure_chrome_text(&label, 11.0, selected) + 22.0).min(w);
        if cx + cw > x + w && cx > x {
            cx = x;
            cy += 38.0;
        }
        let rect = (cx, cy, cw, 30.0);
        choice(
            g,
            s,
            hits,
            rect,
            selected,
            crate::native_settings::Target::Onboarding(action),
        );
        let shown = fit(g, &label, rect.2 - 20.0, 11.0, selected);
        text(
            g,
            rect.0 + 10.0,
            rect.1 + 8.0,
            &shown,
            11.0,
            if selected {
                theme::text()
            } else {
                theme::text_dim()
            },
            selected,
        );
        cx += cw + 6.0;
    }
    *y = cy + 40.0;
}

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
    text(g, x + 2.0, *y + 13.0, label, 12.0, theme::text(), false);
    let right = x + w;
    button(
        g,
        s,
        hits,
        (right - 104.0, *y + 4.0, 30.0, 30.0),
        "−",
        crate::native_settings::Target::Onboarding(minus),
        false,
    );
    button(
        g,
        s,
        hits,
        (right - 30.0, *y + 4.0, 30.0, 30.0),
        "+",
        crate::native_settings::Target::Onboarding(plus),
        false,
    );
    text(
        g,
        right - 66.0,
        *y + 12.0,
        value,
        11.5,
        theme::text_dim(),
        false,
    );
    *y += 46.0;
}

fn choice(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<crate::native_settings::Hit>,
    rect: Rect,
    selected: bool,
    target: crate::native_settings::Target,
) {
    let hover = inside(rect, s.cursor);
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
    stroke(
        g,
        rect,
        if selected {
            theme::accent()
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
    let shown = fit(g, label, rect.2 - 16.0, 11.5, primary);
    let tx = rect.0 + (rect.2 - g.measure_chrome_text(&shown, 11.5, primary)) / 2.0;
    text(
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
    g.rect(rect.0, rect.1, rect.2, 1.0, color);
    g.rect(rect.0, rect.1 + rect.3 - 1.0, rect.2, 1.0, color);
    g.rect(rect.0, rect.1, 1.0, rect.3, color);
    g.rect(rect.0 + rect.2 - 1.0, rect.1, 1.0, rect.3, color);
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
    g.draw_text(
        x,
        y,
        value,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}

fn fit(g: &mut gpu::GpuRenderer, value: &str, width: f32, size: f32, bold: bool) -> String {
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
    fn progress_never_jumps_past_the_furthest_step() {
        let mut state = State::default();
        assert!(!state.go(2));
        assert_eq!(state.step, 0);
        state.loaded = true;
        assert!(state.next());
        assert_eq!((state.step, state.furthest), (1, 1));
        assert!(state.back());
        assert!(state.go(1));
    }

    #[test]
    fn first_logged_in_agent_becomes_the_default_without_overriding_a_choice() {
        let mut state = State::default();
        let mut data = Data::default();
        data.claude.status = AuthStatus::LoggedIn;
        state.apply_loaded(data.clone());
        assert_eq!(state.preferred, Some(AccountProvider::Claude));
        state.preferred = Some(AccountProvider::Codex);
        state.preferred_touched = true;
        state.apply_loaded(data);
        assert_eq!(state.preferred, Some(AccountProvider::Codex));
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
            assert!(!main.contains(removed), "desktop Settings still owns Wry: {removed}");
        }
        let chrome = include_str!("chrome.rs");
        for removed in ["open_student_web_window", "settings.html"] {
            assert!(!chrome.contains(removed), "desktop Settings Wry route remains: {removed}");
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
        assert!(spawn.find("LoginState::Running").unwrap() < spawn.find("std::thread::spawn").unwrap());

        let action = include_str!("native_onboarding.rs")
            .split_once("Action::Login(provider) =>")
            .unwrap()
            .1
            .split_once("Action::CancelLogin")
            .unwrap()
            .0;
        assert!(action.find("hidden_login_running").unwrap() < action.find("settings_apply").unwrap());
    }

    #[test]
    fn imported_profile_refreshes_the_summary_font_immediately() {
        let source = include_str!("native_onboarding.rs");
        let import = source
            .split_once("Action::Import(id) =>")
            .unwrap()
            .1
            .split_once("Action::Theme(key)")
            .unwrap()
            .0;
        assert!(import.contains("onboarding::current_font_family()"));
    }
}
