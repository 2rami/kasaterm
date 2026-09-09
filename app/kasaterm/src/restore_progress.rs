//! Launch restoration stays modal until every saved surface has live output.
use super::*;
use std::time::{Duration, Instant};

pub(crate) struct RestoreProgress {
    pub state: serde_json::Value,
    pub expected: usize,
    pub ready: usize,
    pub failure: Option<String>,
    pub started: Instant,
    pub built: bool,
    entries: HashMap<String, RestoreEntry>,
}

struct RestoreEntry {
    remote: bool,
    agent: bool,
    web: bool,
    ready: bool,
}

fn surface_count(node: &serde_json::Value) -> usize {
    if crate::internal_room::is_saved_window(node) { return 0; }
    if let Some(leaf) = node.get("leaf").filter(|leaf| leaf.is_object()) {
        return 1 + leaf.get("tabs").and_then(|tabs| tabs.as_array()).map_or(0, Vec::len);
    }
    node.get("split").map_or(0, |split| surface_count(&split["a"]) + surface_count(&split["b"]))
}

impl RestoreProgress {
    pub fn new(state: serde_json::Value) -> Self {
        let session = state.get("sessions").and_then(|sessions| sessions.as_array()).and_then(|sessions| {
            sessions.get(state["active_session"].as_u64().unwrap_or(0) as usize).or_else(|| sessions.first())
        });
        let expected = session.map_or(0, |session| {
            ["windows", "undocked"].into_iter().map(|key| {
                session.get(key).and_then(|nodes| nodes.as_array())
                    .map_or(0, |nodes| nodes.iter().map(surface_count).sum::<usize>())
            }).sum()
        });
        Self { state, expected, ready: 0, failure: None, started: Instant::now(), built: false, entries: HashMap::new() }
    }

    pub fn track(&mut self, id: &str, record: &serde_json::Value) {
        self.entries.insert(id.to_string(), RestoreEntry {
            remote: record.get("remote_base").and_then(|v| v.as_str()).is_some(),
            agent: record.get("was_agent").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
            web: record.get("web_url").is_some(),
            ready: false,
        });
    }

    /// Retry only the wait for unfinished entries. The original layout,
    /// assigned IDs and completed sessions are never reconstructed here.
    fn retry_pending(&mut self, now: Instant) -> Vec<String> {
        if !self.built {
            // An invalid/unbuilt snapshot needs an explicit recovery choice,
            // not another destructive whole-workspace restore.
            return Vec::new();
        }
        self.started = now;
        self.failure = None;
        self.entries.iter().filter(|(_, entry)| entry.remote && !entry.ready)
            .map(|(id, _)| id.clone()).collect()
    }
}

impl App {
    /// Explicit isolated harness only; never reads the user's saved session.
    pub(crate) fn run_restore_probe(&mut self) {
        if !crate::verification_run() { return; }
        let Ok(path) = std::env::var("KASATERM_AUTORESTORE_PROBE") else { return; };
        static PHASE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        static ORIGINAL: std::sync::OnceLock<Mutex<HashMap<String, std::sync::Weak<kasa_pty::PtySession>>>> = std::sync::OnceLock::new();
        use std::sync::atomic::Ordering;
        let preserved = |app: &Self| {
            let original = ORIGINAL.get_or_init(Default::default).lock().unwrap();
            original.len() == app.pty.len() && original.iter().all(|(id, old)| {
                old.upgrade().zip(app.pty.get(id)).is_some_and(|(old, current)| Arc::ptr_eq(&old, current))
            })
        };
        match PHASE.load(Ordering::Relaxed) {
            0 => {
                PHASE.store(1, Ordering::Relaxed);
                let state = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).expect("isolated restore fixture");
                self.restore_session_state(&state);
                *ORIGINAL.get_or_init(Default::default).lock().unwrap() = self.pty.iter()
                    .map(|(id, session)| (id.clone(), Arc::downgrade(session))).collect();
                for id in self.pty.keys() { self.send_bytes_to_surface(Some(id), b"RESTORE_BLOCKED_PROBE"); }
                eprintln!("[restore-probe] blocked={} expected={}", self.restoration_blocks_input(), self.restore_progress.as_ref().map_or(0, |p| p.expected));
            }
            1 if self.restore_progress.as_ref().is_some_and(|p| p.failure.is_some()) => {
                eprintln!("[restore-probe] failure=visible input-blocked={} ready={}", self.restoration_blocks_input(), self.restore_progress.as_ref().map_or(0, |p| p.ready));
                PHASE.store(2, Ordering::Relaxed);
                self.retry_restore();
                eprintln!("[restore-probe] retry=queued preserved={} ready={} expected={}", preserved(self),
                    self.restore_progress.as_ref().map_or(0, |p| p.ready),
                    self.restore_progress.as_ref().map_or(0, |p| p.expected));
            }
            1 | 2 if !self.restoration_blocks_input() => {
                PHASE.store(3, Ordering::Relaxed);
                for id in self.pty.keys() { self.send_bytes_to_surface(Some(id), b"RESTORE_ALLOWED_PROBE"); }
                let ws = self.ws.lock().unwrap();
                let tabs: usize = ws.panes.values().map(|p| p.tabs.len()).sum();
                let active: Vec<_> = ws.panes.values().map(|p| p.active_tab).collect();
                eprintln!("[restore-probe] complete=true surfaces={} tabs={tabs} active={active:?} input-blocked=false preserved={}", self.pty.len(), preserved(self));
            }
            _ => {}
        }
    }

    pub(crate) fn restoration_blocks_input(&self) -> bool {
        self.restore_applying.is_some() || self.restore_progress.is_some()
    }

    pub(crate) fn retry_restore(&mut self) {
        let Some(progress) = self.restore_progress.as_mut() else { return };
        let pending_remote = progress.retry_pending(Instant::now());
        for id in pending_remote {
            // Wake only existing unfinished links, never replace their parser
            // or attach a new source pane when an old source has disappeared.
            kasa_mcp::remote::retry_connection(&id);
        }
        // Keep each PTY/parser and queued local resume commands alive.
        // Never schedule restore_session_state: it clears every current pane.
        self.tick_restore_progress();
        self.chrome_dirty = true;
        if let Some(window) = &self.window { window.request_redraw(); }
    }

    pub(crate) fn tick_restore_progress(&mut self) {
        let Some(progress) = self.restore_progress.as_mut().filter(|p| p.built) else { return };
        let mut ready = 0;
        let mut failure = None;
        let ws = self.ws.lock().unwrap();
        for (id, entry) in &mut progress.entries {
            entry.ready = false;
            if entry.web {
                if ws.panes.contains_key(id) && self.pending_web_hosts.is_empty() {
                    entry.ready = true;
                    ready += 1;
                }
                continue;
            }
            let Some(session) = self.pty.get(id) else {
                failure.get_or_insert_with(|| format!("{id} 창을 되살리지 못했어요"));
                continue;
            };
            let term = ws.panes.values().flat_map(|pane| &pane.tabs)
                .find(|tab| tab.pid.as_deref() == Some(id.as_str()))
                .and_then(|tab| tab.term());
            let has_grid = term.is_some_and(|term| term.live_output && !term.cells.is_empty());
            if entry.remote {
                match kasa_mcp::remote::connection_readiness(id) {
                    Some((true, generation, _)) if has_grid && term.is_some_and(|term| term.output_generation == generation) => {
                        entry.ready = true;
                        ready += 1;
                    }
                    Some((_, _, Some(error))) => { failure.get_or_insert_with(|| format!("{id} · {error}")); }
                    None => { failure.get_or_insert_with(|| format!("{id} 원격 연결이 끝났어요")); }
                    _ => {}
                }
            } else {
                let commands_pending = self.pending_restores.iter().any(|(pending, _, _)| Arc::ptr_eq(pending, session));
                if has_grid && !commands_pending && (!entry.agent || session.active_agent().is_some()) {
                    entry.ready = true;
                    ready += 1;
                }
            }
        }
        drop(ws);
        if progress.entries.len() != progress.expected {
            failure.get_or_insert_with(|| "저장된 창·탭 일부를 되살리지 못했어요".to_string());
        }
        if ready < progress.expected && progress.started.elapsed() >= Duration::from_secs(45) {
            failure.get_or_insert_with(|| "아직 준비되지 않은 창이 있어요. 연결을 확인하고 다시 시도해 주세요".to_string());
        }
        let complete = ready == progress.expected && failure.is_none();
        let changed = progress.ready != ready || progress.failure != failure;
        if progress.ready != ready {
            eprintln!("[restore] progress={ready}/{}", progress.expected);
        }
        if complete {
            eprintln!("[restore] complete={ready}/{}", progress.expected);
        }
        progress.ready = ready;
        progress.failure = failure;
        if complete { self.restore_progress = None; }
        if changed || complete {
            self.chrome_dirty = true;
            if let Some(window) = &self.window { window.request_redraw(); }
        }
    }
}

pub(crate) fn last_used_label(state: &serde_json::Value) -> String {
    let Some(saved) = state.get("last_used_unix").and_then(|v| v.as_u64()) else {
        return "마지막 사용 시각 기록 없음".into();
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(saved, |d| d.as_secs());
    let ago = now.saturating_sub(saved);
    let relative = if ago < 60 { "방금 전".into() }
    else if ago < 3600 { format!("{}분 전", ago / 60) }
    else if ago < 86400 { format!("{}시간 전", ago / 3600) }
    else { format!("{}일 전", ago / 86400) };
    #[cfg(unix)]
    {
        let time = saved.min(libc::time_t::MAX as u64) as libc::time_t;
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&time, &mut local) }.is_null() {
            return format!("마지막 사용 · {:04}.{:02}.{:02} {:02}:{:02} ({relative})",
                local.tm_year + 1900, local.tm_mon + 1, local.tm_mday, local.tm_hour, local.tm_min);
        }
    }
    format!("마지막 사용 · {relative}")
}

pub(crate) fn with_file_time(mut state: serde_json::Value) -> serde_json::Value {
    if state.get("last_used_unix").is_none() {
        let modified = crate::socket::session_file_path()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        if let (Some(object), Some(time)) = (state.as_object_mut(), modified) {
            object.insert("last_used_unix".into(), serde_json::json!(time.as_secs()));
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_inactive_tabs_and_undocked_surfaces_in_the_active_session() {
        let state = serde_json::json!({"active_session": 0, "sessions": [{
            "windows": [{"leaf": {"pane_id":"%1", "tabs":[{}, {}]}}],
            "undocked": [{"leaf":{"pane_id":"%4"}}]
        }, {"windows": [{"leaf": {}}]}]});
        assert_eq!(RestoreProgress::new(state).expected, 4);
    }
    #[test]
    fn old_snapshots_do_not_invent_a_last_used_time() {
        assert_eq!(last_used_label(&serde_json::json!({})), "마지막 사용 시각 기록 없음");
    }

    #[test]
    fn retry_preserves_five_ready_surfaces_and_only_targets_two_pending_links() {
        let state = serde_json::json!({"sessions": [{"windows": [{"leaf": {
            "pane_id": "%0", "tabs": [{}, {}, {}, {}, {}, {}]
        }}]}]});
        let mut progress = RestoreProgress::new(state.clone());
        for number in 0..7 {
            let id = format!("%{number}");
            progress.track(&id, &serde_json::json!({"remote_base": "http://restore.invalid"}));
            progress.entries.get_mut(&id).unwrap().ready = number < 5;
        }
        progress.built = true;
        progress.ready = 5;
        progress.failure = Some("timeout".into());
        let now = Instant::now();
        let mut pending = progress.retry_pending(now);
        pending.sort();
        assert_eq!(pending, ["%5", "%6"]);
        assert_eq!((progress.ready, progress.expected, progress.entries.len()), (5, 7, 7));
        assert_eq!(progress.state, state);
        assert!(progress.built);
        assert_eq!(progress.started, now);
        assert!(progress.failure.is_none());
        // Repeated clicks do not allocate IDs, reset completion or repeat work
        // for already restored panes (whose live PTYs belong to the App).
        assert_eq!(progress.retry_pending(now).len(), 2);
        assert_eq!(progress.ready, 5);
        assert!(progress.entries["%0"].ready);
    }

    #[test]
    fn retry_does_not_requeue_local_agent_commands_or_web_hosts() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.built = true;
        progress.track("%agent", &serde_json::json!({"was_agent": "claude"}));
        progress.track("%web", &serde_json::json!({"web_url": "https://example.invalid"}));
        progress.track("%remote", &serde_json::json!({"remote_base": "http://restore.invalid"}));
        assert_eq!(progress.retry_pending(Instant::now()), ["%remote"]);
        assert_eq!(progress.entries.len(), 3);
    }

    #[test]
    fn retry_does_not_rebuild_an_invalid_snapshot_or_clear_its_error() {
        let mut progress = RestoreProgress::new(serde_json::json!({"invalid": true}));
        progress.failure = Some("invalid snapshot".into());
        let started = progress.started;
        assert!(progress.retry_pending(Instant::now()).is_empty());
        assert!(!progress.built);
        assert_eq!(progress.started, started);
        assert_eq!(progress.failure.as_deref(), Some("invalid snapshot"));
    }

    #[test]
    fn retry_saved_rooms_tabs_and_undocked_mirrors_keeps_their_exact_identities() {
        let record = |id: &str, source: &str| serde_json::json!({
            "pane_id": id, "remote_base": "http://restore.invalid",
            "remote_pane": source, "remote_view": true, "was_agent": "claude"
        });
        let records: Vec<_> = (1..=7).map(|n| record(&format!("%{n}"), &format!("%{}", n + 20))).collect();
        let mut first = records[0].clone();
        first["tabs"] = serde_json::json!([records[1], records[2]]);
        let mut second = records[3].clone();
        second["tabs"] = serde_json::json!([records[4]]);
        let state = serde_json::json!({"active_session": 0, "sessions": [{
            "windows": [{"leaf": first}, {"split": {"a": {"leaf": second}, "b": {"leaf": records[5]}}}],
            "undocked": [{"leaf": records[6]}], "active_window": 1
        }]});
        let mut progress = RestoreProgress::new(state.clone());
        for (index, record) in records.iter().enumerate() {
            let id = record["pane_id"].as_str().unwrap();
            progress.track(id, record);
            progress.entries.get_mut(id).unwrap().ready = index < 5;
        }
        progress.built = true;
        progress.ready = 5;
        let mut retry = progress.retry_pending(Instant::now());
        retry.sort();
        assert_eq!(retry, ["%6", "%7"], "retry targets existing local links, not source IDs");
        assert_eq!((progress.ready, progress.expected), (5, 7));
        assert_eq!(progress.state, state, "retry must not rewrite rooms, active tabs or source identities");
    }

    #[test]
    fn applied_live_readiness_survives_resize_but_not_a_different_generation() {
        let mut ws = Workspace::default();
        let update = |live_output, output_generation| kasa_bridge::screen::ScreenUpdate {
            pane_id: "%restore-live-test".into(), cols: 4, rows: 1,
            live_output, output_generation, ..Default::default()
        };
        App::apply_screen_update(&mut ws, update(false, 0));
        assert!(!ws.panes["%restore-live-test"].tabs[0].term().unwrap().live_output);
        App::apply_screen_update(&mut ws, update(true, 2));
        App::apply_screen_update(&mut ws, update(false, 0));
        let term = ws.panes["%restore-live-test"].tabs[0].term().unwrap();
        assert!(term.live_output);
        assert_eq!(term.output_generation, 2);
        assert_ne!(term.output_generation, 3, "a new connection cannot reuse the old applied frame");
        App::apply_screen_update(&mut ws, update(true, 3));
        assert_eq!(ws.panes["%restore-live-test"].tabs[0].term().unwrap().output_generation, 3);
    }
}
