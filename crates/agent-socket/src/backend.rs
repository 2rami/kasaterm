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
    /// Close (kill) a surface by id. Removes its leaf from the layout.
    fn close_surface(&self, surface_id: &str) -> Result<()>;
    /// Set a surface's header title (rename).
    fn rename_surface(&self, surface_id: &str, title: &str) -> Result<()>;
    /// Set a surface's accent color (header band), RGBA 0..255.
    fn set_color(&self, surface_id: &str, color: [u8; 4]) -> Result<()>;
    /// Swap two surfaces' positions in the layout.
    fn swap_surfaces(&self, a: &str, b: &str) -> Result<()>;
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
}
