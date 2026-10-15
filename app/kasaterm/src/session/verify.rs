use super::*;

impl App {
    pub(crate) fn run_pending_session_probe(&mut self) {
        use std::sync::{Mutex, OnceLock};
        static MODE: OnceLock<String> = OnceLock::new();
        static START: OnceLock<Instant> = OnceLock::new();
        static STATE: Mutex<Option<(Instant, String, String, u8)>> = Mutex::new(None);
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let mode = MODE.get_or_init(|| {
            if crate::verification_run() { std::env::var("KASATERM_AUTOSESSION_PROBE").unwrap_or_default() }
            else { String::new() }
        });
        if mode.is_empty() || DONE.load(std::sync::atomic::Ordering::Relaxed) { return; }
        if START.get_or_init(Instant::now).elapsed().as_secs() < 3 { return; }
        if mode == "restore" {
            // This preview has no real conversation IDs and never invokes restoration.
            self.restore_prompt = Some(serde_json::json!({"sessions":[{"windows":[
                {"leaf":{"character":"아리스","was_agent":"codex"}},
                {"leaf":{"character":"마리"}},
                {"leaf":{"character":"호시노","was_agent":"claude","remote_label":"fixture-mini"}}
            ]}]}));
            self.chrome_dirty = true;
            self.render_frame();
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if mode == "retained" {
            let records = serde_json::json!({"stashed_panes":[
                {"pane_id":"%90","folder":"직접 접어 둔 셸","stashed":true,"retained_by":"user","rec":{"pane_id":"%90"}},
                {"pane_id":"%91","folder":"배치에서 회수한 셸","stashed":true,"retained_by":"recovery","rec":{"pane_id":"%91"}},
                {"pane_id":"%92","folder":"이전 판의 기록","stashed":true,"rec":{"pane_id":"%92"}}
            ]});
            self.restore_closed_history(&records);
            if !self.git.col_visible { self.toggle_git_col(); }
            self.info.tab = crate::state::SideTab::Sessions;
            self.chrome_dirty = true;
            self.render_frame();
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if mode != "primary-close" && mode != "primary-preview" { return; }
        let mut state = STATE.lock().unwrap();
        if mode == "primary-preview" {
            if state.is_none() {
                let Some(primary) = self.ws.lock().unwrap().active_pane.clone() else { return; };
                if self.ws.lock().unwrap().find_tab_by_pty(&primary).is_none() { return; }
                self.ws.lock().unwrap().panes.get_mut(&primary).unwrap().tabs.push(crate::PaneTab {
                    content: crate::PaneContent::Web(crate::WebPane { url: "about:blank".into(), host_id: 0 }),
                    ..Default::default()
                });
                self.close_tab(&primary, 0);
                assert!(!self.pty.contains_key(&primary));
                self.remove_finished_surface(&primary);
                *state = Some((Instant::now(), primary, String::new(), 0));
                return;
            }
            let (since, primary, _, _) = state.as_ref().unwrap();
            if since.elapsed().as_secs() < 4 { return; }
            assert!(!self.pty.contains_key(primary));
            assert!(self.leaf_lingers_anywhere(primary));
            assert!(matches!(self.ws.lock().unwrap().panes[primary].tabs[0].content, crate::PaneContent::Web(_)));
            eprintln!("[session-probe] PASS primary close keeps its PTY-free sibling and application alive");
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
            *state = None;
            return;
        }
        if state.is_none() {
            let Some(primary) = self.ws.lock().unwrap().active_pane.clone() else { return; };
            if self.ws.lock().unwrap().find_tab_by_pty(&primary).is_none() { return; }
            let second = self.spawn_new_tab(&primary, false).unwrap();
            let third = self.spawn_new_tab(&primary, false).unwrap();
            let held = self.pty[&primary].clone();
            self.close_tab(&primary, 0);
            assert!(!self.pty.contains_key(&primary), "closing a primary tab leaked its PTY");
            assert!(held.input_closed(), "HTTP references must not keep closed input usable");
            assert!(self.pty.contains_key(&second) && self.pty.contains_key(&third));
            self.remove_finished_surface(&primary);
            assert!(self.leaf_lingers_anywhere(&second), "late EOF removed the surviving cell");
            *state = Some((Instant::now(), second, third, 0));
            return;
        }
        let (since, second, third, step) = state.as_ref().unwrap();
        if since.elapsed().as_secs() < 4 { return; }
        assert!(self.closed_panes.iter().all(|entry| !entry.stashed), "closed tab became hidden");
        assert!(self.pty.contains_key(third), "a sibling tab was terminated");
        if *step == 0 {
            let second = second.clone();
            let third = third.clone();
            self.remove_finished_surface(&second);
            assert!(!self.pty.contains_key(&second));
            assert!(self.leaf_lingers_anywhere(&third));
            *state = Some((Instant::now(), second, third, 1));
            return;
        }
        assert!(self.leaf_lingers_anywhere(third));
        eprintln!("[session-probe] PASS primary close, late EOF, sibling EOF, survivor and no hidden PTY");
        DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        *state = None;
    }
}
