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

use kasa_socket::backend::{
    Backend, PaneActivity, PaneRect, SessionsInfo, SplitDirection, SurfaceInfo, WorkspaceInfo,
};
use kasa_socket::transport::{LocalListener, LocalStream};
use kasa_socket::Server;
use anyhow::{anyhow, Result};
use kasa_pty::{PtyLayout, PtyOptions, PtySession, ScreenReceiver, SplitDir};
use serde_json::{json, Value};
use kasa_bridge::ScreenUpdate;

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
    /// User-set display name from the session panel's rename. `None` falls
    /// back to the auto-derived label (active-window first-pane cwd basename).
    name: Option<String>,
    /// Panes folded into this session's dock — layout leaf removed but the
    /// PtySession stays alive in the global `pty` map, so reconcile never kills
    /// it (pty is live) and prune sees no ghost leaf (no leaf). Per-session so a
    /// session switch shows only this 지구's dock. Restore via undock_surface.
    docked: Vec<String>,
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
    /// Surface ids whose collab start/stop notices are muted (board-panel
    /// toggle). `collab_notify` from a muted pane is dropped before broadcast.
    muted: Mutex<HashSet<String>>,
    /// Panes we just delivered a notice to. The next notify FROM such a pane
    /// (its watcher woke it → it answered → its turn ended → it fires notify)
    /// is dropped once, so the echo dies after one hop instead of ping-ponging.
    notify_suppress: Mutex<HashSet<String>>,
    /// pane id → cwd path, refreshed by the 1s cwd-poll thread. state_view
    /// reads this instead of spawning `lsof` per pane on every broadcast — so a
    /// close/dock/split RPC returns without blocking on N subprocess calls (the
    /// dock× 0.2s lag). spawn_pane seeds new panes; the poll fills the rest
    /// within 1s. cwd is display-only (breadcrumb/label), never a correctness
    /// input, so a brief stale/empty entry is harmless.
    cwd_cache: Mutex<HashMap<String, String>>,
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
        sessions.retain(|s| !s.windows.is_empty() || !s.docked.is_empty());
        let mut asx = self.active_session.lock().unwrap();
        if *asx >= sessions.len() && !sessions.is_empty() {
            *asx = sessions.len() - 1;
        }
        sessions.is_empty()
    }

    /// Drop any layout leaf with no backing PTY and no preview — the
    /// "ghost pane" guard. A leaf can outlive its pane when a close/EOF races
    /// a split: the PTY vanishes (removed from `pty`) *before* its leaf is
    /// registered in the tree, so the pump's `remove_pane` finds nothing to
    /// drop and the leaf lingers. The GUI then paints an empty pane for every
    /// orphan — the "빈 창 증식" bug. Run this before every broadcast/persist so
    /// the layout can never advertise a pane the daemon isn't actually hosting.
    /// No-op in the common case (every leaf is live).
    fn reconcile_layout(&self) {
        let live: HashSet<String> = {
            let pty = self.pty.lock().unwrap();
            let prev = self.previews.lock().unwrap();
            pty.keys().chain(prev.keys()).cloned().collect()
        };
        {
            let mut sessions = self.sessions.lock().unwrap();
            let mut asx = self.active_session.lock().unwrap();
            prune_ghost_leaves(&mut sessions, &mut asx, &live);
        }
        // The active pane may have pointed at a pruned leaf — re-anchor it to a
        // live one so input/cwd never targets a ghost.
        let active_dead = !live.contains(&*self.active.lock().unwrap());
        if active_dead {
            self.update_active_pane();
        }
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
                // Label = user-set name, else active window's first pane's
                // cwd basename.
                let label = s.name.clone().unwrap_or_else(|| {
                    s.windows
                        .get(s.active_window)
                        .and_then(|w| w.leaves().first().map(|l| l.to_string()))
                        .and_then(|id| self.cwd_cache.lock().unwrap().get(&id).cloned())
                        .and_then(|p| {
                            std::path::Path::new(&p)
                                .file_name()
                                .map(|f| f.to_string_lossy().into_owned())
                        })
                        .unwrap_or_default()
                });
                SessionView {
                    windows: s
                        .windows
                        .iter()
                        .map(|w| WindowView { layout: w.clone() })
                        .collect(),
                    active_window: s.active_window,
                    label,
                    docked: s
                        .docked
                        .iter()
                        .map(|id| {
                            let label = self
                                .cwd_cache
                                .lock()
                                .unwrap()
                                .get(id)
                                .and_then(|p| {
                                    std::path::Path::new(p)
                                        .file_name()
                                        .map(|f| f.to_string_lossy().into_owned())
                                })
                                .unwrap_or_default();
                            crate::stream::DockedView { id: id.clone(), label }
                        })
                        .collect(),
                }
            })
            .collect();
        // Per-pane cwd for the GUI breadcrumb — read from the poll-filled cache,
        // NOT lsof, so close/dock/split/move broadcasts return without blocking
        // on N subprocess calls. The 1s cwd poll keeps the cache fresh.
        let pane_cwds = self.cwd_cache.lock().unwrap().clone();
        // tty is fixed for the pane's life (the master's slave path), so unlike
        // cwd it needs no poll — just ship it whenever state broadcasts.
        let pane_ttys = pty
            .iter()
            .filter_map(|(id, s)| s.tty().map(|t| (id.clone(), t.to_string())))
            .collect();
        // Project the transcript watcher's per-pane activity (busy/idle + intent)
        // onto the StateView so the GUI draws a working indicator for EVERY pane
        // across all windows — the cross-window source an on-screen glyph scan
        // can't provide. Cheap clone of a small map (one entry per claude pane).
        let pane_activity = self
            .collab_auto
            .lock()
            .unwrap()
            .iter()
            .map(|(id, a)| {
                (
                    id.clone(),
                    crate::stream::PaneStatusView {
                        status: a.status.clone(),
                        intent: a.intent.clone(),
                        waiting_for: a.waiting_for.clone(),
                    },
                )
            })
            .collect();
        StateView {
            sessions: views,
            active_session: *self.active_session.lock().unwrap(),
            active_pane: Some(self.active.lock().unwrap().clone()),
            pane_cwds,
            pane_ttys,
            pane_previews: self.previews.lock().unwrap().clone(),
            pane_activity,
        }
    }

    fn broadcast_state(&self) {
        // Prune ghost leaves before anyone (GUI or disk) sees the layout, so a
        // pane that lost its PTY can never reach the GUI as an empty pane.
        self.reconcile_layout();
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
    // Seed the cwd cache so the new pane's breadcrumb/label shows immediately
    // instead of waiting up to 1s for the poll thread's first scan.
    if let Some(c) = &cwd {
        state.cwd_cache.lock().unwrap().insert(pane_id.to_string(), c.clone());
    }
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
            *self.state.active.lock().unwrap() = surface_id.to_string();
        } else {
            // The pane isn't in any layout — the GUI focused a pane it just
            // closed (close/focus race). Setting a dead pane active makes a
            // later split fall back to spawning a fresh session, so the closed
            // pane appears to "resurrect" (and that orphan never gets resized,
            // so its grid/input row is wrong). Re-anchor to a live leaf instead.
            self.state.update_active_pane();
        }
        // Reflect the focus change to GUI + disk so the active pointer never
        // drifts out of sync — that drift is the focus-then-split orphan source.
        self.state.broadcast_state();
        Ok(())
    }
    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo> {
        // Prune dead leaves before choosing the split anchor — a closed pane
        // still lingering in this window's layout could otherwise become the
        // anchor (or the active-drift fallback) and orphan/multiply the new pane.
        self.state.reconcile_layout();
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
        // None = no active session/window; Some(false) = split_leaf couldn't
        // find the anchor leaf. Either way the new pane didn't attach, so adopt
        // it as a fresh session — a split must never orphan a pane (alive in the
        // pty map, present in no layout → unrenderable, uncloseable).
        if !matches!(attached, Some(true)) {
            let mut sessions = self.state.sessions.lock().unwrap();
            sessions.push(DaemonSession {
                windows: vec![PtyLayout::single(new_id.clone())],
                active_window: 0,
                name: None,
                docked: Vec::new(),
            });
            *self.state.active_session.lock().unwrap() = sessions.len() - 1;
        }
        // Race guard: the pane can die between `spawn_pane` and the
        // `split_leaf` above (its EOF reaches the pump before the leaf exists,
        // so the pump's remove_pane finds nothing to drop). Only promote it to
        // active if it's still alive; broadcast_state's reconcile_layout then
        // prunes the orphan leaf if it isn't, so the GUI never sees it.
        if self.state.pty.lock().unwrap().contains_key(&new_id) {
            *self.state.active.lock().unwrap() = new_id.clone();
        }
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
        // A docked pane has no layout leaf for remove_pane to find, so scrub the
        // docked Vec too — a killed PTY must vanish from the dock (invariant),
        // else its chip resurrects on the next broadcast and run_daemon re-spawns
        // a zombie shell on restart. Mirror undock_surface's docked removal.
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            for s in sessions.iter_mut() {
                if let Some(pos) = s.docked.iter().position(|d| d == surface_id) {
                    s.docked.remove(pos);
                    break;
                }
            }
        }
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
                name: None,
                docked: Vec::new(),
            });
            *self.state.active_session.lock().unwrap() = 0;
            *self.state.active.lock().unwrap() = new_id;
        } else {
            self.state.update_active_pane();
        }
        self.state.broadcast_state();
        // Push fresh frames for the now-visible panes. Without this, the pane
        // that took over (or the freshly-spawned shell when the last pane
        // closed) has no grid in the GUI yet and renders blank with input
        // going nowhere — the "Cmd+W then everything's dead" bug. close_session
        // and close_window already do this.
        self.state.push_active_snapshots();
        Ok(())
    }
    fn dock_surface(&self, surface_id: &str) -> Result<()> {
        // An idle pane (bare prompt, no child command) isn't worth folding into
        // the dock — close it outright so no empty chip appears (거노 요청). Only
        // panes with a running job (claude/build/editor) become dock chips.
        let idle = {
            let sess = self.state.pty.lock().unwrap().get(surface_id).cloned();
            sess.map(|s| !s.has_active_job()).unwrap_or(true)
        };
        if idle {
            return self.close_surface(surface_id);
        }
        // Fold, don't kill: the PtySession stays in the global `pty` map (so
        // reconcile never prunes it — pty is live), only the layout leaf goes.
        // Register in the owning session's dock BEFORE remove_pane, because
        // remove_pane drops a session whose windows all emptied — the retain
        // guard keeps it alive only if `docked` is already non-empty.
        let located = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let mut hit = false;
            'scan: for s in sessions.iter_mut() {
                for w in &s.windows {
                    if w.leaves().iter().any(|&l| l == surface_id) {
                        if !s.docked.iter().any(|d| d == surface_id) {
                            s.docked.push(surface_id.to_string());
                        }
                        hit = true;
                        break 'scan;
                    }
                }
            }
            hit
        };
        if !located {
            return Ok(()); // not in any layout (already docked/closed) — no-op
        }
        self.state.remove_pane(surface_id); // layout only; pty left alive
        self.state.update_active_pane();
        self.state.broadcast_state();
        self.state.push_active_snapshots();
        Ok(())
    }
    fn undock_surface(&self, surface_id: &str) -> Result<()> {
        // Restore a folded pane into the active window. Find its owning session,
        // make that session active, then reattach beside the active leaf (or as
        // the sole leaf if the window emptied). reconcile first so the anchor is
        // always a live leaf (split_surface uses the same guard).
        self.state.reconcile_layout();
        let target = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let mut idx = None;
            for (si, s) in sessions.iter_mut().enumerate() {
                if let Some(pos) = s.docked.iter().position(|d| d == surface_id) {
                    s.docked.remove(pos);
                    idx = Some(si);
                    break;
                }
            }
            idx
        };
        let Some(si) = target else {
            return Ok(()); // not docked anywhere — no-op
        };
        *self.state.active_session.lock().unwrap() = si;
        let anchor = {
            let lv = self.state.active_window_leaves();
            let cur = self.state.active.lock().unwrap().clone();
            if lv.iter().any(|l| *l == cur) {
                Some(cur)
            } else {
                lv.into_iter().next()
            }
        };
        let attached = match anchor {
            Some(a) => self.state.with_active_layout(|lay| {
                lay.insert_beside(&a, SplitDir::Horizontal, false, surface_id.to_string())
            }),
            None => self.state.with_active_layout(|lay| {
                *lay = PtyLayout::single(surface_id.to_string());
                true
            }),
        };
        if attached == Some(true) {
            *self.state.active.lock().unwrap() = surface_id.to_string();
        }
        self.state.broadcast_state();
        self.state.push_active_snapshots();
        Ok(())
    }
    fn move_surface(&self, surface_id: &str, target: &str, direction: SplitDirection) -> Result<()> {
        if surface_id == target {
            return Ok(());
        }
        // reconcile so the anchor is always a live leaf (split/undock share this).
        self.state.reconcile_layout();
        let (axis, before) = match direction {
            SplitDirection::Left => (SplitDir::Horizontal, true),
            SplitDirection::Right => (SplitDir::Horizontal, false),
            SplitDirection::Up => (SplitDir::Vertical, true),
            SplitDirection::Down => (SplitDir::Vertical, false),
        };
        // Locate target's session+window; make it active so with_active_layout
        // operates on that tree (drag drops onto a visible pane).
        let located = {
            let sessions = self.state.sessions.lock().unwrap();
            let mut hit = None;
            'scan: for (si, s) in sessions.iter().enumerate() {
                for (wi, w) in s.windows.iter().enumerate() {
                    if w.leaves().iter().any(|&l| l == target) {
                        hit = Some((si, wi));
                        break 'scan;
                    }
                }
            }
            hit
        };
        let Some((si, wi)) = located else {
            return Ok(()); // target gone — drop the move
        };
        *self.state.active_session.lock().unwrap() = si;
        if let Some(s) = self.state.sessions.lock().unwrap().get_mut(si) {
            s.active_window = wi;
        }
        // Detach the moving leaf and re-attach beside target — both in the
        // active window. PTY untouched (pure layout move, unlike close). If
        // moving isn't in this window (cross-window drag, not yet supported)
        // remove_leaf returns false and we skip the insert so it's never
        // duplicated into a second copy.
        let moved = self
            .state
            .with_active_layout(|lay| {
                if !lay.remove_leaf(surface_id) {
                    return false;
                }
                if !lay.insert_beside(target, axis, before, surface_id.to_string()) {
                    if let Some(a) = lay.leaves().first().map(|s| s.to_string()) {
                        lay.insert_beside(&a, axis, before, surface_id.to_string());
                    }
                }
                true
            })
            .unwrap_or(false);
        if moved {
            *self.state.active.lock().unwrap() = surface_id.to_string();
        }
        self.state.broadcast_state();
        self.state.push_active_snapshots();
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
            // Give claude's Ink prompt a beat to ingest+render the body before
            // the Enter lands. Sent back-to-back, claude can't keep up with the
            // fast input and treats the \r as text instead of a submit — the
            // line just sits in the prompt unsent (confirmed by repro: same
            // text submits fine when the Enter is delayed ~80ms).
            if !body.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
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
        let muted = self.state.muted.lock().unwrap();
        Ok(self
            .state
            .collab_auto
            .lock()
            .unwrap()
            .values()
            .filter(|a| live.contains(&a.surface_id))
            .map(|a| {
                let mut a = a.clone();
                a.muted = muted.contains(&a.surface_id);
                a
            })
            .collect())
    }
    fn collab_notify(&self, from: &str, kind: &str) -> Result<()> {
        // Muted panes don't wake siblings — the board-panel toggle drops their
        // notices here, before any work.
        if self.state.muted.lock().unwrap().contains(from) {
            return Ok(());
        }
        // Loop-breaker: a pane woken BY a notice (watcher fired → it answered →
        // turn ended → this notify) gets dropped once, so the echo dies after
        // one hop rather than ping-ponging across panes.
        if self.state.notify_suppress.lock().unwrap().remove(from) {
            return Ok(());
        }
        // The announcing pane's latest transcript-derived intent (what it was
        // just doing) rides along so "%2 완료" becomes "%2 완료 · daemon.rs",
        // not a bare ping. Empty if that pane never bound a transcript.
        let intent = self
            .state
            .collab_auto
            .lock()
            .unwrap()
            .get(from)
            .map(|a| a.intent.clone())
            .unwrap_or_default();
        // The `[알림]` prefix is the loop-breaker. Injecting this line wakes a
        // sibling, whose own UserPromptSubmit/Stop would re-fire notify and
        // ping-pong forever — but the notify hook bails when the triggering
        // prompt starts with `[알림]`, so the echo dies after one hop.
        let msg = match kind {
            "start" => format!("[알림] {from} 시작"),
            _ if intent.is_empty() => format!("[알림] {from} 완료"),
            _ => format!("[알림] {from} 완료 · {intent}"),
        };
        // Snapshot targets, then drop the pty lock before send_text (which
        // re-locks pty per pane). Sender excluded; mute filtering lands with
        // the board-panel toggle in a later pass.
        let targets: Vec<String> = {
            let pty = self.state.pty.lock().unwrap();
            pty.keys().filter(|s| s.as_str() != from).cloned().collect()
        };
        let mut suppress = self.state.notify_suppress.lock().unwrap();
        for sid in targets {
            // Append to the pane's inbox file instead of injecting into its PTY.
            // That pane's `run_in_background` watcher tails this and exits → its
            // claude is re-invoked by a task-notification: no fake keystrokes,
            // no IME corruption, works busy or idle. Mark it suppressed so the
            // notify it fires right after reading this is dropped (loop-break).
            let safe = sid.trim_start_matches('%');
            let dir = std::path::Path::new("/tmp/kasaterm-inbox");
            let _ = std::fs::create_dir_all(dir);
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(format!("{safe}.jsonl")))
            {
                let _ = writeln!(f, "{msg}");
            }
            suppress.insert(sid);
        }
        Ok(())
    }
    fn set_collab_mute(&self, surface_id: &str, muted: bool) -> Result<()> {
        let mut set = self.state.muted.lock().unwrap();
        if muted {
            set.insert(surface_id.to_string());
        } else {
            set.remove(surface_id);
        }
        Ok(())
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
                name: None,
                docked: Vec::new(),
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
    fn close_window(&self, idx: usize) -> Result<()> {
        // Daemon-authoritative window close. The GUI used to fake this by
        // closing each pane off its *local* layout, which drifted from the
        // daemon's tree and left windows that resurrected on the next state
        // push (the window-increment bug). Here the daemon removes the window
        // from its own active session and reaps the PTYs — the single source
        // of truth.
        let killed: Vec<String> = {
            let sessions = self.state.sessions.lock().unwrap();
            let asx = *self.state.active_session.lock().unwrap();
            let s = sessions.get(asx).ok_or_else(|| anyhow!("no active session"))?;
            if s.windows.len() <= 1 {
                anyhow::bail!("can't close the last window of a session");
            }
            let w = s
                .windows
                .get(idx)
                .ok_or_else(|| anyhow!("window index {idx} out of range"))?;
            w.leaves().iter().map(|l| l.to_string()).collect()
        };
        {
            let mut pty = self.state.pty.lock().unwrap();
            let mut prev = self.state.previews.lock().unwrap();
            for id in &killed {
                pty.remove(id); // drop = kill
                prev.remove(id);
            }
        }
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            let asx = *self.state.active_session.lock().unwrap();
            if let Some(s) = sessions.get_mut(asx) {
                if idx < s.windows.len() {
                    s.windows.remove(idx);
                    if s.active_window >= s.windows.len() && !s.windows.is_empty() {
                        s.active_window = s.windows.len() - 1;
                    }
                }
            }
        }
        self.state.update_active_pane();
        self.state.broadcast_state();
        self.state.push_active_snapshots();
        Ok(())
    }
    fn rename_session(&self, idx: usize, name: &str) -> Result<()> {
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            let s = sessions
                .get_mut(idx)
                .ok_or_else(|| anyhow!("session index {idx} out of range"))?;
            let trimmed = name.trim();
            // Blank clears back to the auto cwd-basename label.
            s.name = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
        }
        self.state.persist();
        self.state.broadcast_state();
        Ok(())
    }
    fn reset_sessions(&self) -> Result<()> {
        // Spawn the replacement pane first so we never sit session-less even
        // for an instant; then collect every old pane id and drop them.
        let new_id = self.state.alloc_id();
        let cwd = self.state.active_cwd();
        spawn_pane(&self.state, &new_id, cwd)?;
        let old: Vec<String> = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let old = sessions
                .iter()
                .flat_map(|s| {
                    s.windows
                        .iter()
                        .flat_map(|w| w.leaves().iter().map(|l| l.to_string()).collect::<Vec<_>>())
                })
                .collect();
            *sessions = vec![DaemonSession {
                windows: vec![PtyLayout::single(new_id.clone())],
                active_window: 0,
                name: None,
                docked: Vec::new(),
            }];
            *self.state.active_session.lock().unwrap() = 0;
            old
        };
        {
            let mut pty = self.state.pty.lock().unwrap();
            for id in &old {
                pty.remove(id); // drop = kill
            }
        }
        *self.state.active.lock().unwrap() = new_id;
        self.state.persist();
        self.state.broadcast_state();
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
/// Remove every layout leaf not in `live` (no PTY / preview backing it), drop
/// windows and sessions that empty out, and clamp the active-session index.
/// Split out of `reconcile_layout` so the tree surgery can be unit-tested
/// without standing up a live daemon.
fn prune_ghost_leaves(
    sessions: &mut Vec<DaemonSession>,
    active_session: &mut usize,
    live: &HashSet<String>,
) {
    for s in sessions.iter_mut() {
        s.windows.retain_mut(|w| {
            let ghosts: Vec<String> = w
                .leaves()
                .iter()
                .filter(|l| !live.contains(**l))
                .map(|l| l.to_string())
                .collect();
            for g in &ghosts {
                w.remove_leaf(g);
            }
            // Keep the window only if a real pane still survives in it.
            w.leaves().iter().any(|l| live.contains(*l))
        });
        if s.active_window >= s.windows.len() && !s.windows.is_empty() {
            s.active_window = s.windows.len() - 1;
        }
    }
    sessions.retain(|s| !s.windows.is_empty() || !s.docked.is_empty());
    if *active_session >= sessions.len() && !sessions.is_empty() {
        *active_session = sessions.len() - 1;
    }
}

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
                json!({ "active_window": s.active_window, "windows": windows, "name": s.name, "docked": s.docked })
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
            let name = s
                .get("name")
                .and_then(|x| x.as_str())
                .filter(|n| !n.is_empty())
                .map(String::from);
            let docked = s
                .get("docked")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            Some(DaemonSession { windows, active_window, name, docked })
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
                name: None,
                docked: Vec::new(),
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
        // Dock panes have no layout leaf — spawn them too so a restored chip is
        // a live shell that undock can reattach (else the chip points at a dead
        // PTY). Same PtyOptions as a leaf pane.
        for d in &s.docked {
            let meta = metas.get(d);
            let cwd = meta.and_then(|m| m.cwd.clone()).or_else(|| default_cwd.clone());
            let sess = Arc::new(PtySession::start(PtyOptions {
                shell: None,
                cwd,
                cols: 80,
                rows: 24,
                env: Vec::new(),
                pane_id: d.to_string(),
                initial_scrollback: Vec::new(),
            })?);
            spawn_forwarder(sess.screens.clone(), out_tx.clone());
            if let Some(m) = meta {
                if m.was_claude {
                    resumes.push((sess.clone(), m.session_id.clone()));
                }
            }
            if let Some(n) = d.strip_prefix('%').and_then(|s| s.parse::<u32>().ok()) {
                max_id = max_id.max(n);
            }
            map.insert(d.to_string(), sess);
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
        muted: Mutex::new(HashSet::new()),
        notify_suppress: Mutex::new(HashSet::new()),
        previews: Mutex::new(HashMap::new()),
        cwd_cache: Mutex::new(HashMap::new()),
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

    // Follow `cd` AND collab activity: structural broadcasts only fire on
    // split/close/switch, so a shell changing directory — or a pane flipping
    // working↔idle in the transcript watcher — would never reach the GUI on its
    // own. Poll each pane's cwd + status ~1s and re-broadcast only when one
    // actually changed — no disk persist (neither is structural), no churn while
    // idle: status holds a steady "working"/"idle" between transitions, so a
    // busy-but-unchanged pane emits nothing; only the flip broadcasts, which is
    // exactly what drives the GUI's working bar + completion toast.
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let mut last: HashMap<String, String> = HashMap::new();
            let mut last_status: HashMap<String, (String, Option<String>)> = HashMap::new();
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                // Snapshot (id, pid) under the lock, then run lsof OUTSIDE it.
                // pid_cwd shells out to lsof (~40ms each, slower under load);
                // holding `pty` across every pane's lsof meant N panes × 40ms+
                // of lock time each second, during which every RPC/broadcast
                // that needs `pty` blocked — the daemon (and thus the GUI)
                // stalled whenever many claude processes were live ("claude 켜면
                // 어느 순간 멈춘다"). The lock now only spans a cheap pid copy.
                let pids: Vec<(String, u32)> = {
                    let pty = state.pty.lock().unwrap();
                    pty.iter()
                        .filter_map(|(id, s)| s.shell_pid().map(|p| (id.clone(), p)))
                        .collect()
                };
                let cur: HashMap<String, String> = pids
                    .into_iter()
                    .filter_map(|(id, pid)| {
                        pid_cwd(pid).map(|p| (id, p.to_string_lossy().into_owned()))
                    })
                    .collect();
                // Per-pane coarse status (working/idle/waiting/…) — the
                // cross-window busy signal the GUI draws as a working bar +
                // completion toast. Cheap map, compared by value so a steady
                // "working" emits nothing; a working↔idle↔waiting flip changes
                // it. `waiting_for` is in the key so a same-status reason change
                // (permission→input) also forces a broadcast.
                let cur_status: HashMap<String, (String, Option<String>)> = state
                    .collab_auto
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, a)| (id.clone(), (a.status.clone(), a.waiting_for.clone())))
                    .collect();
                if cur != last || cur_status != last_status {
                    // Publish the fresh cwds so state_view reads them from the
                    // cache instead of re-spawning lsof per pane on every RPC
                    // broadcast. Whole-map replace drops cwds of closed panes.
                    *state.cwd_cache.lock().unwrap() = cur.clone();
                    last = cur;
                    last_status = cur_status;
                    // Reconcile first — the RPC broadcast paths (broadcast_state)
                    // do, but this poll path didn't. A close that lands mid-poll
                    // could otherwise ship a layout still referencing the dead
                    // pane, and the GUI's ws.panes.retain repaints it (the
                    // "closed pane resurrects when you open a new one" race).
                    // reconcile_layout is idempotent: it only prunes leaves with
                    // no live PTY, never resurrects or mutates valid ones.
                    state.reconcile_layout();
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
    match kasa_mcp::spawn_http_server(backend.clone(), mcp_port) {
        Ok(actual) => {
            // Record the ACTUAL bound port so the GUI's panel webviews poll the
            // right place even if `mcp_port` was taken and we fell back.
            let _ = std::fs::write(crate::mcp_port_file_path(), actual.to_string());
        }
        Err(e) => eprintln!("[kasaspace-mcp] daemon http start failed: {e}"),
    }
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

#[cfg(test)]
mod ghost_leaf_tests {
    use super::*;

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn session(windows: Vec<PtyLayout>, active_window: usize) -> DaemonSession {
        DaemonSession { windows, active_window, name: None, docked: Vec::new() }
    }

    // Split(%0, %4) where %4 has no PTY → collapses to the live %0.
    #[test]
    fn prunes_dead_leaf_keeps_live_sibling() {
        let mut win = PtyLayout::single("%0");
        win.split_leaf("%0", SplitDir::Horizontal, "%4".into());
        let mut sessions = vec![session(vec![win], 0)];
        let mut asx = 0;
        prune_ghost_leaves(&mut sessions, &mut asx, &live(&["%0"]));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].windows.len(), 1);
        assert_eq!(sessions[0].windows[0].leaves(), vec!["%0"]);
    }

    // Two dead siblings nested under a live pane: Split(%0, Split(%4, %5)),
    // both %4/%5 dead → must fully collapse to %0.
    #[test]
    fn prunes_two_nested_ghosts() {
        let mut win = PtyLayout::single("%0");
        win.split_leaf("%0", SplitDir::Horizontal, "%4".into());
        win.split_leaf("%4", SplitDir::Vertical, "%5".into());
        let mut sessions = vec![session(vec![win], 0)];
        let mut asx = 0;
        prune_ghost_leaves(&mut sessions, &mut asx, &live(&["%0"]));
        assert_eq!(sessions[0].windows[0].leaves(), vec!["%0"]);
    }

    // A window holding only dead leaves is dropped, and active_window clamps.
    #[test]
    fn drops_dead_only_window_and_clamps_active() {
        let w0 = PtyLayout::single("%0");
        let w1 = PtyLayout::single("%4"); // dead-only
        let mut sessions = vec![session(vec![w0, w1], 1)];
        let mut asx = 0;
        prune_ghost_leaves(&mut sessions, &mut asx, &live(&["%0"]));
        assert_eq!(sessions[0].windows.len(), 1);
        assert_eq!(sessions[0].active_window, 0);
    }

    // A session whose every window died is removed, and active_session clamps.
    #[test]
    fn drops_dead_session_and_clamps_active_session() {
        let s0 = session(vec![PtyLayout::single("%0")], 0);
        let s1 = session(vec![PtyLayout::single("%4")], 0); // dead-only
        let mut sessions = vec![s0, s1];
        let mut asx = 1;
        prune_ghost_leaves(&mut sessions, &mut asx, &live(&["%0"]));
        assert_eq!(sessions.len(), 1);
        assert_eq!(asx, 0);
    }

    // Healthy layout is untouched (no-op in the common case).
    #[test]
    fn noop_when_all_leaves_live() {
        let mut win = PtyLayout::single("%0");
        win.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        let mut sessions = vec![session(vec![win], 0)];
        let mut asx = 0;
        prune_ghost_leaves(&mut sessions, &mut asx, &live(&["%0", "%1"]));
        assert_eq!(sessions[0].windows[0].leaves(), vec!["%0", "%1"]);
        assert_eq!(asx, 0);
    }

    // The exact shape seen in the wild: 9 leaves, 4 with no PTY → 5 survive.
    #[test]
    fn matches_observed_ghost_state() {
        // Build one window: %0,%3,%9,%10,%11 live + %4,%5,%6,%12 ghosts.
        let mut win = PtyLayout::single("%0");
        for id in ["%3", "%4", "%5", "%6", "%9", "%10", "%11", "%12"] {
            // graft each onto the current first leaf — exact shape is irrelevant,
            // only the leaf set matters for the prune.
            let target = win.leaves()[0].to_string();
            win.split_leaf(&target, SplitDir::Horizontal, id.into());
        }
        let mut sessions = vec![session(vec![win], 0)];
        let mut asx = 0;
        prune_ghost_leaves(
            &mut sessions,
            &mut asx,
            &live(&["%0", "%3", "%9", "%10", "%11"]),
        );
        let mut survivors = sessions[0].windows[0].leaves();
        survivors.sort();
        assert_eq!(survivors, vec!["%0", "%10", "%11", "%3", "%9"]);
    }
}
