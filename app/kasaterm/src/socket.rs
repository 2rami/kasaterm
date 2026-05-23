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

    fn close_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("close_surface not supported on the tmux backend")
    }

    fn rename_surface(&self, _surface_id: &str, _title: &str) -> Result<()> {
        anyhow::bail!("rename_surface not supported on the tmux backend")
    }

    fn set_color(&self, _surface_id: &str, _color: [u8; 4]) -> Result<()> {
        anyhow::bail!("set_color not supported on the tmux backend")
    }

    fn swap_surfaces(&self, _a: &str, _b: &str) -> Result<()> {
        anyhow::bail!("swap_surfaces not supported on the tmux backend")
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
    Close {
        pane_id: String,
        reply: SyncSender<Result<()>>,
    },
    Rename {
        pane_id: String,
        title: String,
        reply: SyncSender<Result<()>>,
    },
    SetColor {
        pane_id: String,
        color: [u8; 4],
        reply: SyncSender<Result<()>>,
    },
    Swap {
        a: String,
        b: String,
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
    /// PID of the active pane's shell, refreshed alongside surfaces. The git
    /// panel resolves the terminal's current directory and foreground program
    /// (for the AI-commit button) from this pid, live.
    pub active_shell_pid: Option<u32>,
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

    fn close_surface(&self, surface_id: &str) -> Result<()> {
        let id = surface_id.to_string();
        self.submit(|reply| PtyCommand::Close { pane_id: id, reply })
    }

    fn rename_surface(&self, surface_id: &str, title: &str) -> Result<()> {
        let id = surface_id.to_string();
        let title = title.to_string();
        self.submit(|reply| PtyCommand::Rename { pane_id: id, title, reply })
    }

    fn set_color(&self, surface_id: &str, color: [u8; 4]) -> Result<()> {
        let id = surface_id.to_string();
        self.submit(|reply| PtyCommand::SetColor { pane_id: id, color, reply })
    }

    fn swap_surfaces(&self, a: &str, b: &str) -> Result<()> {
        let a = a.to_string();
        let b = b.to_string();
        self.submit(|reply| PtyCommand::Swap { a, b, reply })
    }

    fn active_cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.handle.snapshot.lock().unwrap().active_shell_pid?;
        pid_cwd(pid)
    }

    fn active_process_name(&self) -> Option<String> {
        // Resolve live like active_cwd. Baking it into the snapshot only
        // refreshes on focus/split/etc., so "claude launched in an already-
        // focused pane" would be missed and read as the shell.
        let pid = self.handle.snapshot.lock().unwrap().active_shell_pid?;
        pid_process_name(pid)
    }
}

/// Resolve a process's current working directory via lsof. macOS has no
/// `/proc`; `lsof -d cwd` prints the cwd path. Called ~once/sec by the git
/// panel poll, so the subprocess cost is acceptable.
fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(std::path::PathBuf::from))
}

/// Foreground program of a shell pid: the comm of its most-recently-spawned
/// child (e.g. "claude"). Resolved live from `ps` so the AI-commit button
/// sees a claude started after the last snapshot, mirroring pid_cwd.
fn pid_process_name(shell_pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut best: Option<(u32, String)> = None;
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let (row_pid, row_ppid) = match (
            parts.next().and_then(|x| x.parse::<u32>().ok()),
            parts.next().and_then(|x| x.parse::<u32>().ok()),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        if row_ppid != shell_pid {
            continue;
        }
        let comm = parts.collect::<Vec<_>>().join(" ");
        let name = std::path::Path::new(&comm)
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or(&comm)
            .to_string();
        if best.as_ref().map_or(true, |(p, _)| row_pid > *p) {
            best = Some((row_pid, name));
        }
    }
    best.map(|(_, n)| n)
}

/// Persist each pane's cwd + claude session id so the next launch can restore
/// them. Written on exit, read by start_pty. Best-effort; failures are silent.
pub fn save_session(
    pty: &std::collections::HashMap<String, std::sync::Arc<pty_backend::PtySession>>,
) {
    use std::io::Write;
    let mut ids: Vec<&String> = pty.keys().collect();
    ids.sort(); // %0, %1, … — stable restore order
    let mut panes = Vec::new();
    for pane_id in ids {
        let sess = &pty[pane_id];
        let Some(pid) = sess.shell_pid() else { continue };
        let Some(cwd) = pid_cwd(pid) else { continue };
        let was_claude = sess
            .active_process_name()
            .map_or(false, |p| p.contains("claude"));
        panes.push(serde_json::json!({
            "cwd": cwd.to_string_lossy(),
            "was_claude": was_claude,
            "session_id": latest_claude_session_id(&cwd),
        }));
    }
    if panes.is_empty() {
        return;
    }
    let Some(path) = session_file_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "panes": panes }).to_string();
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}

pub fn session_file_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/session.json"))
}

/// One pane's restore record, as written by save_session.
pub struct PaneRestore {
    pub cwd: std::path::PathBuf,
    pub was_claude: bool,
    pub session_id: Option<String>,
}

/// Read the saved session for restore on launch. None if no file / empty.
pub fn load_session() -> Option<Vec<PaneRestore>> {
    let path = session_file_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let arr = v.get("panes")?.as_array()?;
    let mut out = Vec::new();
    for p in arr {
        let Some(cwd) = p.get("cwd").and_then(|x| x.as_str()) else { continue };
        out.push(PaneRestore {
            cwd: std::path::PathBuf::from(cwd),
            was_claude: p.get("was_claude").and_then(|x| x.as_bool()).unwrap_or(false),
            session_id: p.get("session_id").and_then(|x| x.as_str()).map(String::from),
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// claude stores sessions under ~/.claude/projects/<encoded-cwd>/<uuid>.jsonl,
/// where the abs cwd is encoded by replacing `/` and `.` with `-`. The newest
/// .jsonl there is the session the pane was last on.
fn latest_claude_session_id(cwd: &std::path::Path) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
    let dir = std::path::PathBuf::from(home)
        .join(".claude/projects")
        .join(encoded);
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else { continue };
        let Some(id) = p.file_stem().and_then(|x| x.to_str()) else { continue };
        if newest.as_ref().map_or(true, |(t, _)| mtime > *t) {
            newest = Some((mtime, id.to_string()));
        }
    }
    newest.map(|(_, id)| id)
}
