use super::*;

pub(super) const CODE: &str = "fixture-authorization-code#fixture-authorization-state";

impl App {
    pub(crate) fn run_native_login_paste_probe(&mut self) {
        use std::sync::{atomic::{AtomicBool, Ordering}, OnceLock};
        static START: OnceLock<Instant> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        if !crate::verification_run() || std::env::var_os("KASATERM_LOGIN_PASTE_PROBE").is_none()
            || START.get_or_init(Instant::now).elapsed().as_millis() < 1800
            || DONE.swap(true, Ordering::Relaxed) { return; }
        self.open_settings_room(Some(SettingsCat::Accounts));
        self.set_account_scope_home = false;
        let mode = std::env::var("KASATERM_LOGIN_PASTE_PROBE").unwrap_or_default();
        let state = if mode.ends_with("running") {
            crate::settings::LoginState::Running
        } else {
            crate::settings::LoginState::NeedCode
        };
        let id = if mode == "phone-running" { "phone-login-fixture" } else { "" };
        let seeded = self.seed_login_probe(id, state);
        self.refresh_native_settings_dynamic_cache();
        let mut target = None;
        for _ in 0..24 {
            self.chrome_dirty = true;
            self.render_frame();
            target = self.settings_scene.hits().iter().find(|h| h.target == Target::Paste(SettingsInput::LoginCode)).map(|h| h.rect);
            if target.is_some() { break; }
            if !self.settings_scene.scroll_by(80.0) { break; }
        }
        self.native_settings_focus(SettingsInput::LoginCode);
        self.paste_from_menu();
        let menu = self.login_code_edit == CODE;
        self.login_code_edit.clear();
        self.settings_caret = 0;
        if let Some(r) = target { self.native_settings_click(r.0+r.2/2.0, r.1+r.3/2.0); }
        let button = self.login_code_edit == CODE;
        let terminal_untouched = self.ws.lock().unwrap().panes.values().filter_map(|p| p.term()).all(|t| {
            !t.cells.iter().flatten().map(|c| c.ch).collect::<String>().contains(CODE)
        });
        if let Some(root) = std::env::var_os("TMPDIR").map(std::path::PathBuf::from) {
            let report=serde_json::json!({"seeded":seeded,"field_visible":target.is_some(),"menu_paste":menu,"button_paste":button,"terminal_untouched":terminal_untouched});
            let _=std::fs::write(root.join("login-paste.json"),report.to_string());
            if let Some(gpu)=self.gpu.as_mut() { gpu.capture_next=Some(root.join("login-paste.png").to_string_lossy().into_owned()); }
        }
        self.chrome_dirty=true;
        self.render_frame();
    }
}
