//! kasaspace MCP — streamable-HTTP MCP server exposing kasaterm pane
//! control as model-invoked tools, backed directly by the host's
//! `agent_socket::Backend`. Replaces the external python bridge
//! (mcp/kasaspace_mcp.py): same tool surface, but the long-lived Rust
//! host owns the tools so it can later expose GUI state dynamically.

use std::sync::Arc;

use agent_socket::backend::{Backend, SplitDirection};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;

mod git;
mod http;
mod register;
pub use http::spawn_http_server;
pub use register::register_clients;

/// Default header accent for background jobs — calm blue-gray so job
/// panes read as "machine-driven, watch me". Mirrors the python bridge.
const JOB_COLOR: &str = "#5b7fa6";

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
struct DirectionArgs {
    /// Where the new pane opens relative to the current one: left|right|up|down.
    direction: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SurfaceArgs {
    /// Surface id from kasaspace_list.
    surface_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SendArgs {
    /// Text to type into the pane. Add a trailing newline to submit a command.
    text: String,
    /// Optional target surface id; defaults to the focused pane.
    surface_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SendKeyArgs {
    /// Key name: enter|tab|escape|backspace|delete|up|down|left|right.
    key: String,
    /// Optional target surface id; defaults to the focused pane.
    surface_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RenameArgs {
    /// Surface id from kasaspace_list.
    surface_id: String,
    /// New header title for the pane.
    title: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ColorArgs {
    /// Surface id from kasaspace_list.
    surface_id: String,
    /// Accent color as #rrggbb for the pane header band.
    color: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SwapArgs {
    /// First surface id.
    a: String,
    /// Second surface id — its position is swapped with `a`.
    b: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RunJobArgs {
    /// Shell command to run in the new pane (no trailing newline needed).
    command: String,
    /// Short human label for the pane header (e.g. 'build', 'tests').
    title: Option<String>,
    /// Where the job pane opens: left|right|up|down. Defaults to 'down'.
    direction: Option<String>,
    /// Optional #rrggbb accent for the header. Defaults to a blue-gray.
    color: Option<String>,
    /// If true, the pane closes itself when the command finishes
    /// (appends '; exit'). Use for sub-agents that should vanish when done.
    auto_close: Option<bool>,
}

// --- helpers ---------------------------------------------------------------

fn parse_direction(s: &str) -> Result<SplitDirection, String> {
    match s {
        "left" => Ok(SplitDirection::Left),
        "right" => Ok(SplitDirection::Right),
        "up" => Ok(SplitDirection::Up),
        "down" => Ok(SplitDirection::Down),
        other => Err(format!("direction must be left/right/up/down, got {other:?}")),
    }
}

/// Parse `#rrggbb` (alpha forced to 255) into RGBA bytes for Backend::set_color.
fn parse_hex_color(s: &str) -> Result<[u8; 4], String> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return Err(format!("color must be #rrggbb, got {s:?}"));
    }
    let r = u8::from_str_radix(&h[0..2], 16).map_err(|_| format!("bad hex in {s:?}"))?;
    let g = u8::from_str_radix(&h[2..4], 16).map_err(|_| format!("bad hex in {s:?}"))?;
    let b = u8::from_str_radix(&h[4..6], 16).map_err(|_| format!("bad hex in {s:?}"))?;
    Ok([r, g, b, 255])
}

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

    #[tool(description = "Focus a specific kasaterm pane by its surface id.")]
    async fn kasaspace_focus(
        &self,
        Parameters(args): Parameters<SurfaceArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.focus_surface(&args.surface_id) {
            Ok(()) => ok(format!("Focused {}", args.surface_id)),
            Err(e) => fail(format!("focus failed: {e}")),
        }
    }

    #[tool(
        description = "Split the current pane to create a new pane in the given direction (left/right/up/down). Use when work fans out into parallel tracks."
    )]
    async fn kasaspace_split(
        &self,
        Parameters(args): Parameters<DirectionArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let dir = match parse_direction(&args.direction) {
            Ok(d) => d,
            Err(e) => return fail(e),
        };
        match self.backend.split_surface(dir) {
            Ok(surf) => ok(format!(
                "Split {}. New surface: {}",
                args.direction,
                serde_json::to_string(&surf).unwrap_or_default()
            )),
            Err(e) => fail(format!("split failed: {e}")),
        }
    }

    #[tool(
        description = "Send text to a pane (e.g. a shell command — add a trailing newline to run it). Targets the focused pane unless surface_id is given."
    )]
    async fn kasaspace_send(
        &self,
        Parameters(args): Parameters<SendArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.send_text(args.surface_id.as_deref(), &args.text) {
            Ok(()) => ok(format!(
                "Sent {} chars to {}",
                args.text.len(),
                args.surface_id.as_deref().unwrap_or("focused pane")
            )),
            Err(e) => fail(format!("send failed: {e}")),
        }
    }

    #[tool(
        description = "Send a named key (enter/tab/escape/backspace/delete/up/down/left/right) to a pane. Targets the focused pane unless surface_id is given."
    )]
    async fn kasaspace_send_key(
        &self,
        Parameters(args): Parameters<SendKeyArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.send_key(args.surface_id.as_deref(), &args.key) {
            Ok(()) => ok(format!(
                "Sent key {:?} to {}",
                args.key,
                args.surface_id.as_deref().unwrap_or("focused pane")
            )),
            Err(e) => fail(format!("send_key failed: {e}")),
        }
    }

    #[tool(description = "Close (kill) a pane by its surface id, removing it from the layout.")]
    async fn kasaspace_close(
        &self,
        Parameters(args): Parameters<SurfaceArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.close_surface(&args.surface_id) {
            Ok(()) => ok(format!("Closed {}", args.surface_id)),
            Err(e) => fail(format!("close failed: {e}")),
        }
    }

    #[tool(description = "Rename a pane's header title.")]
    async fn kasaspace_rename(
        &self,
        Parameters(args): Parameters<RenameArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.rename_surface(&args.surface_id, &args.title) {
            Ok(()) => ok(format!("Renamed {} -> {:?}", args.surface_id, args.title)),
            Err(e) => fail(format!("rename failed: {e}")),
        }
    }

    #[tool(description = "Set a pane's accent color (header band) as #rrggbb.")]
    async fn kasaspace_set_color(
        &self,
        Parameters(args): Parameters<ColorArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let rgba = match parse_hex_color(&args.color) {
            Ok(c) => c,
            Err(e) => return fail(e),
        };
        match self.backend.set_color(&args.surface_id, rgba) {
            Ok(()) => ok(format!("Set {} color to {}", args.surface_id, args.color)),
            Err(e) => fail(format!("set_color failed: {e}")),
        }
    }

    #[tool(description = "Swap two panes' positions in the layout (content stays, positions trade).")]
    async fn kasaspace_swap(
        &self,
        Parameters(args): Parameters<SwapArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.backend.swap_surfaces(&args.a, &args.b) {
            Ok(()) => ok(format!("Swapped {} <-> {}", args.a, args.b)),
            Err(e) => fail(format!("swap failed: {e}")),
        }
    }

    #[tool(description = "List workspaces (tmux sessions / cmux workspaces).")]
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
        description = "Run a long-running command in a NEW labelled pane so the user watches progress live. Splits a fresh pane, labels its header with the job title + accent color, then types the command. Use for builds, deploys, dev servers, sub-agents. Output stays visual in the pane — it is not streamed back to you."
    )]
    async fn kasaspace_run_job(
        &self,
        Parameters(args): Parameters<RunJobArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let dir = match parse_direction(args.direction.as_deref().unwrap_or("down")) {
            Ok(d) => d,
            Err(e) => return fail(e),
        };
        let surf = match self.backend.split_surface(dir) {
            Ok(s) => s,
            Err(e) => return fail(format!("run_job split failed: {e}")),
        };
        let surface_id = surf.id.clone();
        let title = args
            .title
            .clone()
            .unwrap_or_else(|| args.command.lines().next().unwrap_or("job").chars().take(40).collect());
        let mut labels = Vec::new();

        // Label + color are best-effort; a backend that doesn't support
        // them shouldn't sink the whole job — the pane still runs.
        match self.backend.rename_surface(&surface_id, &title) {
            Ok(()) => labels.push(format!("title={title:?}")),
            Err(e) => labels.push(format!("rename skipped ({e})")),
        }
        let color = args.color.clone().unwrap_or_else(|| JOB_COLOR.to_string());
        match parse_hex_color(&color).and_then(|rgba| {
            self.backend.set_color(&surface_id, rgba).map_err(|e| e.to_string())
        }) {
            Ok(()) => labels.push(format!("color={color}")),
            Err(e) => labels.push(format!("color skipped ({e})")),
        }

        // auto_close appends '; exit' so the shell quits when the command
        // finishes → PTY EOF → kasaterm reaps the pane.
        let mut run_cmd = args.command.trim_end_matches('\n').to_string();
        if args.auto_close.unwrap_or(false) {
            run_cmd.push_str("; exit");
            labels.push("auto_close".into());
        }
        run_cmd.push('\n');
        if let Err(e) = self.backend.send_text(Some(&surface_id), &run_cmd) {
            return fail(format!("run_job send failed: {e}"));
        }
        ok(format!(
            "Started job in pane {surface_id} ({}; {}): {}",
            args.direction.as_deref().unwrap_or("down"),
            labels.join(", "),
            args.command
        ))
    }
}

#[tool_handler]
impl ServerHandler for KasaspaceTools {}
