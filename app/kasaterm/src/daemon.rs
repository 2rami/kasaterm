//! Headless daemon process: owns every PtySession + the BSP layout tree,
//! serves the JSON-RPC control socket and the bincode stream socket, and
//! outlives the GUI so shells / claude / vim keep running across GUI
//! restarts.
//!
//! Multi-pane (M3): the daemon is the layout authority. split/close/focus
//! mutate the tree here and broadcast a `StreamMsg::Layout` to the attached
//! GUI; per-pane screen frames are multiplexed onto the same stream. Every
//! pane's screen channel is forwarded into one shared mpsc that the pump
//! drains, so a single writer owns the stream socket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_socket::backend::{Backend, SplitDirection, SurfaceInfo, WorkspaceInfo};
use agent_socket::transport::{LocalListener, LocalStream};
use agent_socket::Server;
use anyhow::{anyhow, Result};
use pty_backend::{PtyLayout, PtyOptions, PtySession, ScreenReceiver, SplitDir};
use serde_json::{json, Value};
use tmux_bridge::ScreenUpdate;

use crate::socket::{key_to_bytes, pane_record, pid_cwd};
use crate::stream::{stream_path, write_msg, SessionView, StateView, StreamMsg, WindowView};

const WS_ID: &str = "local-0";
const PANE_0: &str = "%0";

/// Per-pane restore metadata read back from the daemon state file.
struct PaneMeta {
    cwd: Option<String>,
    was_claude: bool,
    session_id: Option<String>,
}

/// `<control-socket>.state` — where the daemon persists its layout + pane
/// cwds/claude sessions so a *daemon* restart (crash, machine reboot) can
/// rebuild the workspace. A live detach/reattach never needs this; it's the
/// floor for when the daemon process itself dies.
fn daemon_state_path(control: &Path) -> PathBuf {
    let mut s = control.as_os_str().to_os_string();
    s.push(".state");
    PathBuf::from(s)
}

/// Snapshot the layout tree + each pane's restore record to disk.
fn save_daemon_state(state: &DaemonState, path: &Path) {
    let layout_v = serde_json::to_value(&*state.layout.lock().unwrap()).unwrap_or(Value::Null);
    let mut panes = serde_json::Map::new();
    for (id, sess) in state.pty.lock().unwrap().iter() {
        panes.insert(id.clone(), pane_record(sess));
    }
    let doc = json!({ "layout": layout_v, "panes": Value::Object(panes) });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, doc.to_string());
}

/// Read back a saved daemon state: the BSP tree + per-pane metadata. None if
/// the file is absent or unparseable (caller falls back to a single pane).
fn load_daemon_state(path: &Path) -> Option<(PtyLayout, HashMap<String, PaneMeta>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let layout: PtyLayout = serde_json::from_value(v.get("layout")?.clone()).ok()?;
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
    Some((layout, metas))
}

/// After a restored claude pane's shell is ready, inject `claude --resume
/// <id>` (or `--continue`) so the conversation picks up where it left off.
fn schedule_claude_resume(sess: Arc<PtySession>, session_id: Option<String>) {
    std::thread::spawn(move || {
        // Wait out the shell's rc files so the command lands at a prompt.
        std::thread::sleep(Duration::from_millis(1500));
        let cmd = match session_id {
            Some(id) => format!("claude --resume {id}\n"),
            None => "claude --continue\n".to_string(),
        };
        let _ = sess.send_bytes(cmd.as_bytes());
    });
}

struct DaemonState {
    pty: Mutex<HashMap<String, Arc<PtySession>>>,
    layout: Mutex<PtyLayout>,
    next_id: Mutex<u32>,
    /// Active pane — split targets it, send_*/scroll default to it.
    active: Mutex<String>,
    /// The currently-attached GUI's frame sink (single attach in M3; a Vec
    /// would make this multi-attach).
    subscriber: Mutex<Option<LocalStream>>,
    /// Every pane forwarder + every layout broadcast feeds this; the pump
    /// drains it and writes to the subscriber. One writer for the socket.
    out_tx: Sender<StreamMsg>,
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

    /// Current structure as a StateView. Single session/window for now —
    /// multi-session lands in a follow-up; the wire shape already supports it.
    fn state_view(&self) -> StateView {
        let lay = self.layout.lock().unwrap().clone();
        let label = lay.leaves().first().map(|s| s.to_string()).unwrap_or_default();
        StateView {
            sessions: vec![SessionView {
                windows: vec![WindowView { layout: lay }],
                active_window: 0,
                label,
            }],
            active_session: 0,
        }
    }

    fn broadcast_state(&self) {
        let _ = self.out_tx.send(StreamMsg::State(self.state_view()));
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
fn spawn_pane(
    state: &DaemonState,
    pane_id: &str,
    cwd: Option<String>,
) -> Result<Arc<PtySession>> {
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
        let lay = self.state.layout.lock().unwrap();
        Ok(lay
            .leaves()
            .into_iter()
            .map(|id| SurfaceInfo {
                id: id.to_string(),
                workspace_id: WS_ID.into(),
                title: None,
            })
            .collect())
    }
    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        *self.state.active.lock().unwrap() = surface_id.to_string();
        Ok(())
    }
    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo> {
        let axis = match direction {
            SplitDirection::Left | SplitDirection::Right => SplitDir::Horizontal,
            SplitDirection::Up | SplitDirection::Down => SplitDir::Vertical,
        };
        let new_id = {
            let mut n = self.state.next_id.lock().unwrap();
            *n += 1;
            format!("%{}", *n)
        };
        let active = self.state.active.lock().unwrap().clone();
        // Inherit the active pane's cwd so a split lands in the same project.
        let cwd = self
            .state
            .pty
            .lock()
            .unwrap()
            .get(&active)
            .and_then(|s| s.shell_pid())
            .and_then(pid_cwd)
            .map(|p| p.to_string_lossy().into_owned());
        spawn_pane(&self.state, &new_id, cwd)?;
        {
            let mut lay = self.state.layout.lock().unwrap();
            lay.split_leaf(&active, axis, new_id.clone());
        }
        *self.state.active.lock().unwrap() = new_id.clone();
        self.state.broadcast_state();
        Ok(SurfaceInfo {
            id: new_id,
            workspace_id: WS_ID.into(),
            title: None,
        })
    }
    fn close_surface(&self, surface_id: &str) -> Result<()> {
        // Dropping the Arc kills the shell.
        self.state.pty.lock().unwrap().remove(surface_id);
        {
            let mut lay = self.state.layout.lock().unwrap();
            lay.remove_leaf(surface_id);
        }
        // Re-point active at a surviving leaf.
        if let Some(first) = self.state.layout.lock().unwrap().leaves().first() {
            *self.state.active.lock().unwrap() = first.to_string();
        }
        self.state.broadcast_state();
        Ok(())
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        self.state
            .pane(surface_id)
            .ok_or_else(|| anyhow!("no pane to send to"))?
            .send_bytes(text.as_bytes())
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
}

/// Send the full attach handshake to a freshly-connected GUI: the structure
/// first (so the GUI knows the session/window/pane layout), then one full
/// snapshot per pane. Returns false if the peer dropped mid-handshake.
fn send_handshake(state: &DaemonState, stream: &mut LocalStream) -> bool {
    if write_msg(stream, &StreamMsg::State(state.state_view())).is_err() {
        return false;
    }
    let panes: Vec<Arc<PtySession>> =
        state.pty.lock().unwrap().values().cloned().collect();
    for s in panes {
        if write_msg(stream, &StreamMsg::Frame(s.full_snapshot())).is_err() {
            return false;
        }
    }
    true
}

/// Run the headless daemon to completion. Returns when the last pane exits or
/// the control socket can't be bound.
pub fn run_daemon(control_path: PathBuf) -> Result<()> {
    std::env::set_var("KASATERM_SOCKET_PATH", &control_path);

    let (out_tx, out_rx) = mpsc::channel::<StreamMsg>();
    let state_path = daemon_state_path(&control_path);

    // Restore the saved layout if the daemon ran before; else one fresh pane.
    let (layout, metas) =
        load_daemon_state(&state_path).unwrap_or_else(|| (PtyLayout::single(PANE_0), HashMap::new()));
    let leaves: Vec<String> = layout.leaves().iter().map(|s| s.to_string()).collect();
    let default_cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    let mut map = HashMap::new();
    let mut max_id = 0u32;
    let mut resumes: Vec<(Arc<PtySession>, Option<String>)> = Vec::new();
    for leaf in &leaves {
        let meta = metas.get(leaf);
        let cwd = meta.and_then(|m| m.cwd.clone()).or_else(|| default_cwd.clone());
        let sess = Arc::new(PtySession::start(PtyOptions {
            shell: None,
            cwd,
            cols: 80,
            rows: 24,
            env: Vec::new(),
            pane_id: leaf.clone(),
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
        map.insert(leaf.clone(), sess);
    }
    let active = leaves.first().cloned().unwrap_or_else(|| PANE_0.to_string());
    let state = Arc::new(DaemonState {
        pty: Mutex::new(map),
        layout: Mutex::new(layout),
        next_id: Mutex::new(max_id),
        active: Mutex::new(active),
        subscriber: Mutex::new(None),
        out_tx,
    });

    // Kick off `claude --resume` on every restored claude pane.
    for (sess, id) in resumes {
        schedule_claude_resume(sess, id);
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

    // Control socket (JSON-RPC) on its own background threads.
    let server = Server::bind(control_path.clone())?;
    let _ctrl = server.spawn(Arc::new(DaemonBackend { state: state.clone() }));

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
                let empty = {
                    state.pty.lock().unwrap().remove(&u.pane_id);
                    state.layout.lock().unwrap().remove_leaf(&u.pane_id);
                    state.pty.lock().unwrap().is_empty()
                };
                // Forward the EOF so the GUI reaps its pane.
                if let Some(st) = state.subscriber.lock().unwrap().as_mut() {
                    let _ = write_msg(st, &msg);
                }
                if empty {
                    break;
                }
                // Re-point active and tell the GUI the new tree.
                if let Some(first) = state.layout.lock().unwrap().leaves().first() {
                    *state.active.lock().unwrap() = first.to_string();
                }
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
