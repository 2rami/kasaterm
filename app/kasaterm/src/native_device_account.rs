use super::*;
use kasa_mcp::oauth_accounts::Provider;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    OpenLogin,
    Login,
    Cancel,
    AskLogout,
    Logout,
    OAuth(Provider),
    Linked,
    CancelOAuth,
    /// The provider signed in an identity no account has yet; the person picks one.
    Choose,
    OpenClaim,
    Signup,
    CancelChoice,
}

/// An unlinked provider sign-in waiting for the person's answer. The gateway ticket stays in
/// `device_auth`; this holds only what the page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    flow_id: String,
    provider: Provider,
    display: String,
    signup: bool,
    claim: bool,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) account: String,
    pub(crate) password: String,
    form: bool,
    confirm_logout: bool,
    status: serde_json::Value,
    message: Option<(String, bool)>,
    pending: Option<Receiver<Result<Action, String>>>,
    providers: serde_json::Value,
    provider_check: Option<Receiver<serde_json::Value>>,
    provider_checked: Option<std::time::Instant>,
    oauth_cancel: Option<Arc<AtomicBool>>,
    oauth_code_rx: Option<Receiver<String>>,
    oauth_code: Option<String>,
    choice_rx: Option<Receiver<Choice>>,
    choice: Option<Choice>,
}

#[derive(Clone)]
pub(crate) struct View {
    account: String,
    password_mask: String,
    form: bool,
    confirm_logout: bool,
    status: serde_json::Value,
    message: Option<(String, bool)>,
    busy: bool,
    providers: serde_json::Value,
    oauth_waiting: bool,
    oauth_code: Option<String>,
    choice: Option<Choice>,
}

pub(crate) fn mask(value: &str) -> String {
    "•".repeat(value.chars().count())
}

impl State {
    pub(crate) fn refresh(&mut self) {
        self.status = kasa_mcp::device_auth::status();
        if self.provider_check.is_none()
            && self
                .provider_checked
                .is_none_or(|checked| checked.elapsed().as_secs() >= 30)
        {
            let (tx, rx) = mpsc::channel();
            self.provider_check = Some(rx);
            self.provider_checked = Some(std::time::Instant::now());
            std::thread::spawn(move || {
                let providers =
                    kasa_mcp::device_auth::handle(&serde_json::json!({"op":"oauth_providers"}))
                        .unwrap_or_else(|_| serde_json::json!({"state":"unavailable"}));
                let _ = tx.send(providers);
            });
        }
    }

    pub(crate) fn view(&self) -> View {
        View {
            account: self.account.clone(),
            password_mask: mask(&self.password),
            form: self.form,
            confirm_logout: self.confirm_logout,
            status: self.status.clone(),
            message: self.message.clone(),
            busy: self.pending.is_some(),
            providers: self.providers.clone(),
            oauth_waiting: self.oauth_cancel.is_some(),
            oauth_code: self.oauth_code.clone(),
            choice: self.choice.clone(),
        }
    }

    /// Verification-run screens of the sign-in page with providers enabled and, optionally, an
    /// unlinked sign-in waiting for the account choice. Holds no gateway capability.
    pub(crate) fn fixture(&mut self, choice: bool, claim: bool) {
        self.providers = serde_json::json!({"state":"ready","providers":[
            {"id":"google","enabled":true},{"id":"github","enabled":true}]});
        self.provider_check = None;
        self.provider_checked = Some(std::time::Instant::now());
        self.choice = choice.then(|| Choice {
            flow_id: String::new(),
            provider: Provider::Google,
            display: "person@example.com".into(),
            signup: true,
            claim,
        });
    }

    pub(crate) fn hide(&mut self) {
        self.password.clear();
        self.form = false;
        self.confirm_logout = false;
        if let Some(cancel) = &self.oauth_cancel {
            if !cancel.swap(true, Ordering::AcqRel) {
                kasa_mcp::device_auth::cancel_oauth();
            }
        }
        if self.choice.take().is_some() {
            kasa_mcp::device_auth::cancel_oauth();
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        match self.oauth_code_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(code)) => {
                self.oauth_code = Some(code);
                self.oauth_code_rx = None;
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => self.oauth_code_rx = None,
            _ => {}
        }
        match self.provider_check.as_ref().map(Receiver::try_recv) {
            Some(Ok(providers)) => {
                self.providers = providers;
                self.provider_check = None;
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.provider_check = None;
                changed = true;
            }
            _ => {}
        }
        let result = match self.pending.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("요청을 마치지 못했어요. 다시 시도해 주세요".into())
            }
            _ => return changed,
        };
        self.pending = None;
        self.oauth_cancel = None;
        self.oauth_code = None;
        self.oauth_code_rx = None;
        if let Some(choice) = self.choice_rx.take().and_then(|rx| rx.try_recv().ok()) {
            self.choice = Some(choice);
        }
        match result {
            Ok(Action::Choose) => {}
            // A wrong password or a short lockout keeps the choice so the person can try again.
            Err(error) if self.choice.is_some() && (error == BAD_CREDENTIALS || error == RATE_LIMITED) => {
                self.message = Some((error, true));
            }
            Ok(Action::Login) => {
                self.form = false;
                self.choice = None;
                // 관문에 붙은 즉시 다른 기기의 에이전트 계정 목록을 받는다.
                kasa_mcp::agent_accounts::poke();
                self.message = Some((
                    "로그인을 저장했어요. 관문 연결을 확인하고 있어요".into(),
                    false,
                ));
            }
            Ok(Action::Linked) => {
                self.message = Some(("이 KASA 계정에 로그인 방법을 연결했어요".into(), false))
            }
            Ok(Action::CancelOAuth) => self.message = Some(("로그인을 취소했어요".into(), false)),
            Ok(Action::CancelChoice) => self.choice = None,
            Ok(_) => {
                self.confirm_logout = false;
                self.message = Some(("이 기기에서 로그아웃했어요".into(), false));
            }
            Err(error) => {
                self.choice = None;
                self.message = Some((error, true));
            }
        }
        true
    }
}

const BAD_CREDENTIALS: &str = "아이디나 비밀번호를 확인해 주세요";
const RATE_LIMITED: &str = "로그인 시도가 많아요. 잠시 후 다시 시도해 주세요";

fn safe_error(error: &str) -> String {
    if error.contains("setup_required") {
        "Google·GitHub 로그인은 서버의 OAuth 앱 등록이 필요해요".into()
    } else if error.contains("account_not_linked") {
        "먼저 기존 KASA 계정으로 로그인한 뒤 Google 또는 GitHub를 연결해 주세요".into()
    } else if error.contains("already_linked") {
        "이미 다른 KASA 계정에 연결된 로그인이에요. 계정은 자동으로 합치지 않아요".into()
    } else if error.contains("cancelled") {
        "브라우저에서 로그인을 취소했어요".into()
    } else if error.contains("expired") || error.contains("account_changed") {
        "로그인 요청이 만료되었거나 계정이 바뀌었어요. 다시 시작해 주세요".into()
    } else if error.contains("아이디나 비밀번호") || error.contains("bad_credentials") {
        BAD_CREDENTIALS.into()
    } else if error.contains("signup_disabled") {
        "이 서버는 새 계정을 받지 않아요. 기존 계정에 연결해 주세요".into()
    } else if error.contains("account_disabled") {
        "사용이 막힌 계정이에요".into()
    } else if error.contains("너무 여러 번") || error.contains("rate_limited") {
        RATE_LIMITED.into()
    } else if error.contains("관문이 꺼져") {
        "기기 연결 서버가 꺼져 있어요".into()
    } else {
        "요청을 마치지 못했어요. 연결 상태를 확인하고 다시 시도해 주세요".into()
    }
}

impl App {
    pub(crate) fn device_account_poll(&mut self) {
        if self.device_account.poll() {
            self.device_account.refresh();
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        if !self.settings_room_active() {
            self.device_account.hide();
        }
    }

    pub(crate) fn device_account_action(&mut self, action: Action) {
        if action == Action::CancelOAuth {
            if let Some(cancel) = &self.device_account.oauth_cancel {
                if !cancel.swap(true, Ordering::AcqRel) {
                    kasa_mcp::device_auth::cancel_oauth();
                }
            }
            return;
        }
        if self.device_account.pending.is_some() {
            return;
        }
        self.device_account.message = None;
        // The password form doubles as the existing-account step of a provider sign-in.
        let action = match action {
            Action::Login if self.device_account.choice.is_some() => Action::Choose,
            action => action,
        };
        match action {
            Action::OpenLogin => {
                self.device_account.refresh();
                if self.device_account.account.is_empty() {
                    self.device_account.account = self.device_account.status["account"]
                        .as_str()
                        .unwrap_or_default()
                        .into();
                }
                self.device_account.form = true;
                self.device_account.confirm_logout = false;
                self.native_settings_focus(SettingsInput::DeviceAccountName);
            }
            Action::Cancel => self.device_account.hide(),
            Action::AskLogout => self.device_account.confirm_logout = true,
            Action::OAuth(provider) => {
                if !provider_enabled(&self.device_account.providers, provider) {
                    return;
                }
                self.native_settings_blur();
                self.device_account.password.clear();
                let link = self.device_account.status["logged_in"] == true;
                let cancel = Arc::new(AtomicBool::new(false));
                self.device_account.oauth_cancel = Some(cancel.clone());
                let (code_tx, code_rx) = mpsc::channel();
                self.device_account.oauth_code_rx = Some(code_rx);
                self.device_account.oauth_code = None;
                let (choice_tx, choice_rx) = mpsc::channel();
                self.device_account.choice_rx = Some(choice_rx);
                self.device_account.choice = None;
                let (tx, rx) = mpsc::channel();
                self.device_account.pending = Some(rx);
                std::thread::spawn(move || {
                    let result = run_oauth(provider, link, &cancel, code_tx, choice_tx);
                    let _ = tx.send(result);
                });
            }
            Action::Linked | Action::CancelOAuth => {}
            Action::OpenClaim => {
                if let Some(choice) = self.device_account.choice.as_mut() {
                    choice.claim = true;
                    self.native_settings_focus(SettingsInput::DeviceAccountName);
                }
            }
            Action::Choose | Action::Signup | Action::CancelChoice => {
                let Some(choice) = self.device_account.choice.clone() else {
                    return;
                };
                if action == Action::Choose
                    && (self.device_account.account.trim().is_empty()
                        || self.device_account.password.is_empty())
                {
                    self.device_account.message =
                        Some(("아이디와 비밀번호를 입력해 주세요".into(), true));
                    self.chrome_dirty = true;
                    return;
                }
                self.native_settings_blur();
                let params = match action {
                    Action::Choose => serde_json::json!({"op":"oauth_claim","flow_id":choice.flow_id,
                        "account":self.device_account.account.trim(),
                        "password":std::mem::take(&mut self.device_account.password)}),
                    Action::Signup => {
                        serde_json::json!({"op":"oauth_signup","flow_id":choice.flow_id})
                    }
                    _ => serde_json::json!({"op":"oauth_cancel","flow_id":choice.flow_id}),
                };
                let (tx, rx) = mpsc::channel();
                self.device_account.pending = Some(rx);
                std::thread::spawn(move || {
                    let result = kasa_mcp::device_auth::handle(&params)
                        .map(|_| {
                            if action == Action::CancelChoice {
                                Action::CancelChoice
                            } else {
                                Action::Login
                            }
                        })
                        .map_err(|error| safe_error(&error.to_string()));
                    let _ = tx.send(result);
                });
            }
            Action::Login | Action::Logout => {
                if action == Action::Login
                    && (self.device_account.account.trim().is_empty()
                        || self.device_account.password.is_empty())
                {
                    self.device_account.message =
                        Some(("아이디와 비밀번호를 입력해 주세요".into(), true));
                    self.chrome_dirty = true;
                    return;
                }
                self.native_settings_blur();
                let params = if action == Action::Login {
                    serde_json::json!({"op":"login", "account":self.device_account.account.trim(),
                        "password":std::mem::take(&mut self.device_account.password)})
                } else {
                    serde_json::json!({"op":"logout"})
                };
                let (tx, rx) = mpsc::channel();
                self.device_account.pending = Some(rx);
                std::thread::spawn(move || {
                    let result = kasa_mcp::device_auth::handle(&params)
                        .map(|_| action)
                        .map_err(|error| safe_error(&error.to_string()));
                    let _ = tx.send(result);
                });
            }
        }
        self.chrome_dirty = true;
    }
}

fn status_label(status: &serde_json::Value) -> (&'static str, bool) {
    match status["state"].as_str() {
        Some("connected") => ("계정으로 연결됨", false),
        Some("connecting") if !status["connection_error"].is_null() => {
            ("로그인 저장됨 · 연결 재시도 중", false)
        }
        Some("connecting") => ("로그인 저장됨 · 연결 확인 중", false),
        Some("reauth_required") => ("다시 로그인이 필요해요", true),
        Some("gateway_changed") => ("연결 서버가 바뀌었어요 · 다시 로그인", true),
        Some("gateway_off") => ("기기 연결 서버 꺼짐", true),
        Some("signed_out") => ("로그인 필요", false),
        _ => ("로그인 상태 확인 중", false),
    }
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let v = &s.device_account;
    // 같은 「계정」 페이지의 Claude·Codex 머리와 같은 급 — 아이콘 17, 이름 14.5 굵게.
    g.queue_icon("users", x, *y - 1.0, 17.0, theme::text());
    draw_text(g, x + 24.0, *y, "KASA 계정", 14.5, theme::text(), true);
    *y += 26.0;
    let (status, error) = status_label(&v.status);
    for line in wrap_words(g, &crate::native_strings::text(status), w, 12.0) {
        draw_text(
            g,
            x,
            *y,
            &line,
            12.0,
            if error {
                theme::danger()
            } else {
                theme::text()
            },
            false,
        );
        *y += 18.0;
    }
    // OAuth 로 만든 계정 이름(oauth_<hex>)은 사람이 못 알아본다 — 관문이 준 이메일·아이디를 쓴다.
    if let Some(account) = v.status["display_name"].as_str().or(v.status["account"].as_str()) {
        let label = fit(g, account, w, 10.5, false);
        draw_text(g, x, *y, &label, 10.5, theme::text_dim(), false);
        *y += 18.0;
    }
    *y += 8.0;
    if v.busy {
        let message = if v.oauth_waiting {
            "브라우저에서 로그인을 완료해 주세요"
        } else {
            "요청 처리 중…"
        };
        let message = fit(g, message, w, 12.0, false);
        draw_text(g, x, *y, &message, 12.0, theme::text_dim(), false);
        *y += ROW_H;
        if v.oauth_waiting {
            if let Some(code) = &v.oauth_code {
                let label = fit(g, &format!("확인 코드 {code}"), w, 12.0, true);
                draw_text(g, x, *y, &label, 12.0, theme::text(), true);
                *y += ROW_H;
            }
            button(
                g,
                s,
                hits,
                (x, *y, w.min(160.0), CTL_H),
                "로그인 취소",
                Target::Setting(SettingsAction::DeviceAccount(Action::CancelOAuth)),
                false,
            );
            *y += ROW_H;
        }
    } else if let Some(choice) = &v.choice {
        paint_choice(g, s, hits, caret, x, y, w, v, choice);
    } else if v.confirm_logout {
        info_slab(
            g,
            x,
            y,
            w,
            "이 기기의 계정 연결을 해제합니다. 다른 기기의 로그인과 기존 기기 연결 설정은 유지됩니다.",
        );
        account_buttons(
            g,
            s,
            hits,
            x,
            y,
            w,
            "로그아웃",
            Action::Logout,
            "취소",
            Action::Cancel,
        );
    } else if v.status["credential_saved"].as_bool() == Some(true) {
        if v.form {
            password_form(g, s, hits, caret, x, y, w, v, "로그인", Action::Cancel);
        } else {
            account_buttons(
                g,
                s,
                hits,
                x,
                y,
                w,
                "다시 로그인",
                Action::OpenLogin,
                "로그아웃",
                Action::AskLogout,
            );
        }
        provider_buttons(g, s, hits, x, y, w, v, true);
    } else {
        // Signing in with Google·GitHub is the main path; an ID and password is the fallback.
        let ready = provider_buttons(g, s, hits, x, y, w, v, false);
        if ready {
            info_slab(
                g,
                x,
                y,
                w,
                "처음 쓰는 Google·GitHub 이면 새 계정을 만들지, 이미 있는 KASA 계정에 연결할지 물어봐요.",
            );
        }
        draw_text(g, x, *y + 4.0, "다른 방법", 11.0, theme::text_dim(), false);
        *y += 24.0;
        if v.form {
            password_form(g, s, hits, caret, x, y, w, v, "로그인", Action::Cancel);
        } else {
            let label = "아이디로 로그인";
            let bw = (g.measure_chrome_text(label, 12.0, false) + 32.0).min(w);
            button(
                g,
                s,
                hits,
                (x, *y, bw, CTL_H),
                label,
                Target::Setting(SettingsAction::DeviceAccount(Action::OpenLogin)),
                false,
            );
            *y += ROW_H;
        }
    }
    if let Some((message, error)) = &v.message {
        for line in wrap_words(g, &crate::native_strings::text(message), w, 10.5) {
            draw_text(
                g,
                x,
                *y,
                &line,
                10.5,
                if *error {
                    theme::danger()
                } else {
                    theme::text_dim()
                },
                false,
            );
            *y += 18.0;
        }
    }
    info_slab(
        g,
        x,
        y,
        w,
        "로그인하면 이 기기를 등록하고, 모양·하단바 같은 공통 설정과 기기 목록을 이 계정의 다른 기기와 맞춥니다. 비밀·경로·업데이트 선택은 이 기기에만 남아요. 실제 연결은 「연결 기기」에서 확인하세요.",
    );
    if let Some(message) = v.status["sync"]["message"].as_str() {
        info_slab(g, x, y, w, message);
    }
    *y += 24.0;
}

fn provider_name(provider: Provider) -> &'static str {
    if provider == Provider::Google {
        "Google"
    } else {
        "GitHub"
    }
}

/// Google·GitHub buttons side by side. Signed out they are the primary way in; signed in they link
/// another login method to this account. Returns whether any provider can be used.
#[allow(clippy::too_many_arguments)]
fn provider_buttons(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    v: &View,
    linked: bool,
) -> bool {
    let width = ((w - 8.0) / 2.0).max(0.0);
    for (index, provider) in [Provider::Google, Provider::Github].into_iter().enumerate() {
        let label = format!(
            "{} {}",
            provider_name(provider),
            if linked { "연결" } else { "로그인" }
        );
        let rect = (x + index as f32 * (width + 8.0), *y, width, CTL_H);
        if provider_enabled(&v.providers, provider) {
            button(
                g,
                s,
                hits,
                rect,
                &label,
                Target::Setting(SettingsAction::DeviceAccount(Action::OAuth(provider))),
                !linked,
            );
        } else {
            crate::native_controls::text_button(
                g,
                rect,
                s.cursor,
                &label,
                crate::native_controls::Style {
                    enabled: false,
                    ..Default::default()
                },
            );
        }
    }
    *y += ROW_H;
    let ready = [Provider::Google, Provider::Github]
        .into_iter()
        .any(|provider| provider_enabled(&v.providers, provider));
    if !ready {
        let hint = match v.providers["state"].as_str() {
            Some("setup_required") => {
                "Google·GitHub 로그인 준비 중 · 서버에 OAuth 앱을 등록해야 사용할 수 있어요."
            }
            Some("isolated") => "검증 실행에서는 실제 계정 로그인을 사용하지 않아요.",
            Some("unavailable") => {
                "로그인 방법을 확인하지 못했어요. 연결 서버의 상태와 업데이트를 확인해 주세요."
            }
            _ => "Google·GitHub 로그인 사용 가능 여부 확인 중…",
        };
        info_slab(g, x, y, w, hint);
    } else if linked {
        info_slab(
            g,
            x,
            y,
            w,
            "현재 KASA 계정에 로그인 방법을 연결합니다. 다른 계정과 자동으로 합치지 않습니다.",
        );
    }
    ready
}

#[allow(clippy::too_many_arguments)]
fn password_form(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
    v: &View,
    submit: &str,
    cancel: Action,
) {
    text_field(
        g,
        s,
        hits,
        caret,
        x,
        *y,
        w,
        "아이디",
        &v.account,
        SettingsInput::DeviceAccountName,
        s.settings_caret,
        false,
    );
    *y += ROW_H;
    text_field(
        g,
        s,
        hits,
        caret,
        x,
        *y,
        w,
        "비밀번호",
        &v.password_mask,
        SettingsInput::DeviceAccountPassword,
        s.settings_caret,
        false,
    );
    *y += ROW_H;
    account_buttons(g, s, hits, x, y, w, submit, Action::Login, "취소", cancel);
}

/// An unlinked Google·GitHub sign-in: start a new account, or connect an existing one by its
/// password once so the provider alone signs in afterwards.
#[allow(clippy::too_many_arguments)]
fn paint_choice(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
    v: &View,
    choice: &Choice,
) {
    let who = if choice.display.is_empty() {
        provider_name(choice.provider).to_string()
    } else {
        format!("{} · {}", provider_name(choice.provider), choice.display)
    };
    let who = fit(g, &who, w, 12.0, true);
    draw_text(g, x, *y, &who, 12.0, theme::text(), true);
    *y += 18.0;
    for line in wrap_words(g, "아직 KASA 계정에 연결되지 않은 로그인이에요.", w, 12.0) {
        draw_text(g, x, *y, &line, 12.0, theme::text(), false);
        *y += 18.0;
    }
    *y += 8.0;
    if choice.claim {
        password_form(
            g,
            s,
            hits,
            caret,
            x,
            y,
            w,
            v,
            "연결하고 로그인",
            Action::CancelChoice,
        );
        info_slab(
            g,
            x,
            y,
            w,
            &format!(
                "기존 KASA 계정의 비밀번호를 이번 한 번만 확인해요. 다음부터는 {} 로그인만으로 들어와요.",
                provider_name(choice.provider)
            ),
        );
        return;
    }
    let mut buttons: Vec<(&str, Action, bool)> = vec![("기존 계정에 연결", Action::OpenClaim, true)];
    if choice.signup {
        buttons.push(("새 계정 만들기", Action::Signup, false));
    }
    buttons.push(("취소", Action::CancelChoice, false));
    let mut bx = x;
    for (label, action, primary) in buttons {
        let bw = g.measure_chrome_text(label, 12.0, primary) + 32.0;
        if bx > x && bx + bw > x + w {
            bx = x;
            *y += ROW_H;
        }
        button(
            g,
            s,
            hits,
            (bx, *y, bw.min(w), CTL_H),
            label,
            Target::Setting(SettingsAction::DeviceAccount(action)),
            primary,
        );
        bx += bw + 8.0;
    }
    *y += ROW_H;
    info_slab(
        g,
        x,
        y,
        w,
        if choice.signup {
            "이미 쓰던 KASA 계정이 있으면 연결하세요. 새 계정을 만들면 기존 계정의 기기·설정과 따로 움직여요."
        } else {
            "이 서버는 새 계정을 받지 않아요. 기존 KASA 계정에 연결해 주세요."
        },
    );
}

fn provider_enabled(providers: &serde_json::Value, provider: Provider) -> bool {
    providers["providers"].as_array().is_some_and(|providers| {
        providers
            .iter()
            .any(|entry| entry["id"] == provider.name() && entry["enabled"] == true)
    })
}

fn run_oauth(
    provider: Provider,
    link: bool,
    cancelled: &AtomicBool,
    code_tx: mpsc::Sender<String>,
    choice_tx: mpsc::Sender<Choice>,
) -> Result<Action, String> {
    let call = |params| {
        kasa_mcp::device_auth::handle(&params).map_err(|error| safe_error(&error.to_string()))
    };
    let started = call(serde_json::json!({"op":"oauth_start","provider":provider,"link":link}))?;
    let flow_id = started["flow_id"]
        .as_str()
        .ok_or_else(|| safe_error("oauth_unavailable"))?;
    // 관문이 앱 리다이렉트(PKCE)를 받으면 확인 코드가 없다 — 브라우저가 로그인 결과를 이 기기로 바로 돌려준다.
    if let Some(code) = started["user_code"].as_str() {
        let _ = code_tx.send(code.to_string());
    }
    if !cancelled.load(Ordering::Acquire) {
        let url = started["authorization_url"]
            .as_str()
            .ok_or_else(|| safe_error("oauth_unavailable"))?;
        crate::chrome::open_url_in_browser(url);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while !cancelled.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        let response = call(serde_json::json!({"op":"oauth_poll","flow_id":flow_id}));
        match response {
            Ok(value) if value["status"] == "complete" => return Ok(Action::Login),
            Ok(value) if value["status"] == "linked" => return Ok(Action::Linked),
            Ok(value) if value["status"] == "choose" => {
                let _ = choice_tx.send(Choice {
                    flow_id: flow_id.to_string(),
                    provider,
                    display: value["display"].as_str().unwrap_or_default().to_string(),
                    signup: value["signup_enabled"] == true,
                    claim: false,
                });
                return Ok(Action::Choose);
            }
            Err(error) => {
                let _ = call(serde_json::json!({"op":"oauth_cancel","flow_id":flow_id}));
                return Err(error);
            }
            _ => std::thread::sleep(std::time::Duration::from_secs(2)),
        }
    }
    let _ = call(serde_json::json!({"op":"oauth_cancel","flow_id":flow_id}));
    Ok(Action::CancelOAuth)
}

#[allow(clippy::too_many_arguments)]
fn account_buttons(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    primary: &str,
    action: Action,
    secondary: &str,
    other: Action,
) {
    let first = (g.measure_chrome_text(primary, 12.0, true) + 32.0).min((w - 8.0) / 2.0);
    let second = (g.measure_chrome_text(secondary, 12.0, false) + 32.0).min(w - first - 8.0);
    button(
        g,
        s,
        hits,
        (x, *y, first, CTL_H),
        primary,
        Target::Setting(SettingsAction::DeviceAccount(action)),
        true,
    );
    button(
        g,
        s,
        hits,
        (x + first + 8.0, *y, second, CTL_H),
        secondary,
        Target::Setting(SettingsAction::DeviceAccount(other)),
        false,
    );
    *y += ROW_H;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_never_enters_the_render_view_or_error() {
        let state = State {
            password: "secret한글".into(),
            ..State::default()
        };
        assert_eq!(state.view().password_mask, "•".repeat(8));
        assert!(!safe_error("server echoed secret한글").contains("secret"));
    }

    #[test]
    fn pending_request_finishes_without_blocking_the_ui() {
        let (tx, rx) = mpsc::channel();
        let mut state = State {
            pending: Some(rx),
            ..State::default()
        };
        assert!(!state.poll());
        assert!(state.view().busy);
        drop(tx);
        assert!(state.poll());
        assert!(state.message.unwrap().1);
        assert!(state.pending.is_none());
    }

    #[test]
    fn leaving_the_page_clears_the_password() {
        let mut state = State {
            form: true,
            password: "temporary".into(),
            ..State::default()
        };
        state.hide();
        assert!(state.password.is_empty());
        assert!(!state.view().form);
    }

    #[test]
    fn missing_provider_configuration_is_disabled_without_click_target() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!({"providers":[]}),
            serde_json::json!({"providers":[{"id":"google","enabled":false}]}),
        ] {
            assert!(!provider_enabled(&value, Provider::Google));
            assert!(!provider_enabled(&value, Provider::Github));
        }
        assert!(provider_enabled(
            &serde_json::json!({"providers":[{"id":"google","enabled":true}]}),
            Provider::Google
        ));
    }

    #[test]
    fn leaving_settings_cancels_browser_login_and_hides_no_capability_in_view() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = State {
            oauth_cancel: Some(cancel.clone()),
            ..Default::default()
        };
        assert!(state.view().oauth_waiting);
        state.hide();
        assert!(cancel.load(Ordering::Acquire));
    }

    #[test]
    fn app_confirmation_code_is_displayed_until_attempt_finishes() {
        let (result_tx, result_rx) = mpsc::channel();
        let (code_tx, code_rx) = mpsc::channel();
        let mut state = State {
            pending: Some(result_rx),
            oauth_code_rx: Some(code_rx),
            oauth_cancel: Some(Arc::new(AtomicBool::new(false))),
            ..Default::default()
        };
        code_tx.send("ABCD-1234".into()).unwrap();
        assert!(state.poll());
        assert_eq!(state.view().oauth_code.as_deref(), Some("ABCD-1234"));
        assert!(state.view().busy);
        result_tx.send(Ok(Action::CancelOAuth)).unwrap();
        assert!(state.poll());
        assert!(state.view().oauth_code.is_none());
    }

    #[test]
    fn unlinked_sign_in_waits_for_choice_and_survives_only_a_wrong_password() {
        let (result_tx, result_rx) = mpsc::channel();
        let (choice_tx, choice_rx) = mpsc::channel();
        let mut state = State {
            pending: Some(result_rx),
            choice_rx: Some(choice_rx),
            oauth_cancel: Some(Arc::new(AtomicBool::new(false))),
            ..Default::default()
        };
        let choice = Choice {
            flow_id: "flow".into(),
            provider: Provider::Google,
            display: "person@example.com".into(),
            signup: true,
            claim: false,
        };
        choice_tx.send(choice.clone()).unwrap();
        result_tx.send(Ok(Action::Choose)).unwrap();
        assert!(state.poll());
        assert_eq!(state.view().choice, Some(choice));
        assert!(!state.view().busy && !state.view().oauth_waiting);

        let (tx, rx) = mpsc::channel();
        state.pending = Some(rx);
        tx.send(Err(safe_error("bad_credentials"))).unwrap();
        assert!(state.poll());
        assert!(state.choice.is_some(), "a wrong password dropped the choice");
        assert!(state.message.as_ref().unwrap().1);

        let (tx, rx) = mpsc::channel();
        state.pending = Some(rx);
        tx.send(Err(safe_error("expired"))).unwrap();
        assert!(state.poll());
        assert!(state.choice.is_none());
    }

    #[test]
    fn leaving_the_page_drops_a_waiting_choice() {
        let mut state = State::default();
        state.fixture(true, true);
        assert!(state.view().choice.is_some());
        state.hide();
        assert!(state.view().choice.is_none());
    }
}
