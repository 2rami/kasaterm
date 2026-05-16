//! Backend impl that bridges agent-socket to this binary's TmuxSession.
//!
//! The single-pane PoC reports a fixed workspace + surface id ("local-0"
//! / "pane-0") because we only own one tmux pane in this binary. Once
//! tmuxify grows multi-pane support the surface ids
//! become real tmux `@N` strings and `list_surfaces` returns one entry
//! per actually-open pane.

use agent_socket::backend::{Backend, SplitDirection, SurfaceInfo, WorkspaceInfo};
use anyhow::{anyhow, Result};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use tmux_bridge::TmuxSession;

const FIXED_WORKSPACE_ID: &str = "local-0";
const FIXED_SURFACE_ID: &str = "pane-0";

pub struct TmuxBackend {
    tmux: Arc<TmuxSession>,
}

impl TmuxBackend {
    pub fn new(tmux: Arc<TmuxSession>) -> Self {
        Self { tmux }
    }
}

impl Backend for TmuxBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }])
    }

    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(Some(WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }))
    }

    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(vec![SurfaceInfo {
            id: FIXED_SURFACE_ID.into(),
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        }])
    }

    fn focus_surface(&self, _surface_id: &str) -> Result<()> {
        // Single pane — no-op. Multi-pane phase will route to tmux's
        // `select-pane -t <id>`.
        Ok(())
    }

    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo> {
        // tmux's split-window takes -h for horizontal split, -v for
        // vertical. cmux's direction terminology is what *cell rows*
        // grow into — right/left are horizontal splits, up/down are
        // vertical. -b prepends the new pane before the current one,
        // which matches cmux's "left" / "up" semantics.
        let cmd = match direction {
            SplitDirection::Right => "split-window -h",
            SplitDirection::Left => "split-window -hb",
            SplitDirection::Down => "split-window -v",
            SplitDirection::Up => "split-window -vb",
        };
        self.tmux.send_cmd(cmd)?;
        // We don't have a way to get the new pane's tmux id back
        // synchronously yet — control-mode reports it via a layout-change
        // event which the host's flusher thread receives. For the PoC
        // return a placeholder that the caller can correlate later.
        Ok(SurfaceInfo {
            id: "pane-new".into(),
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        })
    }

    fn send_text(&self, _surface_id: Option<&str>, text: &str) -> Result<()> {
        // Single pane — surface_id ignored. Send as a hex-encoded
        // payload so newlines and escape sequences pass through tmux's
        // send-keys without quoting drama.
        let hex: String = text
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.tmux.send_keys_hex(None, &hex)
    }

    fn send_key(&self, _surface_id: Option<&str>, key: &str) -> Result<()> {
        // Map cmux's symbolic key names to the byte sequences a terminal
        // emulator emits. Anything unknown gets forwarded as a literal
        // string so clients can send single characters via send_key too.
        let bytes = key_to_bytes(key);
        let hex: String = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.tmux.send_keys_hex(None, &hex)
    }
}

/// Shared key-to-bytes table used by both TmuxBackend and PtyBackend so
/// the wire-level interpretation is identical no matter which backend
/// is wired up. Returns a `Vec<u8>` so the literal-fallback path (when
/// the key isn't a recognized symbolic name) can borrow the original
/// `str`'s bytes without lifetime gymnastics.
pub(crate) fn key_to_bytes(key: &str) -> Vec<u8> {
    match key {
        "enter" => b"\r".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        other => other.as_bytes().to_vec(),
    }
}

// ---------------------------------------------------------------------------
// PtyBackend — Phase C wiring.
//
// Socket calls fire on a worker thread, but every mutation that touches
// the PTY HashMap or the BSP layout has to happen on the main thread
// (it owns the winit Window, drives `split_active_pane`, and updates
// the renderer state). The bridge is two shared Arc<Mutex>:
//
//   - `inbox`: PtyBackend pushes `PtyCommand`s; main thread drains them
//     in `about_to_wait` and runs each through the existing
//     split/focus/send paths.
//   - `snapshot`: main thread publishes the surface list + active pane
//     after every state change; PtyBackend serves read-only methods
//     (list_surfaces, current_workspace) straight from this.
//
// Commands that need a return value (`split_surface` reports the new
// pane id) carry a oneshot `SyncSender` so the worker thread blocks
// until the main loop processes the request.
// ---------------------------------------------------------------------------

/// Direction enum mirroring `pty_backend::SplitDir` without forcing the
/// agent-socket crate to depend on pty-backend. We translate cmux's
/// 4-way direction (left/right/up/down) down into the 2-way BSP split
/// (horizontal/vertical) — left/right both produce a side-by-side
/// horizontal split, up/down both produce a stacked vertical split.
#[derive(Debug, Clone, Copy)]
pub enum PtySplitAxis {
    Horizontal,
    Vertical,
}

impl From<SplitDirection> for PtySplitAxis {
    fn from(d: SplitDirection) -> Self {
        match d {
            SplitDirection::Left | SplitDirection::Right => PtySplitAxis::Horizontal,
            SplitDirection::Up | SplitDirection::Down => PtySplitAxis::Vertical,
        }
    }
}

pub enum PtyCommand {
    Focus {
        pane_id: String,
        reply: SyncSender<Result<()>>,
    },
    Split {
        axis: PtySplitAxis,
        reply: SyncSender<Result<String>>,
    },
    SendBytes {
        pane_id: Option<String>,
        bytes: Vec<u8>,
        reply: SyncSender<Result<()>>,
    },
}

/// Read-only view the main thread publishes after every state change.
/// PtyBackend serves list_surfaces / current_workspace from this so
/// the socket worker never has to lock the workspace itself.
#[derive(Default, Clone)]
pub struct PtySnapshot {
    pub surfaces: Vec<SurfaceInfo>,
    pub active_pane: Option<String>,
}

#[derive(Clone)]
pub struct PtyBackendHandle {
    pub inbox: Arc<Mutex<Vec<PtyCommand>>>,
    pub snapshot: Arc<Mutex<PtySnapshot>>,
    /// Used by the worker thread to ask winit for a redraw after it
    /// pushes a command. Not strictly required (about_to_wait will tick
    /// on its own), but it keeps split/focus latency at one frame
    /// instead of waiting on the blink timer.
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

pub struct PtyBackend {
    handle: PtyBackendHandle,
}

impl PtyBackend {
    pub fn new(handle: PtyBackendHandle) -> Self {
        Self { handle }
    }

    fn submit<T>(&self, build: impl FnOnce(SyncSender<Result<T>>) -> PtyCommand) -> Result<T> {
        let (tx, rx) = sync_channel::<Result<T>>(1);
        self.handle.inbox.lock().unwrap().push(build(tx));
        (self.handle.wake)();
        // Bounded wait so a stuck main thread can't deadlock a socket
        // client — three seconds is a comfortable upper bound for any
        // synchronous frame-tied operation.
        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(res) => res,
            Err(_) => Err(anyhow!("main thread did not respond within 3s")),
        }
    }
}

impl Backend for PtyBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }])
    }

    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(Some(WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }))
    }

    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(self.handle.snapshot.lock().unwrap().surfaces.clone())
    }

    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        let id = surface_id.to_string();
        self.submit(|reply| PtyCommand::Focus { pane_id: id, reply })
    }

    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo> {
        let axis = direction.into();
        let new_id = self.submit(|reply| PtyCommand::Split { axis, reply })?;
        Ok(SurfaceInfo {
            id: new_id,
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        })
    }

    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        let pane_id = surface_id.map(|s| s.to_string());
        let bytes = text.as_bytes().to_vec();
        self.submit(|reply| PtyCommand::SendBytes { pane_id, bytes, reply })
    }

    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()> {
        let pane_id = surface_id.map(|s| s.to_string());
        let bytes = key_to_bytes(key);
        self.submit(|reply| PtyCommand::SendBytes { pane_id, bytes, reply })
    }
}
