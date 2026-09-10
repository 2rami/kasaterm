//! Opt-in, isolated native focus/scroll regression; no source input or resize.
use super::*;

impl App {
    pub(crate) fn run_mirror_focus_probe(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use winit::{application::ApplicationHandler, event::WindowEvent};
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        static PHASE: AtomicU8 = AtomicU8::new(0);
        if !crate::verification_run() || std::env::var_os("KASATERM_AUTOMIRROR_FOCUS").is_none() { return; }
        let elapsed = START.get_or_init(Instant::now).elapsed().as_secs();
        let phase = PHASE.load(Ordering::Relaxed);
        if phase >= 3 || elapsed < 4 + u64::from(phase) * 3
            || self.restore_progress.is_some() || self.restore_applying.is_some() { return; }
        let Some(id) = self.target_surface() else { return };
        if !kasa_mcp::remote::is_view_pane(&id) { return; }
        let session = self.pty[&id].clone();
        let window = self.window.as_ref().unwrap().id();
        if phase == 0 {
            assert!(session.scroll(12) > 0, "fixture must have scrollback");
            self.render_frame();
            eprintln!("[mirror-focus] scrolled viewer only; source size={:?}", session.size());
        } else {
            let before = self.pane_view_shift.get(&id).and_then(|s| s.projection.clone()).unwrap();
            let dimensions = session.size();
            self.window_event(event_loop, window, WindowEvent::Focused(false));
            self.window_event(event_loop, window, WindowEvent::Focused(true));
            self.render_frame();
            let after = self.pane_view_shift.get(&id).and_then(|s| s.projection.clone()).unwrap();
            assert!(after.rows.iter().flatten().any(|c| !matches!(c.ch,' '|'\0')));
            assert_eq!(session.size(), dimensions, "focus resized a passive mirror");
            assert_eq!(before.top_abs, after.top_abs, "focus moved the reading position");
            assert!(self.restore_progress.is_none(), "completed restore became pending again");
            if phase == 2 {
                session.scroll_to_bottom();
                self.mirror_view_scroll.remove(&id);
                self.turn.clear_mirror_target(&id);
                self.render_frame();
                eprintln!("[mirror-focus] PASS: focus retains content/anchor/dimensions; restore loading gone; returned to live input");
            }
        }
        PHASE.store(phase + 1, Ordering::Relaxed);
    }
}
