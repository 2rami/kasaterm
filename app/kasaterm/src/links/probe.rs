use crate::*;

impl App {
    pub(super) fn record_terminal_link_probe(&self, pane: &str, target: &str) -> bool {
        if !crate::verification_run() || std::env::var("KASATERM_LINK_PROBE").as_deref() != Ok("1")
        {
            return false;
        }
        let temporary = |key: &str| {
            std::env::var_os(key)
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.starts_with("/private/tmp") || p.starts_with("/tmp"))
        };
        if !["KASATERM_SETTINGS_FILE", "KASATERM_SESSION_FILE", "TMPDIR"]
            .iter()
            .all(|key| temporary(key))
        {
            return false;
        }
        let file = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap())
            .join("opened-links.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            use std::io::Write;
            let _ = writeln!(file, "{}", serde_json::json!({"pane":pane,"target":target}));
        }
        true
    }
    pub(crate) fn run_terminal_link_probe(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            OnceLock,
        };
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        use winit::keyboard::ModifiersState;
        static START: OnceLock<Instant> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        if !crate::verification_run()
            || std::env::var("KASATERM_LINK_PROBE").as_deref() != Ok("1")
            || DONE.load(Ordering::Relaxed)
            || START.get_or_init(Instant::now).elapsed().as_millis() < 4000
        {
            return;
        }
        let temporary = |key: &str| {
            std::env::var_os(key)
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.starts_with("/private/tmp") || p.starts_with("/tmp"))
        };
        if !["KASATERM_SETTINGS_FILE", "KASATERM_SESSION_FILE", "TMPDIR"]
            .iter()
            .all(|key| temporary(key))
            || std::env::var_os("KASATERM_AUTOQUIT_MS").is_none()
        {
            return;
        }
        let Ok(raw) = std::env::var("KASATERM_LINK_POINTS") else {
            return;
        };
        let Ok(points) = serde_json::from_str::<Vec<(f32, f32, String)>>(&raw) else {
            return;
        };
        DONE.store(true, Ordering::Relaxed);
        let Some(id) = self.window.as_ref().map(|w| w.id()) else {
            return;
        };
        let original = self.modifiers;
        let modifier = if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        };
        let move_to = |app: &mut Self, x: f32, y: f32| {
            let scale = app.effective_scale() as f64;
            app.window_event(
                event_loop,
                id,
                WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(x as f64 * scale, y as f64 * scale),
                },
            );
        };
        let press = |app: &mut Self, state| {
            app.window_event(
                event_loop,
                id,
                WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state,
                    button: MouseButton::Left,
                },
            )
        };
        let mut checks = Vec::new();
        for (x, y, expected) in points {
            move_to(self, x, y);
            checks.push((
                "visible target",
                self.link_hit(x, y)
                    .is_some_and(|(_, _, value)| value == expected),
            ));
            self.modifiers = ModifiersState::empty();
            press(self, ElementState::Pressed);
            checks.push((
                "ordinary click remains selection",
                self.link_armed.is_none(),
            ));
            press(self, ElementState::Released);
            self.modifiers = modifier;
            press(self, ElementState::Pressed);
            checks.push((
                "modified press arms exact link",
                self.link_armed
                    .as_ref()
                    .is_some_and(|(_, value, _)| value == &expected),
            ));
            press(self, ElementState::Released);
            checks.push((
                "modified release clears pending link",
                self.link_armed.is_none(),
            ));
            press(self, ElementState::Pressed);
            move_to(self, x + 20.0, y);
            press(self, ElementState::Released);
            checks.push(("drag disarms", self.link_armed.is_none()));
            move_to(self, x, y);
            press(self, ElementState::Pressed);
            self.modifiers = ModifiersState::empty();
            press(self, ElementState::Released);
        }
        if let Ok(raw) = std::env::var("KASATERM_LINK_MISS_POINTS") {
            if let Ok(points) = serde_json::from_str::<Vec<(f32, f32)>>(&raw) {
                for (x, y) in points {
                    move_to(self, x, y);
                    checks.push(("suffix is outside link", self.link_hit(x, y).is_none()));
                    self.modifiers = modifier;
                    press(self, ElementState::Pressed);
                    checks.push(("suffix press does not arm link", self.link_armed.is_none()));
                    press(self, ElementState::Released);
                    checks.push(("suffix release keeps link unarmed", self.link_armed.is_none()));
                }
            }
        }
        self.modifiers = original;
        let failures = checks.iter().filter(|(_, ok)| !*ok).count();
        if let Some(root) = std::env::var_os("TMPDIR") {
            let _ = std::fs::write(
                std::path::PathBuf::from(root).join("link-probe.json"),
                serde_json::json!({"checks":checks,"failures":failures}).to_string(),
            );
        }
        self.chrome_dirty = true;
    }
}
