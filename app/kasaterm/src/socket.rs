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
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use kasa_bridge::TmuxSession;

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


/// Per-pane transcript tail state held by the watcher thread.
struct TranscriptTail {
    path: PathBuf,
    offset: u64,
    recent: VecDeque<ToolEvent>,
    last: Instant,
}

/// Background thread: tail each pane's claude transcript jsonl and keep
/// `collab_auto` filled with derived PaneActivity, so a pane's board entry
/// reflects its real tool_use without anyone calling `announce`. Polls at
/// 750ms — `notify` isn't a dependency and the board is human-read, so
/// sub-second latency is plenty. Drops tails for closed panes via `live_panes`.
///
/// The transcript path comes ONLY from the hook-driven `bind_transcript` RPC
/// (`kasaterm-bind-transcript.sh`), which reads the exact `transcript_path`
/// from claude's SessionStart/PreToolUse hook payload — the one trustworthy
/// source. A self-map via the claude process's `CLAUDE_CODE_SESSION_ID` env
/// used to back this up, but that env is inherited by child shells: a claude
/// launched from a pane carried its PARENT's session id, so every pane sharing
/// an ancestry collapsed onto one transcript. The env path is gone; a pane
/// with no hook bind simply has no board entry until its next tool call binds.
///
/// `live_panes` yields the set of surviving pane ids so tails for closed panes
/// get dropped.
pub(crate) fn spawn_transcript_watcher<L>(
    collab_auto: Arc<Mutex<HashMap<String, PaneActivity>>>,
    binds: Arc<Mutex<Vec<(String, PathBuf)>>>,
    live_panes: L,
) where
    L: Fn() -> HashSet<String> + Send + 'static,
{
    std::thread::spawn(move || {
        let mut tails: HashMap<String, TranscriptTail> = HashMap::new();
        loop {
            // 1. Drain hook-driven binds. A re-bind to a new path (claude
            //    --resume swaps the jsonl) replaces the tail and reseeds; a
            //    re-bind to the same path is a no-op so the offset survives.
            let new_binds: Vec<(String, PathBuf)> =
                std::mem::take(&mut *binds.lock().unwrap());
            for (sid, path) in new_binds {
                if tails.get(&sid).map_or(true, |t| t.path != path) {
                    let (offset, recent) = seed_tail(&path);
                    tails.insert(sid, TranscriptTail { path, offset, recent, last: Instant::now() });
                }
            }
            // 2. Drop tails/auto entries for panes that have closed.
            let live: HashSet<String> = live_panes();
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
