//! `kasa-serve-web` — the standalone webview backend.
//!
//! kasaterm serves the arona-ui webview + daemon-session endpoints off 8765
//! through `spawn_http_server`; when the terminal exits, the webview loses its
//! backend. This bin runs the SAME http server against a headless
//! `StandaloneBackend`, so the webview keeps showing `claude agents`
//! background sessions and their transcripts with no terminal running.
//!
//!   kasa-serve-web [--cwd <dir>] [--port <n>]   (default port 8766)
//!
//! arona-ui's BASE falls back to this port when 8765 (kasaterm) is down.

use std::path::PathBuf;
use std::sync::Arc;

use kasa_mcp::standalone::StandaloneBackend;

const DEFAULT_PORT: u16 = 8766;

fn main() -> anyhow::Result<()> {
    // claude pane 안에서 손으로 띄우는 경우가 실제로 있다(이사 리허설·수동 배포).
    // 그 env 의 claude 마커를 물려받으면 이 데몬이 낳는 **모든 셸**의 claude 가
    // transcript 저장을 끈다 — 이사(migrate)로 옮겨 온 대화가 그 순간부터 안 남는다.
    // kasaterm 부팅 첫 줄(scrub_inherited_claude_markers, main.rs 목록과 동기)과 같다.
    for k in [
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_TEAMMATE_MODE",
        "CLAUDE_CODE_FORK_SUBAGENT",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_PID",
        "CLAUDECODE",
    ] {
        std::env::remove_var(k);
    }
    let mut cwd: Option<PathBuf> = None;
    let mut port: u16 = DEFAULT_PORT;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cwd" => cwd = it.next().map(PathBuf::from),
            "--port" => {
                if let Some(p) = it.next().and_then(|s| s.parse().ok()) {
                    port = p;
                }
            }
            "-h" | "--help" => {
                eprintln!("usage: kasa-serve-web [--cwd <dir>] [--port <n>]");
                return Ok(());
            }
            _ => {}
        }
    }

    let root = cwd
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let backend = Arc::new(StandaloneBackend::new(root.clone()));
    // Scheduler OFF: firing/consuming ~/.config/kasaterm/schedule.json is kasaterm's
    // job — a headless run bails on delivery yet would still persist items as consumed.
    let bound = kasa_mcp::spawn_http_server_opts(backend, port, false)?;
    // arona-ui probes a FIXED fallback port (VITE_MCP_FALLBACK_PORT, default 8766) with
    // no runtime discovery, so silently landing on an OS-assigned port (spawn_http_server's
    // conflict fallback) would leave the webview blind. Refuse rather than phantom-bind.
    if bound != port {
        eprintln!(
            "kasa-serve-web: port {port} is taken (bound {bound} instead) — arona-ui can't discover that. Free {port} or pass --port <n> matching the webview's fallback port."
        );
        std::process::exit(1);
    }
    eprintln!(
        "kasa-serve-web: standalone webview backend on http://127.0.0.1:{bound}/arona-ui/  (root {})",
        root.display()
    );
    // Park forever — loop because park() is documented to wake spuriously without
    // an unpark; a bare park() could return and tear down the detached http thread.
    loop {
        std::thread::park();
    }
}
