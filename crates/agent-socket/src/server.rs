//! Listener thread + per-connection reader. Owns the local socket, the
//! accept loop, and the connect → newline-delimited JSON → dispatch
//! pipeline.
//!
//! Transport is abstracted behind `crate::transport::{LocalListener,
//! LocalStream}` — Unix domain socket on Unix, Windows named pipe on
//! Windows — so the framing and dispatch layers below don't care.

use crate::backend::Backend;
use crate::methods::dispatch;
use crate::protocol::{codes, Request, Response};
use crate::transport::{LocalListener, LocalStream};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

pub struct Server {
    socket_path: PathBuf,
    listener: LocalListener,
}

impl Server {
    /// Bind to `socket_path`. On Unix the path is taken verbatim and a
    /// stale socket file at that location is removed first. On Windows
    /// the final path component is used as the named-pipe name.
    pub fn bind(socket_path: impl Into<PathBuf>) -> Result<Self> {
        let socket_path = socket_path.into();
        #[cfg(unix)]
        {
            // Stale socket from a previous crash — UnixListener::bind
            // would EADDRINUSE on top of an existing file, so clear it.
            if socket_path.exists() {
                let _ = std::fs::remove_file(&socket_path);
            }
            if let Some(parent) = socket_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir for {socket_path:?}"))?;
            }
        }
        let listener = LocalListener::bind(&socket_path)
            .with_context(|| format!("binding to {socket_path:?}"))?;
        Ok(Self {
            socket_path,
            listener,
        })
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Spawn the accept loop on a background thread. Each incoming
    /// connection gets its own thread for line-delimited request
    /// handling. Backend is `Arc`-shared so multiple concurrent clients
    /// see the same terminal state.
    pub fn spawn(self, backend: Arc<dyn Backend>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for incoming in self.listener.incoming() {
                let stream = match incoming {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[agent-socket] accept failed: {e}");
                        continue;
                    }
                };
                let backend = backend.clone();
                thread::spawn(move || handle_client(stream, backend));
            }
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort cleanup so we don't leave a stale socket file
        // behind on Unix. Windows named pipes vanish when their handle
        // closes, so nothing to do there.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn handle_client(stream: LocalStream, backend: Arc<dyn Backend>) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[agent-socket] clone stream for write failed: {e}");
            return;
        }
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[agent-socket] read line failed: {e}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(backend.as_ref(), req),
            Err(e) => Response::error(
                serde_json::Value::Null,
                codes::PARSE_ERROR,
                format!("invalid JSON: {e}"),
            ),
        };
        let mut payload = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[agent-socket] serialize response failed: {e}");
                continue;
            }
        };
        payload.push('\n');
        if let Err(e) = writer.write_all(payload.as_bytes()) {
            eprintln!("[agent-socket] write response failed: {e}");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{SurfaceInfo, WorkspaceInfo};
    use std::io::Read;

    struct PingOnlyBackend;
    impl Backend for PingOnlyBackend {
        fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
            Ok(vec![])
        }
        fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
            Ok(None)
        }
        fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
            Ok(vec![])
        }
        fn focus_surface(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn split_surface(&self, _: crate::backend::SplitDirection) -> Result<SurfaceInfo> {
            anyhow::bail!("not implemented")
        }
        fn send_text(&self, _: Option<&str>, _: &str) -> Result<()> {
            Ok(())
        }
        fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn temp_socket_path() -> PathBuf {
        // Unix gets a real file under $TMPDIR; Windows gets a name that
        // ends up wrapped as \\.\pipe\<name> by the transport layer.
        let name = format!("agent-socket-test-{}.sock", std::process::id());
        std::env::temp_dir().join(name)
    }

    #[test]
    fn ping_round_trip_over_real_socket() {
        // Bind a real local socket, send a single ping request frame,
        // read the response. Catches problems in the framing /
        // dispatch loop that the in-process method tests can't hit.
        let path = temp_socket_path();
        let server = Server::bind(&path).expect("bind");
        let backend: Arc<dyn Backend> = Arc::new(PingOnlyBackend);
        let _join = server.spawn(backend);
        // Tiny pause to let the accept loop arm. The accept call
        // doesn't need the listener thread fully scheduled, but the
        // alternative (poll for socket-ready) is not worth the noise.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut client = LocalStream::connect(&path).expect("connect");
        client
            .write_all(b"{\"id\":\"r1\",\"method\":\"system.ping\"}\n")
            .expect("write");
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).expect("read");
        let line = std::str::from_utf8(&buf[..n]).expect("utf8");
        let resp: Response = serde_json::from_str(line.trim()).expect("parse");
        assert!(resp.ok, "ping should succeed: {resp:?}");
        assert_eq!(resp.id, serde_json::json!("r1"));
        assert_eq!(resp.result.unwrap(), serde_json::json!({"pong": true}));
    }
}
