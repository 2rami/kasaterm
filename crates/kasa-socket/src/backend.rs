//! Behavior delegation. The protocol crate owns framing and dispatch;
//! the embedding host (tmuxify, kasaterm-sugarloaf-cli, etc.) plugs in
//! a `Backend` that translates method calls into actual terminal
//! operations.
//!
//! The trait is intentionally small. Methods that don't have a concrete
//! mapping yet (notifications, sidebar metadata) return a default
//! "unsupported" error from the dispatcher rather than forcing every
//! backend to stub them out — the trait grows as the feature set does.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Direction passed to `Backend::split_surface`. Mirrors cmux's
/// `surface.split` `direction` parameter exactly so the JSON enum
/// values are stable wire shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Which standalone panel window a panel command targets. The git status
/// and sessions panels are separate OS windows (wry webviews) the host
/// spawns next to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelKind {
    Git,
    Session,
    Board,
}

/// Geometry of a panel window and its embedded webview, returned by
/// `panel_info`. When the panel is responsive the `view_*` (webview)
/// dimensions track the `win_*` (window) ones; a mismatch means the
/// webview failed to follow a window resize — the bug this lets a caller
/// verify without a screenshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelGeom {
    pub open: bool,
    pub win_w: u32,
    pub win_h: u32,
    pub view_w: u32,
    pub view_h: u32,
}

/// A workspace as seen by the protocol — analogous to a tmux session
/// or a cmux workspace. Returned by `workspace.list` /
/// `workspace.current`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
}

/// A surface (pane) inside a workspace. Returned by `surface.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceInfo {
    pub id: String,
    pub workspace_id: String,
    /// Optional pane title. cmux populates this from the OSC 0/2 the
    /// inner shell emits; we forward whatever tmux-bridge captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Multi-session (tmux-style tab) state for the session panel. `count`
/// is the total number of live sessions; `active` is the index of the
/// currently visible one. Default is a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsInfo {
    pub count: usize,
    pub active: usize,
    /// Saved (persisted) sessions from the last shutdown. Surfaced in the
    /// session panel so the user can manually restore them — auto-restore
    /// at launch was retired in favour of "light-launch fresh single pane".
    /// Each entry is a short label (typically the first leaf's cwd basename)
    /// the panel renders as a one-click restore row. Empty when no saved
    /// state is on disk.
    #[serde(default)]
    pub saved: Vec<String>,
    /// Per-session display labels for the LIVE sessions the daemon is
    /// hosting right now — typically each session's active-window
    /// first-pane cwd basename — so the panel shows folder names instead
    /// of "세션 1/2". Parallel to the live session list (index = session
    /// idx); empty falls back to ordinal labels. Distinct from `saved`,
    /// which lists on-disk COLD sessions from a previous shutdown.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Default for SessionsInfo {
    fn default() -> Self {
        Self { count: 1, active: 0, saved: Vec::new(), labels: Vec::new() }
    }
}

/// One pane's self-reported activity, published to a shared board so
/// sibling panes can coordinate without a human relaying between them:
/// avoid editing the same file, wait out a neighbour's build, or notice
/// two panes are chasing the same problem and join forces. Pure
/// metadata — nothing here touches terminal I/O. Returned by
/// `collab.board`, filled by the transcript watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneActivity {
    pub surface_id: String,
    /// The pane session's auto-generated title (`ai-title` line in the
    /// transcript), e.g. "Git 패널 stage 기능". One line saying what this pane
    /// is *for*, as a whole — the board's headline label. Empty until claude
    /// names the session.
    #[serde(default)]
    pub title: String,
    /// The latest user prompt (`last-prompt` line), i.e. what this pane was
    /// just told to do. Empty if nothing's been asked yet.
    #[serde(default)]
    pub last_prompt: String,
    /// The pane's most recent assistant reply text (trimmed/clipped), so the
    /// board shows what claude last *said*, not just what tool it ran.
    #[serde(default)]
    pub last_reply: String,
    /// The most recent tool call as a short label ("Edit auth.ts"), derived
    /// from the transcript's last `tool_use`. What the pane is touching right
    /// now; pairs with `files` for conflict detection.
    pub intent: String,
    /// Coarse state for at-a-glance scanning: conventionally one of
    /// "working" | "building" | "blocked" | "idle", but free text is
    /// allowed so a pane can be specific ("running test suite").
    pub status: String,
    /// Files this pane is currently touching. The conflict-detection
    /// signal: a sibling checks the board before editing and backs off
    /// if a path it wants is already claimed here.
    #[serde(default)]
    pub files: Vec<String>,
    /// The pane's visible screen tail as plain text — only filled when a
    /// caller asks (`collab.board {screen_lines: N}`). Lets an orchestrator
    /// pane read what a sibling is showing (a prompt it's stuck on, an
    /// AskUserQuestion menu) straight from the board, without a separate
    /// `surface.peek` per pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// Why this pane is `status == "waiting"` — the `waitingFor` field from
    /// `claude agents --json` (2.1.162+), e.g. "permission" or "user input".
    /// The transcript watcher can't see this: when claude blocks on a
    /// permission prompt it writes nothing, so the watcher would read the
    /// pane as idle. Only the official `agents --json` poll knows, so this
    /// is always agents-sourced and overrides the watcher's guess. `None`
    /// unless `status == "waiting"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
}

/// One live session from `claude agents --json` (Claude Code 2.1.162+).
/// Only the fields we consume are named; serde ignores the rest (`pid`,
/// `cwd`, `kind`, `startedAt`, `name`). `sessionId` is the join key: it
/// equals the stem of the pane's bound transcript path
/// (`~/.claude/projects/<cwd>/<sessionId>.jsonl`), so the watcher maps a
/// session back to its pane without tracking pids.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentStatus {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// "idle" | "busy" | "waiting".
    pub status: String,
    /// What a `waiting` session is blocked on (e.g. "permission"). 2.1.162+.
    #[serde(rename = "waitingFor", default)]
    pub waiting_for: Option<String>,
}

/// Parse `claude agents --json` stdout into a `sessionId → AgentStatus` map.
/// Pure (string in, map out) so the watcher's subprocess plumbing stays
/// separate from the parse and the parse is unit-testable. Any parse
/// failure or empty output yields an empty map — the caller then leaves the
/// transcript-derived status untouched (fail safe, never worse than today).
pub fn parse_agents_json(stdout: &str) -> std::collections::HashMap<String, AgentStatus> {
    serde_json::from_str::<Vec<AgentStatus>>(stdout)
        .map(|v| v.into_iter().map(|a| (a.session_id.clone(), a)).collect())
        .unwrap_or_default()
}

/// One pane's rectangle in the visible window, as percentages (0..100) of
/// the window's width/height. Percentages rather than cells so a caller
/// (claude deciding where to open a result pane, say) can reason about
/// "right half / top third" without knowing the pixel size. `x,y` is the
/// top-left corner; `w,h` the size. Returned by `window.layout`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneRect {
    pub surface_id: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// One window in the active session, with its panes and their rects.
/// `surface.list` and `window.layout` only ever expose the *active*
/// window, but the daemon holds every window — this lets an agent inspect
/// a window it isn't currently viewing ("what's in window 1, who's there,
/// how is it split"). `idx` matches the left sidebar's window order.
/// Returned by `window.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowOverview {
    pub idx: usize,
    pub active: bool,
    pub surfaces: Vec<String>,
    pub panes: Vec<PaneRect>,
}

/// Plug point for terminal operations. Host apps implement this on a
/// type that already owns the tmux session / portable-pty handle and
/// the renderer state.
pub trait Backend: Send + Sync {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>>;
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>>;
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>>;
    fn focus_surface(&self, surface_id: &str) -> Result<()>;
    fn split_surface(&self, direction: SplitDirection) -> Result<SurfaceInfo>;
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()>;
    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()>;
    /// Send raw bytes straight to a surface's PTY (no symbolic-key mapping).
    /// The GUI client forwards key input to a daemon-hosted pane this way so
    /// escapes/UTF-8/control bytes pass through verbatim. Default unsupported.
    fn send_raw(&self, _surface_id: Option<&str>, _bytes: &[u8]) -> Result<()> {
        anyhow::bail!("send_raw unsupported by this backend")
    }
    /// Resize a surface's PTY grid to cols×rows (drives SIGWINCH). Default
    /// unsupported.
    fn resize_surface(&self, _surface_id: &str, _cols: u16, _rows: u16) -> Result<()> {
        anyhow::bail!("resize_surface unsupported by this backend")
    }
    /// Scroll a surface's scrollback by `lines` (negative = toward older
    /// history). Default unsupported.
    fn scroll_surface(&self, _surface_id: &str, _lines: i32) -> Result<()> {
        anyhow::bail!("scroll_surface unsupported by this backend")
    }
    /// Close (kill) a surface by id. Removes its leaf from the layout.
    /// Default: unsupported — layout-managing backends (PTY) override it.
    fn close_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("close_surface unsupported by this backend")
    }
    /// Fold a surface into its session's dock — the layout leaf is removed but
    /// the PTY stays alive (kill-free). Default: unsupported.
    fn dock_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("dock_surface unsupported by this backend")
    }
    /// Restore a docked surface back into the active window. Default: unsupported.
    fn undock_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("undock_surface unsupported by this backend")
    }
    /// Set a surface's header title (rename). Default: unsupported.
    fn rename_surface(&self, _surface_id: &str, _title: &str) -> Result<()> {
        anyhow::bail!("rename_surface unsupported by this backend")
    }
    /// Set a surface's accent color (header band), RGBA 0..255.
    /// Default: unsupported.
    fn set_color(&self, _surface_id: &str, _color: [u8; 4]) -> Result<()> {
        anyhow::bail!("set_color unsupported by this backend")
    }
    /// Swap two surfaces' positions in the layout. Default: unsupported.
    fn swap_surfaces(&self, _a: &str, _b: &str) -> Result<()> {
        anyhow::bail!("swap_surfaces unsupported by this backend")
    }
    /// Move `surface_id` beside `target` along `direction` — detach its leaf
    /// and re-insert next to target. The PTY stays alive (pure layout move,
    /// unlike close). Drag-and-drop relocation routes through this so the
    /// daemon stays the layout authority. Default: unsupported.
    fn move_surface(&self, _surface_id: &str, _target: &str, _direction: SplitDirection) -> Result<()> {
        anyhow::bail!("move_surface unsupported by this backend")
    }
    /// Set the split ratio at `path` (the seam the GUI just dragged) so the
    /// daemon — the layout authority — persists it and restores it on restart.
    /// `path` is the tree route to the owning Split node (0 = child a, 1 = b).
    /// Default: unsupported.
    fn resize_divider(&self, _path: &[u8], _ratio: f32) -> Result<()> {
        anyhow::bail!("resize_divider unsupported by this backend")
    }
    /// Current working directory of the active pane's shell, if the backend
    /// tracks it. Lets the git panel follow the user's terminal directory.
    /// Default `None` (e.g. the tmux backend doesn't track per-pane cwd).
    fn active_cwd(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Foreground process name of the active pane (e.g. "claude", "zsh").
    /// Lets the AI-commit button decide whether to delegate the commit to a
    /// running claude or fall back. Default `None`.
    fn active_process_name(&self) -> Option<String> {
        None
    }
    /// Fire a "work complete" notification for a surface. The push half of
    /// the board (which is pull-only): a claude `Stop` hook runs
    /// `kasaterm-cli notify`, the host decides whether to raise a desktop
    /// alert (suppressed when that pane is already focused, cmux-style) and
    /// flashes the pane / sidebar. Default unsupported.
    fn notify(&self, _surface_id: &str, _title: &str, _body: &str) -> Result<()> {
        anyhow::bail!("notify unsupported by this backend")
    }
    /// Multi-session (tmux-style tab) state for the session panel. Default
    /// is a single session — backends that don't support sessions just
    /// report one.
    fn sessions(&self) -> SessionsInfo {
        SessionsInfo::default()
    }
    /// Switch the visible session to index `idx`. Default unsupported.
    fn switch_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("switch_session not supported")
    }
    /// Switch the visible window (tmux-style tab within the current session,
    /// shown in the left sidebar) to index `idx`. Default unsupported.
    fn switch_window(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("switch_window not supported")
    }
    /// Reorder the window at index `from` to index `to` within the active
    /// session's window list (sidebar tab drag-reorder). The daemon owns the
    /// window order, so the GUI routes the drop through this. Default unsupported.
    fn reorder_window(&self, _from: usize, _to: usize) -> Result<()> {
        anyhow::bail!("reorder_window not supported")
    }
    /// Create a fresh session and switch to it. Default unsupported.
    fn new_session(&self) -> Result<()> {
        anyhow::bail!("new_session not supported")
    }
    /// Create a fresh window in the current session and switch to it. Default
    /// unsupported.
    fn new_window(&self) -> Result<()> {
        anyhow::bail!("new_window not supported")
    }
    /// Close the session at index `idx`. Backends must keep at least one
    /// session alive (closing the last is rejected). Default unsupported.
    fn close_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("close_session not supported")
    }
    /// Close the window at index `idx` in the active session, reaping its
    /// panes. Backends must keep at least one window alive (closing the last
    /// is rejected). Authoritative window teardown so a GUI need not fake it
    /// by closing panes individually off a stale local layout. Default
    /// unsupported.
    fn close_window(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("close_window not supported")
    }
    /// Restore a saved (on-disk, not-yet-live) session at index `idx` in the
    /// saved-session list — spawns its panes lazily and switches to it.
    /// Default unsupported.
    fn restore_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("restore_session not supported")
    }
    /// Give the session at index `idx` a custom display name (overrides the
    /// auto-derived cwd-basename label). An empty/blank name clears it back to
    /// the auto label. Default unsupported.
    fn rename_session(&self, _idx: usize, _name: &str) -> Result<()> {
        anyhow::bail!("rename_session not supported")
    }
    /// Tear down every session and pane, then leave a single fresh empty
    /// session — the panel's "reset everything" button. Default unsupported.
    fn reset_sessions(&self) -> Result<()> {
        anyhow::bail!("reset_sessions not supported")
    }
    /// Open a preview pane for a file. `kind` is "image" or "markdown";
    /// `path` is an absolute path on the host. `target` is the pane that
    /// requested it (from `$KASATERM_PANE_ID` via imgopen) so the preview
    /// splits beside the *working* pane, not whatever window the sidebar
    /// last focused; None falls back to the active pane. Default unsupported
    /// (e.g. the legacy tmux backend has no window host).
    fn open_preview(&self, _kind: &str, _path: &str, _target: Option<&str>) -> Result<()> {
        anyhow::bail!("open_preview not supported")
    }
    /// Open or close a standalone panel window (git status / sessions).
    /// Default unsupported (e.g. the legacy tmux backend has no window host).
    fn set_panel(&self, _which: PanelKind, _open: bool) -> Result<()> {
        anyhow::bail!("set_panel not supported")
    }
    /// Resize a panel window to `w`x`h` logical px and re-bound its webview
    /// to match. Errors if the panel isn't open. Default unsupported.
    fn resize_panel(&self, _which: PanelKind, _w: u32, _h: u32) -> Result<()> {
        anyhow::bail!("resize_panel not supported")
    }
    /// Report a panel window's geometry (window + webview bounds) so a
    /// caller can verify the webview tracks the window. Default unsupported.
    fn panel_info(&self, _which: PanelKind) -> Result<PanelGeom> {
        anyhow::bail!("panel_info not supported")
    }

    /// Read every pane's activity. Default: empty board — a backend that
    /// doesn't track activity reports nothing rather than erroring, so
    /// callers can always scan.
    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        Ok(Vec::new())
    }

    /// Geometry of the panes in the visible window, as window-relative
    /// percentages — so a caller can see who sits where (right half, top
    /// third) and pick a spot to split. Default: empty (backends that don't
    /// track a layout report nothing rather than erroring).
    fn window_layout(&self) -> Result<Vec<PaneRect>> {
        Ok(Vec::new())
    }

    /// Every window in the active session, each with its panes and rects —
    /// unlike `window_layout`/`list_surfaces`, which only expose the active
    /// window. Lets an agent inspect a window it isn't viewing ("what's in
    /// window 1"). Default: empty (single-window backends report nothing
    /// beyond what `window_layout` already gives).
    fn windows_overview(&self) -> Result<Vec<WindowOverview>> {
        Ok(Vec::new())
    }

    /// Read the visible screen text (last `lines` rows) of a pane so a
    /// sibling can check on a build or long-running job without focusing
    /// it. Default unsupported.
    fn peek(&self, _surface_id: &str, _lines: usize) -> Result<String> {
        anyhow::bail!("peek not supported")
    }

    /// Register a pane's claude-code transcript file (the
    /// `~/.claude/projects/<cwd>/<session>.jsonl` it streams to) so the
    /// host can tail it and auto-fill that pane's board activity from the
    /// tool_use calls inside — no manual `announce` needed. Called by a
    /// SessionStart/PreToolUse hook that knows both the `transcript_path`
    /// (from its stdin) and the pane id (from `$KASATERM_PANE_ID`).
    /// Default unsupported.
    fn bind_transcript(&self, _surface_id: &str, _path: &str) -> Result<()> {
        anyhow::bail!("bind_transcript not supported")
    }

    /// Read the last `turns` conversation turns (user prompts + assistant
    /// replies) from a pane's bound transcript. Where `peek` shows the raw
    /// screen (whatever's currently rendered), this gives the structured
    /// dialogue — what a sibling claude was *asked* and what it *answered* —
    /// including turns that have already scrolled off-screen. An orchestrator
    /// pane reads this to monitor what its workers are actually doing.
    /// Default: empty (a backend that tracks no transcripts reports nothing).
    fn transcript_tail(&self, _surface_id: &str, _turns: usize) -> Result<Vec<ConversationTurn>> {
        Ok(Vec::new())
    }
}

/// One turn of a pane's claude conversation, extracted from its transcript
/// jsonl. `role` is "user" (a typed prompt — tool_results are skipped as
/// noise) or "assistant" (the reply text, tool_use blocks dropped). Returned
/// by `transcript_tail` / `collab.transcript`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agents_keys_by_session_and_reads_waiting_for() {
        // Real `claude agents --json` shape: extra fields (pid/cwd/…) ignored,
        // a waiting session carries waitingFor, others omit it.
        let json = r#"[
            {"pid":284,"cwd":"/a","kind":"interactive","startedAt":1,"sessionId":"sess-idle","status":"idle"},
            {"pid":99,"cwd":"/b","kind":"interactive","startedAt":2,"sessionId":"sess-busy","name":"그림","status":"busy"},
            {"pid":12,"cwd":"/c","kind":"interactive","startedAt":3,"sessionId":"sess-wait","status":"waiting","waitingFor":"permission"}
        ]"#;
        let map = parse_agents_json(json);
        assert_eq!(map.len(), 3);
        assert_eq!(map["sess-idle"].status, "idle");
        assert_eq!(map["sess-idle"].waiting_for, None);
        assert_eq!(map["sess-busy"].status, "busy");
        assert_eq!(map["sess-wait"].status, "waiting");
        assert_eq!(map["sess-wait"].waiting_for.as_deref(), Some("permission"));
    }

    #[test]
    fn parse_agents_empty_or_garbage_is_empty_map() {
        assert!(parse_agents_json("").is_empty());
        assert!(parse_agents_json("not json").is_empty());
        assert!(parse_agents_json("[]").is_empty());
    }
}
