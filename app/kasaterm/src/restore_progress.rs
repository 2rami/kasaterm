//! Nonblocking launch progress; only unfinished surfaces keep an input guard.
use super::*;
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub(crate) struct ProgressLayout {
    pub card: (f32, f32, f32, f32),
    pub retry: (f32, f32, f32, f32),
    pub continue_button: (f32, f32, f32, f32),
}

pub(crate) struct ToastLayout {
    pub card: (f32, f32, f32, f32),
    pub retry: (f32, f32, f32, f32),
}

/// Coordinates use the same units as the window. Reserve the actual bottom
/// chrome height, so the toast never covers the device selector/status bar.
pub(crate) fn toast_layout(width: f32, height: f32, bottom_reserved: f32) -> ToastLayout {
    let finite = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
    let width = finite(width);
    let height = finite(height);
    let available = (height - finite(bottom_reserved)).max(0.0);
    let margin_x = 12.0_f32.min(width / 4.0);
    let margin_y = 12.0_f32.min(available / 4.0);
    let w = 360.0_f32.min((width - 2.0 * margin_x).max(0.0));
    let h = 132.0_f32.min((available - 2.0 * margin_y).max(0.0));
    let x = width - margin_x - w;
    let y = available - margin_y - h;
    let pad = 12.0_f32.min(w / 4.0).min(h / 4.0);
    let bw = 88.0_f32.min((w - 2.0 * pad).max(0.0));
    let bh = 28.0_f32.min((h - 2.0 * pad).max(0.0));
    ToastLayout { card: (x, y, w, h), retry: (x + w - pad - bw, y + h - pad - bh, bw, bh) }
}

pub(crate) fn progress_layout(width: f32, height: f32) -> ProgressLayout {
    let w = (width - 24.0).max(1.0).min(440.0);
    let stacked = w < 340.0 && height >= 244.0;
    let h = (if stacked { 220.0_f32 } else { 180.0 }).min((height - 24.0).max(1.0));
    let x = (width - w) / 2.0;
    let y = (height - h) / 2.0;
    let pad = 16.0_f32.min(w / 8.0);
    let bh = 34.0_f32.min((h - 32.0).max(1.0) / 2.0);
    let action_width = (w - 2.0 * pad - 8.0).max(1.0);
    let continue_width = 170.0_f32.min(action_width * 0.62);
    let continue_button = if stacked {
        (x + pad, y + h - pad - bh, w - 2.0 * pad, bh)
    } else {
        (x + w - pad - continue_width, y + h - pad - bh, continue_width, bh)
    };
    let retry = if stacked {
        (x + pad, continue_button.1 - bh - 8.0, w - 2.0 * pad, bh)
    } else {
        (x + pad, continue_button.1, 104.0_f32.min(action_width - continue_width), bh)
    };
    ProgressLayout { card: (x, y, w, h), retry, continue_button }
}

pub(crate) struct RestoreProgress {
    pub state: serde_json::Value,
    pub expected: usize,
    pub ready: usize,
    pub failure: Option<String>,
    pub started: Instant,
    pub built: bool,
    pub background: bool,
    entries: HashMap<String, RestoreEntry>,
    entry_order: Vec<String>,
    cancelled_ids: HashSet<String>,
}

#[derive(Clone, Copy)]
enum RestoreStage { Pane, Agent, RemoteConnection, RemoteScreen, Web }

struct RestoreEntry {
    remote: bool,
    agent: bool,
    web: bool,
    ready: bool,
    // Agent startup and terminal input readiness are not the same thing. A
    // resumed CLI may be at login/error/permission UI or process detection may
    // lag; its live local PTY must remain usable while the toast is visible.
    local_input_ready: bool,
    character: Option<String>,
    remote_label: Option<String>,
    stage: RestoreStage,
    record: serde_json::Value,
}

impl RestoreEntry {
    fn update_local(&mut self, has_live_grid: bool, commands_pending: bool, agent_seen: bool) {
        self.local_input_ready = has_live_grid && !commands_pending;
        self.ready = self.local_input_ready && (!self.agent || agent_seen);
        self.stage = if has_live_grid && self.agent { RestoreStage::Agent } else { RestoreStage::Pane };
    }

    fn update_remote(&mut self, live_label: Option<&str>, connected: bool) {
        if let Some(label) = live_label.and_then(display_label) {
            self.remote_label = Some(label);
        }
        self.stage = if connected { RestoreStage::RemoteScreen } else { RestoreStage::RemoteConnection };
    }

    fn status_line(&self) -> String {
        match self.stage {
            RestoreStage::Pane => "pane을 복원하는 중…".into(),
            RestoreStage::Web => "웹 화면을 불러오는 중…".into(),
            RestoreStage::Agent => format!("{} 불러오는 중…", object_name(self.character.as_deref().unwrap_or("학생"))),
            RestoreStage::RemoteConnection => format!("{} 연결하는 중…", object_name(self.remote_label.as_deref().unwrap_or("원격 기기"))),
            RestoreStage::RemoteScreen => format!("{}의 화면을 불러오는 중…", self.remote_label.as_deref().unwrap_or("원격 기기")),
        }
    }
}

/// Labels can come from old connection records whose fallback was the base
/// URL. Never render an endpoint, credential, pane ID or opaque routing slug.
fn display_label(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 48
        || value.chars().any(|c| c.is_control() || ":/@\\?#%~=_".contains(c))
        || value.contains('.') || !value.chars().any(char::is_alphabetic)
    { return None; }
    Some(value.into())
}

fn character_label(value: &str) -> Option<String> {
    // Saved `character` is the display name, not agent_name/slug. Reject old
    // all-lowercase ASCII identifiers instead of presenting them as students.
    let label = display_label(value)?;
    if label.is_ascii() && !label.chars().any(char::is_uppercase) { return None; }
    Some(label)
}

fn object_name(name: &str) -> String {
    let final_consonant = name.chars().rev().find(|c| *c != ')' && *c != ' ')
        .is_some_and(|c| ('가'..='힣').contains(&c) && (c as u32 - '가' as u32) % 28 != 0);
    format!("{name}{}", if final_consonant { "을" } else { "를" })
}

fn surface_count(node: &serde_json::Value) -> usize {
    if crate::internal_room::is_saved_window(node) { return 0; }
    if let Some(leaf) = node.get("leaf").filter(|leaf| leaf.is_object()) {
        return 1 + leaf.get("tabs").and_then(|tabs| tabs.as_array()).map_or(0, Vec::len);
    }
    node.get("split").map_or(0, |split| surface_count(&split["a"]) + surface_count(&split["b"]))
}

impl RestoreProgress {
    fn blocks_all_input(&self) -> bool { !self.built }

    pub fn status_line(&self) -> String {
        if !self.built { return "pane을 복원하는 중…".into(); }
        self.entry_order.iter().filter_map(|id| self.entries.get(id))
            .find(|entry| !entry.ready).map_or_else(|| "pane을 복원하는 중…".into(), RestoreEntry::status_line)
    }

    /// Preserve execution identity while taking layout/presentation from the
    /// current workspace. A half-started shell is not a new saved session.
    pub(crate) fn preserve_record(&self, record: &mut serde_json::Value) {
        let id = record.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(entry) = self.entries.get(id).filter(|entry| !entry.ready) {
            let current = record.clone();
            *record = entry.record.clone();
            if let Some(obj) = record.as_object_mut() {
                // Tabs belong to the current layout, not the old outer leaf.
                obj.remove("tabs");
                obj.remove("active_tab");
                obj.remove("title");
                for key in ["pane_id", "surface_key", "title", "character", "tabs", "active_tab",
                    "remote_base", "remote_pane", "remote_surface_key", "remote_label", "remote_view",
                    "remote_owned", "remote_cwd", "remote_origin_cwd"] {
                    if let Some(value) = current.get(key) { obj.insert(key.into(), value.clone()); }
                }
                // An exact newly bound conversation is authoritative even if
                // foreground-process detection still has not caught up.
                if let Some(sid) = current.get("session_id").filter(|v| v.as_str().is_some_and(|s| !s.is_empty())) {
                    obj.insert("session_id".into(), sid.clone());
                    if let Some(agent) = current.get("was_agent").filter(|v| v.as_str().is_some_and(|s| !s.is_empty())) {
                        obj.insert("was_agent".into(), agent.clone());
                    }
                }
            }
        }
        if let Some(tabs) = record.get_mut("tabs").and_then(|v| v.as_array_mut()) {
            for tab in tabs { self.preserve_record(tab); }
        }
    }

    pub(crate) fn preserve_snapshot(&self, state: &mut serde_json::Value) {
        fn walk(progress: &RestoreProgress, node: &mut serde_json::Value, seen: &mut HashSet<String>) {
            if let Some(record) = node.get_mut("leaf") {
                progress.preserve_record(record);
                collect_record_ids(record, seen);
            } else if let Some(split) = node.get_mut("split") {
                walk(progress, &mut split["a"], seen);
                walk(progress, &mut split["b"], seen);
            }
        }
        if !self.built { return; }
        let mut seen = HashSet::new();
        if let Some(sessions) = state.get_mut("sessions").and_then(|v| v.as_array_mut()) {
            for session in sessions {
                for key in ["windows", "undocked"] {
                    for node in session.get_mut(key).and_then(|v| v.as_array_mut()).into_iter().flatten() {
                        walk(self, node, &mut seen);
                    }
                }
            }
        }
        for closed in state.get_mut("stashed_panes").and_then(|v| v.as_array_mut()).into_iter().flatten() {
            if let Some(record) = closed.get_mut("rec") {
                self.preserve_record(record);
                collect_record_ids(record, &mut seen);
            }
        }
        // A failed spawn may never have entered the live layout. Keep its
        // original record in an extra saved window, without creating anything
        // in the running app or rolling back the user's current rooms/tabs.
        let active = state["active_session"].as_u64().unwrap_or(0) as usize;
        let Some(sessions) = state.get_mut("sessions").and_then(|v| v.as_array_mut()) else { return; };
        let index = active.min(sessions.len().saturating_sub(1));
        let Some(windows) = sessions.get_mut(index).and_then(|s| s.get_mut("windows")).and_then(|v| v.as_array_mut()) else { return; };
        for id in &self.entry_order {
            let Some(entry) = self.entries.get(id).filter(|entry| !entry.ready && !seen.contains(id)) else { continue; };
            let mut record = entry.record.clone();
            if let Some(obj) = record.as_object_mut() {
                // If the outer pane never spawned, restore_leaf never reached
                // its tabs. Keep those unattempted records too, but do not
                // resurrect tabs explicitly closed or already saved elsewhere.
                if let Some(tabs) = obj.get_mut("tabs").and_then(|v| v.as_array_mut()) {
                    tabs.retain(|tab| tab.get("pane_id").and_then(|v| v.as_str()).is_none_or(|id|
                        !seen.contains(id) && !self.entries.contains_key(id) && !self.cancelled_ids.contains(id)));
                }
                obj.remove("active_tab");
            }
            windows.push(serde_json::json!({"leaf": record}));
        }
    }

    fn forget_surface(&mut self, id: &str) {
        if self.entries.remove(id).is_some() {
            self.cancelled_ids.insert(id.into());
            self.entry_order.retain(|entry| entry != id);
            self.expected = self.expected.saturating_sub(1);
            self.ready = self.entries.values().filter(|entry| entry.ready).count();
        }
    }

    fn dismiss_modal(&mut self) {
        self.background = true;
    }

    fn blocks_surface(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|entry| {
            !entry.ready && (entry.remote || entry.web || !entry.local_input_ready)
        })
    }

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
        Self { state, expected, ready: 0, failure: None, started: Instant::now(), built: false, background: false, entries: HashMap::new(), entry_order: Vec::new(), cancelled_ids: HashSet::new() }
    }

    pub fn track(&mut self, id: &str, record: &serde_json::Value) {
        if !self.entries.contains_key(id) { self.entry_order.push(id.to_string()); }
        let remote = record.get("remote_base").and_then(|v| v.as_str()).is_some();
        let web = record.get("web_url").is_some();
        let mut saved_record = record.clone();
        if let Some(obj) = saved_record.as_object_mut() { obj.insert("pane_id".into(), serde_json::json!(id)); }
        self.entries.insert(id.to_string(), RestoreEntry {
            remote,
            agent: record.get("was_agent").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
            web,
            ready: false,
            local_input_ready: false,
            character: record.get("character").and_then(|v| v.as_str()).and_then(character_label),
            remote_label: record.get("remote_label").and_then(|v| v.as_str()).and_then(display_label),
            stage: if web { RestoreStage::Web } else if remote { RestoreStage::RemoteConnection } else { RestoreStage::Pane },
            record: saved_record,
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
        self.entry_order.iter().filter(|id| self.entries.get(*id).is_some_and(|entry| entry.remote && !entry.ready))
            .cloned().collect()
    }
}

fn collect_record_ids(record: &serde_json::Value, ids: &mut HashSet<String>) {
    if let Some(id) = record.get("pane_id").and_then(|v| v.as_str()) { ids.insert(id.into()); }
    for tab in record.get("tabs").and_then(|v| v.as_array()).into_iter().flatten() { collect_record_ids(tab, ids); }
}

type RestoreHost<'a> = (&'a HashMap<String, Arc<kasa_pty::PtySession>>, &'a Arc<Mutex<Workspace>>);

fn find_restore_host<'a>(mut hosts: impl Iterator<Item = RestoreHost<'a>>, id: &str) -> Option<RestoreHost<'a>> {
    hosts.find(|(pty, ws)| pty.contains_key(id) || ws.lock().unwrap().panes.contains_key(id))
}

impl App {
    pub(crate) fn cancel_restore_surface(&mut self, id: &str) {
        if let Some(progress) = self.restore_progress.as_mut() { progress.forget_surface(id); }
        self.chrome_dirty = true;
    }

    /// Explicit user close/hide only. Automatic PTY failure must retain its
    /// original restore record and must not be mistaken for a cancellation.
    pub(crate) fn cancel_restore_pane(&mut self, id: &str) {
        if self.restore_progress.is_none() { return; }
        let mut ids = vec![id.to_string()];
        for ws in std::iter::once(&self.ws).chain(self.sessions.iter().flatten().map(|s| &s.ws)) {
            if let Some(pane) = ws.lock().unwrap().panes.get(id) {
                ids.extend(pane.tabs.iter().filter_map(|tab| tab.pid.clone()));
            }
        }
        for id in ids { self.cancel_restore_surface(&id); }
    }

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
                if std::env::var_os("KASATERM_AUTORESTORE_HOLD_FAILURE").is_some() { return; }
                eprintln!("[restore-probe] failure=visible input-blocked={} ready={}", self.restoration_blocks_input(), self.restore_progress.as_ref().map_or(0, |p| p.ready));
                PHASE.store(2, Ordering::Relaxed);
                if std::env::var_os("KASATERM_AUTORESTORE_BACKGROUND_PROBE").is_some() {
                    self.continue_restore_in_background();
                    for id in self.pty.keys() { self.send_bytes_to_surface(Some(id), b"BACKGROUND_READY_PROBE"); }
                    eprintln!("[restore-probe] background=true blocked={} preserved={}", self.restoration_blocks_input(), preserved(self));
                }
                self.retry_restore();
                eprintln!("[restore-probe] retry=queued preserved={} ready={} expected={}", preserved(self),
                    self.restore_progress.as_ref().map_or(0, |p| p.ready),
                    self.restore_progress.as_ref().map_or(0, |p| p.expected));
            }
            1 | 2 if self.restore_applying.is_none() && self.restore_progress.is_none() => {
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
        self.restore_applying.is_some() || self.restore_progress.as_ref().is_some_and(RestoreProgress::blocks_all_input)
    }

    pub(crate) fn restoration_blocks_surface(&self, id: &str) -> bool {
        self.restore_progress.as_ref().is_some_and(|p| p.blocks_surface(id))
    }

    pub(crate) fn continue_restore_in_background(&mut self) {
        let Some(progress) = self.restore_progress.as_mut() else { return; };
        // Legacy recovery probe compatibility. The toast is already nonmodal;
        // unfinished surfaces remain protected and every PTY stays intact.
        progress.dismiss_modal();
        self.restore_retry_rect = None;
        self.chrome_dirty = true;
        if let Some(window) = &self.window { window.request_redraw(); }
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
        let old_status = progress.status_line();
        let mut ready = 0;
        let mut failure = None;
        for id in &progress.entry_order {
            let Some(entry) = progress.entries.get_mut(id) else { continue; };
            entry.ready = false;
            entry.local_input_ready = false;
            // Switching sessions parks the whole workspace. Pending restore
            // entries still belong to that workspace, not the new active one.
            let host = find_restore_host(std::iter::once((&self.pty, &self.ws))
                .chain(self.sessions.iter().flatten().map(|s| (&s.pty, &s.ws))), id);
            let Some((pty, ws)) = host else {
                failure.get_or_insert_with(|| format!("{id} 창을 되살리지 못했어요"));
                continue;
            };
            let ws = ws.lock().unwrap();
            if entry.web {
                if ws.panes.contains_key(id) && self.pending_web_hosts.is_empty() {
                    entry.ready = true;
                    ready += 1;
                }
                continue;
            }
            let Some(session) = pty.get(id) else {
                failure.get_or_insert_with(|| format!("{id} 창을 되살리지 못했어요"));
                continue;
            };
            let term = ws.panes.values().flat_map(|pane| &pane.tabs)
                .find(|tab| tab.pid.as_deref() == Some(id.as_str()))
                .and_then(|tab| tab.term());
            let has_grid = term.is_some_and(|term| term.live_output && !term.cells.is_empty());
            if entry.remote {
                let readiness = kasa_mcp::remote::connection_readiness(id);
                let info = kasa_mcp::remote::remote_info(id);
                entry.update_remote(info.as_ref().map(|info| info.label.as_str()), readiness.as_ref().is_some_and(|(connected, _, _)| *connected));
                match readiness {
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
                entry.update_local(has_grid, commands_pending, session.active_agent().is_some());
                if entry.ready {
                    ready += 1;
                }
            }
        }
        if progress.entries.len() != progress.expected {
            failure.get_or_insert_with(|| "저장된 창·탭 일부를 되살리지 못했어요".to_string());
        }
        if ready < progress.expected && progress.started.elapsed() >= Duration::from_secs(45) {
            failure.get_or_insert_with(|| "아직 준비되지 않은 창이 있어요. 연결을 확인하고 다시 시도해 주세요".to_string());
        }
        let complete = ready == progress.expected && failure.is_none();
        let changed = progress.ready != ready || progress.failure != failure || progress.status_line() != old_status;
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
    fn pending_readiness_uses_parked_workspace_after_switching_sessions() {
        let active = Arc::new(Mutex::new(Workspace::default()));
        let parked = Arc::new(Mutex::new(Workspace::default()));
        App::apply_screen_update(&mut parked.lock().unwrap(), kasa_bridge::screen::ScreenUpdate {
            pane_id:"%parked".into(), cols:4, rows:1, live_output:true, output_generation:7,
            ..Default::default()
        });
        let pty = HashMap::new();
        let (_, found) = find_restore_host([(&pty, &active), (&pty, &parked)].into_iter(), "%parked").unwrap();
        assert!(Arc::ptr_eq(found, &parked));
        let ws = found.lock().unwrap();
        let term = ws.panes["%parked"].tabs[0].term().unwrap();
        assert!(term.live_output);
        assert_eq!(term.output_generation, 7);
        drop(ws);
        assert!(find_restore_host([(&pty, &active), (&pty, &parked)].into_iter(), "%missing").is_none());
    }

    #[test]
    fn closing_one_surface_keeps_sibling_restore_and_does_not_preserve_closed_tab() {
        let mut progress = RestoreProgress::new(serde_json::json!({"sessions":[{"windows":[{"leaf":{"tabs":[{}]}}]}]}));
        progress.track("%outer", &serde_json::json!({"pane_id":"%outer", "was_agent":"claude", "tabs":[{"pane_id":"%tab"}]}));
        progress.track("%tab", &serde_json::json!({"pane_id":"%tab", "remote_base":"http://restore.invalid"}));
        progress.built = true;
        progress.forget_surface("%outer");
        assert_eq!(progress.expected, 1);
        assert_eq!(progress.entry_order, ["%tab"]);
        assert!(progress.blocks_surface("%tab"));
        assert_eq!(progress.retry_pending(Instant::now()), ["%tab"]);
        let mut current = serde_json::json!({"sessions":[{"windows":[{"leaf":{"pane_id":"%tab"}}]}]});
        progress.preserve_snapshot(&mut current);
        assert_eq!(current["sessions"][0]["windows"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn saving_during_restore_keeps_current_layout_and_original_pending_session() {
        let original = serde_json::json!({"pane_id":"%1", "was_agent":"codex", "session_id":"old-conversation",
            "cwd":"/original", "model":"gpt-model", "effort":"high", "bypass":true, "character":"아즈사",
            "tabs":[{"pane_id":"%2", "was_agent":"claude", "session_id":"tab-conversation"}], "active_tab":0});
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%1", &original);
        progress.track("%2", &original["tabs"][0]);
        progress.built = true;
        let mut current = serde_json::json!({"active_session":0, "last_used_unix":123,
            "sessions":[{"active_window":1, "windows":[{"split":{"dir":"h", "ratio":0.73,
                "a":{"leaf":{"pane_id":"%1", "was_agent":null, "session_id":null, "cwd":"/shell",
                    "title":"새 이름", "surface_key":"live-key", "tabs":[{"pane_id":"%new-tab", "cwd":"/new"}], "active_tab":1}},
                "b":{"leaf":{"pane_id":"%new", "was_agent":"claude", "session_id":"new-work"}}
            }}, {"leaf":{"pane_id":"%2", "was_agent":null, "session_id":null}}],
                "undocked":[{"leaf":{"pane_id":"%undocked", "cwd":"/undocked"}, "frame":[1,2,300,400]}]}]});
        progress.preserve_snapshot(&mut current);
        let session = &current["sessions"][0];
        assert_eq!(session["active_window"], 1);
        assert_eq!(session["windows"].as_array().unwrap().len(), 2);
        assert_eq!(session["windows"][0]["split"]["ratio"], 0.73);
        let first = &session["windows"][0]["split"]["a"]["leaf"];
        assert_eq!(first["session_id"], "old-conversation");
        assert_eq!(first["was_agent"], "codex");
        assert_eq!(first["cwd"], "/original");
        assert_eq!(first["bypass"], true);
        assert_eq!(first["surface_key"], "live-key");
        assert_eq!(first["title"], "새 이름");
        assert_eq!(first["tabs"][0]["pane_id"], "%new-tab");
        assert_eq!(first["active_tab"], 1);
        assert_eq!(session["windows"][1]["leaf"]["session_id"], "tab-conversation");
        assert_eq!(session["windows"][0]["split"]["b"]["leaf"]["session_id"], "new-work");
        assert_eq!(session["undocked"][0]["frame"], serde_json::json!([1,2,300,400]));
        assert_eq!(current["last_used_unix"], 123);
        let once = current.clone();
        progress.preserve_snapshot(&mut current);
        assert_eq!(current, once, "repeated autosaves must not append duplicate recovery panes");
    }

    #[test]
    fn saving_missing_spawn_keeps_unattempted_tabs_without_replacing_new_work() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%7", &serde_json::json!({"pane_id":"%old", "remote_base":"http://original.invalid",
            "remote_pane":"%source", "remote_surface_key":"original-source-key",
            "tabs":[{"pane_id":"%8", "session_id":"unattempted-conversation", "was_agent":"claude"}]}));
        progress.built = true;
        let new_work = serde_json::json!({"leaf":{"pane_id":"%new", "session_id":"new-conversation"}});
        let mut current = serde_json::json!({"active_session":1, "sessions":[
            {"windows":[{"leaf":{"pane_id":"%other-session"}}]}, {"windows":[new_work.clone()], "active_window":0}]});
        progress.preserve_snapshot(&mut current);
        assert_eq!(current["sessions"][1]["windows"][0], new_work);
        assert_eq!(current["sessions"][1]["windows"][1]["leaf"]["pane_id"], "%7");
        assert_eq!(current["sessions"][1]["windows"][1]["leaf"]["remote_surface_key"], "original-source-key");
        assert_eq!(current["sessions"][1]["windows"][1]["leaf"]["tabs"][0]["session_id"], "unattempted-conversation");
        assert_eq!(current["sessions"][0]["windows"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn current_remote_identity_and_new_binding_override_stale_restore_metadata() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%1", &serde_json::json!({"pane_id":"%old", "was_agent":"claude", "session_id":"old",
            "remote_base":"http://old.invalid", "remote_pane":"%old-source", "remote_surface_key":"old-key"}));
        let mut record = serde_json::json!({"pane_id":"%1", "was_agent":"codex", "session_id":"new",
            "remote_base":"http://new.invalid", "remote_pane":"%new-source", "remote_surface_key":"new-key"});
        let current = record.clone();
        progress.preserve_record(&mut record);
        assert_eq!(record, current);
        progress.entries.get_mut("%1").unwrap().ready = true;
        let mut ready_record = serde_json::json!({"pane_id":"%1", "cwd":"/new-work", "was_agent":null});
        let ready = ready_record.clone();
        progress.preserve_record(&mut ready_record);
        assert_eq!(ready_record, ready, "completed panes serialize only current truth");
    }

    #[test]
    fn explicit_close_cancels_progress_but_hidden_record_keeps_pending_resume() {
        let mut progress = RestoreProgress::new(serde_json::json!({"sessions":[{"windows":[{"leaf":{}}]}]}));
        progress.track("%1", &serde_json::json!({"pane_id":"%1", "was_agent":"claude", "session_id":"resume-me"}));
        progress.built = true;
        let mut closed = serde_json::json!({"pane_id":"%1", "was_agent":null, "session_id":null});
        progress.preserve_record(&mut closed);
        progress.forget_surface("%1");
        progress.forget_surface("%1");
        assert_eq!((progress.expected, progress.ready), (0, 0));
        assert!(progress.entry_order.is_empty());
        assert!(progress.retry_pending(Instant::now()).is_empty());
        assert!(!progress.blocks_surface("%1"));
        let mut state = serde_json::json!({"sessions":[{"windows":[{"leaf":{"pane_id":"%new"}}]}],
            "stashed_panes":[{"rec":closed}]});
        progress.preserve_snapshot(&mut state);
        assert_eq!(state["sessions"][0]["windows"].as_array().unwrap().len(), 1, "explicitly closed panes never resurrect as recovery windows");
        assert_eq!(state["stashed_panes"][0]["rec"]["session_id"], "resume-me");
    }

    #[test]
    fn unbuilt_restore_does_not_overlay_or_enable_saving() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%1", &serde_json::json!({"pane_id":"%1", "session_id":"old"}));
        let mut state = serde_json::json!({"sessions":[{"windows":[{"leaf":{"pane_id":"%new"}}]}]});
        let original = state.clone();
        progress.preserve_snapshot(&mut state);
        assert_eq!(state, original);
        assert!(progress.blocks_all_input());
    }

    #[test]
    fn toast_reserves_bottom_chrome_and_clamps_every_rectangle() {
        for (width, height, bottom) in [
            (1280.0, 720.0, 40.0), (240.0, 320.0, 48.0),
            (320.0, 160.0, 44.0), (30.0, 30.0, 20.0),
            (1.0, 1.0, 0.0), (0.0, 0.0, 0.0), (100.0, 20.0, 100.0),
        ] {
            let layout = toast_layout(width, height, bottom);
            let (x, y, w, h) = layout.card;
            assert!(x >= 0.0 && y >= 0.0 && w >= 0.0 && h >= 0.0);
            assert!(x + w <= width && y + h <= (height - bottom).max(0.0));
            assert!(w <= 360.0 && h <= 132.0);
            let (bx, by, bw, bh) = layout.retry;
            assert!(bw >= 0.0 && bh >= 0.0);
            assert!(bx >= x && by >= y && bx + bw <= x + w && by + bh <= y + h);
        }
        let layout = toast_layout(1280.0, 720.0, 40.0);
        assert_eq!(layout.card, (908.0, 536.0, 360.0, 132.0));
        assert_eq!(layout.retry, (1168.0, 628.0, 88.0, 28.0));
        for rect in [toast_layout(f32::NAN, f32::INFINITY, -10.0).card,
            toast_layout(-1.0, -1.0, f32::NAN).retry] {
            assert!([rect.0, rect.1, rect.2, rect.3].iter().all(|n| n.is_finite() && *n >= 0.0));
        }
    }

    #[test]
    fn built_restore_never_globally_blocks_even_with_pending_mirrors() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%local", &serde_json::json!({"was_agent": "claude", "character": "아즈사"}));
        progress.track("%remote", &serde_json::json!({"remote_base": "http://restore.invalid"}));
        assert!(progress.blocks_all_input(), "batch allocation still protects workspace state");
        progress.dismiss_modal();
        assert!(progress.blocks_all_input(), "legacy hide flag cannot bypass batch allocation");
        progress.background = false;
        progress.built = true;
        progress.entries.get_mut("%local").unwrap().update_local(true, false, false);
        assert!(!progress.blocks_all_input(), "a visible toast is never a modal input gate");
        assert!(!progress.blocks_surface("%local"));
        assert!(!progress.entries["%local"].ready, "process readiness criterion is unchanged");
        assert!(progress.blocks_surface("%remote"));
        assert!(!progress.blocks_surface("%new"), "new work is not trapped in the restore queue");
    }

    #[test]
    fn status_tracks_real_stages_and_stable_saved_order() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        let agent = serde_json::json!({"was_agent": "claude", "character": "아즈사"});
        progress.track("%20", &agent);
        progress.track("%3", &serde_json::json!({"remote_base": "http://restore.invalid", "remote_label": "나쵸네코(맥미니)"}));
        progress.track("%20", &agent);
        progress.track("%web", &serde_json::json!({"web_url": "https://private.invalid"}));
        assert_eq!(progress.entry_order, ["%20", "%3", "%web"]);
        assert_eq!(progress.status_line(), "pane을 복원하는 중…");
        progress.built = true;
        assert_eq!(progress.status_line(), "pane을 복원하는 중…");
        progress.entries.get_mut("%20").unwrap().update_local(true, true, false);
        assert_eq!(progress.status_line(), "아즈사를 불러오는 중…");
        progress.entries.get_mut("%20").unwrap().update_local(true, false, true);
        assert_eq!(progress.status_line(), "나쵸네코(맥미니)를 연결하는 중…");
        progress.entries.get_mut("%3").unwrap().update_remote(Some("나쵸네코"), true);
        assert_eq!(progress.status_line(), "나쵸네코의 화면을 불러오는 중…");
        assert!(!progress.entries["%3"].ready, "connection alone cannot fake an applied live grid");
        progress.entries.get_mut("%3").unwrap().update_remote(Some("나쵸네코"), false);
        assert_eq!(progress.status_line(), "나쵸네코를 연결하는 중…");
        progress.entries.get_mut("%3").unwrap().ready = true;
        assert_eq!(progress.status_line(), "웹 화면을 불러오는 중…");
        assert_eq!(object_name("아리스"), "아리스를");
    }

    #[test]
    fn status_never_uses_endpoints_credentials_slugs_or_raw_errors() {
        for label in ["http://user:secret@127.0.0.1:8765", "127.0.0.1:8765", "host.local", "~machine-id", "%9", "x\nsecret", "bearer_token=secret"] {
            let mut progress = RestoreProgress::new(serde_json::json!({}));
            progress.track("%private-id", &serde_json::json!({"remote_base": "http://secret.invalid", "remote_label": label}));
            progress.built = true;
            progress.failure = Some("private error http://user:secret@host:8765".into());
            assert_eq!(progress.status_line(), "원격 기기를 연결하는 중…");
        }
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%1", &serde_json::json!({"remote_base": "http://secret.invalid", "remote_label": "작업 기기"}));
        progress.built = true;
        progress.entries.get_mut("%1").unwrap().update_remote(Some("http://secret.invalid"), false);
        assert_eq!(progress.status_line(), "작업 기기를 연결하는 중…", "unsafe live fallback must not overwrite a saved display label");
        progress.track("%agent", &serde_json::json!({"was_agent": "claude", "character": "azusa-p1-xyz"}));
        progress.entries.get_mut("%1").unwrap().ready = true;
        progress.entries.get_mut("%agent").unwrap().update_local(true, false, false);
        assert_eq!(progress.status_line(), "학생을 불러오는 중…");
    }

    #[test]
    fn retry_order_is_stable_and_skips_completed_targets() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        for id in ["%20", "%3", "%11"] {
            progress.track(id, &serde_json::json!({"remote_base": "http://restore.invalid"}));
        }
        progress.built = true;
        progress.entries.get_mut("%3").unwrap().ready = true;
        for _ in 0..4 { assert_eq!(progress.retry_pending(Instant::now()), ["%20", "%11"]); }
        assert_eq!(progress.entry_order, ["%20", "%3", "%11"]);
    }

    #[test]
    fn background_restore_accepts_local_input_without_claiming_agent_ready() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%local", &serde_json::json!({"was_agent": "claude"}));
        progress.built = true;
        progress.dismiss_modal();
        progress.entries.get_mut("%local").unwrap().update_local(true, false, false);
        assert!(!progress.blocks_surface("%local"), "live local terminal must accept recovery/login input");
        assert!(!progress.entries["%local"].ready, "shell output alone is not successful agent restoration");
        assert!(progress.retry_pending(Instant::now()).is_empty(), "retry must not relaunch its agent");
        assert!(!progress.blocks_surface("%local"));
        progress.entries.get_mut("%local").unwrap().update_local(true, false, true);
        assert!(progress.entries["%local"].ready, "late process detection can still complete restore");
    }

    #[test]
    fn local_input_remains_blocked_until_live_output_and_queued_commands_finish() {
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track("%local", &serde_json::json!({"was_agent": "claude"}));
        progress.track("%remote", &serde_json::json!({"remote_base": "http://restore.invalid"}));
        progress.dismiss_modal();
        for (live, pending) in [(false, false), (false, true), (true, true)] {
            progress.entries.get_mut("%local").unwrap().update_local(live, pending, true);
            assert!(progress.blocks_surface("%local"));
        }
        progress.entries.get_mut("%local").unwrap().update_local(true, false, false);
        assert!(!progress.blocks_surface("%local"));
        assert!(progress.blocks_surface("%remote"), "unconnected mirrors must not leak input");
    }

    #[cfg(unix)]
    #[test]
    fn isolated_local_pty_accepts_recovery_input_while_agent_restore_is_pending() {
        // /bin/cat is our isolated local process, not a user's shell/account.
        // It models an agent that printed a login/error screen but is not yet
        // detectable as Claude/Codex. Restored scrollback alone cannot unlock it.
        let id = format!("restore-input-test-{}", std::process::id());
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: Some("/bin/cat".into()), cwd: Some("/tmp".into()),
            cols: 80, rows: 8, env: Vec::new(), pane_id: id.clone(),
            initial_scrollback: Vec::new(),
        }).unwrap();
        session.send_bytes(b"LOCAL_RESTORE_SCREEN\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "isolated PTY produced no live frame");
            if session.screens.recv_timeout(Duration::from_millis(100))
                .is_ok_and(|frame| frame.live_output) { break; }
        }
        let mut progress = RestoreProgress::new(serde_json::json!({}));
        progress.track(&id, &serde_json::json!({"was_agent": "claude"}));
        progress.dismiss_modal();
        progress.entries.get_mut(&id).unwrap().update_local(true, false, false);
        assert!(!progress.entries[&id].ready);
        assert!(!progress.blocks_surface(&id));
        if !progress.blocks_surface(&id) {
            session.send_bytes(b"RECOVERY_INPUT_ACCEPTED\n").unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.visible_text(8).contains("RECOVERY_INPUT_ACCEPTED") {
            assert!(Instant::now() < deadline, "local recovery input was swallowed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn progress_actions_stay_inside_small_and_large_windows_without_overlap() {
        for (width, height) in [(240.0, 320.0), (320.0, 200.0), (480.0, 260.0), (1280.0, 720.0)] {
            let layout = progress_layout(width, height);
            let (x, y, w, h) = layout.card;
            assert!(x >= 0.0 && y >= 0.0 && x + w <= width && y + h <= height);
            for (bx, by, bw, bh) in [layout.retry, layout.continue_button] {
                assert!(bx >= x && by >= y && bx + bw <= x + w && by + bh <= y + h);
                assert!(by >= y + 104.0, "actions must not overlap the progress bar");
            }
            let a = layout.retry;
            let b = layout.continue_button;
            assert!(a.0 + a.2 <= b.0 || a.1 + a.3 <= b.1);
        }
    }

    #[test]
    fn dismissing_progress_keeps_restore_state_and_only_unlocks_ready_surfaces() {
        let mut progress = RestoreProgress::new(serde_json::json!({"sessions": [{"windows": [{"leaf": {
            "pane_id": "%0", "tabs": [{}, {}, {}, {}, {}, {}]
        }}]}]}));
        for number in 0..7 {
            let id = format!("%{number}");
            progress.track(&id, &serde_json::json!({"remote_base": "http://restore.invalid"}));
            progress.entries.get_mut(&id).unwrap().ready = number < 5;
        }
        progress.ready = 5;
        progress.built = true;
        let original = progress.state.clone();
        progress.dismiss_modal();
        assert!(progress.background);
        assert_eq!((progress.ready, progress.expected, progress.entries.len()), (5, 7, 7));
        assert_eq!(progress.state, original);
        assert!(!progress.blocks_surface("%0"));
        assert!(progress.blocks_surface("%5"));
        assert!(!progress.blocks_surface("%new"));
        progress.entries.get_mut("%5").unwrap().ready = true;
        assert!(!progress.blocks_surface("%5"));
    }
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
