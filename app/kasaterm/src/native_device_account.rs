use super::*;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    OpenLogin,
    Login,
    Cancel,
    AskLogout,
    Logout,
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
}

pub(crate) fn mask(value: &str) -> String {
    "•".repeat(value.chars().count())
}

impl State {
    pub(crate) fn refresh(&mut self) {
        self.status = kasa_mcp::device_auth::status();
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
        }
    }

    pub(crate) fn hide(&mut self) {
        self.password.clear();
        self.form = false;
        self.confirm_logout = false;
    }

    fn poll(&mut self) -> bool {
        let result = match self.pending.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("요청을 마치지 못했어요. 다시 시도해 주세요".into())
            }
            _ => return false,
        };
        self.pending = None;
        match result {
            Ok(Action::Login) => {
                self.form = false;
                // 관문에 붙은 즉시 다른 기기의 에이전트 계정 목록을 받는다.
                kasa_mcp::agent_accounts::poke();
                self.message = Some((
                    "로그인을 저장했어요. 관문 연결을 확인하고 있어요".into(),
                    false,
                ));
            }
            Ok(_) => {
                self.confirm_logout = false;
                self.message = Some(("이 기기에서 로그아웃했어요".into(), false));
            }
            Err(error) => self.message = Some((error, true)),
        }
        true
    }
}

fn safe_error(error: &str) -> String {
    if error.contains("아이디나 비밀번호") {
        "아이디나 비밀번호를 확인해 주세요".into()
    } else if error.contains("너무 여러 번") {
        "로그인 시도가 많아요. 잠시 후 다시 시도해 주세요".into()
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
        if self.device_account.pending.is_some() {
            return;
        }
        self.device_account.message = None;
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
    draw_text(g, x, *y, "기기 연결 계정", 12.0, theme::text(), true);
    *y += 24.0;
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
    if let Some(account) = v.status["account"].as_str() {
        let label = fit(g, account, w, 10.5, false);
        draw_text(g, x, *y, &label, 10.5, theme::text_dim(), false);
        *y += 18.0;
    }
    *y += 8.0;
    if v.busy {
        draw_text(g, x, *y, "요청 처리 중…", 12.0, theme::text_dim(), false);
        *y += ROW_H;
    } else if v.form {
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
        account_buttons(
            g,
            s,
            hits,
            x,
            y,
            w,
            "로그인",
            Action::Login,
            "취소",
            Action::Cancel,
        );
        info_slab(
            g,
            x,
            y,
            w,
            "비밀번호는 저장하지 않습니다. 로그인 정보는 이 기기에 유지됩니다.",
        );
    } else if v.confirm_logout {
        info_slab(g, x, y, w, "이 기기의 계정 연결을 해제합니다. 다른 기기의 로그인과 기존 기기 연결 설정은 유지됩니다.");
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
    } else {
        let saved = v.status["credential_saved"].as_bool() == Some(true);
        if saved {
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
        } else {
            let bw = (g.measure_chrome_text("로그인", 12.0, true) + 32.0).min(w);
            button(
                g,
                s,
                hits,
                (x, *y, bw, CTL_H),
                "로그인",
                Target::Setting(SettingsAction::DeviceAccount(Action::OpenLogin)),
                true,
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
        "계정 로그인은 이 기기를 등록합니다. 다른 기기와의 실제 연결은 아래 목록에서 확인하세요.",
    );
    if let Some(message) = v.status["sync"]["message"].as_str() { info_slab(g, x, y, w, message); }
    *y += 24.0;
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
}
