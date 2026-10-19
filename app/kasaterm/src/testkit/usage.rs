use super::*;
use std::sync::{Mutex, OnceLock};

struct Case {
    at: Instant,
    first: String,
    second: Option<String>,
    held: Arc<kasa_pty::PtySession>,
    survivor: Option<Arc<kasa_pty::PtySession>>,
    count: usize,
    prepared: bool,
    stopped: bool,
    target_pid: u32,
}

impl App {
    pub(crate) fn run_usage_lifecycle_probe(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if !crate::verification_run() || std::env::var_os("KASATERM_AUTOUSAGE_LIFECYCLE").is_none() { return; }
        static START: OnceLock<Instant> = OnceLock::new();
        static STATE: Mutex<(usize, Option<Case>)> = Mutex::new((0, None));
        if START.get_or_init(Instant::now).elapsed().as_secs() < 3 { return; }
        let mut state = STATE.lock().unwrap();
        let (number, case) = &mut *state;
        if *number >= 4 { return; }
        if case.is_none() {
            self.new_window();
            let first = self.ws.lock().unwrap().active_pane.clone().unwrap();
            let held = self.pty[&first].clone();
            let target_pid = held.shell_pid().unwrap();
            *case = Some(Case { at: Instant::now(), first, second: None, held, survivor: None, count: self.pty.len(), prepared: false, stopped: false, target_pid });
            return;
        }
        let current = case.as_mut().unwrap();
        if current.at.elapsed().as_millis() < 600 { return; }
        if !current.prepared {
            assert!(self.ws.lock().unwrap().panes.contains_key(&current.first));
            current.second = (*number < 2).then(|| self.spawn_new_tab(&current.first, false).unwrap());
            if *number == 2 {
                self.ws.lock().unwrap().panes.get_mut(&current.first).unwrap().tabs.push(PaneTab {
                    title: Some("retained document".into()), preview_path: Some("/tmp/retained.md".into()), ..Default::default()
                });
            }
            let target = if *number == 1 { current.second.as_ref().unwrap() } else { &current.first };
            let kept = if *number == 1 { Some(&current.first) } else { current.second.as_ref() };
            current.held = self.pty[target].clone();
            current.survivor = kept.map(|id| self.pty[id].clone());
            current.target_pid = current.held.shell_pid().unwrap();
            current.count = self.pty.len();
            if *number < 3 { self.stash_pane(&current.first); }
            current.prepared = true;
            current.at = Instant::now();
            return;
        }
        let target = if *number == 1 { current.second.as_ref().unwrap() } else { &current.first };
        if !current.stopped {
            self.statusbar.usage_processes = crate::input::fresh_usage_processes().unwrap();
            self.decorate_usage_processes();
            let row = self.statusbar.usage_processes.iter().find(|row| row.id.pid == current.target_pid).unwrap().clone();
            assert_ne!(row.id.started, 0);
            if *number == 2 {
                assert!(!crate::process_usage::stoppable(&row, &self.statusbar.usage_processes));
                self.open_usage_shell(&row.id);
                assert!(self.window_of_pane(&current.first).is_some());
                let ws = self.ws.lock().unwrap();
                assert!(ws.panes[&current.first].tabs.iter().any(|tab| tab.title.as_deref() == Some("retained document")));
                assert!(Arc::ptr_eq(&self.pty[&current.first], &current.held));
            } else {
                self.stop_usage_process(&row.id);
                assert!(self.pty.contains_key(target), "first click only confirms");
                self.stop_usage_process(&row.id);
                assert!(!self.pty.contains_key(target), "{:?}", self.collab.toast);
                if *number < 2 {
                    let kept = if *number == 1 { &current.first } else { current.second.as_ref().unwrap() };
                    assert!(Arc::ptr_eq(&self.pty[kept], current.survivor.as_ref().unwrap()));
                    let key = row.owner.as_ref().unwrap().key.clone();
                    let dead = self.closed_panes.iter().find(|record| !record.alive && record.rec["surface_key"].as_str() == Some(&key)).unwrap();
                    assert!(dead.rec.get("tabs").is_none());
                    let dead_id = dead.ui_id;
                    let kept_index = self.closed_panes.iter().position(|record| record.alive && record.pane_id == *kept).unwrap();
                    assert!(self.closed_panes[kept_index].rec.get("tabs").is_none());
                    self.reopen_closed_pane_at(kept_index);
                    assert!(Arc::ptr_eq(&self.pty[kept], current.survivor.as_ref().unwrap()));
                    let dead_index = self.closed_panes.iter().position(|record| record.ui_id == dead_id).unwrap();
                    self.reopen_closed_pane_at(dead_index);
                    assert_eq!(self.pty.len(), current.count);
                    self.dead_panes.lock().unwrap().push(DeadPane { id: target.clone(), session: Arc::downgrade(&current.held) });
                    self.reap_dead_panes(event_loop);
                    assert_eq!(self.pty.len(), current.count, "late EOF must not reap the restored shell");
                } else {
                    assert!(current.held.input_closed());
                    assert!(current.held.send_bytes(b"must not run\r").is_err());
                }
            }
            current.at = Instant::now();
            current.stopped = true;
            return;
        }
        if *number != 2 { assert!(crate::input::usage_process_birth(current.target_pid).is_none(), "selected shell must really exit even while an Arc is held"); }
        if let Some(survivor) = &current.survivor {
            assert!(crate::input::usage_process_birth(survivor.shell_pid().unwrap()).is_some());
            assert_eq!(self.pty.len(), current.count);
        }
        eprintln!("[usage-lifecycle] PASS case={number}: selected shell only, retained sibling/receipt, stale EOF ignored");
        *number += 1;
        *case = None;
    }
}
