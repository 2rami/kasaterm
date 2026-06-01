//! Headless daemon process: owns every PtySession + the full session>window>
//! pane structure, serves the JSON-RPC control socket and the bincode stream
//! socket, and outlives the GUI so shells / claude / vim keep running across
//! GUI restarts.
//!
//! The daemon is the authority for layout AND for sessions/windows: split/
//! close/focus/new/switch mutate the tree here and broadcast a
//! `StreamMsg::State` (the whole structure) to the attached GUI. Per-pane
//! screen frames are multiplexed onto the same stream — every pane's screen
//! channel is forwarded into one shared mpsc the pump drains, so a single
//! writer owns the stream socket. pane_id is allocated here (single source of
//! truth) so the GUI never collides.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_socket::backend::{
    Backend, PaneActivity, PaneRect, SessionsInfo, SplitDirection, SurfaceInfo, WorkspaceInfo,
};
use agent_socket::transport::{LocalListener, LocalStream};
use agent_socket::Server;
use anyhow::{anyhow, Result};
use pty_backend::{PtyLayout, PtyOptions, PtySession, ScreenReceiver, SplitDir};
use serde_json::{json, Value};
use tmux_bridge::ScreenUpdate;

use crate::socket::{key_to_bytes, pane_record, pid_cwd};
use crate::stream::{
    stream_path, write_msg, PanePreview, SessionView, StateView, StreamMsg, WindowView,
};

const WS_ID: &str = "local-0";
const PANE_0: &str = "%0";

/// Per-pane restore metadata read back from the daemon state file.
struct PaneMeta {
    cwd: Option<String>,
    was_claude: bool,
    session_id: Option<String>,
}

/// One session the daemon hosts: its windows (each a BSP tree) + which is
/// active. All panes across all sessions live in the one global `pty` map.
struct DaemonSession {
    windows: Vec<PtyLayout>,
    active_window: usize,
}

struct DaemonState {
    pty: Mutex<HashMap<String, Arc<PtySession>>>,
    sessions: Mutex<Vec<DaemonSession>>,
    active_session: Mutex<usize>,
    next_id: Mutex<u32>,
    /// Active pane — split targets it, send_*/scroll default to it.
    active: Mutex<String>,
    /// The currently-attached GUI's frame sink (single attach for now; a Vec
    /// would make this multi-attach).
    subscriber: Mutex<Option<LocalStream>>,
    /// Every pane forwarder + every state broadcast feeds this; the pump
    /// drains it and writes to the subscriber. One writer for the socket.
    out_tx: Sender<StreamMsg>,
    /// Where the structure is persisted. Held here so any mutation can save
    /// immediately (real-time) instead of waiting on the 10s backup timer —
    /// the layout the user left is always on disk for the next launch.
    state_path: PathBuf,
    /// Collab board: transcript-derived activity (collab_auto), filled by the
    /// tail watcher. `binds` queues each pane's transcript path for the
    /// watcher. The in-process PtyBackend used to host this, but it's dead
    /// under the daemon — so the board lives here now.
    collab_auto: Arc<Mutex<HashMap<String, PaneActivity>>>,
    binds: Arc<Mutex<Vec<(String, PathBuf)>>>,
    /// Non-terminal panes (image / markdown previews), keyed by pane id. These
    /// have no PtySession — they live only as a layout leaf + this metadata,
    /// which `state_view` ships to the GUI to decode/render. See `open_preview`.
    previews: Mutex<HashMap<String, PanePreview>>,
}

impl DaemonState {
    /// Resolve a target pane: explicit id, else the active pane, else any.
    fn pane(&self, surface_id: Option<&str>) -> Option<Arc<PtySession>> {
        let map = self.pty.lock().unwrap();
        match surface_id {
            Some(id) => map.get(id).cloned(),
            None => {
                let active = self.active.lock().unwrap().clone();
                map.get(&active)
                    .cloned()
                    .or_else(|| map.values().next().cloned())
            }
        }
    }

    fn alloc_id(&self) -> String {
        let mut n = self.next_id.lock().unwrap();
        *n += 1;
        format!("%{}", *n)
    }

    /// Mutate the active session's active-window layout, if any.
    fn with_active_layout<R>(&self, f: impl FnOnce(&mut PtyLayout) -> R) -> Option<R> {
        let mut sessions = self.sessions.lock().unwrap();
        let asx = *self.active_session.lock().unwrap();
        let sess = sessions.get_mut(asx)?;
        let aw = sess.active_window;
        sess.windows.get_mut(aw).map(f)
    }

    /// Leaf pane ids in the active session's active window.
    fn active_window_leaves(&self) -> Vec<String> {
        let sessions = self.sessions.lock().unwrap();
        let asx = *self.active_session.lock().unwrap();
        sessions
            .get(asx)
            .and_then(|s| s.windows.get(s.active_window))
            .map(|w| w.leaves().iter().map(|l| l.to_string()).collect())
            .unwrap_or_default()
    }

    /// Point `active` at the active window's first leaf (after switch/close).
    fn update_active_pane(&self) {
        if let Some(first) = self.active_window_leaves().into_iter().next() {
            *self.active.lock().unwrap() = first;
        }
    }

    /// Remove a pane from whichever window holds it, dropping a window that
    /// becomes empty and a session whose windows all went away. Fixes up the
    /// active indices. Returns true when no sessions remain.
    fn remove_pane(&self, pane_id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        for s in sessions.iter_mut() {
            s.windows.retain_mut(|w| {
                let lv = w.leaves();
                let only = lv.len() == 1 && lv[0] == pane_id;
                let has = lv.iter().any(|&l| l == pane_id);
                if only {
                    false // window held just this pane → drop the window
                } else {
                    if has {
                        w.remove_leaf(pane_id);
                    }
                    true
                }
            });
            if s.active_window >= s.windows.len() && !s.windows.is_empty() {
                s.active_window = s.windows.len() - 1;
            }
        }
        sessions.retain(|s| !s.windows.is_empty());
        let mut asx = self.active_session.lock().unwrap();
        if *asx >= sessions.len() && !sessions.is_empty() {
            *asx = sessions.len() - 1;
        }
        sessions.is_empty()
    }

    /// Push a full snapshot of every pane in the active window — used after a
    /// switch so the GUI repaints the now-visible window.
    fn push_active_snapshots(&self) {
        let leaves = self.active_window_leaves();
        let pty = self.pty.lock().unwrap();
        for id in leaves {
            if let Some(s) = pty.get(&id) {
                let _ = self.out_tx.send(StreamMsg::Frame(s.full_snapshot()));
            }
        }
    }

    fn state_view(&self) -> StateView {
        let sessions = self.sessions.lock().unwrap();
        let pty = self.pty.lock().unwrap();
        let views: Vec<SessionView> = sessions
            .iter()
            .map(|s| {
                // Label = active window's first pane's cwd basename.
                let label = s
                    .windows
                    .get(s.active_window)
                    .and_then(|w| w.leaves().first().map(|l| l.to_string()))
                    .and_then(|id| pty.get(&id).and_then(|p| p.shell_pid()).and_then(pid_cwd))
                    .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                SessionView {
                    windows: s
                        .windows
                        .iter()
                        .map(|w| WindowView { layout: w.clone() })
                        .collect(),
                    active_window: s.active_window,
                    label,
                }
            })
            .collect();
        // Per-pane cwd for the GUI breadcrumb — resolved here because the
        // daemon owns the PtySessions. lsof-per-pane, but state_view only
        // fires on structural changes + the ~1s cwd poll, never per frame.
        let pane_cwds = pty
            .iter()
            .filter_map(|(id, s)| {
                s.shell_pid()
                    .and_then(pid_cwd)
                    .map(|p| (id.clone(), p.to_string_lossy().into_owned()))
            })
            .collect();
        StateView {
            sessions: views,
            active_session: *self.active_session.lock().unwrap(),
            active_pane: Some(self.active.lock().unwrap().clone()),
            pane_cwds,
            pane_previews: self.previews.lock().unwrap().clone(),
        }
    }

    fn broadcast_state(&self) {
        let _ = self.out_tx.send(StreamMsg::State(self.state_view()));
        // Every structural mutation funnels through here, so persisting in the
        // same breath keeps the on-disk session in lock-step with the live
        // layout — splits/closes survive even a hard kill, no 10s window.
        self.persist();
    }

    /// Snapshot the structure to disk now. Cheap (the file is ~1 KB) and only
    /// fires on structural changes, so it never thrashes.
    fn persist(&self) {
        save_daemon_state(self, &self.state_path);
    }

    /// cwd of the active pane (for inheriting into a split/new window/session).
    fn active_cwd(&self) -> Option<String> {
        let active = self.active.lock().unwrap().clone();
        self.pty
            .lock()
            .unwrap()
            .get(&active)
            .and_then(|s| s.shell_pid())
            .and_then(pid_cwd)
            .map(|p| p.to_string_lossy().into_owned())
    }
}

/// Forward a pane's screen channel into the shared stream as Frame messages.
fn spawn_forwarder(screens: ScreenReceiver<ScreenUpdate>, out: Sender<StreamMsg>) {
    std::thread::spawn(move || {
        while let Ok(u) = screens.recv() {
            if out.send(StreamMsg::Frame(u)).is_err() {
                break;
            }
        }
    });
}

/// Spawn a fresh pane shell with an inherited cwd, register its forwarder, and
/// insert it into the pty map. Returns the new session.
fn spawn_pane(state: &DaemonState, pane_id: &str, cwd: Option<String>) -> Result<Arc<PtySession>> {
    let sess = Arc::new(PtySession::start(PtyOptions {
        shell: None,
        cwd,
        cols: 80,
        rows: 24,
        env: Vec::new(),
        pane_id: pane_id.to_string(),
        initial_scrollback: Vec::new(),
    })?);
    spawn_forwarder(sess.screens.clone(), state.out_tx.clone());
    state
        .pty
        .lock()
        .unwrap()
        .insert(pane_id.to_string(), sess.clone());
    Ok(sess)
}

struct DaemonBackend {
    state: Arc<DaemonState>,
}

impl Backend for DaemonBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo { id: WS_ID.into(), name: "kasaterm".into() }])
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(Some(WorkspaceInfo { id: WS_ID.into(), name: "kasaterm".into() }))
    }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(self
            .state
            .active_window_leaves()
            .into_iter()
            .map(|id| SurfaceInfo {
                id,
                workspace_id: WS_ID.into(),
                title: None,
            })
            .collect())
    }
    fn peek(&self, surface_id: &str, lines: usize) -> Result<String> {
        let sess = self
            .state
            .pane(Some(surface_id))
            .ok_or_else(|| anyhow::anyhow!("no such pane: {surface_id}"))?;
        Ok(sess.visible_text(lines))
    }
    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        // Keep the active tuple consistent: making a pane "active" must also
        // point active_session/active_window at the session+window that holds
        // it. Otherwise a later split runs `with_active_layout` (the active
        // session's window) but `split_leaf(active)` with a pane from another
        // session — the target leaf isn't there, the split is dropped, and the
        // freshly spawned pane is orphaned (in the pty map, in no layout). That
        // orphan is the "empty pane" a cross-session focus-then-split produced.
        let located = {
            let sessions = self.state.sessions.lock().unwrap();
            let mut hit = None;
            'scan: for (si, s) in sessions.iter().enumerate() {
                for (wi, w) in s.windows.iter().enumerate() {
                    if w.leaves().iter().any(|&l| l == surface_id) {
                        hit = Some((si, wi));
                        break 'scan;
                    }
                }
            }
            hit
        };
        if let Some((si, wi)) = located {
            *self.state.active_session.lock().unwrap() = si;
            if let Some(s) = self.state.sessions.lock().unwrap().get_mut(si) {
                s.active_window = wi;
            }
        }
        *self.state.active.lock().unwrap() = surface_id.to_string();
        Ok(())
    }
    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo> {
        let axis = match direction {
            SplitDirection::Left | SplitDirection::Right => SplitDir::Horizontal,
            SplitDirection::Up | SplitDirection::Down => SplitDir::Vertical,
        };
        let new_id = self.state.alloc_id();
        // Split the active window's *real* leaf. If `active` drifted to another
        // session's pane (belt-and-suspenders with the focus fix above), fall
        // back to this window's first leaf so the new pane is never orphaned.
        let active = {
            let leaves = self.state.active_window_leaves();
            let cur = self.state.active.lock().unwrap().clone();
            if leaves.iter().any(|l| *l == cur) {
                cur
            } else {
                leaves.into_iter().next().unwrap_or(cur)
            }
        };
        let cwd = self.state.active_cwd();
        spawn_pane(&self.state, &new_id, cwd)?;
        let attached = self
            .state
            .with_active_layout(|lay| lay.split_leaf(&active, axis, new_id.clone()));
        if attached.is_none() {
            // No active session/window to split into (e.g. the last pane was
            // closed, emptying `sessions`). Adopt the new pane as a fresh
            // session so a split can never orphan into an invisible pane.
            let mut sessions = self.state.sessions.lock().unwrap();
            sessions.push(DaemonSession {
                windows: vec![PtyLayout::single(new_id.clone())],
                active_window: 0,
            });
            *self.state.active_session.lock().unwrap() = sessions.len() - 1;
        }
        *self.state.active.lock().unwrap() = new_id.clone();
        self.state.broadcast_state();
        Ok(SurfaceInfo {
            id: new_id,
            workspace_id: WS_ID.into(),
            title: None,
        })
    }
    fn open_preview(&self, kind: &str, path: &str) -> Result<()> {
        // Image / markdown panes have no PTY: add a layout leaf + record the
        // kind+path, then broadcast. The GUI builds the PaneContent from the
        // path (it decodes locally — same as the in-process split_*_pane).
        // Split the active terminal, like split_image_pane does.
        let new_id = self.state.alloc_id();
        let active = {
            let leaves = self.state.active_window_leaves();
            let cur = self.state.active.lock().unwrap().clone();
            if leaves.iter().any(|l| *l == cur) {
                cur
            } else {
                leaves
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("no pane to open preview beside"))?
            }
        };
        let attached = self
            .state
            .with_active_layout(|lay| lay.split_leaf(&active, SplitDir::Horizontal, new_id.clone()));
        if attached != Some(true) {
            return Err(anyhow!("no active window to open preview in"));
        }
        self.state.previews.lock().unwrap().insert(
            new_id,
            PanePreview { kind: kind.to_string(), path: path.to_string() },
        );
        // Keep focus on the terminal that opened it — previews take no input.
        self.state.broadcast_state();
        Ok(())
    }
    fn close_surface(&self, surface_id: &str) -> Result<()> {
        self.state.pty.lock().unwrap().remove(surface_id); // drop = kill
        self.state.previews.lock().unwrap().remove(surface_id); // preview, if any
        let emptied = self.state.remove_pane(surface_id);
        if emptied {
            // That was the last pane of the last session. Rather than leave a
            // session-less daemon — where split/new have nothing to attach to
            // and silently orphan (the "split does nothing" brick) — spawn a
            // fresh shell as a new session so the window always has something
            // usable, like a normal terminal opening a new prompt.
            let new_id = self.state.alloc_id();
            let cwd = crate::resolve_initial_cwd();
            spawn_pane(&self.state, &new_id, cwd)?;
            let mut sessions = self.state.sessions.lock().unwrap();
            sessions.push(DaemonSession {
                windows: vec![PtyLayout::single(new_id.clone())],
                active_window: 0,
            });
            *self.state.active_session.lock().unwrap() = 0;
            *self.state.active.lock().unwrap() = new_id;
        } else {
            self.state.update_active_pane();
        }
        self.state.broadcast_state();
        Ok(())
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        let pane = self
            .state
            .pane(surface_id)
            .ok_or_else(|| anyhow!("no pane to send to"))?;
        // Body and trailing submit (\r) go in separate writes — each
        // send_bytes flushes on its own — so a multibyte tail isn't
        // truncated into a lone surrogate. See split_trailing_submit.
        let (body, submit) = crate::socket::split_trailing_submit(text.as_bytes());
        if !body.is_empty() {
            pane.send_bytes(body)?;
        }
        if !submit.is_empty() {
            pane.send_bytes(submit)?;
        }
        Ok(())
    }
    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()> {
        self.state
            .pane(surface_id)
            .ok_or_else(|| anyhow!("no pane to send to"))?
            .send_bytes(&key_to_bytes(key))
    }
    fn send_raw(&self, surface_id: Option<&str>, bytes: &[u8]) -> Result<()> {
        self.state
            .pane(surface_id)
            .ok_or_else(|| anyhow!("no pane to send to"))?
            .send_bytes(bytes)
    }
    fn resize_surface(&self, surface_id: &str, cols: u16, rows: u16) -> Result<()> {
        self.state
            .pane(Some(surface_id))
            .ok_or_else(|| anyhow!("no pane to resize"))?
            .resize(cols, rows)
    }
    fn scroll_surface(&self, surface_id: &str, lines: i32) -> Result<()> {
        self.state
            .pane(Some(surface_id))
            .ok_or_else(|| anyhow!("no pane to scroll"))?
            .scroll(lines);
        Ok(())
    }
    fn sessions(&self) -> SessionsInfo {
        // Reuse the same per-session labels the stream StateView already
        // derives (active-window first-pane cwd basename), so the session
        // panel shows live folder names. `saved` stays empty here — that's
        // the on-disk cold-session list, a separate (later) concern.
        let view = self.state.state_view();
        SessionsInfo {
            count: view.sessions.len(),
            active: view.active_session,
            saved: Vec::new(),
            labels: view.sessions.iter().map(|s| s.label.clone()).collect(),
        }
    }
    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        // Live = every pane the daemon owns, across sessions; drop board
        // entries for closed panes so no ghosts linger. Activity is
        // transcript-derived (collab_auto) — it reflects what each pane is
        // actually doing, so a pane never has to report itself.
        let live: HashSet<String> = self.state.pty.lock().unwrap().keys().cloned().collect();
        Ok(self
            .state
            .collab_auto
            .lock()
            .unwrap()
            .values()
            .filter(|a| live.contains(&a.surface_id))
            .cloned()
            .collect())
    }
    fn window_layout(&self) -> Result<Vec<PaneRect>> {
        // leaf_rects over a 100×100 grid → window-relative percentages, so
        // callers reason about position/size without knowing the pixel size.
        let rects = self
            .state
            .with_active_layout(|lay| lay.leaf_rects(100, 100))
            .unwrap_or_default();
        Ok(rects
            .into_iter()
            .map(|(surface_id, x, y, w, h)| PaneRect { surface_id, x, y, w, h })
            .collect())
    }
    fn bind_transcript(&self, surface_id: &str, path: &str) -> Result<()> {
        // Queue for the tail watcher; re-binding (claude --resume swaps the
        // jsonl) replaces the pending entry rather than stacking.
        let mut binds = self.state.binds.lock().unwrap();
        binds.retain(|(s, _)| s != surface_id);
        binds.push((surface_id.to_string(), PathBuf::from(path)));
        Ok(())
    }
    fn new_session(&self) -> Result<()> {
        let new_id = self.state.alloc_id();
        let cwd = self.state.active_cwd();
        spawn_pane(&self.state, &new_id, cwd)?;
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            sessions.push(DaemonSession {
                windows: vec![PtyLayout::single(new_id.clone())],
                active_window: 0,
            });
            *self.state.active_session.lock().unwrap() = sessions.len() - 1;
        }
        *self.state.active.lock().unwrap() = new_id;
        // No push_active_snapshots: a freshly-spawned pane's Term isn't ready
        // for a full snapshot yet (grid still sizing) — its forwarder sends the
        // first frame as soon as the shell prints, which is what paints it.
        self.state.broadcast_state();
        Ok(())
    }
    fn new_window(&self) -> Result<()> {
        let new_id = self.state.alloc_id();
        let cwd = self.state.active_cwd();
        spawn_pane(&self.state, &new_id, cwd)?;
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            let asx = *self.state.active_session.lock().unwrap();
            if let Some(s) = sessions.get_mut(asx) {
                s.windows.push(PtyLayout::single(new_id.clone()));
                s.active_window = s.windows.len() - 1;
            }
        }
        *self.state.active.lock().unwrap() = new_id;
        // New pane's forwarder paints it (see new_session); no full snapshot.
        self.state.broadcast_state();
        Ok(())
    }
    fn switch_session(&self, idx: usize) -> Result<()> {
        {
            let sessions = self.state.sessions.lock().unwrap();
            if idx >= sessions.len() {
                anyhow::bail!("session index {idx} out of range");
            }
        }
        *self.state.active_session.lock().unwrap() = idx;
        self.state.update_active_pane();
        self.state.broadcast_state();
        self.state.push_active_snapshots();
        Ok(())
    }
    fn switch_window(&self, idx: usize) -> Result<()> {
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            let asx = *self.state.active_session.lock().unwrap();
            let s = sessions
                .get_mut(asx)
                .ok_or_else(|| anyhow!("no active session"))?;
            if idx >= s.windows.len() {
                anyhow::bail!("window index {idx} out of range");
            }
            s.active_window = idx;
        }
        self.state.update_active_pane();
        self.state.broadcast_state();
        self.state.push_active_snapshots();
        Ok(())
    }
    fn close_session(&self, idx: usize) -> Result<()> {
        let killed: Vec<String> = {
            let mut sessions = self.state.sessions.lock().unwrap();
            if sessions.len() <= 1 {
                anyhow::bail!("can't close the last session");
            }
            if idx >= sessions.len() {
                anyhow::bail!("session index {idx} out of range");
            }
            let removed = sessions.remove(idx);
            let mut asx = self.state.active_session.lock().unwrap();
            if *asx >= sessions.len() {
                *asx = sessions.len() - 1;
            } else if *asx > idx {
                *asx -= 1;
            }
            removed
                .windows
                .iter()
                .flat_map(|w| w.leaves().iter().map(|l| l.to_string()).collect::<Vec<_>>())
                .collect()
        };
        {
            let mut pty = self.state.pty.lock().unwrap();
            for id in &killed {
                pty.remove(id); // drop = kill
            }
        }
        self.state.update_active_pane();
        self.state.broadcast_state();
        self.state.push_active_snapshots();
        Ok(())
    }
}

/// Send the full attach handshake to a freshly-connected GUI: the structure
/// first (so the GUI knows the session/window/pane layout), then one full
/// snapshot per pane in the active window. Returns false if the peer dropped.
fn send_handshake(state: &DaemonState, stream: &mut LocalStream) -> bool {
    if write_msg(stream, &StreamMsg::State(state.state_view())).is_err() {
        return false;
    }
    let leaves = state.active_window_leaves();
    let pty = state.pty.lock().unwrap();
    for id in leaves {
        if let Some(s) = pty.get(&id) {
            if write_msg(stream, &StreamMsg::Frame(s.full_snapshot())).is_err() {
                return false;
            }
        }
    }
    true
}

/// `<control-socket>.state` — where the daemon persists its structure so a
/// *daemon* restart (crash, machine reboot) can rebuild the workspace. A live
/// detach/reattach never needs this.
fn daemon_state_path(control: &Path) -> PathBuf {
    let mut s = control.as_os_str().to_os_string();
    s.push(".state");
    PathBuf::from(s)
}

/// Snapshot the full structure + each pane's restore record to disk.
fn save_daemon_state(state: &DaemonState, path: &Path) {
    let sessions_json: Vec<Value> = {
        let sessions = state.sessions.lock().unwrap();
        sessions
            .iter()
            .map(|s| {
                let windows: Vec<Value> = s
                    .windows
                    .iter()
                    .filter_map(|w| serde_json::to_value(w).ok())
                    .collect();
                json!({ "active_window": s.active_window, "windows": windows })
            })
            .collect()
    };
    let mut panes = serde_json::Map::new();
    for (id, sess) in state.pty.lock().unwrap().iter() {
        panes.insert(id.clone(), pane_record(sess));
    }
    let doc = json!({
        "active_session": *state.active_session.lock().unwrap(),
        "sessions": sessions_json,
        "panes": Value::Object(panes),
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, doc.to_string());
}

/// Read back saved daemon state: sessions/windows + active index + per-pane
/// metadata. None if absent/unparseable (caller starts one fresh pane).
fn load_daemon_state(
    path: &Path,
) -> Option<(Vec<DaemonSession>, usize, HashMap<String, PaneMeta>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let active_session = v.get("active_session").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let sessions: Vec<DaemonSession> = v
        .get("sessions")?
        .as_array()?
        .iter()
        .filter_map(|s| {
            let active_window = s.get("active_window").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            let windows: Vec<PtyLayout> = s
                .get("windows")?
                .as_array()?
                .iter()
                .filter_map(|w| serde_json::from_value::<PtyLayout>(w.clone()).ok())
                .collect();
            if windows.is_empty() {
                return None;
            }
            Some(DaemonSession { windows, active_window })
        })
        .collect();
    if sessions.is_empty() {
        return None;
    }
    let mut metas = HashMap::new();
    if let Some(panes) = v.get("panes").and_then(|p| p.as_object()) {
        for (id, m) in panes {
            metas.insert(
                id.clone(),
                PaneMeta {
                    cwd: m.get("cwd").and_then(|x| x.as_str()).map(String::from),
                    was_claude: m.get("was_claude").and_then(|x| x.as_bool()).unwrap_or(false),
                    session_id: m.get("session_id").and_then(|x| x.as_str()).map(String::from),
                },
            );
        }
    }
    let active_session = active_session.min(sessions.len() - 1);
    Some((sessions, active_session, metas))
}

/// After a restored claude pane's shell is ready, inject `claude --resume
/// <id>` (or `--continue`) so the conversation picks up where it left off.
fn schedule_claude_resume(sess: Arc<PtySession>, session_id: Option<String>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let cmd = match session_id {
            Some(id) => format!("claude --resume {id}\n"),
            None => "claude --continue\n".to_string(),
        };
        let _ = sess.send_bytes(cmd.as_bytes());
    });
}

/// Run the headless daemon to completion. Returns when the last pane exits or
/// the control socket can't be bound.
pub fn run_daemon(control_path: PathBuf) -> Result<()> {
    std::env::set_var("KASATERM_SOCKET_PATH", &control_path);

    let (out_tx, out_rx) = mpsc::channel::<StreamMsg>();
    let state_path = daemon_state_path(&control_path);

    // Restore the saved structure if the daemon ran before; else one session
    // with one fresh pane.
    let (sessions, active_session, metas) = load_daemon_state(&state_path).unwrap_or_else(|| {
        (
            vec![DaemonSession {
                windows: vec![PtyLayout::single(PANE_0)],
                active_window: 0,
            }],
            0,
            HashMap::new(),
        )
    });
    let default_cwd = crate::resolve_initial_cwd();

    let mut map = HashMap::new();
    let mut max_id = 0u32;
    let mut resumes: Vec<(Arc<PtySession>, Option<String>)> = Vec::new();
    for s in &sessions {
        for w in &s.windows {
            for leaf in w.leaves() {
                let meta = metas.get(leaf);
                let cwd = meta.and_then(|m| m.cwd.clone()).or_else(|| default_cwd.clone());
                let sess = Arc::new(PtySession::start(PtyOptions {
                    shell: None,
                    cwd,
                    cols: 80,
                    rows: 24,
                    env: Vec::new(),
                    pane_id: leaf.to_string(),
                    initial_scrollback: Vec::new(),
                })?);
                spawn_forwarder(sess.screens.clone(), out_tx.clone());
                if let Some(m) = meta {
                    if m.was_claude {
                        resumes.push((sess.clone(), m.session_id.clone()));
                    }
                }
                if let Some(n) = leaf.strip_prefix('%').and_then(|s| s.parse::<u32>().ok()) {
                    max_id = max_id.max(n);
                }
                map.insert(leaf.to_string(), sess);
            }
        }
    }
    // Active pane = active session's active window's first leaf.
    let active = sessions
        .get(active_session)
        .and_then(|s| s.windows.get(s.active_window))
        .and_then(|w| w.leaves().first().map(|l| l.to_string()))
        .unwrap_or_else(|| PANE_0.to_string());

    let state = Arc::new(DaemonState {
        pty: Mutex::new(map),
        sessions: Mutex::new(sessions),
        active_session: Mutex::new(active_session),
        next_id: Mutex::new(max_id),
        active: Mutex::new(active),
        subscriber: Mutex::new(None),
        out_tx,
        state_path: state_path.clone(),
        collab_auto: Arc::new(Mutex::new(HashMap::new())),
        binds: Arc::new(Mutex::new(Vec::new())),
        previews: Mutex::new(HashMap::new()),
    });

    // Cold restore (claude --resume into a restored pane) is deferred — the
    // "live-first" decision: keep claude alive via the surviving daemon across
    // GUI restarts, but on a *daemon* restart (crash/reboot) a restored pane
    // comes up as a plain shell, not an auto-resumed claude. Auto-resume was
    // resurrecting stale sessions (and firing surprise `claude --resume`s), so
    // it's gated off by default; opt in with KASATERM_COLD_RESTORE=1.
    if std::env::var("KASATERM_COLD_RESTORE").as_deref() == Ok("1") {
        for (sess, id) in resumes {
            schedule_claude_resume(sess, id);
        }
    } else {
        let _ = resumes; // metadata still captured for a future opt-in restore
    }

    // Collab board under the daemon: tail each bound pane's claude transcript
    // and fill collab_auto, so `board` works even though the in-process
    // PtyBackend that used to host the watcher is dead here. Live panes = the
    // pty map keys (every pane across every session).
    {
        let st = state.clone();
        crate::socket::spawn_transcript_watcher(
            state.collab_auto.clone(),
            state.binds.clone(),
            // The set of live pane ids; the watcher drops tails for panes not
            // in it. Transcript paths come from the hook-driven bind, not from
            // walking processes, so the shell pid is no longer needed here.
            move || st.pty.lock().unwrap().keys().cloned().collect(),
        );
    }

    // Persist state every 10s so a daemon crash loses at most that window.
    {
        let state = state.clone();
        let path = state_path.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            save_daemon_state(&state, &path);
        });
    }

    // Follow `cd`: structural broadcasts only fire on split/close/switch, so a
    // shell changing directory would never reach the GUI breadcrumb on its
    // own. Poll each pane's cwd ~1s and re-broadcast the state only when one
    // actually moved — no disk persist (cwd isn't structural), no churn while
    // idle.
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let mut last: HashMap<String, String> = HashMap::new();
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                let cur: HashMap<String, String> = {
                    let pty = state.pty.lock().unwrap();
                    pty.iter()
                        .filter_map(|(id, s)| {
                            s.shell_pid()
                                .and_then(pid_cwd)
                                .map(|p| (id.clone(), p.to_string_lossy().into_owned()))
                        })
                        .collect()
                };
                if cur != last {
                    last = cur;
                    let _ = state.out_tx.send(StreamMsg::State(state.state_view()));
                }
            }
        });
    }

    // Control socket (JSON-RPC) + the kasaspace-mcp HTTP server (so the GUI's
    // session-panel webview, which polls 127.0.0.1:8765/sessions, sees the
    // daemon's sessions instead of the GUI's empty in-process backend).
    let backend: Arc<dyn Backend> = Arc::new(DaemonBackend { state: state.clone() });
    let mcp_port = std::env::var("KASASPACE_MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8765);
    let _ = kasaspace_mcp::spawn_http_server(backend.clone(), mcp_port);
    let server = Server::bind(control_path.clone())?;
    let _ctrl = server.spawn(backend);

    // Stream socket: accept a GUI, send the attach handshake, register it.
    let spath = stream_path(&control_path);
    let _ = std::fs::remove_file(&spath);
    let listener = LocalListener::bind(&spath)?;
    {
        let state = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                if send_handshake(&state, &mut stream) {
                    *state.subscriber.lock().unwrap() = Some(stream);
                }
            }
        });
    }

    // Pump: drain the shared channel and forward to the attached GUI. A pane's
    // EOF frame removes it from the tree; when the last pane exits the daemon
    // has nothing left to host, so it stops.
    loop {
        let msg = match out_rx.recv() {
            Ok(m) => m,
            Err(_) => break,
        };
        if let StreamMsg::Frame(u) = &msg {
            if u.eof {
                state.pty.lock().unwrap().remove(&u.pane_id);
                let empty = state.remove_pane(&u.pane_id);
                // Forward the EOF so the GUI reaps its pane.
                if let Some(st) = state.subscriber.lock().unwrap().as_mut() {
                    let _ = write_msg(st, &msg);
                }
                if empty {
                    break;
                }
                state.update_active_pane();
                state.broadcast_state();
                continue;
            }
        }
        let mut sub = state.subscriber.lock().unwrap();
        if let Some(st) = sub.as_mut() {
            if write_msg(st, &msg).is_err() {
                *sub = None; // GUI detached — keep hosting the PTYs.
            }
        }
    }

    // Clean exit (every pane was closed) → drop the restore state so the next
    // launch starts fresh rather than resurrecting a workspace the user shut.
    let _ = std::fs::remove_file(&state_path);
    let _ = std::fs::remove_file(&control_path);
    let _ = std::fs::remove_file(&spath);
    Ok(())
}
