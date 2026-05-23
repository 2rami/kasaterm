//! Stand-alone harness: serve the kasaspace MCP tools over HTTP backed by
//! a no-op mock Backend, so we can verify the MCP handshake + tool surface
//! with curl without launching the full GUI host.
//!
//!   cargo run -p kasaspace-mcp --example serve_mock
//!   curl ... http://127.0.0.1:8765/mcp   (see verify script)

use std::sync::Arc;

use agent_socket::backend::{Backend, SplitDirection, SurfaceInfo, WorkspaceInfo};
use anyhow::Result;

struct MockBackend;

impl Backend for MockBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo { id: "local-0".into(), name: "local".into() }])
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(Some(WorkspaceInfo { id: "local-0".into(), name: "local".into() }))
    }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(vec![SurfaceInfo {
            id: "%1".into(),
            workspace_id: "local-0".into(),
            title: Some("mock".into()),
        }])
    }
    fn focus_surface(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn split_surface(&self, _: SplitDirection) -> Result<SurfaceInfo> {
        Ok(SurfaceInfo { id: "%2".into(), workspace_id: "local-0".into(), title: None })
    }
    fn send_text(&self, _: Option<&str>, _: &str) -> Result<()> {
        Ok(())
    }
    fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> {
        Ok(())
    }
    fn close_surface(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn rename_surface(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn set_color(&self, _: &str, _: [u8; 4]) -> Result<()> {
        Ok(())
    }
    fn swap_surfaces(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}

fn main() -> Result<()> {
    let port = kasaspace_mcp::spawn_http_server(Arc::new(MockBackend), 8765)?;
    eprintln!("mock kasaspace MCP serving on http://127.0.0.1:{port}/mcp");
    std::thread::park();
    Ok(())
}
