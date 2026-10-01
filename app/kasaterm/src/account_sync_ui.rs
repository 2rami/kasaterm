use super::*;

impl App {
    pub(crate) fn register_account_sync(&self) {
        let proxy = self.proxy.clone();
        kasa_mcp::account_sync::set_apply_hook(Arc::new(move |request| {
            let (tx, rx) = std::sync::mpsc::channel();
            proxy.send_event(UserEvent::AccountSyncApply(request, tx)).map_err(|_| "app closed".to_string())?;
            rx.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| "app sync apply timed out".to_string())?
        }));
    }

    pub(crate) fn apply_account_sync(&mut self, request: kasa_mcp::account_sync::PendingApply) -> Result<(), String> {
        if self.settings_input.is_some() || self.machine_edit.is_some() {
            return Err("settings edit in progress".into());
        }
        let refresh = request.changes_runtime();
        let students = ["character_theme", "character_picks"].iter().any(|key| request.changes_setting(key));
        kasa_mcp::account_sync::apply_pending(request)?;
        if !refresh { return Ok(()); }
        let settings = socket::read_settings();
        let persona = self.set_claude_persona;
        self.set_claude_persona = socket::read_claude_persona();
        self.set_file_tree_default = socket::read_file_tree_default();
        self.set_footer_default = socket::read_footer_default();
        self.set_usage_compact = socket::read_usage_compact();
        self.set_status_h = socket::read_status_h();
        self.set_pane_footer_h = socket::read_pane_footer_h();
        self.set_statusbar = statusbar_config::Prefs::from_settings(&settings);
        self.weather.settings = crate::weather::WeatherState::load(settings.get("weather"));
        self.weather_settings_changed();
        self.tabs_on_top = socket::read_tab_position() == "top";
        self.cursor_shape = socket::read_cursor_shape();
        self.cursor_thickness = socket::read_cursor_thickness();
        self.mouse_cursor = socket::read_mouse_cursor();
        self.font_size = socket::read_font_size();
        theme::apply_from_settings_read_only();
        if persona != self.set_claude_persona { self.regen_pane_shims(); }
        // Before the settings cache reloads: it reads the roster and theme cards through these.
        if students { self.reload_student_choices(); }
        self.settings_scene.refresh_cache();
        self.refresh_native_settings_dynamic_cache();
        self.reload_native_settings_media_cache();
        self.apply_effective_scale();
        self.repaint_all();
        if let Some(window) = &self.window { window.request_redraw(); }
        Ok(())
    }
}
