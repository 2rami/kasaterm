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
            // But never clobber a *live* listener: if connect() succeeds
            // another instance owns this path, and removing+rebinding would
            // hijack it (every kasaterm-cli would then hit us instead). Refuse
            // so the caller (resolve_kasaterm_socket_path) keeps us isolated.
            if socket_path.exists() {
                if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                    anyhow::bail!(
                        "socket {socket_path:?} is already owned by a live instance — refusing to hijack"
                    );
                }
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
        #[cfg(unix)]
        spawn_path_watchdog(self.socket_path.clone(), backend.clone());
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

/// 소켓 파일이 사라지면 다시 만든다.
///
/// 유닉스 소켓은 **경로가 곧 주소**라, 파일 노드가 지워지면 프로세스도 listener 도
/// 멀쩡한 채로 아무도 못 붙는 상태가 된다. accept 는 영원히 블록하고 밖에서는
/// 되살릴 방법이 없다 — bind 는 그 프로세스만 할 수 있으니 앱을 껐다 켜는 것 말고는
/// 손이 없다. 붙어 있던 모든 세션이 같이 죽는다.
///
/// 실제로 겪었다(2026-08-10): 테스트 인스턴스를 정리하려던 `rm -f …/kasaterm-*.sock`
/// 한 줄이 살아 있는 앱의 소켓까지 지웠다. 지우는 쪽을 조심하는 것으로는 못 막는다 —
/// 임시 디렉터리는 청소 도구·OS·사람이 다 같이 건드리는 자리다. 그래서 없어지면
/// 다시 만드는 쪽으로 막는다.
///
/// 옛 listener 는 그대로 둔다. 닫을 방법이 마땅치 않고, 아무도 못 붙는 fd 하나는
/// 프로세스가 끝날 때까지 놀고 있을 뿐이다.
#[cfg(unix)]
fn spawn_path_watchdog(path: PathBuf, backend: Arc<dyn Backend>) {
    thread::spawn(move || loop {
        thread::sleep(std::time::Duration::from_secs(5));
        if path.exists() {
            continue;
        }
        match Server::bind(&path) {
            Ok(s) => {
                eprintln!("[agent-socket] socket file vanished — rebound {path:?}");
                // 새 서버가 자기 감시자를 띄운다. 이쪽은 여기서 손을 뗀다.
                let _ = s.spawn(backend);
                return;
            }
            // 남이 그 경로를 가져갔으면 bind 가 거절한다(hijack 방지). 그건 옳은
            // 결과이므로 물러나지 않고 다음 주기에 다시 본다 — 그 인스턴스가 죽으면
            // 우리가 도로 주인이 된다.
            Err(e) => eprintln!("[agent-socket] rebind {path:?} failed: {e:#}"),
        }
    });
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
        fn split_surface(
            &self,
            _: crate::backend::SplitDirection,
            _focus: bool,
            _from: Option<&str>,
        ) -> Result<SurfaceInfo> {
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
