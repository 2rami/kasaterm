//! Ordinary close is a short undo grace, not background execution.
use super::*;

pub(crate) fn marker_path(pane: &str) -> Option<std::path::PathBuf> {
    let id = pane.strip_prefix('%')?;
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)) { return None; }
    let socket = std::env::var("KASATERM_SOCKET_PATH").ok()?;
    Some(std::path::PathBuf::from(format!("{socket}.closing")).join(id))
}

pub(crate) fn clear_marker(pane: &str) {
    if let Some(path) = marker_path(pane) { let _ = std::fs::remove_file(path); }
}

pub(crate) fn expired(record: &ClosedPane, now: Instant, grace: std::time::Duration) -> bool {
    record.alive && !record.stashed && record.idle_since.is_some_and(|at| now.saturating_duration_since(at) >= grace)
}

impl App {
    /// Isolated native lifecycle regression; never armed by normal app startup.
    pub(crate) fn run_pending_close_grace_probe(&mut self) {
        use std::sync::{Mutex, OnceLock};
        type Probe = (Instant, String, String, Arc<kasa_pty::PtySession>);
        static START: OnceLock<Instant> = OnceLock::new();
        static STATE: Mutex<Option<Probe>> = Mutex::new(None);
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if std::env::var_os("KASATERM_AUTOCLOSEGRACE").is_none()
            || DONE.load(std::sync::atomic::Ordering::Relaxed) { return; }
        if START.get_or_init(Instant::now).elapsed().as_secs() < 3 { return; }
        let mut state = STATE.lock().unwrap();
        if state.is_none() {
            let victim = self.split_active_pane(kasa_pty::SplitDir::Horizontal).unwrap();
            let held = self.pty[&victim].clone();
            self.hide_pane(&victim);
            assert!(held.input_closed());
            assert!(held.send_bytes(b"must not run\r").is_err());
            self.reopen_closed_pane();
            assert!(Arc::ptr_eq(&held, &self.pty[&victim]));
            assert!(!held.input_closed());
            let stashed = self.split_active_pane(kasa_pty::SplitDir::Vertical).unwrap();
            self.stash_pane(&stashed);
            self.hide_pane(&victim);
            eprintln!("[close-grace] immediate input blocked; early undo reused session; closed={victim} hidden={stashed}");
            *state = Some((Instant::now(), victim, stashed, held));
            return;
        }
        let (since, victim, hidden, held) = state.as_ref().unwrap();
        if since.elapsed() < crate::closed_pane_idle_reap() + std::time::Duration::from_secs(1) { return; }
        self.finish_close_grace();
        assert!(!self.pty.contains_key(victim));
        assert!(held.input_closed());
        assert!(!self.pty[hidden].input_closed());
        let at = self.closed_pane_index(victim).unwrap();
        assert!(!self.closed_panes[at].alive);
        assert!(!self.closed_panes[at].rec.is_null());
        let state_json = self.session_state_json().unwrap();
        assert!(state_json["stashed_panes"].as_array().unwrap().iter()
            .any(|c| c["pane_id"].as_str() == Some(victim.as_str()) && c["stashed"] == false));
        self.reopen_closed_pane_at(at);
        let pid = self.ws.lock().unwrap().active_pane.clone().unwrap();
        assert!(!Arc::ptr_eq(held, &self.pty[&pid]));
        assert!(!self.pty[&pid].input_closed());
        eprintln!("[close-grace] PASS: expired execution, retained serialized record, hidden alive, late undo starts fresh session");
        DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        *state = None;
    }

    pub(crate) fn close_grace_pids(&self, pane: &str) -> Vec<String> {
        let ws = self.ws.lock().unwrap();
        let mut ids = vec![pane.to_string()];
        if let Some(p) = ws.panes.get(pane) { ids.extend(p.tabs.iter().filter_map(|t| t.pid.clone())); }
        ids.sort(); ids.dedup(); ids
    }

    pub(crate) fn close_grace_input(&self, pane: &str, closed: bool) {
        for id in self.close_grace_pids(pane) {
            if closed {
                if let Some(path) = marker_path(&id) {
                    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
                    let _ = std::fs::write(path, b"closed");
                }
            } else { clear_marker(&id); }
            if let Some(pty) = self.pty.get(&id) {
                let interrupt = closed && !kasa_mcp::remote::is_view_pane(&id);
                let _ = pty.set_input_closed(closed, interrupt);
            }
        }
        if closed {
            let proxy = self.proxy.clone();
            let grace = crate::closed_pane_idle_reap();
            std::thread::spawn(move || {
                std::thread::sleep(grace);
                let _ = proxy.send_event(UserEvent::CloseGraceExpired);
            });
        }
    }

    pub(crate) fn finish_close_grace(&mut self) {
        let now = Instant::now();
        let grace = crate::closed_pane_idle_reap();
        let ids: Vec<_> = self.closed_panes.iter().filter(|c| expired(c, now, grace))
            .map(|c| c.pane_id.clone()).collect();
        for id in ids {
            // kill_hidden_pane also checks this; never expire a resurrected ID.
            if self.leaf_lingers_anywhere(&id) { continue; }
            for pid in self.close_grace_pids(&id) {
                if let Some(pty) = self.pty.get(&pid) { pty.terminate_local(); }
            }
            self.kill_hidden_pane(&id);
            for c in self.closed_panes.iter_mut().filter(|c| c.pane_id == id && c.alive) {
                c.alive = false;
                c.idle_since = None;
            }
            self.session_touched = true;
            self.chrome_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_deadline_is_absolute_and_preserves_the_recovery_record() {
        let at = Instant::now();
        let mut c = ClosedPane { rec: serde_json::json!({"session_id":"keep-me"}),
            pane_id:"%1".into(), character:String::new(), folder:String::new(), neighbor:None,
            window:0, alive:true, stashed:false, idle_since:Some(at), preview:None };
        let grace = std::time::Duration::from_secs(10);
        assert!(!expired(&c, at + std::time::Duration::from_millis(9999), grace));
        assert!(expired(&c, at + grace, grace));
        c.stashed = true;
        assert!(!expired(&c, at + grace * 10, grace));
        c.stashed = false; c.alive = false;
        assert!(!expired(&c, at + grace * 10, grace));
        assert_eq!(c.rec["session_id"], "keep-me");
    }
}
