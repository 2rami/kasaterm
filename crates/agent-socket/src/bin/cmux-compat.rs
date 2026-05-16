//! Thin CLI that wraps the cmux-compatible JSON-RPC protocol. Mirrors
//! the subset of cmux's official CLI commands that we use for testing,
//! scripting, and the eventual claude-code teammateMode handshake.
//!
//! Subcommands map 1:1 to protocol methods:
//!
//!   cmux-compat ping
//!   cmux-compat capabilities
//!   cmux-compat identify
//!   cmux-compat list workspaces|surfaces
//!   cmux-compat focus  <surface_id>
//!   cmux-compat split  <left|right|up|down>
//!   cmux-compat send   <text>                  # writes to focused pane
//!   cmux-compat send   <surface_id> <text>     # writes to specific pane
//!   cmux-compat key    <enter|tab|...>
//!
//! Socket path resolution mirrors what the host exports:
//!   $TMUXIFY_SOCKET_PATH > $CMUX_SOCKET_PATH > /tmp/cmux.sock
//!
//! The CLI prints the raw JSON response to stdout — scripts pipe it
//! through `jq`. Exit code is 0 on `ok: true`, 1 on `ok: false`, 2 on
//! a transport / framing error.

use agent_socket::protocol::{Request, Response};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

fn main() {
    match run() {
        Ok(Some(resp)) => {
            // Print the wire response so scripts can `| jq .result`.
            println!("{}", serde_json::to_string(&resp).unwrap());
            std::process::exit(if resp.ok { 0 } else { 1 });
        }
        Ok(None) => {} // help / version path — already printed.
        Err(e) => {
            eprintln!("cmux-compat: {e:#}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<Option<Response>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_help();
        return Ok(None);
    }
    let cmd = args.remove(0);
    let request = build_request(&cmd, &args)?;
    let socket_path = resolve_socket_path()?;
    let response = roundtrip(&socket_path, &request)?;
    Ok(Some(response))
}

fn print_help() {
    eprintln!("cmux-compatible JSON-RPC CLI for tmuxify / agent-socket\n");
    eprintln!("Usage:");
    eprintln!("  cmux-compat ping");
    eprintln!("  cmux-compat capabilities");
    eprintln!("  cmux-compat identify");
    eprintln!("  cmux-compat list <workspaces|surfaces>");
    eprintln!("  cmux-compat focus <surface_id>");
    eprintln!("  cmux-compat split <left|right|up|down>");
    eprintln!("  cmux-compat send  <text>");
    eprintln!("  cmux-compat send  --surface <id> <text>");
    eprintln!("  cmux-compat key   <enter|tab|escape|backspace|delete|up|down|left|right>");
    eprintln!();
    eprintln!("Socket: $TMUXIFY_SOCKET_PATH > $CMUX_SOCKET_PATH > /tmp/cmux.sock");
}

fn build_request(cmd: &str, args: &[String]) -> Result<Request> {
    // Caller-supplied id so async clients can correlate; we just stamp
    // a process-id-based string for the CLI path where nobody cares.
    let id = json!(format!("cli-{}", std::process::id()));
    let (method, params): (&str, Value) = match cmd {
        "ping" => ("system.ping", json!({})),
        "capabilities" => ("system.capabilities", json!({})),
        "identify" => ("system.identify", json!({})),
        "list" => {
            let what = args
                .first()
                .ok_or_else(|| anyhow!("list needs `workspaces` or `surfaces`"))?;
            match what.as_str() {
                "workspaces" => ("workspace.list", json!({})),
                "surfaces" => ("surface.list", json!({})),
                other => return Err(anyhow!("unknown list target: {other}")),
            }
        }
        "focus" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("focus needs a surface_id"))?;
            ("surface.focus", json!({ "surface_id": surface }))
        }
        "split" => {
            let dir = args
                .first()
                .ok_or_else(|| anyhow!("split needs a direction"))?;
            ("surface.split", json!({ "direction": dir }))
        }
        "send" => {
            // Two argument shapes:
            //   send <text>
            //   send --surface <id> <text>
            let (surface, text) = if args.first().is_some_and(|a| a == "--surface") {
                let surface = args
                    .get(1)
                    .ok_or_else(|| anyhow!("--surface needs an id"))?
                    .clone();
                let text = args
                    .get(2..)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("send needs a text payload"))?
                    .join(" ");
                (Some(surface), text)
            } else {
                let text = args
                    .first()
                    .ok_or_else(|| anyhow!("send needs a text payload"))?
                    .clone();
                (None, text)
            };
            let mut params = json!({ "text": text });
            if let Some(s) = surface {
                params["surface_id"] = json!(s);
            }
            ("surface.send_text", params)
        }
        "key" => {
            let key = args
                .first()
                .ok_or_else(|| anyhow!("key needs a key name"))?;
            ("surface.send_key", json!({ "key": key }))
        }
        other => return Err(anyhow!("unknown command: {other}")),
    };
    Ok(Request {
        id,
        method: method.to_string(),
        params,
    })
}

fn resolve_socket_path() -> Result<String> {
    std::env::var("TMUXIFY_SOCKET_PATH")
        .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
        .or_else(|_| Ok("/tmp/cmux.sock".to_string()))
}

fn roundtrip(socket_path: &str, request: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket_path)
        .with_context(|| format!("connect to {socket_path:?}"))?;
    let mut writer = stream.try_clone().context("clone stream")?;
    let mut payload = serde_json::to_string(request).context("serialize request")?;
    payload.push('\n');
    writer
        .write_all(payload.as_bytes())
        .context("write request")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).context("read response")?;
    if line.is_empty() {
        return Err(anyhow!("server closed connection without a response"));
    }
    let resp: Response =
        serde_json::from_str(line.trim()).context("parse response JSON")?;
    Ok(resp)
}
