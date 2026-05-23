//! Streamable-HTTP serving glue. The host (kasaterm) is a synchronous
//! winit/wgpu app, so we own a small multi-thread tokio runtime on a
//! dedicated background thread and run axum there. The `Backend` is
//! channel-based and `Send + Sync`, so calling it from async handlers on
//! another thread is safe.

use std::sync::Arc;

use agent_socket::backend::Backend;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpService,
};

use crate::KasaspaceTools;

/// Bind an MCP-over-HTTP server at `127.0.0.1:<port>/mcp` and run it on a
/// background thread. Tries `preferred_port` first, then falls back to an
/// OS-assigned port. Returns the actual port bound so the host can write
/// it into `.mcp.json` / an env var.
pub fn spawn_http_server(
    backend: Arc<dyn Backend>,
    preferred_port: u16,
) -> std::io::Result<u16> {
    // Bind synchronously so we can learn (and return) the real port before
    // handing the socket to tokio.
    let listener = std::net::TcpListener::bind(("127.0.0.1", preferred_port))
        .or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0)))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;

    std::thread::Builder::new()
        .name("kasaspace-mcp-http".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[kasaspace-mcp] tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[kasaspace-mcp] listener convert failed: {e}");
                        return;
                    }
                };
                let service = StreamableHttpService::new(
                    move || Ok(KasaspaceTools::new(backend.clone())),
                    Arc::new(LocalSessionManager::default()),
                    Default::default(),
                );
                let app = axum::Router::new().nest_service("/mcp", service);
                if let Err(e) = axum::serve(tokio_listener, app).await {
                    eprintln!("[kasaspace-mcp] serve error: {e}");
                }
            });
        })?;

    Ok(port)
}
