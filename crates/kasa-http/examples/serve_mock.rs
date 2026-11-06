//! Local HTTP host with a mock Backend, independent of the GUI.
//! Run with cargo run -p kasa-http --example serve_mock.

use std::sync::Arc;

use kasa_socket::backend::{Backend, SplitDirection, SurfaceInfo, WorkspaceInfo};
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
            cwd: None,
            character: None,
        }])
    }
    fn focus_surface(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn split_surface(&self, _: SplitDirection, _focus: bool, _: Option<&str>) -> Result<SurfaceInfo> {
        Ok(SurfaceInfo { id: "%2".into(), workspace_id: "local-0".into(), title: None, cwd: None, character: None })
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
    let port = kasa_http::spawn_http_server(Arc::new(MockBackend), 8765)?;
    eprintln!("mock kasaspace MCP serving on http://127.0.0.1:{port}/mcp");
    std::thread::park();
    Ok(())
}
