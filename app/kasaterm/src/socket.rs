//! Backend impl that bridges agent-socket to this binary's TmuxSession.
//!
//! The single-pane PoC reports a fixed workspace + surface id ("local-0"
//! / "pane-0") because we only own one tmux pane in this binary. Once
//! tmuxify grows multi-pane support the surface ids
//! become real tmux `@N` strings and `list_surfaces` returns one entry
//! per actually-open pane.

use kasa_socket::backend::{
    Backend, PaneActivity, SplitDirection, SurfaceInfo, WorkspaceInfo,
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use kasa_bridge::TmuxSession;

use crate::transcript::snapshot_from_tail;
use crate::{UserEvent, Workspace};
use winit::event_loop::EventLoopProxy;

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

/// Local PTY-mode cmux socket backend. The socket server (claude tmux shim,
/// kasaterm-cli, pane collab) runs on its own thread and can't touch
/// `App.pty` (a plain HashMap, not Arc<Mutex>), so every pane write / split /
/// focus is routed to the GUI thread through the EventLoopProxy.
pub struct PtyBackend {
    proxy: EventLoopProxy<UserEvent>,
    ws: Arc<Mutex<Workspace>>,
    /// surface_id → claude transcript path (hook-driven via `bind_transcript`).
    /// The single source of truth for the board: `collab_board` reads each
    /// pane's transcript tail *on demand* (pull) — there is no background
    /// watcher thread filling a cache.
    bound: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// surface_id → why it's blocked (the `Notification` hook's message, may be
    /// ""). Set by `attention`, cleared by `notify` (turn done) or when the
    /// pane's transcript grows again (claude resumed). The board's only source
    /// of `waiting`: a blocked claude writes nothing, so the transcript tail
    /// can't tell `collab_board` the pane is stuck — this map can.
    attention: Arc<Mutex<HashMap<String, String>>>,
    /// Cached `claude agents --json` output (sessionId → official status:
    /// idle/busy/waiting). The board polls ~1/s; shelling out to `claude` that
    /// often both costs a process spawn and risks racing claude's session
    /// registry, so we refresh at most once every 2s.
    agents_cache: Arc<Mutex<Option<(std::time::Instant, HashMap<String, String>)>>>,
}

impl PtyBackend {
    pub fn new(proxy: EventLoopProxy<UserEvent>, ws: Arc<Mutex<Workspace>>) -> Self {
        Self {
            proxy,
            ws,
            bound: Arc::new(Mutex::new(HashMap::new())),
            attention: Arc::new(Mutex::new(HashMap::new())),
            agents_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// sessionId → official claude status (idle/busy/waiting), cached 2s.
    /// `claude agents --json` is authoritative; the transcript-mtime heuristic
    /// in `read_tail`/`snapshot_from_tail` is only a fallback for sessions
    /// claude doesn't report. One sessionId can span several processes (shells
    /// inherit the parent's session id), so we collapse to the most-active
    /// state (busy > waiting > idle).
    fn agents_status(&self) -> HashMap<String, String> {
        const TTL: std::time::Duration = std::time::Duration::from_secs(2);
        let now = std::time::Instant::now();
        if let Some((at, map)) = self.agents_cache.lock().unwrap().as_ref() {
            if now.duration_since(*at) < TTL {
                return map.clone();
            }
        }
        let mut map: HashMap<String, String> = HashMap::new();
        if let Ok(out) = std::process::Command::new("claude")
            .args(["agents", "--json"])
            .output()
        {
            if out.status.success() {
                if let Ok(items) =
                    serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
                {
                    let rank = |s: &str| match s {
                        "busy" => 3,
                        "waiting" => 2,
                        _ => 1,
                    };
                    for it in &items {
                        let (Some(sid), Some(st)) = (
                            it.get("sessionId").and_then(|v| v.as_str()),
                            it.get("status").and_then(|v| v.as_str()),
                        ) else {
                            continue;
                        };
                        let e = map
                            .entry(sid.to_string())
                            .or_insert_with(|| st.to_string());
                        if rank(st) > rank(e) {
                            *e = st.to_string();
                        }
                    }
                }
            }
        }
        *self.agents_cache.lock().unwrap() = Some((now, map.clone()));
        map
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
        let ws = self.ws.lock().unwrap();
        Ok(ws
            .panes
            .keys()
            .map(|id| SurfaceInfo {
                id: id.clone(),
                workspace_id: FIXED_WORKSPACE_ID.into(),
                title: None,
            })
            .collect())
    }

    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        let _ = self
            .proxy
            .send_event(UserEvent::SocketFocus(surface_id.to_string()));
        Ok(())
    }

    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo> {
        let dir = match direction {
            SplitDirection::Right | SplitDirection::Left => kasa_pty::SplitDir::Horizontal,
            SplitDirection::Up | SplitDirection::Down => kasa_pty::SplitDir::Vertical,
        };
        // Split runs on the GUI thread; block on a reply channel so we can hand
        // the new pane's real id back to the caller. The teammate launcher uses
        // it as the `-t` target for every follow-up send-keys — returning the
        // old "pane-new" placeholder dropped the `claude …` launch silently.
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.proxy.send_event(UserEvent::SocketSplit(dir, tx));
        let id = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "pane-new".into());
        Ok(SurfaceInfo {
            id,
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        })
    }

    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketBytes(
            surface_id.map(|s| s.to_string()),
            text.as_bytes().to_vec(),
        ));
        Ok(())
    }

    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketBytes(
            surface_id.map(|s| s.to_string()),
            key_to_bytes(key),
        ));
        Ok(())
    }

    fn bind_transcript(&self, surface_id: &str, path: &str) -> Result<()> {
        // Record the pane's transcript path; `collab_board`/`transcript_tail`
        // read it on demand. Re-binding (claude --resume swaps the jsonl)
        // replaces the entry rather than stacking.
        self.bound
            .lock()
            .unwrap()
            .insert(surface_id.to_string(), PathBuf::from(path));
        Ok(())
    }

    fn peek(&self, surface_id: &str, lines: usize) -> Result<String> {
        let ws = self.ws.lock().unwrap();
        let pane = ws
            .panes
            .get(surface_id)
            .ok_or_else(|| anyhow::anyhow!("no such pane: {surface_id}"))?;
        Ok(pane.visible_text(lines))
    }

    fn transcript_tail(
        &self,
        surface_id: &str,
        turns: usize,
    ) -> Result<Vec<kasa_socket::backend::ConversationTurn>> {
        let path = self
            .bound
            .lock()
            .unwrap()
            .get(surface_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pane {surface_id} has no bound transcript"))?;
        // Read the whole jsonl, parse every line to a turn, keep the last N.
        // Transcripts are line-appended and rarely huge; a full read keeps this
        // simple and correct (no offset bookkeeping like the watcher needs).
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read transcript {path:?}: {e}"))?;
        let mut all: Vec<kasa_socket::backend::ConversationTurn> =
            text.lines().filter_map(crate::transcript::parse_turn).collect();
        if turns > 0 && all.len() > turns {
            all.drain(0..all.len() - turns);
        }
        Ok(all)
    }

    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        // Pull, not push: read each open & bound pane's transcript tail right
        // now and derive its row. No background watcher, no cache — the board
        // is exactly as fresh as the moment it's asked for. Panes with no hook
        // bind (no claude / not started) simply don't appear.
        let live: HashSet<String> = self.ws.lock().unwrap().panes.keys().cloned().collect();
        let agents = self.agents_status();
        let bound = self.bound.lock().unwrap();
        let mut attention = self.attention.lock().unwrap();
        let mut board: Vec<PaneActivity> = bound
            .iter()
            .filter(|(sid, _)| live.contains(sid.as_str()))
            .map(|(sid, path)| {
                let (tail, mtime_idle) = read_tail(path, 64 * 1024);
                let mut row = snapshot_from_tail(sid, &tail, mtime_idle);
                // Prefer claude's official status when it reports this session
                // (matched by transcript filename stem == sessionId). The
                // mtime heuristic above is only a fallback for sessions claude
                // doesn't list. `effectively_idle` then drives the attention
                // (permission-prompt) override below.
                let official = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|stem| agents.get(stem))
                    .map(|s| s.as_str());
                let effectively_idle = match official {
                    Some("busy") => {
                        row.status = "working".into();
                        false
                    }
                    Some("waiting") => {
                        row.status = "waiting".into();
                        false
                    }
                    Some(_) => {
                        row.status = "idle".into();
                        true
                    }
                    None => mtime_idle,
                };
                // A claude blocked on a permission/input prompt writes nothing
                // and reports idle, so its Notification hook flag is the only
                // `waiting` signal. Apply it only when otherwise idle; drop the
                // stale flag once the pane is active again.
                if effectively_idle {
                    if let Some(reason) = attention.get(sid) {
                        row.status = "waiting".to_string();
                        row.waiting_for = (!reason.is_empty()).then(|| reason.clone());
                    }
                } else {
                    attention.remove(sid);
                }
                row
            })
            .collect();
        // Drop flags for panes that have closed since they were set.
        attention.retain(|sid, _| live.contains(sid.as_str()));
        board.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
        Ok(board)
    }

    fn notify(&self, surface_id: &str, title: &str, body: &str) -> Result<()> {
        // The turn finished → the pane can't still be blocked waiting. Clear any
        // attention flag so the board drops back to idle even if the resume
        // didn't write enough transcript to flip `idle` first.
        self.attention.lock().unwrap().remove(surface_id);
        // Hand off to the GUI thread — the desktop alert (objc/osascript) and
        // any pane/sidebar flash both need App state we can't touch here.
        let _ = self.proxy.send_event(UserEvent::Notify {
            surface_id: surface_id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
        });
        Ok(())
    }

    fn attention(&self, surface_id: &str, reason: &str) -> Result<()> {
        // Remember it for the board (socket-side, pull), then hand the GUI-side
        // surfacing (toast / flash / desktop alert) to the GUI thread.
        self.attention
            .lock()
            .unwrap()
            .insert(surface_id.to_string(), reason.to_string());
        let _ = self.proxy.send_event(UserEvent::Attention {
            surface_id: surface_id.to_string(),
            reason: reason.to_string(),
        });
        Ok(())
    }
}

/// Peel any trailing submit bytes (CR/LF) off `bytes`, returning
/// `(body, submit)` so a caller can ship them in two separate PTY writes.
///
/// `kasaterm-cli tell` appends `\r` to the message. When the body ends in a
/// multibyte codepoint (한글·이모지) and that codepoint shares a single write
/// with the trailing `\r`, claude (Ink) can submit on the CR before the
/// last codepoint's bytes finish arriving across the read boundary — the
/// half-delivered character is truncated into a lone UTF-16 high surrogate
/// (`\ud83c` with no low half). That poisons the session's saved transcript
/// and every later API request 400s ("no low surrogate in string"). Writing
/// the body first, then the CR on its own, keeps the codepoint whole.
pub(crate) fn split_trailing_submit(bytes: &[u8]) -> (&[u8], &[u8]) {
    let body_len = bytes
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(0, |i| i + 1);
    bytes.split_at(body_len)
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



/// Read the last `max_bytes` of a file as lossy UTF-8, plus whether it's gone
/// idle (no write in 60s — claude transcripts are append-only, so file mtime
/// is the last activity time; no need to parse ISO timestamps). The leading
/// (possibly mid-line) fragment of a tail read just fails to parse in
/// `snapshot_from_tail`, so it's harmless. Any IO error → empty + idle.
fn read_tail(path: &std::path::Path, max_bytes: u64) -> (String, bool) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return (String::new(), true);
    };
    let meta = f.metadata().ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let idle = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs() >= 60)
        .unwrap_or(true);
    if len > max_bytes {
        let _ = f.seek(SeekFrom::Start(len - max_bytes));
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    (String::from_utf8_lossy(&buf).into_owned(), idle)
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


/// Build one layout-tree leaf's restore record from a live PtySession: its
/// cwd, whether it was running claude, and the newest claude session id under
/// that cwd (for `claude --resume`). `cwd` is null when the shell pid/cwd
/// can't be resolved — restore then falls back to the default cwd.
pub fn pane_record(sess: &kasa_pty::PtySession) -> serde_json::Value {
    let shell_pid = sess.shell_pid();
    let cwd = shell_pid.and_then(pid_cwd);
    let was_claude = sess
        .active_process_name()
        .map_or(false, |p| p.contains("claude"));
    // Only record a session id for panes actually running claude. Prefer the
    // id straight off the running claude's argv (exact per-pane); two claudes
    // in the same cwd no longer collapse onto one id the way the cwd-mtime
    // guess does. Fall back to the mtime guess for a fresh `claude` whose argv
    // carries no id. Crucially the mtime fallback is INSIDE the was_claude
    // guard — otherwise a plain shell pane (no claude) would still get the
    // cwd's newest session id stapled on, so every pane sharing a cwd collapsed
    // onto one id and `claude --resume` restored the wrong/duplicate session.
    let session_id = if was_claude {
        shell_pid
            .and_then(claude_session_id_from_cmdline)
            .or_else(|| cwd.as_ref().and_then(|c| latest_claude_session_id(c)))
    } else {
        None
    };
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

fn window_size_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_WINDOW_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/window.json"))
}

fn settings_file_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_SETTINGS_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/settings.json"))
}

/// User's `default_cwd` preference for where new shells start — mirrors the
/// "working directory" setting every other terminal exposes. Returns the raw
/// string: `"last"` (inherit the spawning pane's cwd, the standard default),
/// `"home"`, or an absolute/`~`-prefixed path. Missing file/key → `"last"`.
pub fn read_default_cwd_mode() -> String {
    let fallback = || "last".to_string();
    let Some(path) = settings_file_path() else { return fallback() };
    let Ok(txt) = std::fs::read_to_string(&path) else { return fallback() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return fallback() };
    v.get("default_cwd")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(fallback)
}

/// Whole `settings.json` as a JSON object (empty object if missing/invalid).
/// The settings screen reads this once to populate its controls.
pub fn read_settings() -> serde_json::Value {
    let empty = || serde_json::json!({});
    let Some(path) = settings_file_path() else { return empty() };
    let Ok(txt) = std::fs::read_to_string(&path) else { return empty() };
    serde_json::from_str::<serde_json::Value>(&txt)
        .ok()
        .filter(|v| v.is_object())
        .unwrap_or_else(empty)
}

/// Set one key in `settings.json`, preserving every other key. Loads the
/// existing object first so writing `default_shell` never clobbers
/// `default_cwd`. Silently no-ops if the path/dir can't be resolved.
pub fn write_setting(key: &str, value: serde_json::Value) {
    use std::io::Write;
    let Some(path) = settings_file_path() else { return };
    let mut obj = match read_settings() {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(key.to_string(), value);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(txt) = serde_json::to_string_pretty(&serde_json::Value::Object(obj)) {
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(txt.as_bytes());
        }
    }
}

/// Whether the file-tree sidebar starts open on launch. Default `false`
/// (terminal-only first screen).
pub fn read_file_tree_default() -> bool {
    read_settings()
        .get("file_tree_default")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// User's preferred shell override (`default_shell` key). Empty/missing → None,
/// letting `$SHELL`/login-shell detection take over.
pub fn read_default_shell() -> Option<String> {
    read_settings()
        .get("default_shell")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Persist the last logical window size so the next launch restores it instead
/// of the hardcoded default. Logical (DPI-independent) so moving between a
/// Retina and an external display restores the same on-screen size.
pub fn write_window_size(w: f64, h: f64) {
    use std::io::Write;
    let Some(path) = window_size_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(format!("{{\"w\":{w},\"h\":{h}}}").as_bytes());
    }
}

/// Read the persisted logical window size. Rejects degenerate sizes (a window
/// minimized/zero at exit) so a bad value can't trap the next launch tiny.
pub fn read_window_size() -> Option<(f64, f64)> {
    let path = window_size_path()?;
    let txt = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let w = v.get("w")?.as_f64()?;
    let h = v.get("h")?.as_f64()?;
    if w >= 400.0 && h >= 300.0 {
        Some((w, h))
    } else {
        None
    }
}


/// Pull the claude session id straight off the running claude process's argv
/// (`--resume <uuid>` / `--session-id <uuid>`, `=`-joined or space-separated).
/// Exact per-pane — unlike the cwd-mtime guess, two claudes in the same cwd
/// keep distinct ids. Returns None for a fresh `claude` with no id on its argv.
fn claude_session_id_from_cmdline(shell_pid: u32) -> Option<String> {
    // Most-recently-spawned claude child of this shell — shared with the
    // transcript watcher's self-map path.
    let pid = claude_child_pid(shell_pid)?;
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

/// The pid of the claude child of a shell pane, if any. Picks the most-recent
/// (highest-pid) `claude`-named direct child of `shell_pid`. Returns None when
/// no claude is running under the shell.
fn claude_child_pid(shell_pid: u32) -> Option<u32> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut best: Option<u32> = None;
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
        if is_claude && best.map_or(true, |p| pid > p) {
            best = Some(pid);
        }
    }
    best
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

