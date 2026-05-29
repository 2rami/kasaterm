//! Backend impl that bridges agent-socket to this binary's TmuxSession.
//!
//! The single-pane PoC reports a fixed workspace + surface id ("local-0"
//! / "pane-0") because we only own one tmux pane in this binary. Once
//! tmuxify grows multi-pane support the surface ids
//! become real tmux `@N` strings and `list_surfaces` returns one entry
//! per actually-open pane.

use agent_socket::backend::{
    Backend, PaneActivity, PanelGeom, PanelKind, SessionsInfo, SplitDirection, SurfaceInfo,
    WorkspaceInfo,
};
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tmux_bridge::TmuxSession;

use crate::transcript::{build_activity, parse_line, ToolEvent, RECENT_MAX};

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

/// Which kind of preview window a `PtyCommand::OpenPreview` should spawn.
/// Decoded from the MCP `/open-image` vs `/open-markdown` endpoints (and
/// the `imgopen` / `mdopen` shims behind them).
#[derive(Debug, Clone, Copy)]
pub enum PreviewKind {
    Image,
    Markdown,
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
    /// Switch the visible tmux-style session to index `idx`.
    SwitchSession {
        idx: usize,
        reply: SyncSender<Result<()>>,
    },
    /// Switch the visible window (sidebar tab within the current session) to
    /// index `idx`.
    SwitchWindow {
        idx: usize,
        reply: SyncSender<Result<()>>,
    },
    /// Create a new session and switch to it.
    NewSession {
        reply: SyncSender<Result<()>>,
    },
    /// Close the tmux-style session at index `idx`.
    CloseSession {
        idx: usize,
        reply: SyncSender<Result<()>>,
    },
    /// Restore a saved (on-disk, not-yet-live) session at index `idx` in
    /// `saved_session_labels`. Spawns its panes lazily and switches to it.
    RestoreSession {
        idx: usize,
        reply: SyncSender<Result<()>>,
    },
    /// Open a separate wry preview window (image viewer / markdown editor)
    /// for the file at `path`. Window creation needs the winit
    /// `ActiveEventLoop`, which only the main thread has, so the socket
    /// worker queues this and the main loop spawns the window in its drain.
    OpenPreview {
        kind: PreviewKind,
        path: String,
        reply: SyncSender<Result<()>>,
    },
    /// Open or close a standalone panel window (git status / sessions).
    /// Window creation needs the winit `ActiveEventLoop`, which only the
    /// main thread has, so the socket worker queues this.
    SetPanel {
        which: PanelKind,
        open: bool,
        reply: SyncSender<Result<()>>,
    },
    /// Resize a panel window and re-bound its webview to match.
    ResizePanel {
        which: PanelKind,
        w: u32,
        h: u32,
        reply: SyncSender<Result<()>>,
    },
    /// Read a panel window's geometry (window + webview bounds).
    PanelInfo {
        which: PanelKind,
        reply: SyncSender<Result<PanelGeom>>,
    },
    /// Read the visible screen text (last `lines` rows, history + current
    /// screen) of a pane. Lives on the main thread because that's where
    /// the composed cell grid is — the socket worker only has snapshots.
    Peek {
        pane_id: String,
        lines: usize,
        reply: SyncSender<Result<String>>,
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
    /// Total number of tmux-style sessions. The session panel polls this to
    /// draw one tab per session. 0 until the first snapshot refresh.
    pub session_count: usize,
    /// Index of the visible session within the session list.
    pub active_session: usize,
    /// Persisted sessions from the previous shutdown — surfaced by the
    /// session panel so the user can manually restore them (auto-restore at
    /// launch was retired in favour of light-launch). Each entry is a label.
    pub saved_sessions: Vec<String>,
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
    /// Manual collaboration board: surface_id -> what that pane explicitly
    /// announced. Lives on the backend rather than the main thread because
    /// it's pure metadata — announce/board never touch the renderer, so they
    /// skip the submit() round-trip and just lock this map. The whole backend
    /// is `Arc`-shared across socket connections, so every pane sees the same
    /// board. Kept separate from `collab_auto` so the transcript tail worker
    /// and a manual announce never clobber each other.
    collab: Arc<Mutex<HashMap<String, PaneActivity>>>,
    /// Auto board: surface_id -> activity derived by tailing that pane's
    /// claude-code transcript (tool_use calls). Filled by the watcher thread,
    /// not by any RPC. `collab_board` merges this with `collab` (auto wins,
    /// since it reflects what the pane is *actually* doing) — so a pane never
    /// has to call `announce`.
    collab_auto: Arc<Mutex<HashMap<String, PaneActivity>>>,
    /// Pending `bind_transcript` requests (surface_id, jsonl path) the watcher
    /// thread drains. Pushed by the hook-driven RPC, consumed by the tail
    /// worker. Re-binding a surface (e.g. `claude --resume` swaps the file)
    /// replaces its entry.
    binds: Arc<Mutex<Vec<(String, std::path::PathBuf)>>>,
}

impl PtyBackend {
    pub fn new(handle: PtyBackendHandle) -> Self {
        let collab_auto = Arc::new(Mutex::new(HashMap::new()));
        let binds = Arc::new(Mutex::new(Vec::new()));
        // Tail each bound pane's claude transcript on a background thread,
        // auto-filling collab_auto. Shares the snapshot so it can drop tails
        // for closed panes. Renderer-free, so it never touches the main loop.
        spawn_transcript_watcher(collab_auto.clone(), binds.clone(), handle.snapshot.clone());
        Self {
            handle,
            collab: Arc::new(Mutex::new(HashMap::new())),
            collab_auto,
            binds,
        }
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

    fn sessions(&self) -> SessionsInfo {
        let snap = self.handle.snapshot.lock().unwrap();
        // Before the first snapshot refresh the count is 0; report a single
        // session so the panel never renders an empty list.
        SessionsInfo {
            count: snap.session_count.max(1),
            active: snap.active_session,
            saved: snap.saved_sessions.clone(),
        }
    }

    fn switch_session(&self, idx: usize) -> Result<()> {
        self.submit(|reply| PtyCommand::SwitchSession { idx, reply })
    }

    fn switch_window(&self, idx: usize) -> Result<()> {
        self.submit(|reply| PtyCommand::SwitchWindow { idx, reply })
    }

    fn new_session(&self) -> Result<()> {
        self.submit(|reply| PtyCommand::NewSession { reply })
    }

    fn close_session(&self, idx: usize) -> Result<()> {
        self.submit(|reply| PtyCommand::CloseSession { idx, reply })
    }

    fn restore_session(&self, idx: usize) -> Result<()> {
        self.submit(|reply| PtyCommand::RestoreSession { idx, reply })
    }

    fn open_preview(&self, kind: &str, path: &str) -> Result<()> {
        let kind = match kind {
            "image" => PreviewKind::Image,
            "markdown" => PreviewKind::Markdown,
            other => return Err(anyhow!("unknown preview kind: {other}")),
        };
        let path = path.to_string();
        self.submit(|reply| PtyCommand::OpenPreview { kind, path, reply })
    }

    fn set_panel(&self, which: PanelKind, open: bool) -> Result<()> {
        self.submit(|reply| PtyCommand::SetPanel { which, open, reply })
    }

    fn resize_panel(&self, which: PanelKind, w: u32, h: u32) -> Result<()> {
        self.submit(|reply| PtyCommand::ResizePanel { which, w, h, reply })
    }

    fn panel_info(&self, which: PanelKind) -> Result<PanelGeom> {
        self.submit(|reply| PtyCommand::PanelInfo { which, reply })
    }

    fn announce(
        &self,
        surface_id: &str,
        intent: &str,
        status: &str,
        files: &[String],
    ) -> Result<()> {
        self.collab.lock().unwrap().insert(
            surface_id.to_string(),
            PaneActivity {
                surface_id: surface_id.to_string(),
                intent: intent.to_string(),
                status: status.to_string(),
                files: files.to_vec(),
            },
        );
        Ok(())
    }

    fn peek(&self, surface_id: &str, lines: usize) -> Result<String> {
        let id = surface_id.to_string();
        self.submit(|reply| PtyCommand::Peek { pane_id: id, lines, reply })
    }

    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        // Drop entries for panes that have since closed so the board never
        // shows a ghost. The live set comes from the same snapshot
        // list_surfaces reads.
        let live: HashSet<String> = self
            .handle
            .snapshot
            .lock()
            .unwrap()
            .surfaces
            .iter()
            .map(|s| s.id.clone())
            .collect();
        // Merge manual announce (`collab`) with transcript-derived activity
        // (`collab_auto`), auto winning — it reflects what the pane is
        // *actually* doing, so a pane never has to call announce. Manual is
        // the fallback for panes with no bound transcript (codex, etc.).
        let mut by_id: HashMap<String, PaneActivity> = HashMap::new();
        for a in self.collab.lock().unwrap().values() {
            by_id.insert(a.surface_id.clone(), a.clone());
        }
        for a in self.collab_auto.lock().unwrap().values() {
            by_id.insert(a.surface_id.clone(), a.clone());
        }
        Ok(by_id
            .into_values()
            .filter(|a| live.contains(&a.surface_id))
            .collect())
    }

    fn bind_transcript(&self, surface_id: &str, path: &str) -> Result<()> {
        // Queue for the tail worker; renderer-free so no submit() round-trip.
        // Re-binding a surface (claude --resume swaps the jsonl) replaces its
        // pending entry rather than stacking.
        let mut binds = self.binds.lock().unwrap();
        binds.retain(|(s, _)| s != surface_id);
        binds.push((surface_id.to_string(), std::path::PathBuf::from(path)));
        Ok(())
    }
}

/// Per-pane transcript tail state held by the watcher thread.
struct TranscriptTail {
    path: PathBuf,
    offset: u64,
    recent: VecDeque<ToolEvent>,
    last: Instant,
}

/// Background thread: tail each bound pane's claude transcript jsonl and keep
/// `collab_auto` filled with derived PaneActivity, so a pane's board entry
/// reflects its real tool_use without anyone calling `announce`. Polls at
/// 750ms — `notify` isn't a dependency and the board is human-read, so
/// sub-second latency is plenty. Drops tails for closed panes via `snapshot`.
fn spawn_transcript_watcher(
    collab_auto: Arc<Mutex<HashMap<String, PaneActivity>>>,
    binds: Arc<Mutex<Vec<(String, PathBuf)>>>,
    snapshot: Arc<Mutex<PtySnapshot>>,
) {
    std::thread::spawn(move || {
        let mut tails: HashMap<String, TranscriptTail> = HashMap::new();
        loop {
            // 1. Drain new binds. Re-bind (claude --resume swaps the file)
            //    replaces the tail and reseeds.
            let new_binds: Vec<(String, PathBuf)> =
                std::mem::take(&mut *binds.lock().unwrap());
            for (sid, path) in new_binds {
                let (offset, recent) = seed_tail(&path);
                tails.insert(sid, TranscriptTail { path, offset, recent, last: Instant::now() });
            }
            // 2. Drop tails + auto entries for panes that have closed.
            let live: HashSet<String> = snapshot
                .lock()
                .unwrap()
                .surfaces
                .iter()
                .map(|s| s.id.clone())
                .collect();
            tails.retain(|sid, _| live.contains(sid));
            collab_auto.lock().unwrap().retain(|sid, _| live.contains(sid));
            // 3. Incremental read each tail → update recent ring → collab_auto.
            for (sid, tail) in tails.iter_mut() {
                let events = read_new_events(&tail.path, &mut tail.offset);
                if !events.is_empty() {
                    for ev in events {
                        tail.recent.push_back(ev);
                        while tail.recent.len() > RECENT_MAX {
                            tail.recent.pop_front();
                        }
                    }
                    tail.last = Instant::now();
                }
                let idle = tail.last.elapsed() > Duration::from_secs(60);
                let act = build_activity(sid, &tail.recent, idle);
                collab_auto.lock().unwrap().insert(sid.clone(), act);
            }
            std::thread::sleep(Duration::from_millis(750));
        }
    });
}

/// Read bytes appended since `offset`, parse complete lines into ToolEvents,
/// and advance `offset` to the last newline (a partial trailing line waits
/// for the next tick). Resets to 0 if the file shrank (rotation/truncate).
fn read_new_events(path: &PathBuf, offset: &mut u64) -> Vec<ToolEvent> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };
    if len < *offset {
        *offset = 0;
    }
    if len <= *offset || f.seek(SeekFrom::Start(*offset)).is_err() {
        return Vec::new();
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    let last_nl = match buf.rfind('\n') {
        Some(i) => i,
        None => return Vec::new(),
    };
    *offset += (last_nl + 1) as u64;
    buf[..=last_nl].lines().flat_map(parse_line).collect()
}

/// On bind, seed `recent` from the tail of an existing file (so a `--resume`d
/// pane shows activity right away) and set offset to EOF — the rest of the
/// history is ignored. Scans only the last ~64KB to keep it cheap.
fn seed_tail(path: &PathBuf) -> (u64, VecDeque<ToolEvent>) {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (0, VecDeque::new()),
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(64 * 1024);
    let mut recent = VecDeque::new();
    if f.seek(SeekFrom::Start(start)).is_ok() {
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() {
            // If we started mid-file, drop the first (partial) line.
            let body = if start > 0 {
                buf.find('\n').map(|i| &buf[i + 1..]).unwrap_or("")
            } else {
                &buf[..]
            };
            for ev in body.lines().flat_map(parse_line) {
                recent.push_back(ev);
                while recent.len() > RECENT_MAX {
                    recent.pop_front();
                }
            }
        }
    }
    (len, recent)
}

/// Resolve a process's current working directory via lsof. macOS has no
/// `/proc`; `lsof -d cwd` prints the cwd path. Called ~once/sec by the git
/// panel poll, so the subprocess cost is acceptable.
pub(crate) fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
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

/// Build one layout-tree leaf's restore record from a live PtySession: its
/// cwd, whether it was running claude, and the newest claude session id under
/// that cwd (for `claude --resume`). `cwd` is null when the shell pid/cwd
/// can't be resolved — restore then falls back to the default cwd.
pub fn pane_record(sess: &pty_backend::PtySession) -> serde_json::Value {
    let shell_pid = sess.shell_pid();
    let cwd = shell_pid.and_then(pid_cwd);
    let was_claude = sess
        .active_process_name()
        .map_or(false, |p| p.contains("claude"));
    // Prefer the id straight off the running claude's argv (exact per-pane);
    // two claudes in the same cwd no longer collapse onto one id the way the
    // cwd-mtime guess does. Fall back to the mtime guess for a fresh `claude`
    // whose argv carries no id.
    let session_id = shell_pid
        .filter(|_| was_claude)
        .and_then(claude_session_id_from_cmdline)
        .or_else(|| cwd.as_ref().and_then(|c| latest_claude_session_id(c)));
    serde_json::json!({
        "cwd": cwd.as_ref().map(|c| c.to_string_lossy().into_owned()),
        "was_claude": was_claude,
        "session_id": session_id,
    })
}

/// Write the full multi-session restore state (built by the caller from each
/// session's layout tree). Written on exit, read by start_pty. Best-effort;
/// failures are silent.
pub fn write_session_state(state: &serde_json::Value) {
    use std::io::Write;
    let Some(path) = session_file_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(state.to_string().as_bytes());
    }
}

pub fn session_file_path() -> Option<std::path::PathBuf> {
    // Override lets a debug instance keep its restore state out of the daily
    // app's shared file (and lets users relocate it).
    if let Ok(p) = std::env::var("KASATERM_SESSION_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/session.json"))
}

/// One pane's restore record — the payload carried by a layout-tree leaf.
#[allow(dead_code)]
pub struct PaneRestore {
    /// None when the saved cwd couldn't be resolved; restore uses the default.
    pub cwd: Option<std::path::PathBuf>,
    pub was_claude: bool,
    pub session_id: Option<String>,
    /// Saved scrollback as plain-text lines (oldest→newest), restored into the
    /// pane's history so scroll-up shows what was on screen before the restart.
    /// Empty when nothing was captured. Color/attrs are dropped in v1 — the
    /// content is what matters for "what I typed/saw is still there".
    pub scrollback: Vec<String>,
}

/// A node in a session's restore layout tree — mirrors `pty_backend::PtyLayout`
/// but carries per-pane restore data at the leaves instead of live pane ids.
pub enum RestoreNode {
    Leaf(PaneRestore),
    Split {
        /// true = side-by-side (PtyLayout Horizontal), false = stacked.
        horizontal: bool,
        ratio: f32,
        a: Box<RestoreNode>,
        b: Box<RestoreNode>,
    },
}

/// One restored session: its windows (each a layout tree) + which window was
/// active. A session can hold several windows; each window shares the
/// session's panes/ws once rebuilt.
pub struct SessionRestore {
    pub windows: Vec<RestoreNode>,
    pub active_window: usize,
}

/// Full restore state: every session (with its windows) + which one was active.
pub struct RestoreState {
    pub active_session: usize,
    pub sessions: Vec<SessionRestore>,
}

/// Read the saved session for restore on launch. Handles both the new nested
/// `{active_session, sessions:[<node>]}` format and the legacy flat
/// `{panes:[...]}` (restored as one single-pane session). None if no file.
pub fn load_session_state() -> Option<RestoreState> {
    let path = session_file_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if let Some(arr) = v.get("sessions").and_then(|x| x.as_array()) {
        let sessions: Vec<SessionRestore> = arr.iter().filter_map(parse_session).collect();
        if sessions.is_empty() {
            return None;
        }
        let active = v
            .get("active_session")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as usize;
        return Some(RestoreState {
            active_session: active.min(sessions.len() - 1),
            sessions,
        });
    }
    // Legacy flat format: one session, one window, first pane only.
    if let Some(arr) = v.get("panes").and_then(|x| x.as_array()) {
        let leaf = parse_leaf(arr.first()?);
        return Some(RestoreState {
            active_session: 0,
            sessions: vec![SessionRestore {
                windows: vec![RestoreNode::Leaf(leaf)],
                active_window: 0,
            }],
        });
    }
    None
}

/// Parse one session entry. New format carries `{windows:[<node>...],
/// active_window:N}`. Older saves stored each session as a single layout node
/// (one window) — we wrap that as a one-window session so old session files
/// still restore.
fn parse_session(v: &serde_json::Value) -> Option<SessionRestore> {
    if let Some(arr) = v.get("windows").and_then(|x| x.as_array()) {
        let windows: Vec<RestoreNode> = arr.iter().filter_map(parse_node).collect();
        if windows.is_empty() {
            return None;
        }
        let active_window = v
            .get("active_window")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as usize;
        return Some(SessionRestore {
            active_window: active_window.min(windows.len() - 1),
            windows,
        });
    }
    // Legacy: the session entry *is* a single layout node (one window).
    let node = parse_node(v)?;
    Some(SessionRestore {
        windows: vec![node],
        active_window: 0,
    })
}

fn parse_node(v: &serde_json::Value) -> Option<RestoreNode> {
    if let Some(leaf) = v.get("leaf") {
        return Some(RestoreNode::Leaf(parse_leaf(leaf)));
    }
    if let Some(split) = v.get("split") {
        let horizontal = split.get("dir").and_then(|x| x.as_str()) == Some("h");
        let ratio = split.get("ratio").and_then(|x| x.as_f64()).unwrap_or(0.5) as f32;
        let a = Box::new(parse_node(split.get("a")?)?);
        let b = Box::new(parse_node(split.get("b")?)?);
        return Some(RestoreNode::Split { horizontal, ratio, a, b });
    }
    None
}

fn parse_leaf(v: &serde_json::Value) -> PaneRestore {
    PaneRestore {
        cwd: v
            .get("cwd")
            .and_then(|x| x.as_str())
            .map(std::path::PathBuf::from),
        was_claude: v.get("was_claude").and_then(|x| x.as_bool()).unwrap_or(false),
        session_id: v.get("session_id").and_then(|x| x.as_str()).map(String::from),
        scrollback: v
            .get("scrollback")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Pull the claude session id straight off the running claude process's argv
/// (`--resume <uuid>` / `--session-id <uuid>`, `=`-joined or space-separated).
/// Exact per-pane — unlike the cwd-mtime guess, two claudes in the same cwd
/// keep distinct ids. Returns None for a fresh `claude` with no id on its argv.
fn claude_session_id_from_cmdline(shell_pid: u32) -> Option<String> {
    // Most-recently-spawned child whose comm is claude — mirrors how
    // pid_process_name picks the pane's foreground program.
    let listing = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&listing.stdout);
    let mut claude_pid: Option<u32> = None;
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let (pid, ppid) = match (
            parts.next().and_then(|x| x.parse::<u32>().ok()),
            parts.next().and_then(|x| x.parse::<u32>().ok()),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        if ppid != shell_pid {
            continue;
        }
        let comm = parts.collect::<Vec<_>>().join(" ");
        let is_claude = std::path::Path::new(&comm)
            .file_name()
            .and_then(|x| x.to_str())
            .map_or(false, |n| n.contains("claude"));
        if is_claude && claude_pid.map_or(true, |p| pid > p) {
            claude_pid = Some(pid);
        }
    }
    let pid = claude_pid?;
    let args_out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;
    let argv = String::from_utf8_lossy(&args_out.stdout);
    let tokens: Vec<&str> = argv.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        for flag in ["--resume=", "--session-id="] {
            if let Some(v) = tok.strip_prefix(flag) {
                if is_uuid(v) {
                    return Some(v.to_string());
                }
            }
        }
        if matches!(*tok, "--resume" | "-r" | "--session-id") {
            if let Some(v) = tokens.get(i + 1) {
                if is_uuid(v) {
                    return Some((*v).to_string());
                }
            }
        }
    }
    None
}

/// claude session ids are canonical UUIDs (8-4-4-4-12 hex). Validating guards
/// against grabbing a non-id token after a bare `-r`/`--resume` (the picker).
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
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
