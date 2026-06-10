//! kasaspace MCP — streamable-HTTP MCP server exposing kasaterm pane
//! control as model-invoked tools, backed directly by the host's
//! `kasa_socket::Backend`. Replaces the external python bridge
//! (mcp/kasa_mcp.py): same tool surface, but the long-lived Rust
//! host owns the tools so it can later expose GUI state dynamically.

use std::sync::Arc;

use kasa_socket::backend::{Backend, PanelKind};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;

pub mod git;
mod http;
mod register;
pub use http::spawn_http_server;
// 호스트(kasaterm)의 첫 실행 온보딩이 GET /mode 와 같은 마커로 판정하게 노출.
pub use http::mode_marker_path;
pub use register::register_clients;

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
struct PanelArgs {
    /// Which panel window: "git" (git status) or "session" (sessions list).
    which: String,
    /// What to do: "open" | "close" | "resize" | "info". "info" returns the
    /// window + webview geometry so you can verify the webview tracks the
    /// window (view_* should equal win_* when responsive).
    action: String,
    /// Width in logical px. Required for "resize".
    w: Option<u32>,
    /// Height in logical px. Required for "resize".
    h: Option<u32>,
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

    #[tool(
        description = "Control a standalone panel window (git status / sessions). action: open|close|resize|info. which: git|session. For resize pass w and h (logical px). info returns window + webview geometry (view_* tracks win_* when the panel is responsive) so you can verify layout without a screenshot."
    )]
    async fn kasaspace_panel(
        &self,
        Parameters(args): Parameters<PanelArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let which = match args.which.as_str() {
            "git" => PanelKind::Git,
            "session" | "sessions" => PanelKind::Session,
            other => return fail(format!("unknown panel {other:?} (expected git|session)")),
        };
        match args.action.as_str() {
            "open" => match self.backend.set_panel(which, true) {
                Ok(()) => ok(format!("Opened {} panel", args.which)),
                Err(e) => fail(format!("open failed: {e}")),
            },
            "close" => match self.backend.set_panel(which, false) {
                Ok(()) => ok(format!("Closed {} panel", args.which)),
                Err(e) => fail(format!("close failed: {e}")),
            },
            "resize" => {
                let (w, h) = match (args.w, args.h) {
                    (Some(w), Some(h)) => (w, h),
                    _ => return fail("resize requires both w and h (logical px)"),
                };
                match self.backend.resize_panel(which, w, h) {
                    Ok(()) => ok(format!("Resized {} panel to {w}x{h}", args.which)),
                    Err(e) => fail(format!("resize failed: {e}")),
                }
            }
            "info" => match self.backend.panel_info(which) {
                Ok(g) => ok(serde_json::to_string_pretty(&g).unwrap_or_else(|e| e.to_string())),
                Err(e) => fail(format!("info failed: {e}")),
            },
            other => fail(format!("unknown action {other:?} (open|close|resize|info)")),
        }
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

}

#[tool_handler]
impl ServerHandler for KasaspaceTools {}
