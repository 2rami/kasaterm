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
/// `collab.board`, written by `collab.announce`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneActivity {
    pub surface_id: String,
    /// What this pane is doing and *why*, free text. The "why" is the
    /// point — "fixing the login 500 in auth.ts" lets a sibling realise
    /// it's on the same bug, where a bare "editing auth.ts" wouldn't.
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
    /// Restore a saved (on-disk, not-yet-live) session at index `idx` in the
    /// saved-session list — spawns its panes lazily and switches to it.
    /// Default unsupported.
    fn restore_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("restore_session not supported")
    }
    /// Open a preview window for a file. `kind` is "image" or "markdown";
    /// `path` is an absolute path on the host. The host spawns a separate
    /// wry webview window (image viewer / markdown editor). Default
    /// unsupported (e.g. the legacy tmux backend has no window host).
    fn open_preview(&self, _kind: &str, _path: &str) -> Result<()> {
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

    /// Publish (or overwrite) this pane's activity on the shared
    /// collaboration board. Pure metadata, so a backend can satisfy this
    /// without touching the renderer. Default unsupported.
    fn announce(
        &self,
        _surface_id: &str,
        _intent: &str,
        _status: &str,
        _files: &[String],
    ) -> Result<()> {
        anyhow::bail!("announce not supported")
    }

    /// Read every pane's announced activity. Default: empty board — a
    /// backend that doesn't track activity reports nothing rather than
    /// erroring, so callers can always scan.
    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
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
}
