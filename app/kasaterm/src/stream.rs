//! Daemon→GUI screen-frame stream over a local socket, separate from the
//! JSON-RPC control socket.
//!
//! The daemon owns the PtySessions and pushes bincode-encoded `ScreenUpdate`
//! frames here; the GUI decodes them into its Workspace. One-directional and
//! high-frequency, so it's kept off the line-delimited JSON-RPC control path
//! on purpose. Attach handshake (layout, pane ids) goes over the control
//! socket; this socket carries only the screen frames.
//!
//! Wire format: a `u32` little-endian length prefix followed by that many
//! bincode bytes. One frame = one `ScreenUpdate`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kasa_socket::transport::LocalStream;
use kasa_pty::PtyLayout;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use kasa_bridge::ScreenUpdate;

/// One window inside a session: its BSP layout tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowView {
    pub layout: PtyLayout,
}

/// A non-terminal pane the daemon hosts (image / markdown preview). These have
/// no PTY and emit no frames, so the daemon can't ship them as a screen grid —
/// it ships the kind + file path and the GUI decodes/renders locally (same as
/// the in-process `split_image_pane`/`split_markdown_pane` path).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PanePreview {
    /// "image" | "markdown".
    pub kind: String,
    pub path: String,
}

/// A pane folded into the dock: its id + a display label (cwd basename).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DockedView {
    pub id: String,
    pub label: String,
}

/// One pane's coarse activity, projected from the daemon's transcript watcher
/// (`PaneActivity`) for the GUI's working indicator + completion toast. `busy`
/// is simply `status != "idle"`. Derives `PartialEq` so the GUI's repaint gate
/// can tell when a pane flipped working↔idle and force a frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PaneStatusView {
    /// "working" | "building" | "blocked" | "idle" | "waiting" (free text from
    /// the watcher; "waiting" is agents-sourced — claude blocked on a prompt).
    pub status: String,
    /// Free-text "what + why" the pane is doing — shown in the completion toast
    /// so "%3 완료" becomes "%3 완료 · git 패널 통합".
    pub intent: String,
    /// Why `status == "waiting"` (agents --json `waitingFor`), so the GUI can
    /// label "⚠ 권한 대기중". `None` unless waiting. The GUI's repaint gate keys
    /// on `PartialEq`, so a change here forces a frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
}

/// One session: its windows + which is active + a display label (the active
/// pane's cwd basename, for the session panel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub windows: Vec<WindowView>,
    pub active_window: usize,
    pub label: String,
    /// Panes folded into this session's dock — bottom-bar chips. Per-session so
    /// a session switch shows only this 지구's dock. `#[serde(default)]` so an
    /// older stream/disk without the field still deserializes.
    #[serde(default)]
    pub docked: Vec<DockedView>,
}

/// The daemon's full session>window>pane structure pushed to the GUI. The GUI
/// renders the active session's active window and uses the rest for the
/// sidebar (windows) + session panel. Replaces the single-tree `Layout` msg —
/// a single-pane daemon is just one session with one window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateView {
    pub sessions: Vec<SessionView>,
    pub active_session: usize,
    /// The daemon's authoritative active pane id. The GUI adopts this directly
    /// instead of guessing — a stale guess (e.g. pointing at a just-closed
    /// pane) is what made a close drop to the single-pane fallback and draw a
    /// ghost. Optional so an older daemon stream still deserializes.
    #[serde(default)]
    pub active_pane: Option<String>,
    /// Per-pane shell cwd (pane id → absolute path), for the GUI header
    /// breadcrumb. The daemon owns the PtySessions, so only it can resolve
    /// these; a daemon-side poll re-broadcasts the state when a `cd` moves one.
    /// Optional so an older daemon stream still deserializes.
    #[serde(default)]
    pub pane_cwds: std::collections::HashMap<String, String>,
    /// Per-pane controlling tty short name (pane id → "ttys004"), for the pane
    /// header — ghostty / Terminal.app surface this. The daemon resolves it (it
    /// owns the PtySessions). Immutable per pane, so it rides the StateView.
    /// Optional so an older daemon stream still deserializes.
    #[serde(default)]
    pub pane_ttys: std::collections::HashMap<String, String>,
    /// Non-terminal panes (image / markdown), keyed by pane id. The GUI builds
    /// a `PaneContent::Image/Markdown` for each from the path. Optional so an
    /// older daemon stream still deserializes.
    #[serde(default)]
    pub pane_previews: std::collections::HashMap<String, PanePreview>,
    /// Per-pane collab activity (busy/idle + intent), so the GUI draws a working
    /// indicator in the pane header AND the sidebar window list — for every pane
    /// across all windows, not just the visible one. The visible window could
    /// scan its own screen for a spinner glyph, but off-screen windows can't, so
    /// this StateView field is the single cross-window source. Sourced from the
    /// transcript watcher's `collab_auto`. Optional/default so an older daemon
    /// stream still deserializes.
    #[serde(default)]
    pub pane_activity: std::collections::HashMap<String, PaneStatusView>,
}

/// Daemon→GUI stream message. `Frame` carries a screen diff (per pane, keyed by
/// `pane_id`); `State` carries the whole session>window>pane structure after a
/// split/close/new/switch so the GUI re-lays-out. Multiplexed over the one
/// stream socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamMsg {
    Frame(ScreenUpdate),
    State(StateView),
}

/// `<control-socket>-stream` — the screen-frame socket path derived from the
/// control socket path, so discovery only needs the one base path.
pub fn stream_path(control: &Path) -> PathBuf {
    let mut s = control.as_os_str().to_os_string();
    s.push("-stream");
    PathBuf::from(s)
}

/// Write one length-prefixed bincode message and flush.
pub fn write_msg(w: &mut impl Write, msg: &StreamMsg) -> io::Result<()> {
    let bytes = bincode::serialize(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = bytes.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read one length-prefixed bincode message. Returns `Ok(None)` on a clean EOF
/// (peer detached) so the caller can stop the read loop without treating it as
/// an error.
pub fn read_msg(r: &mut impl Read) -> io::Result<Option<StreamMsg>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let msg = bincode::deserialize(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(msg))
}

/// GUI-side handle to the daemon's control socket. Sends fire-and-forget
/// JSON-RPC (input / resize / scroll); a background thread drains responses
/// so the daemon never blocks on an unread reply.
pub struct DaemonClient {
    ctrl: Mutex<LocalStream>,
}

impl DaemonClient {
    pub fn connect(ctrl_path: &Path) -> io::Result<Self> {
        let stream = LocalStream::connect(ctrl_path)?;
        // Drain (and discard) responses on a background thread — input RPC is
        // fire-and-forget, but an unread reply would eventually back-pressure
        // the daemon's per-connection writer.
        let mut drain = stream.try_clone()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match drain.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        Ok(Self { ctrl: Mutex::new(stream) })
    }

    fn rpc(&self, method: &str, params: Value) {
        let line = serde_json::to_string(&json!({"id": 0, "method": method, "params": params}))
            .unwrap_or_default();
        if let Ok(mut s) = self.ctrl.lock() {
            let _ = s.write_all(line.as_bytes());
            let _ = s.write_all(b"\n");
            let _ = s.flush();
        }
    }

    /// Forward raw key bytes to a pane's PTY (space-separated hex on the wire,
    /// matching the daemon's `surface.send_raw` decoder).
    pub fn send_raw(&self, surface_id: Option<&str>, bytes: &[u8]) {
        let hex: String = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut p = json!({ "hex": hex });
        if let Some(t) = surface_id {
            p["surface_id"] = json!(t);
        }
        self.rpc("surface.send_raw", p);
    }

    pub fn resize(&self, surface_id: &str, cols: u16, rows: u16) {
        self.rpc(
            "surface.resize",
            json!({ "surface_id": surface_id, "cols": cols, "rows": rows }),
        );
    }

    pub fn scroll(&self, surface_id: &str, lines: i32) {
        self.rpc(
            "surface.scroll",
            json!({ "surface_id": surface_id, "lines": lines }),
        );
    }

    /// Split the daemon's active pane along an axis (Horizontal = side-by-side,
    /// Vertical = stacked). The daemon picks the new pane id and pushes a
    /// `Layout` message back; we don't need the RPC reply.
    pub fn split_dir(&self, dir: kasa_pty::SplitDir) {
        let d = match dir {
            kasa_pty::SplitDir::Horizontal => "right",
            kasa_pty::SplitDir::Vertical => "down",
        };
        self.rpc("surface.split", json!({ "direction": d }));
    }

    pub fn close(&self, surface_id: &str) {
        self.rpc("surface.close", json!({ "surface_id": surface_id }));
    }

    /// Fold a pane into the dock (kill-free) — daemon-authoritative like close.
    pub fn dock(&self, surface_id: &str) {
        self.rpc("surface.dock", json!({ "surface_id": surface_id }));
    }

    /// Restore a docked pane into the active window.
    pub fn undock(&self, surface_id: &str) {
        self.rpc("surface.undock", json!({ "surface_id": surface_id }));
    }

    /// Move a pane beside `target` along `direction` (left/right/up/down).
    /// Daemon-authoritative layout move — PTY stays alive. Drag-and-drop
    /// relocation uses this instead of mutating the GUI-local tree (which the
    /// next daemon State would overwrite, leaving the pane dead — drag먹통).
    pub fn move_pane(&self, surface_id: &str, target: &str, direction: &str) {
        self.rpc(
            "surface.move",
            json!({ "surface_id": surface_id, "target": target, "direction": direction }),
        );
    }

    /// Switch the active session (지구) to index `idx`.
    pub fn switch_session(&self, idx: usize) {
        self.rpc("session.switch", json!({ "idx": idx }));
    }

    pub fn focus(&self, surface_id: &str) {
        self.rpc("surface.focus", json!({ "surface_id": surface_id }));
    }

    pub fn new_session(&self) {
        self.rpc("session.new", json!({}));
    }

    pub fn new_window(&self) {
        self.rpc("window.new", json!({}));
    }

    pub fn switch_window(&self, idx: usize) {
        self.rpc("window.switch", json!({ "idx": idx }));
    }

    /// Close window `idx` in the active session — daemon-authoritative, so the
    /// closed window can't resurrect on the next state push.
    pub fn close_window(&self, idx: usize) {
        self.rpc("window.close", json!({ "idx": idx }));
    }

    /// Ask the daemon to open a non-PTY preview leaf (image or markdown) for
    /// `path`. The daemon spawns the preview pane and pushes a `Layout` back,
    /// so we don't wait on the RPC reply — same fire-and-forget as `split_dir`.
    pub fn open_preview(&self, kind: &str, path: &str) {
        self.rpc("surface.open_preview", json!({ "kind": kind, "path": path }));
    }
}

/// Spawn `self --daemon --socket <ctrl>` detached. The child outlives this GUI
/// process. stdout/stderr go to a log file so daemon-side panics/errors are
/// diagnosable (stdio-null would silently swallow them).
pub fn spawn_daemon(ctrl_path: &Path) -> io::Result<()> {
    use std::process::Stdio;
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/kasaterm-daemon.log")
        .ok();
    let (out, err) = match log {
        Some(f) => match f.try_clone() {
            Ok(f2) => (Stdio::from(f), Stdio::from(f2)),
            Err(_) => (Stdio::null(), Stdio::null()),
        },
        None => (Stdio::null(), Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--daemon")
        .arg("--socket")
        .arg(ctrl_path)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    // Put the daemon in its OWN process group so it survives whatever kills
    // the GUI: Ctrl+C (SIGINT) and terminal close (SIGHUP) are delivered to
    // the foreground *group*, and the daemon must not be in it — otherwise it
    // dies with the app and the whole point (outliving the GUI) is lost.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    Ok(())
}

/// Poll-connect until the daemon's control socket accepts (or `timeout`).
pub fn wait_for_socket(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if LocalStream::connect(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    false
}
