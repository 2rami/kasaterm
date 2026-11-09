use super::*;
use anyhow::Context;
use winit::{event::KeyEvent, keyboard::{Key, NamedKey}};

pub(crate) type Choice = (&'static str, &'static str, String);

#[derive(Clone)]
pub(crate) enum Target {
    Window,
    Tab { pane: String, key: String },
    Split { pane: String, key: String, dir: kasa_pty::SplitDir },
}

impl Target {
    fn pane(&self) -> Option<(&str, &str)> {
        match self {
            Self::Window => None,
            Self::Tab { pane, key } | Self::Split { pane, key, .. } => Some((pane, key)),
        }
    }
    pub(crate) fn title(&self) -> &'static str {
        match self { Self::Window => "새 창의 셸", Self::Tab { .. } => "이 칸에 새 셸 탭", Self::Split { .. } => "새 칸의 셸" }
    }
}

#[derive(Default)]
pub(crate) struct Picker {
    target: Option<Target>,
    pub(crate) choices: Vec<Choice>,
    pub(crate) selected: usize,
}

impl Picker {
    pub(crate) fn is_open(&self) -> bool { self.target.is_some() }
    pub(crate) fn title(&self) -> &'static str { self.target.as_ref().map_or("셸 선택", Target::title) }
    pub(crate) fn open(&mut self, target: Target, choices: Vec<Choice>, preferred: Option<&str>) -> bool {
        if choices.is_empty() { self.close(); return false; }
        self.selected = preferred.and_then(|p| choices.iter().position(|(_, _, cmd)| cmd.eq_ignore_ascii_case(p))).unwrap_or(0);
        self.choices = choices;
        self.target = Some(target);
        true
    }
    pub(crate) fn close(&mut self) { self.target = None; self.choices.clear(); self.selected = 0; }
    pub(crate) fn step(&mut self, delta: isize) {
        if self.choices.is_empty() { return; }
        self.selected = (self.selected as isize + delta).rem_euclid(self.choices.len() as isize) as usize;
    }
    fn take(&mut self, command: &str) -> Option<(Target, String)> {
        let command = self.choices.iter().find(|(_, _, c)| c == command)?.2.clone();
        let target = self.target.take()?;
        self.close();
        Some((target, command))
    }
}

impl App {
    pub(crate) fn request_new_window(&mut self) {
        if !self.open_shell_picker(Target::Window) { self.new_window(); }
    }

    pub(crate) fn request_shell_tab(&mut self, pane: &str) -> Result<()> {
        let target = Target::Tab { pane: pane.into(), key: kasa_collab::surface_keys::ensure(pane) };
        if self.open_shell_picker(target) { Ok(()) } else { self.spawn_new_tab(pane, true).map(|_| ()) }
    }

    pub(crate) fn request_shell_split(&mut self, dir: kasa_pty::SplitDir) -> Result<()> {
        let pane = self.ws.lock().unwrap().active_pane.clone().context("분할할 칸이 없어요")?;
        let target = Target::Split { key: kasa_collab::surface_keys::ensure(&pane), pane, dir };
        if self.open_shell_picker(target) { Ok(()) } else { self.split_active_pane_focused(dir).map(|_| ()) }
    }

    fn open_shell_picker(&mut self, target: Target) -> bool {
        let choices = available_shells();
        if choices.is_empty() || self.tmux.is_some()
            || target.pane().is_some_and(|(pane, _)| kasa_remote::remote::remote_info(&self.ws.lock().unwrap().active_tab_pid(pane)).is_some()) { return false; }
        let preferred = target.pane().and_then(|(pane, _)| self.pty_for_pane(pane))
            .and_then(|pty| pty.shell_program()).map(str::to_string).or_else(resolve_default_shell);
        self.handle_menu = None;
        self.account_menu = false;
        self.shell_picker.open(target, choices, preferred.as_deref());
        self.chrome_dirty = true;
        // The opening event must publish hit rectangles before the next click arrives.
        self.render_frame();
        true
    }

    pub(crate) fn select_shell(&mut self, command: &str) {
        let Some((target, command)) = self.shell_picker.take(command) else { return; };
        self.chrome_dirty = true;
        if let Some((pane, key)) = target.pane() {
            if kasa_collab::surface_keys::get(pane).as_deref() != Some(key)
                || !self.ws.lock().unwrap().panes.contains_key(pane) {
                self.set_toast("선택한 칸이 닫혔어요. 다시 열어 주세요".into());
                return;
            }
        }
        if !crate::spawn_env::shell_program_available(&command) {
            self.set_toast("선택한 셸을 찾을 수 없어요".into());
            return;
        }
        self.pending_shell = Some(command);
        let result = match target {
            Target::Window => { self.new_window(); Ok(()) },
            Target::Tab { pane, .. } => self.spawn_new_tab(&pane, true).map(|_| ()),
            Target::Split { pane, dir, .. } => {
                self.focus_pane(&pane);
                self.split_active_pane_focused(dir).map(|_| ())
            }
        };
        // Failed or inherited spawns must not leave a choice for an unrelated request.
        self.pending_shell = None;
        if let Err(error) = result { self.set_toast(format!("셸을 열지 못했어요: {error:#}")); }
    }

    pub(crate) fn shell_picker_key(&mut self, event: &KeyEvent) -> bool {
        if !self.shell_picker.is_open() || self.main_confirmation_modal() { return false; }
        if event.state.is_pressed() {
            match event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.settings_scene.arm_activation_key(NamedKey::Escape);
                    self.shell_picker.close();
                }
                Key::Named(NamedKey::ArrowDown) => self.shell_picker.step(1),
                Key::Named(NamedKey::ArrowUp) => self.shell_picker.step(-1),
                Key::Named(NamedKey::Enter) if !event.repeat => {
                    self.settings_scene.arm_activation_key(NamedKey::Enter);
                    if let Some((_, _, command)) = self.shell_picker.choices.get(self.shell_picker.selected).cloned() {
                        self.select_shell(&command);
                    }
                }
                _ => {}
            }
            self.chrome_dirty = true;
            if let Some(window) = &self.window { window.request_redraw(); }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<Choice> { vec![("CMD", "terminal", "cmd.exe".into()), ("PowerShell 7", "terminal", "pwsh.exe".into())] }

    #[test]
    fn cancellation_and_unknown_choices_do_not_commit_a_request() {
        let mut p = Picker::default();
        assert!(!p.open(Target::Window, vec![], None));
        assert!(p.open(Target::Window, choices(), Some("PWSH.EXE")));
        assert_eq!(p.selected, 1);
        assert!(p.take("not-in-the-list").is_none());
        assert!(p.is_open());
        p.close();
        assert!(p.take("pwsh.exe").is_none());
    }

    #[test]
    fn keyboard_choice_commits_only_once_and_keeps_the_target() {
        let mut p = Picker::default();
        p.open(Target::Tab { pane: "%2".into(), key: "identity".into() }, choices(), None);
        p.step(-1);
        assert_eq!(p.selected, 1);
        p.step(1);
        assert_eq!(p.selected, 0);
        let (target, command) = p.take("cmd.exe").unwrap();
        assert!(matches!(target, Target::Tab { pane, key } if pane == "%2" && key == "identity"));
        assert_eq!(command, "cmd.exe");
        assert!(!p.is_open());
        assert!(p.take("cmd.exe").is_none());
    }
}
