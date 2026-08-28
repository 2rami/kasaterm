//! kasaspace MCP — streamable-HTTP MCP server exposing kasaterm pane
//! control as model-invoked tools, backed directly by the host's
//! `kasa_socket::Backend`. Replaces the external python bridge
//! (mcp/kasa_mcp.py): same tool surface, but the long-lived Rust
//! host owns the tools so it can later expose GUI state dynamically.

use std::sync::Arc;

use kasa_socket::backend::Backend;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;

pub mod character;
pub mod dispatch;
pub mod git;
pub mod gridwire;
mod http;
pub mod persona;
mod proxy;
mod register;
pub mod remote;
#[cfg(unix)]
pub mod adopt;
pub mod machines;
pub mod remoteboard;
mod resume_visibility;
pub mod standalone;
pub mod team;
pub mod tunnel;
pub use http::{claude_bin, remote_token, session_token, spawn_http_server, spawn_http_server_opts};
pub use register::register_clients;

/// `Command` with the console window suppressed on Windows. kasaterm is a GUI
/// (non-console) process, so spawning a console program (git, etc.) flashes a
/// fresh console window each call — and a polled spawn flashes it on a loop.
/// CREATE_NO_WINDOW keeps it hidden. No-op on other platforms.
pub(crate) fn no_window_command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    // `mut` 는 아래 windows 블록이 쓴다 — 떼면 그쪽 빌드가 깨지므로
    // 다른 플랫폼에서만 나는 경고를 끈다.
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

#[derive(Clone)]
pub struct KasaspaceTools {
    backend: Arc<dyn Backend>,
    // Read by the dispatch code that `#[tool_handler]` generates; the
    // Clone derive hides that from dead-code analysis, hence the allow.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

// --- tool argument schemas -------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
struct RenameArgs {
    /// New pane title (header tab label). Keep it short — a task name like
    /// "auth refactor" or "fixing tests" so the user can tell panes apart at
    /// a glance.
    title: String,
    /// Surface id to rename (e.g. "%3"). Defaults to your own pane
    /// ($KASATERM_PANE_ID), so usually you can omit it and just pass `title`.
    surface_id: Option<String>,
    /// If true, also rename the window/session sidebar label, not just the
    /// pane header. Default false.
    window: Option<bool>,
}

// --- helpers ---------------------------------------------------------------

fn ok(text: impl Into<String>) -> Result<CallToolResult, rmcp::ErrorData> {
    Ok(CallToolResult::success(vec![Content::text(text.into())]))
}

/// Tool-level failure: surfaced to the model via isError so it can react,
/// not raised as a JSON-RPC protocol error.
fn fail(text: impl Into<String>) -> Result<CallToolResult, rmcp::ErrorData> {
    Ok(CallToolResult::error(vec![Content::text(text.into())]))
}

// --- tools -----------------------------------------------------------------

#[tool_router]
impl KasaspaceTools {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List kasaterm surfaces (panes) and workspaces. Returns surface ids you pass to other kasaspace tools."
    )]
    async fn kasaspace_list(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let surfaces = match self.backend.list_surfaces() {
            Ok(s) => s,
            Err(e) => return fail(format!("list_surfaces failed: {e}")),
        };
        let workspaces = self.backend.list_workspaces().unwrap_or_default();
        let payload = serde_json::json!({ "surfaces": surfaces, "workspaces": workspaces });
        ok(serde_json::to_string_pretty(&payload).unwrap_or_else(|e| e.to_string()))
    }

    #[tool(description = "List workspaces (cmux workspaces / kasaterm windows).")]
    async fn kasaspace_workspace_list(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.list_workspaces() {
            Ok(ws) => ok(serde_json::to_string_pretty(&ws).unwrap_or_default()),
            Err(e) => fail(format!("workspace_list failed: {e}")),
        }
    }

    #[tool(description = "Get the current (focused) workspace.")]
    async fn kasaspace_workspace_current(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.current_workspace() {
            Ok(Some(ws)) => ok(serde_json::to_string_pretty(&ws).unwrap_or_default()),
            Ok(None) => ok("null"),
            Err(e) => fail(format!("workspace_current failed: {e}")),
        }
    }

    #[tool(
        description = "Rename your kasaterm pane so the user can tell panes apart by what each is doing. Call this when your task focus is clear or changes — e.g. \"auth refactor\", \"fixing tests\". surface_id defaults to your own pane ($KASATERM_PANE_ID); set window=true to also rename the window/session sidebar label."
    )]
    async fn kasaspace_rename(
        &self,
        Parameters(args): Parameters<RenameArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let id = match args
            .surface_id
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()))
        {
            Some(id) => id,
            None => return fail("no surface_id given and KASATERM_PANE_ID is unset"),
        };
        if let Err(e) = self.backend.rename_surface(&id, &args.title) {
            return fail(format!("rename failed: {e}"));
        }
        if args.window.unwrap_or(false) {
            if let Err(e) = self.backend.rename_window(&id, &args.title) {
                return fail(format!("pane renamed but window rename failed: {e}"));
            }
        }
        ok(format!("renamed {id} → {:?}", args.title))
    }

}

#[tool_handler]
impl ServerHandler for KasaspaceTools {}
