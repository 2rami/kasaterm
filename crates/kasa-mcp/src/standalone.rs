//! A headless `Backend` for the standalone webview server (the `kasa-serve-web`
//! bin). kasaterm normally serves the arona-ui webview off 8765 via
//! `spawn_http_server`, so when the terminal exits the webview goes blind. This
//! backend lets a tiny stand-alone process keep serving the *daemon* view —
//! `claude agents` background sessions and their transcripts — with no GUI, no
//! live panes, just the `~/.claude/projects` files on disk.
//!
//! The `Backend` trait already defaults 57 of its methods to a safe
//! bail/empty, so this only writes the 7 required-no-default methods (live-pane
//! ops that are meaningless here → bail; list queries → empty) plus the 3 that
//! actually do work off disk: `active_cwd`, `recent_sessions`,
//! `session_transcript_raw`. `/background-agents` needs no method — its handler
//! shells out to `claude agents --json --all` and only reads
//! `pane_session_ids()` (default empty), so background sessions simply come
//! back without a `parentSurface` tag, which is correct for a paneless host.

use std::path::PathBuf;

use anyhow::Result;
use kasa_socket::backend::{Backend, RecentSession, SplitDirection, SurfaceInfo, WorkspaceInfo};
use kasa_socket::sessions::{is_uuid, recent_sessions_for, session_jsonl_path};

pub struct StandaloneBackend {
    /// The cwd all disk lookups resolve against when a caller doesn't pass one.
    /// There's no GUI proxy to ask for the "active" pane cwd, so it's fixed at
    /// construction (from `--cwd` or the process cwd). `recent_sessions` and
    /// `session_transcript_raw` fall back to this when their `cwd` arg is None.
    root: PathBuf,
}

impl StandaloneBackend {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Backend for StandaloneBackend {
    // --- required (no trait default) ---------------------------------------
    // Standalone has no live panes: list queries return empty, pane ops bail.
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(Vec::new())
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(None)
    }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(Vec::new())
    }
    fn focus_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("standalone webview server has no live panes")
    }
    fn split_surface(&self, _direction: SplitDirection, _focus: bool) -> Result<SurfaceInfo> {
        anyhow::bail!("standalone webview server has no live panes")
    }
    fn send_text(&self, _surface_id: Option<&str>, _text: &str) -> Result<()> {
        anyhow::bail!("standalone webview server has no live panes")
    }
    fn send_key(&self, _surface_id: Option<&str>, _key: &str) -> Result<()> {
        anyhow::bail!("standalone webview server has no live panes")
    }

    // --- the 3 that actually work off disk ---------------------------------
    fn active_cwd(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn recent_sessions(&self, cwd: Option<&str>) -> Result<Vec<RecentSession>> {
        let base = cwd.map(PathBuf::from).unwrap_or_else(|| self.root.clone());
        Ok(recent_sessions_for(&base, 20))
    }

    fn session_transcript_raw(&self, id: &str, cwd: Option<&str>) -> Result<String> {
        // Offline read by uuid — same resolution as PtyBackend, minus the live
        // pane: uuid guard → cwd (arg else root) → jsonl path → read.
        if !is_uuid(id) {
            anyhow::bail!("invalid session id: {id}");
        }
        let base = cwd
            .map(PathBuf::from)
            .or_else(|| self.active_cwd())
            .ok_or_else(|| anyhow::anyhow!("no cwd for session {id}"))?;
        let path = session_jsonl_path(&base, id)
            .ok_or_else(|| anyhow::anyhow!("no HOME — cannot locate session {id}"))?;
        std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read session transcript {path:?}: {e}"))
    }
}
